use crate::app_error::AppError;
use crate::locale::LOCALES;
use crate::models::user::{AuthSession, Language, User};
use crate::web::context::CommonContext;
use anyhow;
use anyhow::Result;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse};
use axum::{
    async_trait,
    extract::FromRequestParts,
    http::{
        header::{HeaderValue, ACCEPT_LANGUAGE},
        request::Parts,
    },
};
use data_encoding::BASE64URL_NOPAD;
use uuid::Uuid;

use fluent::bundle::FluentBundle;
use fluent::FluentResource;
use fluent_langneg::convert_vec_str_to_langids_lossy;
use fluent_langneg::negotiate_languages;
use fluent_langneg::parse_accepted_languages;
use fluent_langneg::NegotiationStrategy;
use intl_memoizer::concurrent::IntlLangMemoizer;
use minijinja::context;

use super::state::AppState;

pub mod about;
pub mod account;
pub mod activitypub;
pub mod admin;
pub mod auth;
pub mod collaborate;
pub mod collaborate_cleanup;
pub mod community;
pub mod devices;
pub mod draw;
pub mod tag;
pub mod home;
pub mod identity;
pub mod notifications;
pub mod password_reset;
pub mod policy;
pub mod post;
pub mod privacy;
pub mod profile;
pub mod report;
pub mod search;
pub mod store;
pub mod supporter;
pub mod well_known;

pub async fn handler_404(
    auth_session: AuthSession,
    ExtractFtlLang(ftl_lang): ExtractFtlLang,
    State(state): State<AppState>,
) -> Result<impl IntoResponse, AppError> {
    // The header counts only exist for signed-in users, and this handler
    // absorbs every bot scan for /wp-admin and friends — so don't open a
    // transaction we have nothing to ask.
    let (draft_post_count, unread_notification_count) = match auth_session.user.as_ref() {
        Some(user) => {
            let mut tx = state.db_pool.begin().await?;
            let common_ctx = CommonContext::build(&mut tx, Some(user.id)).await?;
            (
                common_ctx.draft_post_count,
                common_ctx.unread_notification_count,
            )
        }
        None => (0, 0),
    };

    let template: minijinja::Template<'_, '_> = state.env.get_template("404.jinja")?;
    let rendered: String = template.render(context! {
        current_user => auth_session.user,
        draft_post_count,
        unread_notification_count,
        ftl_lang
    })?;

    Ok((StatusCode::NOT_FOUND, Html(rendered)).into_response())
}

/// Liveness/readiness probe. Checks that a connection can actually be taken
/// from the pool and used, so a wedged or exhausted pool fails the check
/// instead of reporting healthy because the process is still running.
pub async fn health(State(state): State<AppState>) -> impl IntoResponse {
    match sqlx::query_scalar::<_, i32>("SELECT 1")
        .fetch_one(&state.db_pool)
        .await
    {
        Ok(_) => (StatusCode::OK, "ok").into_response(),
        Err(e) => {
            tracing::error!("health check failed: {}", e);
            (StatusCode::SERVICE_UNAVAILABLE, "database unavailable").into_response()
        }
    }
}

pub async fn render_403(
    auth_session: &AuthSession,
    state: &AppState,
    ftl_lang: String,
) -> Result<impl IntoResponse, AppError> {
    let db = &state.db_pool;
    let mut tx = db.begin().await?;

    let common_ctx =
        CommonContext::build(&mut tx, auth_session.user.as_ref().map(|u| u.id)).await?;

    let template: minijinja::Template<'_, '_> = state.env.get_template("403.jinja")?;
    let rendered: String = template.render(context! {
        current_user => auth_session.user,
        draft_post_count => common_ctx.draft_post_count,
        unread_notification_count => common_ctx.unread_notification_count,
        ftl_lang
    })?;

    Ok((StatusCode::FORBIDDEN, Html(rendered)).into_response())
}

pub fn detect_preferred_language(accept_language: &HeaderValue) -> Option<Language> {
    let header_str = accept_language.to_str().ok()?;
    let requested = parse_accepted_languages(header_str);
    let available = convert_vec_str_to_langids_lossy(["ko", "ja", "en", "zh"]);

    let supported = negotiate_languages(
        &requested,
        &available,
        None, // No default - if no match, return None
        NegotiationStrategy::Filtering,
    );

    let lang_code = supported.first().map(|l| l.language.as_str())?;

    match lang_code {
        "ko" => Some(Language::Ko),
        "ja" => Some(Language::Ja),
        "en" => Some(Language::En),
        "zh" => Some(Language::Zh),
        _ => None, // No match - return None instead of defaulting
    }
}

pub struct ExtractAcceptLanguage(HeaderValue);

#[async_trait]
impl<S> FromRequestParts<S> for ExtractAcceptLanguage
where
    S: Send + Sync,
{
    type Rejection = (StatusCode, &'static str);

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        if let Some(accept_language) = parts.headers.get(ACCEPT_LANGUAGE) {
            Ok(ExtractAcceptLanguage(accept_language.clone()))
        } else {
            Ok(ExtractAcceptLanguage(HeaderValue::from_static("")))
        }
    }
}

/// Extractor that provides the computed locale string for templates
pub struct ExtractFtlLang(pub String);

#[async_trait]
impl<S> FromRequestParts<S> for ExtractFtlLang
where
    S: Send + Sync,
{
    type Rejection = (StatusCode, &'static str);

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        // Extract Accept-Language header
        let accept_language = if let Some(accept_language) = parts.headers.get(ACCEPT_LANGUAGE) {
            accept_language.clone()
        } else {
            HeaderValue::from_static("")
        };

        // Extract AuthSession to get user preferences
        let auth_session = AuthSession::from_request_parts(parts, state)
            .await
            .map_err(|_| {
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Failed to extract auth session",
                )
            })?;

        // Get user's preferred language
        let user_preferred_language = auth_session
            .user
            .as_ref()
            .and_then(|u| u.preferred_language.clone());

        // Get the bundle and extract locale
        let bundle = get_bundle(&accept_language, user_preferred_language);
        let ftl_lang = bundle
            .locales
            .first()
            .map(|l| l.to_string())
            .unwrap_or_else(|| "en".to_string());

        Ok(ExtractFtlLang(ftl_lang))
    }
}

/// Extractor that admits only site-wide admins.
///
/// Every handler that reads through the normal visibility rules (private
/// communities, drafts, soft-deleted posts) takes this, so the unfiltered
/// queries in `models::admin` are unreachable without it.
pub struct AdminUser(pub User);

#[async_trait]
impl<S> FromRequestParts<S> for AdminUser
where
    S: Send + Sync,
{
    type Rejection = AppError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let auth_session = AuthSession::from_request_parts(parts, state)
            .await
            .map_err(|_| AppError::Anyhow(anyhow::anyhow!("Failed to extract auth session")))?;

        let user = auth_session.user.ok_or(AppError::Unauthorized)?;

        if !user.is_admin() {
            return Err(AppError::Forbidden);
        }

        Ok(AdminUser(user))
    }
}

pub(crate) fn get_bundle(
    accept_language: &HeaderValue,
    user_preferred_language: Option<Language>,
) -> FluentBundle<&FluentResource, IntlLangMemoizer> {
    match user_preferred_language {
        Some(lang) => {
            let language = match lang {
                Language::Ko => "ko",
                Language::Ja => "ja",
                Language::En => "en",
                Language::Zh => "zh",
            };
            let ftl = LOCALES
                .get(language)
                .or_else(|| LOCALES.get("en"))
                .expect("English locale must exist");

            let lang_id = language
                .parse()
                .expect("Hardcoded language string should parse");
            let mut bundle = FluentBundle::new_concurrent(vec![lang_id]);
            bundle.add_resource(ftl).expect("Failed to add a resource.");

            bundle
        }
        None => {
            // Fallback to "en" if header is not valid UTF-8
            let header_str = accept_language.to_str().unwrap_or("en");
            let requested = parse_accepted_languages(header_str);
            let available = convert_vec_str_to_langids_lossy(["ko", "ja", "en", "zh"]);
            let default = "en".parse().expect("Failed to parse a langid.");

            let supported = negotiate_languages(
                &requested,
                &available,
                Some(&default),
                NegotiationStrategy::Filtering,
            );

            let lang_code = supported
                .first()
                .map(|l| l.language.as_str())
                .unwrap_or("en");

            let ftl = LOCALES
                .get(lang_code)
                .or_else(|| LOCALES.get("en"))
                .expect("English locale must exist");

            let lang_id = lang_code.parse().expect("Negotiated language should parse");
            let mut bundle = FluentBundle::new_concurrent(vec![lang_id]);
            bundle.add_resource(ftl).expect("Failed to add a resource.");

            bundle
        }
    }
}

/// Parse ID from URL path, supporting both UUID format and legacy base64 format.
/// Returns either the parsed UUID, a redirect response for legacy URLs, or an error response.
pub enum ParsedId {
    Uuid(Uuid),
    Redirect(axum::response::Redirect),
    InvalidId(axum::response::Response),
}

pub fn parse_id_with_legacy_support(
    id_str: &str,
    base_path: &str,
    state: &crate::web::state::AppState,
) -> Result<ParsedId, AppError> {
    // First try to parse as UUID directly
    if let Ok(uuid) = Uuid::parse_str(id_str) {
        return Ok(ParsedId::Uuid(uuid));
    }

    // If that fails, try to decode as base64 and then parse as UUID
    match BASE64URL_NOPAD.decode(id_str.as_bytes()) {
        Ok(decoded_bytes) => {
            // Try to parse bytes directly as UUID (16 bytes expected)
            if decoded_bytes.len() == 16 {
                if let Ok(uuid) = Uuid::from_slice(&decoded_bytes) {
                    // Create redirect to UUID version
                    let redirect_url = format!("{}/{}", base_path, uuid);
                    return Ok(ParsedId::Redirect(axum::response::Redirect::permanent(
                        &redirect_url,
                    )));
                }
            }
        }
        Err(_) => {
            // Not valid base64, continue to error handling
        }
    }

    // If neither UUID nor base64 decoding worked, render custom error page
    match state.env.get_template("invalid_id_error.jinja") {
        Ok(template) => {
            match template.render(context! {}) {
                Ok(rendered) => {
                    let response = axum::response::Html(rendered).into_response();
                    return Ok(ParsedId::InvalidId(response));
                }
                Err(_) => {
                    // If template rendering fails, fall back to generic error
                }
            }
        }
        Err(_) => {
            // If template not found, fall back to generic error
        }
    }

    // Fallback to generic error if template rendering fails
    Err(AppError::from(anyhow::anyhow!("Invalid ID format")))
}

/// Helper function to safely get a Fluent message without panicking
/// Returns the translation key itself if the message is not found
pub fn safe_get_message(
    bundle: &FluentBundle<&FluentResource, IntlLangMemoizer>,
    key: &str,
) -> String {
    let message = match bundle.get_message(key) {
        Some(msg) => msg,
        None => {
            // Log missing translation key to Sentry
            sentry::capture_message(
                &format!("Missing translation key: {}", key),
                sentry::Level::Warning,
            );
            return key.to_string();
        }
    };

    let pattern = match message.value() {
        Some(p) => p,
        None => {
            // Log translation key with no value to Sentry
            sentry::capture_message(
                &format!("Translation key {} has no value", key),
                sentry::Level::Warning,
            );
            return key.to_string();
        }
    };

    let mut errors = vec![];
    let formatted = bundle.format_pattern(pattern, None, &mut errors);

    if !errors.is_empty() {
        // Log formatting errors to Sentry
        sentry::capture_message(
            &format!("Error formatting {}: {:?}", key, errors),
            sentry::Level::Warning,
        );
        return key.to_string();
    }

    formatted.to_string()
}

/// Helper function to safely format a Fluent message with arguments
/// Returns the translation key itself if the message is not found
pub fn safe_format_message(
    bundle: &FluentBundle<&FluentResource, IntlLangMemoizer>,
    key: &str,
    args: Option<&fluent::FluentArgs>,
) -> String {
    let message = match bundle.get_message(key) {
        Some(msg) => msg,
        None => {
            // Log missing translation key to Sentry
            sentry::capture_message(
                &format!("Missing translation key: {}", key),
                sentry::Level::Warning,
            );
            return key.to_string();
        }
    };

    let pattern = match message.value() {
        Some(p) => p,
        None => {
            // Log translation key with no value to Sentry
            sentry::capture_message(
                &format!("Translation key {} has no value", key),
                sentry::Level::Warning,
            );
            return key.to_string();
        }
    };

    let mut errors = vec![];
    let formatted = bundle.format_pattern(pattern, args, &mut errors);

    if !errors.is_empty() {
        // Log formatting errors to Sentry
        sentry::capture_message(
            &format!("Error formatting {}: {:?}", key, errors),
            sentry::Level::Warning,
        );
        return key.to_string();
    }

    formatted.to_string()
}

/// Helper function to safely parse a UUID string
pub fn safe_parse_uuid(s: &str) -> Result<Uuid, AppError> {
    Uuid::parse_str(s).map_err(|e| AppError::InvalidUuid(format!("{}: {}", s, e)))
}

/// Helper function to safely decode a hex hash string
pub fn safe_decode_hash(s: &str) -> Result<Vec<u8>, AppError> {
    data_encoding::HEXLOWER
        .decode(s.as_bytes())
        .map_err(|e| AppError::InvalidHash(format!("{}: {}", s, e)))
}

/// Helper function to safely parse an email address
pub fn safe_parse_email(s: &str) -> Result<lettre::Address, AppError> {
    s.parse()
        .map_err(|e| AppError::InvalidEmail(format!("{}: {}", s, e)))
}

#[cfg(test)]
pub(crate) mod test_support {
    //! Shared minijinja environment for template render tests.
    //!
    //! Must mirror the setup in `main.rs` — most importantly the autoescape
    //! callback, or tests would pass while production rendered unescaped.

    use minijinja::{path_loader, Environment, State};
    use std::path::PathBuf;

    pub fn env() -> Environment<'static> {
        let mut env = Environment::new();
        env.set_auto_escape_callback(|_| minijinja::AutoEscape::Html);
        minijinja_contrib::add_to_environment(&mut env);
        env.add_filter("cachebuster", |value: String| value);
        env.add_filter("markdown", |value: String| value);
        // The real filter: it is pure, so tests render what production does.
        env.add_filter("ago", crate::relative_time::ago_filter);
        env.add_function("ftl_get_message", |_state: &State, id: String| id);
        // The real function interpolates the arguments into the locale's
        // pattern. This stub has no bundle to interpolate into, so it appends
        // them as `id(name=value)` instead of dropping them: with the arguments
        // discarded, a template that passes the wrong variable — or none —
        // renders byte for byte like one that passes the right one, and no test
        // can tell the difference.
        env.add_function(
            "ftl_format_pattern",
            |_state: &State, id: String, args: minijinja::Value| {
                let mut pairs: Vec<String> = Vec::new();
                if let Ok(keys) = args.try_iter() {
                    for key in keys {
                        if let Ok(value) = args.get_item(&key) {
                            pairs.push(format!("{key}={value}"));
                        }
                    }
                }
                // Argument order is not guaranteed; sort so assertions are stable.
                pairs.sort();
                if pairs.is_empty() {
                    id
                } else {
                    format!("{id}({})", pairs.join(","))
                }
            },
        );
        env.add_global("r2_public_endpoint_url", "https://example.test");
        env.add_global("base_url", "https://oeee.test");
        env.set_loader(path_loader(
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("templates"),
        ));
        env
    }
}

#[cfg(test)]
mod social_meta_tests {
    //! The link-preview card lives in `base.jinja` and is overridden per page.
    //! These render the real templates because the failure mode is silent —
    //! a typo'd variable renders as an empty `content=""`, not an error.

    use super::test_support;
    use minijinja::context;
    use serde_json::json;

    fn chrome() -> minijinja::Value {
        context! {
            current_user => json!(null),
            messages => Vec::<serde_json::Value>::new(),
            draft_post_count => 0,
            unread_notification_count => 0,
            ftl_lang => "en",
        }
    }

    /// The `<head>` with runs of whitespace collapsed. Templates are formatted
    /// by djlint, which wraps long tags across lines, so asserting on raw
    /// output would break on reformatting rather than on behaviour. Scoping to
    /// the head also keeps body text from satisfying a meta-tag assertion.
    fn head(rendered: &str) -> String {
        let start = rendered.find("<head>").expect("page has a head");
        let end = rendered.find("</head>").expect("head is closed");
        rendered[start..end]
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
    }

    #[test]
    fn html_lang_names_the_language_the_page_was_rendered_in() {
        // This used to read `ftl_get_message('lang')`, and no locale defines a
        // `lang` message, so every page shipped `<html lang="lang">` — the
        // fallback for a missing key is the key itself, which is silent.
        for lang in ["ko", "ja", "en", "zh"] {
            let env = test_support::env();
            let rendered = env
                .get_template("404.jinja")
                .expect("404 template loads")
                .render(context! { ftl_lang => lang, ..chrome() })
                .expect("404 renders");

            assert!(
                rendered.contains(&format!(r#"<html lang="{}">"#, lang)),
                "expected the document to declare {lang}, got: {}",
                &rendered[..rendered.find("<head>").unwrap_or(80)]
            );
        }
    }

    #[test]
    fn pages_without_an_override_get_the_site_card() {
        let env = test_support::env();
        let rendered = env
            .get_template("404.jinja")
            .expect("404 template loads")
            .render(chrome())
            .expect("404 renders");

        let head = head(&rendered);
        assert!(head.contains(r#"<meta property="og:title" content="brand" />"#));
        assert!(head.contains(r#"<meta property="og:description" content="about" />"#));
        assert!(
            head.contains(r#"<meta property="og:url" content="https://oeee.test/" />"#),
            "site card should point at the site root"
        );
        assert!(head.contains(r#"<meta name="twitter:card" content="summary" />"#));
    }

    #[test]
    fn public_community_gets_a_card_and_stays_indexable() {
        let env = test_support::env();
        let rendered = env
            .get_template("community.jinja")
            .expect("community template loads")
            .render(context! {
                community => json!({
                    "id": "00000000-0000-0000-0000-000000000001",
                    "name": "Open Studio",
                    "description": "Draw with us",
                    "slug": "open",
                    "visibility": "public",
                    "owner_id": "00000000-0000-0000-0000-000000000002",
                }),
                community_id => "00000000-0000-0000-0000-000000000001",
                domain => "oeee.test",
                feed => context! { posts => Vec::<serde_json::Value>::new(), has_more => false },
                ..chrome()
            })
            .expect("community renders");

        let head = head(&rendered);
        assert!(head.contains(r#"<meta property="og:title" content="Open Studio" />"#));
        assert!(head.contains(r#"<meta property="og:url" content="https://oeee.test/@open" />"#));
        assert!(
            !head.contains("noindex"),
            "a public community should be indexable"
        );
    }

    #[test]
    fn private_community_is_noindexed_and_leaks_no_preview() {
        let env = test_support::env();
        let rendered = env
            .get_template("community.jinja")
            .expect("community template loads")
            .render(context! {
                community => json!({
                    "id": "00000000-0000-0000-0000-000000000001",
                    "name": "Secret Studio",
                    "description": "Members only",
                    "slug": "secret",
                    "visibility": "private",
                    "owner_id": "00000000-0000-0000-0000-000000000002",
                }),
                community_id => "00000000-0000-0000-0000-000000000001",
                domain => "oeee.test",
                feed => context! { posts => Vec::<serde_json::Value>::new(), has_more => false },
                ..chrome()
            })
            .expect("community renders");

        let head = head(&rendered);
        assert!(head.contains(r#"<meta name="robots" content="noindex, nofollow" />"#));
        assert!(
            !head.contains("og:title"),
            "a private community must not emit a preview card"
        );
        assert!(
            !head.contains("Members only"),
            "the description must not leak into meta tags"
        );
    }

    #[test]
    fn profile_card_uses_the_banner_when_there_is_one() {
        let env = test_support::env();
        let user = json!({
            "id": "00000000-0000-0000-0000-000000000001",
            "login_name": "artist",
            "display_name": "An Artist",
            "created_at": "2024-03-05T12:00:00Z",
        });
        let ctx = context! {
            user => user,
            domain => "oeee.test",
            banner => json!({
                "image_filename": "abcdef.png",
                "width": 200,
                "height": 40,
            }),
            followings => Vec::<serde_json::Value>::new(),
            links => Vec::<serde_json::Value>::new(),
            public_community_posts => Vec::<serde_json::Value>::new(),
            private_community_posts => Vec::<serde_json::Value>::new(),
            is_following => false,
            ..chrome()
        };

        let rendered = env
            .get_template("profile.jinja")
            .expect("profile template loads")
            .render(ctx)
            .expect("profile renders");

        let head = head(&rendered);
        assert!(head.contains(r#"<meta property="og:title" content="An Artist (@artist)" />"#));
        assert!(head.contains(r#"<meta property="og:url" content="https://oeee.test/@artist" />"#));
        assert!(
            head.contains(
                r#"<meta property="og:image" content="https://example.test/image/ab/abcdef.png" />"#
            ),
            "the banner is the profile's own image and should be the preview"
        );
    }
}

#[cfg(test)]
mod community_page_tests {
    use super::test_support;
    use minijinja::context;
    use serde_json::json;

    /// Saving or cancelling the edit form asks for the header block alone, and
    /// those handlers pass no feed. Reaching a block still walks the template
    /// around it, so the drawing grid below has to survive the missing value —
    /// the swap 500s if the grid reaches into `feed` without checking.
    #[test]
    fn the_header_block_renders_without_a_feed() {
        let env = test_support::env();
        let rendered = env
            .get_template("community.jinja")
            .expect("community template loads")
            .eval_to_state(context! {
                current_user => json!(null),
                community => json!({
                    "id": "00000000-0000-0000-0000-000000000001",
                    "name": "Open Studio",
                    "description": "Draw with us",
                    "slug": "open",
                    "visibility": "public",
                    "owner_id": "00000000-0000-0000-0000-000000000002",
                }),
                community_id => "00000000-0000-0000-0000-000000000001",
                domain => "oeee.test",
                ftl_lang => "en",
            })
            .expect("template evaluates")
            .render_block("community_edit_block")
            .expect("the header block renders on its own");

        assert!(rendered.contains("Open Studio"));
        assert!(
            !rendered.contains("posts-grid"),
            "the block is the header only"
        );
    }

    /// The header says who keeps the community and how much is in it, and
    /// drawing is its primary action: the painter's choices are asked for in
    /// a dialog when it is pressed, rather than sitting open on the page.
    #[test]
    fn the_header_credits_the_owner_and_drawing_opens_a_dialog() {
        let env = test_support::env();
        let community = |background: serde_json::Value| {
            json!({
                "id": "00000000-0000-0000-0000-000000000001",
                "name": "Open Studio",
                "description": "Draw with us",
                "slug": "open",
                "visibility": "public",
                "owner_id": "00000000-0000-0000-0000-000000000002",
                "background_color": background,
                "foreground_color": "#000000",
            })
        };
        let render = |community: serde_json::Value| {
            env.get_template("community.jinja")
                .expect("community template loads")
                .render(context! {
                    current_user => json!({"id": "00000000-0000-0000-0000-000000000003"}),
                    messages => Vec::<serde_json::Value>::new(),
                    draft_post_count => 0,
                    unread_notification_count => 0,
                    community => community,
                    header => json!({
                        "owner": {"login_name": "keeper", "display_name": "The Keeper"},
                        "posts_count": 12,
                        "contributors_count": 4,
                    }),
                    community_id => "00000000-0000-0000-0000-000000000001",
                    domain => "oeee.test",
                    feed => json!({"posts": [], "has_more": false}),
                    ftl_lang => "en",
                })
                .expect("community renders")
        };

        let rendered = render(community(json!(null)));
        assert!(rendered.contains("The Keeper"));
        assert!(rendered.contains("12 community-stats-posts"));
        let dialog = rendered
            .find("id=\"community-draw-modal\"")
            .expect("the drawing dialog");
        let size = rendered.find("community-draw-size").expect("size choice");
        assert!(size > dialog, "the drawing form is back on the page");
        assert!(rendered.contains("/collaborate?community=open"));

        // Two-tone: the dialog asks for an orientation, and there is no
        // drawing together to offer.
        let rendered = render(community(json!("#ffffff")));
        assert!(rendered.contains("name=\"orientation\""));
        assert!(rendered.contains("community-colors"));
        assert!(!rendered.contains("/collaborate?community=open"));
    }

    /// Cancel and Save swap the edit card back out for the header. The
    /// delete area was a sibling of the form, so it outlived the swap and
    /// stayed on the page under the restored header; now the one element
    /// that is swapped holds all of it.
    #[test]
    fn the_edit_card_is_one_swappable_element() {
        let env = test_support::env();
        for visibility in ["public", "private"] {
            let rendered = env
                .get_template("community_edit.jinja")
                .expect("edit template loads")
                .render(context! {
                    community => json!({
                        "name": "Open Studio",
                        "slug": "open",
                        "description": "Draw with us",
                        "visibility": visibility,
                    }),
                    community_id => "00000000-0000-0000-0000-000000000001",
                    ftl_lang => "en",
                })
                .expect("edit form renders");
            let rendered = rendered.trim();
            assert!(rendered.ends_with("</section>"), "something follows the card");
            assert_eq!(rendered.matches("<section").count(), 1);
            assert!(rendered.contains("hx-target:inherited=\"this\""));
            assert!(rendered.contains("delete-community-btn"));
            assert_eq!(
                rendered.contains("name=\"visibility\" value=\"private\""),
                visibility == "private"
            );
        }
    }

    /// Drafts are drawn in the same grid, with the same Per row, as every
    /// other page of drawings; each leads to publishing it, and says how
    /// long ago it was last touched rather than printing a timestamp.
    #[test]
    fn drafts_share_the_grid_and_its_control() {
        let env = test_support::env();
        let updated = (chrono::Utc::now() - chrono::Duration::hours(3)).to_rfc3339();
        let rendered = env
            .get_template("draft_posts.jinja")
            .expect("drafts template loads")
            .render(context! {
                current_user => json!({"login_name": "someone"}),
                messages => Vec::<serde_json::Value>::new(),
                draft_post_count => 1,
                unread_notification_count => 0,
                ftl_lang => "en",
                r2_public_endpoint_url => "https://example.test",
                posts => vec![json!({
                    "id": "00000000-0000-0000-0000-000000000001",
                    "title": null,
                    "content": null,
                    "community_id": null,
                    "community_name": null,
                    "image_filename": "abcdef.png",
                    "image_width": 300,
                    "image_height": 300,
                    "updated_at": updated,
                })],
            })
            .expect("drafts render");
        assert!(rendered.contains("class=\"posts-grid\" id=\"post-feed-grid\""));
        assert!(rendered.contains("data-per-row"));
        assert!(rendered.contains("&#x2f;posts&#x2f;00000000-0000-0000-0000-000000000001&#x2f;publish")
            || rendered.contains("/posts/00000000-0000-0000-0000-000000000001/publish"));
        assert!(rendered.contains(">3h<"), "not a relative time");
    }

    /// A guestbook's list and its empty note are one or the other by CSS
    /// (`:empty`), so the list has to be empty to the letter when it has no
    /// entries, and an entry has to bring no whitespace around it -- or the
    /// last one deleted would leave a blank card and no note.
    #[test]
    fn a_guestbook_list_is_empty_to_the_letter() {
        let env = test_support::env();
        let render = |entries: Vec<serde_json::Value>| {
            env.get_template("guestbook.jinja")
                .expect("loads")
                .render(context! {
                    user => json!({"login_name": "oeee", "display_name": "오이", "id": "u1"}),
                    current_user => json!(null),
                    messages => Vec::<serde_json::Value>::new(),
                    draft_post_count => 0,
                    unread_notification_count => 0,
                    ftl_lang => "en",
                    guestbook_entries => entries,
                })
                .expect("renders")
        };
        let empty = render(vec![]);
        assert!(empty.contains(r#"id="guestbook-entries"></div>"#), "the list is not :empty");
        assert!(empty.contains("guestbook-empty"));
        let entry = json!({
            "id": "e1", "author_id": "u2", "recipient_id": "u1",
            "author_login_name": "someone", "author_display_name": "Someone",
            "content": "hi", "reply": null,
            "created_at": chrono::Utc::now().to_rfc3339(),
        });
        let one = render(vec![entry.clone()]);
        assert!(one.contains(r#"id="guestbook-entries"><div class="guestbook-entry">"#));
        let alone = env
            .get_template("guestbook_entry.jinja")
            .expect("loads")
            .render(context! { entry, user => json!({"login_name": "oeee"}), current_user => json!(null), ftl_lang => "en" })
            .expect("renders");
        assert!(alone.starts_with("<div") && alone.ends_with("</div>"), "{alone:?}");
    }

    /// Throwing a draft away answers with what else changes, out of band:
    /// the count always, and at the last one the empty state in the grid's
    /// place and the per-row control gone. Ids the drafts page carries.
    #[test]
    fn a_thrown_away_draft_updates_the_count_and_empties_the_page() {
        let env = test_support::env();
        let oob = env.get_template("draft_delete_oob.jinja").expect("loads");
        let some = oob.render(context! { remaining => 2, ftl_lang => "en" }).expect("renders");
        assert!(some.contains(r#"<span id="drafts-count" hx-swap-oob="true">2</span>"#));
        assert!(!some.contains("drafts-body"), "drafts left, the grid stays");
        let none = oob.render(context! { remaining => 0, ftl_lang => "en" }).expect("renders");
        assert!(none.contains(r#"<div id="drafts-body" hx-swap-oob="true">"#));
        assert!(none.contains("draft-empty"));
        assert!(none.contains(r#"<div id="drafts-tools" class="drafts-tools" hx-swap-oob="true"></div>"#));

        let page = env
            .get_template("draft_posts.jinja")
            .expect("loads")
            .render(context! {
                current_user => json!({"login_name": "someone"}),
                messages => Vec::<serde_json::Value>::new(),
                draft_post_count => 0,
                unread_notification_count => 0,
                ftl_lang => "en",
                posts => Vec::<serde_json::Value>::new(),
            })
            .expect("renders");
        for id in ["drafts-count", "drafts-tools", "drafts-body"] {
            assert!(page.contains(&format!(r#"id="{id}""#)), "the page has no #{id}");
        }
    }

    /// The grid is the shared feed fragment, so its sentinel points wherever
    /// the handler said — and it must be this community's endpoint rather than
    /// the home feed's, or scrolling a community page loads the front page.
    #[test]
    fn the_grid_continues_from_the_communitys_own_endpoint() {
        use crate::models::post::SerializablePostForHome;
        use crate::web::handlers::home::{feed_context, HOME_POSTS_PER_BATCH};

        // A full batch, because that is what tells the feed there is more.
        let posts = (0..HOME_POSTS_PER_BATCH)
            .map(|i| SerializablePostForHome {
                id: uuid::Uuid::from_u128(i as u128 + 1),
                title: Some(format!("Drawing {i}")),
                author_id: uuid::Uuid::from_u128(999),
                user_login_name: "artist".to_string(),
                paint_duration: "0".to_string(),
                stroke_count: 1,
                viewer_count: 0,
                image_filename: "abcdef.png".to_string(),
                image_width: 300,
                image_height: 300,
                replay_filename: None,
                is_sensitive: false,
                community_slug: Some("open".to_string()),
                community_name: Some("Open Studio".to_string()),
                published_at: Some(chrono::Utc::now()),
                created_at: chrono::Utc::now(),
                updated_at: chrono::Utc::now(),
            })
            .collect();

        let env = test_support::env();
        let rendered = env
            .get_template("community.jinja")
            .expect("community template loads")
            .render(context! {
                current_user => json!(null),
                messages => Vec::<serde_json::Value>::new(),
                draft_post_count => 0,
                unread_notification_count => 0,
                community => json!({
                    "id": "00000000-0000-0000-0000-000000000001",
                    "name": "Open Studio",
                    "description": "Draw with us",
                    "slug": "open",
                    "visibility": "public",
                    "owner_id": "00000000-0000-0000-0000-000000000002",
                }),
                community_id => "00000000-0000-0000-0000-000000000001",
                domain => "oeee.test",
                feed => feed_context(posts, "/api/communities/@open/posts", 0),
                ftl_lang => "en",
            })
            .expect("community renders");

        // Minijinja escapes the slashes and the ampersand in an attribute; the
        // browser reads them back as the URL, so assert against that.
        let links_in = rendered.replace("&#x2f;", "/").replace("&amp;", "&");
        assert!(
            links_in.contains(&format!(
                r#"hx-get="/api/communities/@open/posts?offset={}&limit={}""#,
                HOME_POSTS_PER_BATCH, HOME_POSTS_PER_BATCH
            )),
            "the sentinel should ask this community for the next batch"
        );
        assert!(
            rendered.contains(r#"id="post-feed-grid""#),
            "the column control drives the grid by id"
        );
    }
}

#[cfg(test)]
mod locale_tests {
    use super::{get_bundle, Language};
    use axum::http::header::HeaderValue;

    /// Building a bundle panics on a duplicate message id, and it happens per
    /// request — a duplicate key takes every page down with a 502 while
    /// `cargo check` and every template test stay green, because the template
    /// tests stub ftl_get_message and never load the real bundles.
    #[test]
    fn every_locale_bundle_builds() {
        let empty = HeaderValue::from_static("");
        for lang in [Language::Ko, Language::Ja, Language::En, Language::Zh] {
            let bundle = get_bundle(&empty, Some(lang.clone()));
            assert!(
                !bundle.locales.is_empty(),
                "{lang:?} bundle built with no locale"
            );
        }
        // The header-negotiated path builds its own bundle; cover it too.
        for header in ["ko", "ja", "en", "zh", "", "xx"] {
            let value = HeaderValue::from_str(header).expect("valid header");
            let _ = get_bundle(&value, None);
        }
    }
}

#[cfg(test)]
mod template_tests {
    //! Templates are loaded and evaluated at runtime, so `cargo check` says
    //! nothing about them and a mistake only surfaces when someone requests the
    //! page. These close that gap in two tiers: every template has to parse,
    //! and the ones with fixtures have to actually render.
    //!
    //! Parsing alone would not have caught the outage these were written for --
    //! `{{ post.image_width + 24 }}`, where the context hands templates strings
    //! and minijinja refuses to add a number to one. Only rendering catches
    //! that, which is why the fixtures mirror the real context's types rather
    //! than using conveniently-typed stand-ins.

    use super::test_support;
    use minijinja::context;
    use serde_json::json;
    use std::collections::BTreeSet;
    use std::path::PathBuf;

    fn template_dir() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("templates")
    }

    fn template_names() -> BTreeSet<String> {
        let mut names = BTreeSet::new();
        for entry in std::fs::read_dir(template_dir()).expect("templates directory") {
            let path = entry.expect("directory entry").path();
            if path.extension().and_then(|e| e.to_str()) == Some("jinja") {
                names.insert(
                    path.file_name()
                        .and_then(|n| n.to_str())
                        .expect("template file name")
                        .to_string(),
                );
            }
        }
        names
    }

    #[test]
    fn every_template_parses() {
        let env = test_support::env();
        let names = template_names();
        assert!(
            names.len() > 20,
            "expected to find the template set, found {}",
            names.len()
        );

        for name in &names {
            if let Err(error) = env.get_template(name) {
                panic!("{name} does not parse: {error:#}");
            }
        }
    }

    fn chrome() -> minijinja::Value {
        context! {
            current_user => json!(null),
            messages => Vec::<serde_json::Value>::new(),
            draft_post_count => 0,
            unread_notification_count => 0,
            ftl_lang => "en",
        }
    }

    /// Shaped like what the replay handler passes: a map whose values are all
    /// strings, including the dimensions. Anything that does arithmetic on
    /// those has to coerce first.
    fn replay_post() -> serde_json::Value {
        json!({
            "id": "9c881320-2b43-4afa-b2bb-7128c8a3e985",
            "title": "Tandemaus",
            "content": "a description",
            "image_width": "640",
            "image_height": "480",
            "image_filename": "abcdef0123.png",
            "replay_filename": "30ca3f590dda85e21dbc94250199a692b4fa5c7d626ea3445acef3bcf3c1338a.pch",
            "published_at": "2025-03-26 21:15:04",
            "paint_duration": "00:14:58",
            "community_slug": "tegaki",
            "community_name": "Tegaki",
            "login_name": "someone",
        })
    }

    /// The post page's own view of a post, string-valued like the real
    /// context, with the replay switch and author left to the caller.
    fn post_page(allow_replay: &str, author_id: &str) -> serde_json::Value {
        json!({
            "id": "9c881320-2b43-4afa-b2bb-7128c8a3e985",
            "author_id": author_id,
            "title": "Tandemaus",
            "content": "a description",
            "image_width": "640",
            "image_height": "480",
            "image_filename": "abcdef0123.png",
            "image_tool": "neo-cucumber",
            "replay_filename": "30ca3f590dda85e21dbc94250199a692b4fa5c7d626ea3445acef3bcf3c1338a.pch",
            "published_at": "2025-03-26 21:15:04",
            "paint_duration": "00:14:58",
            "viewer_count": "3",
            "allow_relay": "true",
            "allow_replay": allow_replay,
            "login_name": "someone",
            "display_name": "Someone",
        })
    }

    fn render_post_page(allow_replay: &str, author_id: &str, viewer: serde_json::Value) -> String {
        let env = test_support::env();
        env.get_template("post_view.jinja")
            .unwrap_or_else(|e| panic!("post_view.jinja loads: {e:#}"))
            .render(context! {
                post => post_page(allow_replay, author_id),
                post_id => "9c881320-2b43-4afa-b2bb-7128c8a3e985",
                current_user => viewer,
                r2_public_endpoint_url => "https://images.example",
                base_url => "https://oeee.example",
                domain => "oeee.example",
                comments => Vec::<serde_json::Value>::new(),
                collaborative_participants => Vec::<serde_json::Value>::new(),
                reaction_counts => Vec::<serde_json::Value>::new(),
                tags => Vec::<serde_json::Value>::new(),
                child_posts => Vec::<serde_json::Value>::new(),
                post_community => json!(null),
                parent_post_data => json!(null),
                ..chrome()
            })
            .unwrap_or_else(|e| panic!("post_view.jinja renders: {e:#}"))
    }

    /// Posting a comment swaps the list for the one the server sends back.
    /// The form used to sit inside the swapped element, so it went with it
    /// and a second comment needed a reload; it has to sit outside it.
    #[test]
    fn the_comment_form_survives_posting_a_comment() {
        let author = "b95e3d1e-5a25-4d0a-9d3a-3a0b0a9b1c2d";
        let viewer = json!({"id": "0d2a2b4c-7e8f-4a1b-8c9d-1e2f3a4b5c6d", "role": "user", "login_name": "viewer"});
        let rendered = render_post_page("true", author, viewer);
        let list = rendered.find("id=\"comments\"").expect("comment list");
        let form = rendered.find("id=\"comment-form\"").expect("comment form");
        assert!(form > list, "the form comes after the list");
        let list_end = rendered[list..].find("</div>").map(|i| list + i).unwrap();
        assert!(form > list_end, "the form is inside the swapped list");
        assert!(rendered.contains("hx-target=\"#comments\" hx-swap=\"innerHTML\""));
    }

    /// A NEO drawing's replay controls are under it from the start, and the
    /// drawing itself no longer leads to relaying it -- the Relay button does.
    #[test]
    fn a_replay_is_on_the_stage_and_the_drawing_is_not_a_link() {
        let author = "b95e3d1e-5a25-4d0a-9d3a-3a0b0a9b1c2d";
        let rendered = render_post_page("true", author, json!(null));
        assert!(rendered.contains("id=\"post-stage-replay\""));
        assert!(rendered.contains("data-poster="));
        let stage = rendered.find("class=\"post-stage\"").expect("stage");
        let side = rendered.find("class=\"post-side\"").expect("side");
        assert!(
            !rendered[stage..side].contains("/relay"),
            "the drawing links to relaying it again"
        );
    }

    /// The blur on sensitive drawings is one CSS rule, and a stylesheet
    /// edit elsewhere once took it out with the rules around it: for a
    /// day every sensitive drawing showed in the grids unblurred, and
    /// nothing failed. The card puts the class on the image; this holds the
    /// rule that makes it mean something.
    #[test]
    fn sensitive_drawings_stay_blurred() {
        let css = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("static/style.css"),
        )
        .expect("style.css reads");
        let rule = css
            .split(".sensitive {")
            .nth(1)
            .and_then(|rest| rest.split('}').next())
            .expect("a .sensitive rule");
        assert!(rule.contains("filter: blur("), "the .sensitive rule no longer blurs");
        let card = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("templates/post_card.jinja"),
        )
        .expect("post_card.jinja reads");
        assert!(card.contains("sensitive{% endif %}"), "the card no longer marks sensitive drawings");
    }

    /// The words the apps say over the page (app_bridge.jinja) come from the
    /// site's catalogues, and a missing one is not an error anywhere: the
    /// real ftl_get_message answers with the message's id, which an app
    /// would put in a dialog as it is. So every one is looked for in every
    /// language, and the head that carries them is rendered.
    #[test]
    fn the_apps_are_given_their_words_in_every_language() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        // app_sign_in.jinja says the one thing the page says for the Steam app.
        let bridge = ["app_bridge.jinja", "app_sign_in.jinja"]
            .map(|name| std::fs::read_to_string(root.join("templates").join(name)).unwrap())
            .join("\n");
        let ids: Vec<&str> = bridge
            .split("ftl_get_message(\"")
            .skip(1)
            .filter_map(|rest| rest.split('"').next())
            .collect();
        assert!(ids.len() >= 12, "{ids:?}");
        for lang in ["en", "ko", "ja", "zh"] {
            let ftl = std::fs::read_to_string(root.join(format!("locales/{lang}.ftl"))).unwrap();
            for id in &ids {
                assert!(
                    ftl.lines()
                        .any(|line| line.starts_with(&format!("{id} = "))),
                    "{id} is missing from {lang}.ftl"
                );
            }
        }

        let head = test_support::env()
            .get_template("theme_head.jinja")
            .unwrap()
            .render(context! { ftl_lang => "en" })
            .unwrap();
        // A string the page hands a script is a JSON literal, not text
        // pasted between quotes.
        assert!(head.contains(r#"leaveTitle: "app-leave-title","#), "{head}");
        assert!(head.contains("window.oeeeApp.signIn = {"));
        assert!(head.contains(r#"OeeeCafe((?: (?:platform|store)\/\w+)+)"#));
    }

    /// What app_bridge.jinja tells the apps is read from marks the templates
    /// make, and an app sees nothing wrong when one goes missing: it is just
    /// told 0, or nothing. So the marks are pinned here.
    #[test]
    fn the_apps_are_told_the_unread_count_and_who_is_signed_in() {
        let env = test_support::env();
        let bell = |count: i64| {
            env.get_template("nav_notifications.jinja")
                .unwrap()
                .render(context! { unread_notification_count => count, ftl_lang => "en" })
                .unwrap()
        };
        let three = bell(3);
        assert!(three.contains(r#"data-unread="3""#), "{three}");
        assert!(bell(0).contains(r#"data-unread="0""#));

        let toolbar = |current_user: serde_json::Value| {
            env.get_template("toolbar.jinja")
                .unwrap()
                .render(context! { current_user, ..chrome() })
                .unwrap()
        };
        assert!(!toolbar(json!(null)).contains("data-signed-in"));
        let signed_in = toolbar(json!({
            "id": "9c881320-2b43-4afa-b2bb-7128c8a3e985",
            "login_name": "reader",
            "display_name": "Reader",
            "email_verified_at": "2026-01-01",
        }));
        assert!(signed_in.contains(r#"data-tauri-drag-region="deep" data-signed-in"#));
        // What the apps call on the page is on one object (app_bridge.jinja);
        // the toolbar adds its commands to it.
        assert!(signed_in.contains("window.oeeeApp.command = command;"));
    }

    /// Only a drawing that is not blurred is offered to an app's long-press
    /// menu, whose preview would show it as it is.
    #[test]
    fn only_drawings_shown_as_they_are_are_offered_to_the_apps() {
        let card = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("templates/post_card.jinja"),
        )
        .expect("post_card.jinja reads");
        assert!(card.contains("{% if not post.is_sensitive and not admin %}data-oeee-drawing{% endif %}"));
    }

    /// Signing up asks for agreement to the two pages it links, and the box
    /// is required in the page as the handler requires it on the server.
    #[test]
    fn signup_asks_for_agreement_to_the_guidelines_and_privacy_policy() {
        let env = test_support::env();
        let rendered = env
            .get_template("signup.jinja")
            .expect("signup loads")
            .render(context! {
                current_user => json!(null),
                messages => Vec::<serde_json::Value>::new(),
                next => "/collaborate",
                ftl_lang => "en",
            })
            .expect("signup renders");
        assert!(rendered.contains(r#"<input type="checkbox" name="agree" value="1" required />"#));
        assert!(rendered.contains(r#"href="/policy""#) || rendered.contains("&#x2f;policy"));
        assert!(rendered.contains(r#"href="/privacy""#) || rendered.contains("&#x2f;privacy"));
        assert!(rendered.contains("signup-agree"));
        // Signing in instead keeps where the reader was going.
        assert!(rendered.contains("/login?next="));
    }

    /// Signing up after signing in with Steam: a handle and a name, the
    /// agreement, no password -- and a way to sign into an existing account
    /// instead, which keeps where the reader was going.
    #[test]
    fn the_welcome_page_asks_for_a_handle_and_the_agreement() {
        let env = test_support::env();
        let render = |provider_name: serde_json::Value, error: serde_json::Value| {
            env.get_template("identity_welcome.jinja")
                .expect("welcome loads")
                .render(context! {
                    provider => "Steam",
                    provider_name,
                    login_name => "",
                    display_name => "오이",
                    error,
                    next => "/collaborate",
                    ..chrome()
                })
                .expect("welcome renders")
        };

        let rendered = render(json!("오이"), json!(null));
        assert!(rendered.contains(r#"name="login_name""#));
        assert!(rendered.contains(r#"value="오이""#));
        assert!(rendered.contains(r#"<input type="checkbox" name="agree" value="1" required />"#));
        assert!(!rendered.contains(r#"type="password""#));
        assert!(rendered.contains("identity-welcome-body(name=오이,provider=Steam)"));
        assert!(rendered.contains("/login?next="));
        assert!(rendered.contains(r#"action="/auth/cancel""#));
        assert!(!rendered.contains("auth-error"));

        let rendered = render(json!(null), json!("This username is already taken."));
        assert!(rendered.contains("identity-welcome-body-unnamed(provider=Steam)"));
        assert!(rendered.contains("This username is already taken."));
    }

    #[test]
    fn signing_in_offers_steam_only_where_it_is_on_and_says_what_it_will_link() {
        let env = test_support::env();
        let render = |steam_enabled: bool, linking_provider: serde_json::Value| {
            env.get_template("login.jinja")
                .expect("login loads")
                .render(context! {
                    next => "/draw",
                    steam_enabled,
                    linking_provider,
                    ..chrome()
                })
                .expect("login renders")
        };

        let off = render(false, json!(null));
        assert!(!off.contains(r#"href="/auth/steam/app"#));
        assert!(!off.contains("identity-login-notice"));

        let on = render(true, json!(null));
        assert!(on.contains("auth-steam"));
        assert!(on.contains("/auth/steam/app?next="));

        // Signing in to claim a Steam account: say so, offer a way out, and
        // do not offer Steam again.
        let linking = render(true, json!("Steam"));
        assert!(linking.contains("identity-login-notice(provider=Steam)"));
        assert!(linking.contains(r#"action="/auth/cancel""#));
        assert!(!linking.contains(r#"href="/auth/steam/app"#));
    }

    #[test]
    fn signing_in_offers_apple_only_where_it_is_on() {
        let env = test_support::env();
        let render = |apple_enabled: bool, linking_provider: serde_json::Value| {
            env.get_template("login.jinja")
                .expect("login loads")
                .render(context! {
                    next => "/draw",
                    apple_enabled,
                    linking_provider,
                    ..chrome()
                })
                .expect("login renders")
        };
        assert!(!render(false, json!(null)).contains(r#"href="/auth/apple"#));
        let on = render(true, json!(null));
        assert!(on.contains("auth-apple"));
        assert!(on.contains("/auth/apple?next="));
        assert!(on.contains("sign-in-with-apple"));
        assert!(!render(true, json!("Apple")).contains(r#"href="/auth/apple"#));
    }

    #[test]
    fn signing_in_offers_google_only_where_it_is_on() {
        let env = test_support::env();
        let render = |google_enabled: bool, linking_provider: serde_json::Value| {
            env.get_template("login.jinja")
                .expect("login loads")
                .render(context! {
                    next => "/draw",
                    google_enabled,
                    linking_provider,
                    ..chrome()
                })
                .expect("login renders")
        };
        assert!(!render(false, json!(null)).contains(r#"href="/auth/google"#));
        let on = render(true, json!(null));
        assert!(on.contains("auth-google"));
        assert!(on.contains("/auth/google?next="));
        assert!(on.contains("sign-in-with-google"));
        assert!(!render(true, json!("Google")).contains(r#"href="/auth/google"#));
        // Google's kit has its button in English only; elsewhere the words
        // are the site's own, beside the kit's G.
        assert!(on.contains("/static/signin/google-light.svg"));
        let ko = env
            .get_template("login.jinja")
            .expect("login loads")
            .render(context! { google_enabled => true, ftl_lang => "ko", ..chrome() })
            .expect("login renders");
        assert!(ko.contains("/static/signin/google-mark.svg"));
        assert!(!ko.contains("google-light.svg"));
    }

    /// Apple's answer, posted on from this site: every field it carried, as
    /// a value and never as markup.
    #[test]
    fn apples_answer_is_posted_on_as_it_came() {
        let env = test_support::env();
        let rendered = env
            .get_template("identity_apple_return.jinja")
            .expect("return page loads")
            .render(context! {
                answer => json!({
                    "state": "the-state",
                    "id_token": "a.b.c",
                    "user": r#"{"name":{"firstName":"\"><script>"}}"#,
                    "error": null,
                }),
                ftl_lang => "en",
            })
            .expect("return page renders");
        assert!(rendered.contains(r#"action="/auth/apple""#));
        assert!(rendered.contains(r#"name="state" value="the-state""#));
        assert!(rendered.contains(r#"name="id_token" value="a.b.c""#));
        assert!(rendered.contains(r#"name="user""#));
        assert!(!rendered.contains("<script>\""));
        assert!(!rendered.contains(r#"name="error""#));
        assert_eq!(rendered.matches("<script").count(), 1);
        // `no-referrer` would send the post with `Origin: null`, which
        // from_this_site turns away.
        assert!(!rendered.contains("no-referrer"));
    }

    /// Only a supporter is asked whether to be in the credits, and the box
    /// says what they chose.
    #[test]
    fn a_supporter_chooses_whether_to_be_credited() {
        let render = |show_in_credits: serde_json::Value| {
            render_account(show_in_credits, json!(["steam"]), json!("steam"))
        };
        let squash = |html: String| html.split_whitespace().collect::<Vec<_>>().join(" ");
        let listed = squash(render(json!(true)));
        assert!(listed.contains(r#"action="/account/credits""#));
        assert!(listed.contains(r#"value="on" checked"#));
        let hidden = squash(render(json!(false)));
        assert!(hidden.contains(r#"action="/account/credits""#));
        assert!(!hidden.contains(r#"name="show_in_credits" id="show_in_credits" value="on" checked"#));
        assert!(!render(json!(null)).contains("/account/credits"));
    }

    /// The account page as the handler renders it, for a supporter who
    /// bought this year's pack on `supporter_platforms` and wears
    /// `worn_mark`.
    fn render_account(
        show_in_credits: serde_json::Value,
        supporter_platforms: serde_json::Value,
        worn_mark: serde_json::Value,
    ) -> String {
        test_support::env()
            .get_template("account.jinja")
            .expect("account loads")
            .render(context! {
                current_user => json!({
                    "id": "b95e3d1e-5a25-4d0a-9d3a-3a0b0a9b1c2d",
                    "login_name": "oeee",
                    "display_name": "오이",
                    "email": null,
                    "email_verified_at": null,
                    "created_at": "2026-09-22T00:00:00Z",
                    "preferred_language": null,
                    "show_sensitive_content": false,
                    "role": "user",
                }),
                languages => vec![("ko", "한국어"), ("en", "English")],
                identities => json!([{"provider": "steam", "display_hint": "오이", "subject": "76561197960287930"}]),
                has_password => true,
                show_in_credits,
                supporter_platforms,
                worn_mark,
                steam_enabled => true,
                steam_linked => true,
                ..chrome()
            })
            .expect("account renders")
    }

    /// The Supporter Pack's page: what there is to press depends on who is
    /// reading and on which store their app sells through. The handler
    /// chooses the buttons (handlers/supporter.rs) -- the reader's store's
    /// products on sale this year -- and the page draws exactly those, with
    /// Restore only for the App Store.
    #[test]
    fn the_supporter_page_offers_what_there_is_to_buy() {
        let env = test_support::env();
        let render = |current_user: serde_json::Value,
                      store: serde_json::Value,
                      offers: serde_json::Value,
                      nothing_this_year: bool,
                      supporter_standings: serde_json::Value| {
            env.get_template("supporter.jinja")
                .expect("supporter loads")
                .render(context! {
                    this_year => 2026,
                    restorable => store == json!("apple"),
                    store,
                    offers,
                    nothing_this_year,
                    supports_this_year => supporter_standings
                        .as_array()
                        .is_some_and(|standings| standings.iter().any(|s| s["year"] == json!(2026))),
                    supporter_standings,
                    worn_mark => json!("steam"),
                    ..context! { current_user, ..chrome() }
                })
                .expect("supporter renders")
        };
        let signed_in = json!({"login_name": "oeee", "display_name": "오이"});
        let none = json!(null);
        let pack = |product: &str, label: Option<&str>| json!({"product": product, "label": label});

        // Signed out: somewhere to sign in, and nothing to buy with.
        let out = render(
            none.clone(),
            json!("apple"),
            json!([]),
            false,
            json!([]),
        );
        assert!(out.contains(r#"href="/login?next=/supporter""#));
        // The buttons themselves, not the script that listens for them.
        assert!(!out.contains(r#"data-product="#));
        assert!(!out.contains(r#"class="ds-button supporter-restore""#));

        // In the App Store: a button per product, in its own words where it
        // has some, and Restore.
        let apple = render(
            signed_in.clone(),
            json!("apple"),
            json!([
                pack("cafe.oeee.supporter.2026", None),
                pack("cafe.oeee.supporter.2026.more", Some("Support twice as much")),
            ]),
            false,
            json!([]),
        );
        let squashed = apple.split_whitespace().collect::<Vec<_>>().join(" ");
        assert_eq!(apple.matches(r#"class="ds-button ds-button-primary supporter-buy""#).count(), 2);
        assert!(squashed.contains(r#"data-product="cafe.oeee.supporter.2026">supporter-pack-buy(year=2026)<span"#));
        assert!(squashed.contains(r#"data-product="cafe.oeee.supporter.2026.more">Support twice as much<span"#));
        assert!(apple.contains(r#"class="ds-button supporter-restore""#));
        // Room for the price the app will fill in, keyed by the product it
        // belongs to, and empty until then.
        assert!(squashed
            .contains(r#"<span class="supporter-price" data-product="cafe.oeee.supporter.2026"></span>"#));
        assert!(apple.contains("(app.store = app.store || {}).prices = "));
        assert!(!apple.contains("supporter-pack-none"));

        // On Steam, and in the Microsoft Store: no Restore, which only the
        // App Store has.
        for store in ["steam", "microsoft"] {
            let page = render(
                signed_in.clone(),
                json!(store),
                json!([pack("481", None)]),
                false,
                json!([]),
            );
            assert!(page.contains(r#"data-product="481""#), "{store}");
            assert!(!page.contains(r#"class="ds-button supporter-restore""#), "{store}");
        }

        // A browser: no store, no button, and the line saying where the
        // pack is sold.
        let browser = render(signed_in.clone(), none.clone(), json!([]), false, json!([]));
        assert!(!browser.contains(r#"data-product="#));
        assert!(!browser.contains(r#"class="ds-button supporter-restore""#));
        assert!(browser.contains("supporter-pack-elsewhere"));

        // Nothing for this year yet: the page says so instead.
        let nothing = render(signed_in.clone(), json!("steam"), json!([]), true, json!([]));
        assert!(!nothing.contains(r#"data-product="#));
        assert!(nothing.contains("supporter-pack-none(year=2026)"));

        // Already bought, and the years before it: the handler offers
        // nothing more in the store it was bought in.
        let owned = render(
            signed_in,
            json!("apple"),
            json!([]),
            false,
            json!([
                {"store": "steam", "year": 2025, "since": "2025-03-02T00:00:00Z"},
                {"store": "apple", "year": 2026, "since": "2026-01-08T00:00:00Z"},
                {"store": "microsoft", "year": 2026, "since": "2026-02-08T00:00:00Z"},
            ]),
        );
        assert!(owned.contains("supporter-pack-have(year=2026)"));
        assert!(!owned.contains(r#"data-product="#));
        assert!(!owned.contains("supporter-pack-none"));
        assert_eq!(owned.matches("supporter-chip").count(), 3, "one per year");
        assert!(owned.contains("🎮</span>2025"));
        assert!(owned.contains("🍎</span>2026"));
        assert!(owned.contains("🛍️</span>2026"));
    }

    /// The heart in the toolbar is there when this deployment sells a pack
    /// at all *and* the reader is signed in -- the pack is bought against an
    /// account. Which store they are in front of hides or shows it before
    /// paint, and is not the template's business.
    #[test]
    fn the_toolbar_has_a_heart_only_for_a_signed_in_reader_where_a_pack_is_sold() {
        let env = test_support::env();
        let render = |on_sale: serde_json::Value, current_user: serde_json::Value| {
            env.get_template("toolbar.jinja")
                .expect("toolbar loads")
                .render(context! {
                    supporter_packs_on_sale => on_sale,
                    current_user,
                    ..chrome()
                })
                .expect("toolbar renders")
        };
        let reader = json!({"login_name": "oeee", "display_name": "오이"});

        let selling = render(json!(true), reader.clone());
        assert!(selling.contains(r#"class="toolbar-square toolbar-button toolbar-supporter" href="/supporter""#));

        // Nothing to sell: no heart for anyone.
        assert!(!render(json!(false), reader.clone()).contains("toolbar-supporter"));
        assert!(!render(json!(null), reader).contains("toolbar-supporter"));

        // Selling, but nobody to sell to yet: /supporter is still there to
        // read, it is just not in the bar.
        assert!(!render(json!(true), json!(null)).contains("toolbar-supporter"));
    }

    /// Search in the bar is a form now, not a link to the page that holds
    /// one: a field the glass opens, because the glass is its label. One
    /// field and no button, which is what lets Enter submit it, and no
    /// link to /search left in the bar to go there instead.
    #[test]
    fn the_bar_holds_the_search_field_rather_than_a_way_to_one() {
        let env = test_support::env();
        let bar = env
            .get_template("toolbar.jinja")
            .expect("toolbar loads")
            .render(chrome())
            .expect("toolbar renders");

        assert!(bar.contains(r#"class="toolbar-search" method="get" action="/search""#));
        // htmx 4 answers a boosted form's submit by fetching the page and
        // swapping nothing in, so this one is not boosted -- the bar would
        // sit there looking as though the search had not been pressed.
        assert!(bar.contains(r#"role="search" hx-boost="false""#));
        assert!(bar.contains(r#"for="toolbar-search-field""#), "the glass labels it");
        assert!(
            bar.contains(r#"<span class="toolbar-search-pill">"#),
            "the glass and the field share the pill that opens"
        );
        assert!(bar.contains(r#"id="toolbar-search-field""#));
        assert!(bar.contains(r#"type="search""#) && bar.contains(r#"name="q""#));
        assert!(
            !bar.contains(r#"href="/search""#),
            "going to /search for a field is the thing this replaces"
        );
    }

    /// What the site is stays somewhere a reader would look for it. Signed
    /// in that is the foot of the account menu, under their own name;
    /// signed out there is no such menu, and the only square at the bar's
    /// end is the theme's — so it is a button in the bar, not a line under
    /// a sun. `/about` is reachable from the toolbar and nowhere else, so
    /// the menu it is hidden in is the whole way in.
    #[test]
    fn about_is_in_the_bar_signed_out_and_in_the_account_menu_signed_in() {
        let env = test_support::env();
        let render = |current_user: serde_json::Value| {
            env.get_template("toolbar.jinja")
                .expect("toolbar loads")
                .render(context! {
                    current_user,
                    messages => Vec::<serde_json::Value>::new(),
                    draft_post_count => 0,
                    unread_notification_count => 0,
                    ftl_lang => "en",
                })
                .expect("toolbar renders")
        };

        let out = render(json!(null));
        assert!(
            out.contains(r#"<a class="toolbar-square toolbar-button toolbar-about" href="/about" aria-label="nav-about-menu" title="nav-about-menu"><svg"#),
            "signed out, About is its own button at the bar's end: a mark, with its words for its label"
        );
        assert!(
            out.contains(r#"<a class="toolbar-square toolbar-button toolbar-sign-in" href="/login" aria-label="sign-in" title="sign-in"><svg"#),
            "and signing in is a mark with its words for its label"
        );
        assert!(
            out.contains(r#"id="nav-draw-button" class="toolbar-square toolbar-draw""#),
            "Draw is the filled button signed out too: a guest can draw"
        );
        assert!(
            out.contains(r#"<a class="toolbar-square toolbar-button toolbar-drafts" href="/posts/drafts" hx-boost="false" data-server-count="0" hidden aria-label="drafts" title="drafts"><svg"#),
            "and this browser's drafts are a square of their own, hidden until the script finds some"
        );
        // The theme square opens onto the three-way switch and nothing
        // else: one link to `/about` in the whole bar, and it is not that
        // menu's. (The desktop app's Help menu also reaches it, by script.)
        assert_eq!(out.matches(r#"href="/about""#).count(), 1);
        assert!(!out.contains("toolbar-menu-about"));
        assert!(!out.contains("toolbar-menu-drafts"));

        let signed_in = render(json!({"login_name": "oeee", "display_name": "오이"}));
        assert!(
            signed_in.contains(r#"<a class="toolbar-menu-about" href="/about">"#),
            "signed in, About is the last item of the account menu"
        );
        assert_eq!(signed_in.matches(r#"href="/about""#).count(), 1);
        // Signed in with none, the drafts square is there but hidden, for
        // the script to show if this browser holds some.
        assert!(signed_in.contains(r#"toolbar-drafts" href="/posts/drafts" hx-boost="false" data-server-count="0" hidden aria-label="drafts""#));
    }

    /// Drafts on the server show the drafts square from the first paint,
    /// counted in its label, beside the account menu's line for them.
    #[test]
    fn an_account_with_drafts_has_the_drafts_square_in_the_bar() {
        let env = test_support::env();
        let out = env
            .get_template("toolbar.jinja")
            .expect("toolbar loads")
            .render(context! {
                current_user => json!({"id": "00000000-0000-0000-0000-000000000001", "login_name": "oeee", "display_name": "오이"}),
                messages => Vec::<serde_json::Value>::new(),
                draft_post_count => 3,
                unread_notification_count => 0,
                ftl_lang => "en",
            })
            .expect("toolbar renders");
        assert!(out.contains(r#"<a class="toolbar-square toolbar-button toolbar-drafts" href="/posts/drafts" hx-boost="false" data-server-count="3" aria-label="drafts (3)" title="drafts (3)"><svg"#));
        // Drafts are the square's alone: no line in the account menu, and
        // the person does not pulse.
        assert!(!out.contains("toolbar-menu-drafts"));
        assert!(!out.contains("toolbar-avatar-pulse"));
    }

    /// Which platform's mark to wear is only a question for someone who
    /// supports on more than one, and the answer they gave is the one
    /// selected.
    #[test]
    fn only_a_supporter_on_two_platforms_is_asked_which_mark_to_wear() {
        let one = render_account(json!(true), json!(["steam"]), json!("steam"));
        assert!(!one.contains(r#"name="mark""#), "nothing to choose between");

        let both = render_account(json!(true), json!(["steam", "apple"]), json!("apple"));
        let squashed = both.split_whitespace().collect::<Vec<_>>().join(" ");
        assert_eq!(squashed.matches(r#"name="mark""#).count(), 2, "one for each");
        assert!(squashed.contains(r#"name="mark" value="apple" checked"#));
        assert!(!squashed.contains(r#"name="mark" value="steam" checked"#));
        // Saved by the same button as the credits, in the same form.
        assert_eq!(squashed.matches(r#"action="/account/credits""#).count(), 1);

        // Nobody else is asked at all.
        assert!(!render_account(json!(null), json!([]), json!(null)).contains(r#"name="mark""#));
    }

    /// An account made with Steam has no password: it is offered one to set
    /// rather than asked for its current one, and deleting it asks for its
    /// handle.
    #[test]
    fn the_account_page_asks_what_the_account_can_answer() {
        let env = test_support::env();
        let render = |has_password: bool, identities: serde_json::Value| {
            env.get_template("account.jinja")
                .expect("account loads")
                .render(context! {
                    current_user => json!({
                        "id": "b95e3d1e-5a25-4d0a-9d3a-3a0b0a9b1c2d",
                        "login_name": "oeee",
                        "display_name": "오이",
                        "email": null,
                        "email_verified_at": null,
                        "created_at": "2026-09-22T00:00:00Z",
                        "preferred_language": null,
                        "show_sensitive_content": false,
                        "role": "user",
                    }),
                    languages => vec![("ko", "한국어"), ("en", "English")],
                    identities,
                    has_password,
                    steam_enabled => true,
                    steam_linked => false,
                    apple_enabled => true,
                    apple_linked => false,
                    google_enabled => true,
                    google_linked => false,
                    messages => Vec::<serde_json::Value>::new(),
                    draft_post_count => 0,
                    unread_notification_count => 0,
                    ftl_lang => "en",
                })
                .expect("account renders")
        };

        let with_password = render(true, json!([]));
        assert!(with_password.contains(r#"name="current_password""#));
        assert!(with_password.contains(r#"name="password""#));
        assert!(!with_password.contains(r#"id="delete_login_name""#));
        assert!(with_password.contains("account-linked-accounts-none"));
        assert!(with_password.contains("/auth/steam/app?next=/account"));
        assert!(with_password.contains("/auth/apple?next=/account"));
        assert!(with_password.contains("/auth/google?next=/account"));

        let without = render(
            false,
            json!([{"provider": "steam", "display_hint": "오이", "subject": "76561197960287930"}]),
        );
        assert!(!without.contains(r#"name="current_password""#));
        assert!(without.contains("account-set-password"));
        assert!(without.contains(r#"id="delete_login_name""#));
        assert!(without.contains("account-delete-type-login-name(loginName=oeee)"));
        assert!(without.contains(r#"action="/account/identities/steam/unlink""#));
        assert!(without.contains("Steam: 오이"));

        // Apple gives a name only the first time; the address stands in.
        let apple = render(
            true,
            json!([{"provider": "apple", "display_hint": null, "email": "x@privaterelay.appleid.com", "subject": "001234.abc"}]),
        );
        assert!(apple.contains("Apple: x@privaterelay.appleid.com"));

        // Apple and Google lead with the address even when they gave a name
        // too.
        let apple_named = render(
            true,
            json!([{"provider": "apple", "display_hint": "오이", "email": "x@privaterelay.appleid.com", "subject": "001234.abc"}]),
        );
        assert!(apple_named.contains("Apple: x@privaterelay.appleid.com"));
        assert!(!apple_named.contains("Apple: 오이"));
        let google = render(
            true,
            json!([{"provider": "google", "display_hint": "오이", "email": "oeee@example.test", "subject": "1234"}]),
        );
        assert!(google.contains("Google: oeee@example.test"));
        assert!(!google.contains("Google: 오이"));
        assert!(apple.contains(r#"action="/account/identities/apple/unlink""#));
    }

    /// The replay switch is enforced in the handler; this is the other half of
    /// it -- the link a stranger is not supposed to be offered.
    #[test]
    fn a_closed_replay_is_linked_for_its_author_only() {
        let author = "b95e3d1e-5a25-4d0a-9d3a-3a0b0a9b1c2d";
        let stranger = json!({"id": "0d2a2b4c-7e8f-4a1b-8c9d-1e2f3a4b5c6d", "role": "user"});
        // A NEO replay is not linked but played under the drawing: what a
        // stranger must not be handed is the recording's address, which the
        // stage carries for the viewer.
        let link = "/replay/30/30ca3f590dda85e21dbc94250199a692b4fa5c7d626ea3445acef3bcf3c1338a.pch";

        let open = render_post_page("true", author, json!(null));
        assert!(
            open.contains(link),
            "an open replay should be linked for anyone"
        );

        let closed_to_stranger = render_post_page("false", author, stranger);
        assert!(
            !closed_to_stranger.contains(link),
            "a closed replay should not be linked for someone else"
        );

        let closed_to_author =
            render_post_page("false", author, json!({"id": author, "role": "user"}));
        assert!(
            closed_to_author.contains(link),
            "a closed replay should still be linked for its author"
        );
        assert!(
            closed_to_author.contains("(replay-private)"),
            "the author should be told the replay is only theirs to watch"
        );

        // Staff keep the link for moderation, under a label that does not tell
        // them the replay is theirs.
        let closed_to_staff = render_post_page(
            "false",
            author,
            json!({"id": "0d2a2b4c-7e8f-4a1b-8c9d-1e2f3a4b5c6d", "role": "admin"}),
        );
        assert!(
            closed_to_staff.contains(link),
            "a closed replay should still be linked for staff"
        );
        assert!(
            closed_to_staff.contains("(replay-private-staff)"),
            "staff should be told the replay is private, not that it is theirs"
        );
    }

    /// The edit form is where a published post's replay gets turned off, and
    /// an unchecked box submits nothing -- so a box that fails to reflect the
    /// stored value silently flips it on the next save.
    #[test]
    fn the_edit_form_reflects_the_stored_replay_switch() {
        let env = test_support::env();
        let template = env
            .get_template("post_edit.jinja")
            .unwrap_or_else(|e| panic!("post_edit.jinja loads: {e:#}"));
        let render = |allow_replay: &str| {
            template
                .render(context! {
                    post => json!({
                        "title": "Tandemaus",
                        "content": "a description",
                        "is_sensitive": "false",
                        "allow_relay": "true",
                        "allow_replay": allow_replay,
                    }),
                    post_id => "9c881320-2b43-4afa-b2bb-7128c8a3e985",
                    tags => "",
                    ..chrome()
                })
                .unwrap_or_else(|e| panic!("post_edit.jinja renders: {e:#}"))
        };

        /// The rest of the `<input>` tag that carries the replay switch.
        fn checkbox(rendered: &str) -> String {
            let (_, tail) = rendered
                .rsplit_once("id=\"allow_replay\"")
                .expect("the replay checkbox");
            let (tag, _) = tail.split_once('>').expect("the checkbox tag ends");
            tag.to_string()
        }

        assert!(
            checkbox(&render("true")).contains("checked"),
            "an open replay should render a checked box"
        );
        assert!(
            !checkbox(&render("false")).contains("checked"),
            "a closed replay should render an unchecked box"
        );
    }

    /// The relay page is now rendered for personal posts too, where there is
    /// no community to name above the canvas or to link back to. Every use of
    /// one has to survive its absence.
    #[test]
    fn the_relay_page_renders_with_and_without_a_community() {
        let env = test_support::env();
        let template = env
            .get_template("draw_post_cucumber.jinja")
            .unwrap_or_else(|e| panic!("draw_post_cucumber.jinja loads: {e:#}"));
        let render = |community_name: serde_json::Value, community_slug: serde_json::Value| {
            template
                .render(context! {
                    parent_post => json!({
                        "id": "9c881320-2b43-4afa-b2bb-7128c8a3e985",
                        "title": "Tandemaus",
                        "image_width": "640",
                        "image_height": "480",
                        "image_filename": "abcdef0123.png",
                        "login_name": "someone",
                    }),
                    width => 640,
                    height => 480,
                    community_name => community_name,
                    community_slug => community_slug,
                    community_id => json!(null),
                    is_relay => true,
                    painter_config => "{}",
                    ..chrome()
                })
                .unwrap_or_else(|e| panic!("draw_post_cucumber.jinja renders: {e:#}"))
        };

        // The painter has no bar of its own under the toolbar any more, so
        // the page title is what names the community a relay lands in.
        let title = |rendered: &str| {
            let start = rendered.find("<title>").expect("a title") + "<title>".len();
            let end = rendered.find("</title>").expect("a closed title");
            rendered[start..end].split_whitespace().collect::<Vec<_>>().join(" ")
        };
        let in_a_community = title(&render(json!("Tegaki"), json!("tegaki")));
        assert!(
            in_a_community.contains("Re: Tandemaus") && in_a_community.contains("@ Tegaki"),
            "a relay in a community should be titled with both, got: {in_a_community}"
        );

        let personal = title(&render(json!(null), json!(null)));
        assert!(personal.contains("Re: Tandemaus"), "got: {personal}");
        assert!(
            !personal.contains(" @ "),
            "a personal relay should name no community, got: {personal}"
        );
    }

    /// The design system's reference page draws every component, so it has
    /// to render -- a broken one is the first place a change to ds.css shows.
    #[test]
    fn the_design_reference_renders_every_component() {
        let rendered = test_support::env()
            .get_template("design.jinja")
            .expect("design.jinja loads")
            .render(chrome())
            .expect("design.jinja renders");
        for class in [
            "ds-button-primary",
            "ds-select",
            "ds-input",
            "ds-segmented",
            "ds-window",
            "ds-notice-error",
        ] {
            assert!(rendered.contains(class), "{class} missing from the reference");
        }
        assert!(rendered.contains("<body class=\"ds-page\">"));
    }

    /// The page's `<header>`: the toolbar and whatever notices sit under it.
    fn header(rendered: &str) -> &str {
        let start = rendered.find("<header>").expect("page has a header");
        let end = rendered.find("</header>").expect("header closes");
        &rendered[start..end]
    }

    /// Flash messages are notices, coloured by their level. The context gets
    /// axum-messages' own type, whose level serialises as `"Error"`, so this
    /// renders that type and not a stand-in shaped the way the template wishes.
    #[test]
    fn flash_messages_render_as_notices_by_level() {
        let message = |level, text: &str| axum_messages::Message {
            level,
            message: text.to_string(),
            metadata: None,
        };
        let rendered = test_support::env()
            .get_template("design.jinja")
            .expect("design.jinja loads")
            .render(context! {
                current_user => json!(null),
                messages => vec![
                    message(axum_messages::Level::Success, "Welcome, Tandemaus"),
                    message(axum_messages::Level::Error, "<b>not bold</b>"),
                ],
                draft_post_count => 0,
                unread_notification_count => 0,
                ftl_lang => "en",
            })
            .expect("design.jinja renders");
        let header = header(&rendered);
        assert!(header.contains("ds-notice ds-notice-success"), "got: {header}");
        assert!(header.contains("ds-notice ds-notice-error"), "got: {header}");
        assert!(header.contains("Welcome, Tandemaus"));
        assert!(header.contains("&lt;b&gt;not bold"), "a message is text, not markup");
        assert!(header.contains("ds-notice-close"));
    }

    /// With nothing to say the header holds no notice list at all, so there is
    /// no empty strip under the toolbar.
    #[test]
    fn no_flash_messages_render_no_notice_list() {
        let rendered = test_support::env()
            .get_template("design.jinja")
            .expect("design.jinja loads")
            .render(chrome())
            .expect("design.jinja renders");
        let header = header(&rendered);
        assert!(!header.contains("<ul class=\"ds-notices\">"), "got: {header}");
        assert!(header.contains(r#"<div id="htmx-error" class="ds-notices htmx-error"></div>"#));
    }

    /// The painter pages carry the site's toolbar, so every window has the
    /// same title bar and the desktop app can seat its controls in it. It has
    /// to come before the painter -- it is the page's first row -- and its
    /// stylesheet after the painter's, whose reset would otherwise restyle it.
    #[test]
    fn the_painter_pages_carry_the_toolbar() {
        let env = test_support::env();
        let rendered = env
            .get_template("draw_post_cucumber.jinja")
            .expect("painter template loads")
            .render(context! {
                width => 300,
                height => 300,
                community_id => json!(null),
                painter_config => "{}",
                current_user => json!({"login_name": "someone", "display_name": "Someone"}),
                messages => Vec::<serde_json::Value>::new(),
                draft_post_count => 2,
                unread_notification_count => 3,
                ftl_lang => "en",
            })
            .expect("painter renders");
        let nav = rendered
            .find("<nav class=\"nav-bar\"")
            .expect("the painter page has the toolbar");
        assert!(rendered.contains("/static/ds.css"));
        assert!(rendered.contains("toolbar-bell-unread"), "unread bell");
        assert!(
            nav < rendered.find("id=\"neo-cucumber-root\"").unwrap(),
            "the toolbar is the painter page's first row"
        );
        if let Some(painter_css) = rendered.find("offline.css") {
            let ds_css = rendered.find("ds.css").unwrap();
            assert!(painter_css < ds_css, "ds.css loads after the painter's reset");
        }
    }

    /// The author's own comment carries a delete button, and it asks the
    /// site's route -- `/api/v1/comments/:id` went with the apps' JSON API.
    #[test]
    fn a_comment_deletes_itself_through_the_sites_own_route() {
        let env = test_support::env();
        let rendered = env
            .get_template("post_comments.jinja")
            .expect("post_comments loads")
            .render(context! {
                comments => json!([{
                    "id": "0c8f0000-0000-0000-0000-000000000001",
                    "actor_name": "Someone",
                    "actor_handle": "@someone@oeee.cafe",
                    "actor_login_name": "someone",
                    "actor_url": "/@someone",
                    "is_local": true,
                    "content": "hello",
                    "content_html": null,
                    "created_at": "2026-01-02T03:04:05Z",
                    "deleted_at": null,
                    "children": [],
                }]),
                current_user => json!({"login_name": "someone"}),
                supporters => json!({}),
                ftl_lang => "en",
            })
            .expect("post_comments renders");
        assert!(
            rendered.contains(r#"hx-delete="/comments/0c8f0000-0000-0000-0000-000000000001""#),
            "{rendered}"
        );
        assert!(!rendered.contains("/api/v1"));
    }

    /// Achievements along the foot of the profile card, a badge each, named,
    /// with what it was for in its tooltip; no strip at all for someone with
    /// none.
    #[test]
    fn the_profile_shows_what_its_owner_has_achieved() {
        let env = test_support::env();
        let render = |achievements: serde_json::Value| {
            env.get_template("profile.jinja")
                .expect("profile loads")
                .render(context! {
                    user => json!({
                        "id": "b95e3d1e-5a25-4d0a-9d3a-3a0b0a9b1c2d",
                        "login_name": "oeee",
                        "display_name": "오이",
                        "created_at": "2024-03-05T12:00:00Z",
                    }),
                    banner => json!(null),
                    links => Vec::<serde_json::Value>::new(),
                    followings => json!([{
                        "login_name": "a", "display_name": "에이",
                        "banner_image_filename": "abcdef.png",
                        "banner_image_width": 200, "banner_image_height": 40,
                    }]),
                    achievements,
                    public_community_posts => Vec::<serde_json::Value>::new(),
                    private_community_posts => Vec::<serde_json::Value>::new(),
                    domain => "oeee.cafe",
                    is_following => false,
                    ..chrome()
                })
                .expect("profile renders")
        };
        let with = render(json!([
            {"achievement": "FIRST_DRAWING", "key": "first-drawing", "earned_at": "2026-09-22T00:00:00Z"},
            {"achievement": "STEAM_SUPPORTER", "key": "steam-supporter", "earned_at": "2026-09-22T01:00:00Z"},
        ]));
        assert!(with.contains("profile-achievements"));
        assert!(with.contains(r#"title="achievement-first-drawing-description"#));
        assert!(with.contains("achievement-steam-supporter"));
        // Each with its own Material Symbols icon: the brush for a first
        // drawing, the game controller for buying on Steam.
        assert_eq!(with.matches(r#"class="achievement-icon""#).count(), 2);
        assert!(with.contains(r#"d="M6 21q-1.125 0-2.225-.55T2 19"#));
        assert!(with.contains(r#"d="M4.55 19q-1.275 0-1.975-.888"#));
        assert!(!render(json!([])).contains("profile-achievements"));
        // In the card, over the switch; those they follow are behind it,
        // after their drawings, not stacked above them.
        let at = |needle: &str| with.find(needle).unwrap_or_else(|| panic!("no {needle}"));
        assert!(at("profile-achievements") < at("data-profile-tab=\"public\""));
        assert!(at("data-profile-panel=\"public\"") < at("data-profile-panel=\"following\""));
    }

    /// Under the handle, the month they joined -- in Seoul, so an account
    /// made on the evening of 29 February UTC joined in March. The locale is
    /// handed numbers, not a formatted date, and the `<time>` carries the
    /// machine-readable month.
    #[test]
    fn the_profile_says_when_its_owner_joined() {
        let env = test_support::env();
        let rendered = env
            .get_template("profile.jinja")
            .expect("profile loads")
            .render(context! {
                user => json!({
                    "id": "u1",
                    "login_name": "oeee",
                    "display_name": "오이",
                    // What chrono's serde writes for a `DateTime<Utc>`.
                    "created_at": "2024-02-29T16:30:00.123456Z",
                }),
                banner => json!(null),
                links => Vec::<serde_json::Value>::new(),
                followings => Vec::<serde_json::Value>::new(),
                achievements => Vec::<serde_json::Value>::new(),
                public_community_posts => Vec::<serde_json::Value>::new(),
                private_community_posts => Vec::<serde_json::Value>::new(),
                domain => "oeee.cafe",
                is_following => false,
                ..chrome()
            })
            .expect("profile renders");
        assert!(
            rendered.contains(r#"<time datetime="2024-03">profile-member-since(month=3,year=2024)</time>"#),
            "{rendered}"
        );
        let at = |needle: &str| rendered.find(needle).unwrap_or_else(|| panic!("no {needle}"));
        assert!(at("profile-handle") < at("profile-joined"));
    }

    /// A comment as `build_comment_thread_tree` serializes one.
    fn comment(login_name: Option<&str>, name: &str) -> serde_json::Value {
        json!({
            "id": uuid::Uuid::new_v4().to_string(),
            "post_id": "9c881320-2b43-4afa-b2bb-7128c8a3e985",
            "actor_id": uuid::Uuid::new_v4().to_string(),
            "parent_comment_id": null,
            "content": "hi",
            "content_html": "<p>hi</p>",
            "iri": null,
            "actor_name": name,
            "actor_handle": format!("@{}@oeee.example", login_name.unwrap_or(name)),
            "actor_url": "/@someone",
            "actor_login_name": login_name,
            "is_local": login_name.is_some(),
            "updated_at": "2026-09-22T00:00:00Z",
            "created_at": "2026-09-22T00:00:00Z",
            "deleted_at": null,
            "children": [],
        })
    }

    /// A supporter's mark goes beside every name on a post's page that
    /// belongs to one -- the author, someone who drew with them, a
    /// commenter, a reply -- and beside no one else's, remote accounts
    /// included however they are named. Each wears their own platform's.
    #[test]
    fn supporters_wear_their_platforms_mark_on_a_post_page() {
        let env = test_support::env();
        let mut reply = comment(Some("fan"), "Fan");
        reply["children"] = json!([comment(Some("someone"), "Someone")]);
        reply["children"][0]["parent_comment_id"] = reply["id"].clone();
        let comments = json!([
            reply,
            comment(Some("plain"), "Plain"),
            // A remote account sharing a supporter's local name.
            comment(None, "fan"),
        ]);
        let render = |supporters: serde_json::Value| {
            env.get_template("post_view.jinja")
                .expect("post_view.jinja loads")
                .render(context! {
                    post => post_page("true", "b95e3d1e-5a25-4d0a-9d3a-3a0b0a9b1c2d"),
                    post_id => "9c881320-2b43-4afa-b2bb-7128c8a3e985",
                    r2_public_endpoint_url => "https://images.example",
                    base_url => "https://oeee.example",
                    domain => "oeee.example",
                    comments => comments.clone(),
                    supporters,
                    collaborative_participants => json!([
                        {"login_name": "someone", "display_name": "Someone"},
                        {"login_name": "friend", "display_name": "Friend"},
                    ]),
                    reaction_counts => Vec::<serde_json::Value>::new(),
                    tags => Vec::<serde_json::Value>::new(),
                    child_posts => Vec::<serde_json::Value>::new(),
                    post_community => json!(null),
                    parent_post_data => json!(null),
                    ..chrome()
                })
                .expect("post_view.jinja renders")
        };
        let badges = |html: &str| html.matches(r#"class="supporter-badge""#).count();

        let page = render(json!({"someone": "steam", "friend": "apple", "fan": "steam"}));
        // Author, co-drawer, the commenter and the author's reply to them.
        assert_eq!(badges(&page), 4);
        let byline = page.find("post-inspector-byline").unwrap();
        let handle = page[byline..].find("post-inspector-handle").unwrap() + byline;
        assert!(page[byline..handle].contains("supporter-badge"), "beside the author");
        assert!(page.contains(r#"href="/about#supporters""#));
        // The co-drawer bought elsewhere and wears the other mark: one
        // storefront on the page, the rest gamepads.
        assert_eq!(page.matches(r#"aria-label="supporter-badge-apple""#).count(), 1);
        assert_eq!(page.matches(r#"aria-label="supporter-badge-steam""#).count(), 3);

        assert_eq!(badges(&render(json!({}))), 0);
        // The comments fragment an HTMX post swaps in, the same way.
        let fragment = env
            .get_template("post_comments.jinja")
            .unwrap()
            .render(context! { comments, supporters => json!({"fan": "steam"}), ..chrome() })
            .unwrap();
        assert_eq!(badges(&fragment), 1);
    }

    /// Every year they have supported, earliest first, each on the platform
    /// that year's pack was bought on -- including years that have passed,
    /// whose mark they no longer wear.
    #[test]
    fn a_supporters_profile_says_so_first() {
        let env = test_support::env();
        let render = |supporter_standings: serde_json::Value| {
            env.get_template("profile.jinja")
                .expect("profile loads")
                .render(context! {
                    user => json!({"id": "u1", "login_name": "oeee", "display_name": "오이", "created_at": "2024-03-05T12:00:00Z"}),
                    banner => json!(null),
                    links => Vec::<serde_json::Value>::new(),
                    followings => Vec::<serde_json::Value>::new(),
                    achievements => Vec::<serde_json::Value>::new(),
                    supporter_standings,
                    public_community_posts => Vec::<serde_json::Value>::new(),
                    private_community_posts => Vec::<serde_json::Value>::new(),
                    domain => "oeee.cafe",
                    is_following => false,
                    ..chrome()
                })
                .expect("profile renders")
        };
        let supporter = render(json!([
            {"store": "steam", "year": 2026, "since": "2026-09-22T00:00:00Z"},
            {"store": "apple", "year": 2027, "since": "2027-01-04T00:00:00Z"},
        ]));
        let chip = supporter.find("supporter-chip").expect("a supporter chip");
        assert!(chip < supporter.find("/@oeee/guestbook").unwrap(), "before the guestbook");
        assert!(supporter.contains(r#"href="/about#supporters""#));
        assert_eq!(supporter.matches("supporter-chip").count(), 2, "one per year");
        let steam = supporter.find("supporter-badge-steam").expect("the Steam chip");
        let apple = supporter.find("supporter-badge-apple").expect("the App Store chip");
        assert!(steam < apple, "earliest year first");
        // The year is the chip, and the platform is what it is read as.
        assert!(supporter.contains("🎮</span>2026</a>"), "{supporter}");
        assert!(supporter.contains("🍎</span>2027</a>"));
        assert!(supporter.contains("supporter-year(platform=supporter-badge-steam,year=2026)"));
        assert!(!render(json!([])).contains("supporter-chip"));
    }

    /// The credits: a section on /about that every badge leads to, left out
    /// while there is nobody in it.
    #[test]
    fn the_about_page_thanks_its_supporters() {
        let env = test_support::env();
        let render = |supporters: serde_json::Value| {
            env.get_template("about.jinja")
                .expect("about loads")
                .render(context! {
                    supporters,
                    users_with_public_posts_and_banner => Vec::<serde_json::Value>::new(),
                    ..chrome()
                })
                .expect("about renders")
        };
        let about = render(json!([
            {"login_name": "a", "display_name": "에이", "mark": "steam", "since": "2026-09-22T00:00:00Z"},
            {"login_name": "b", "display_name": "비", "mark": "apple", "since": "2026-09-23T00:00:00Z"},
        ]));
        assert!(about.contains(r#"id="supporters""#));
        // Each chip wears its own platform's mark.
        assert_eq!(about.matches("supporter-mark").count(), 2);
        assert!(about.contains("about-supporters-thanks"));
        let a = about.find(r#"href="/@a""#).expect("a is thanked");
        let b = about.find(r#"href="/@b""#).expect("b is thanked");
        assert!(a < b, "earliest first");
        assert!(!render(json!([])).contains(r#"id="supporters""#));
    }

    /// The commit that is serving, linked to on GitHub, and nothing at all
    /// outside a deployed image.
    #[test]
    fn the_about_page_names_the_commit_it_runs() {
        let env = test_support::env();
        let render = |git_commit: Option<&str>| {
            env.get_template("about.jinja")
                .expect("about loads")
                .render(context! {
                    supporters => Vec::<serde_json::Value>::new(),
                    users_with_public_posts_and_banner => Vec::<serde_json::Value>::new(),
                    git_commit,
                    ..chrome()
                })
                .expect("about renders")
        };
        let sha = "e6851d5a0b1c2d3e4f5a6b7c8d9e0f1a2b3c4d5e";
        let about = render(Some(sha));
        assert!(about.contains(&format!(r#"href="https://github.com/oeee-cafe/web/commit/{sha}""#)));
        assert!(about.contains(">e6851d5a0b1c<span"), "{about}");
        assert!(!render(None).contains("about-version"));
    }

    /// Following, behind its own tab with its count: everyone the same
    /// shape, a banner where they have drawn one and a frame of the same
    /// size holding their name where they have not. No tab at all for
    /// someone who follows nobody, and with nothing to switch between, no
    /// switch.
    #[test]
    fn the_profile_shows_everyone_followed_the_same_way() {
        let env = test_support::env();
        let render = |followings: serde_json::Value| {
            env.get_template("profile.jinja")
                .expect("profile loads")
                .render(context! {
                    user => json!({"id": "u1", "login_name": "oeee", "display_name": "오이", "created_at": "2024-03-05T12:00:00Z"}),
                    banner => json!(null),
                    links => Vec::<serde_json::Value>::new(),
                    followings,
                    achievements => Vec::<serde_json::Value>::new(),
                    public_community_posts => Vec::<serde_json::Value>::new(),
                    private_community_posts => Vec::<serde_json::Value>::new(),
                    domain => "oeee.cafe",
                    is_following => false,
                    ..chrome()
                })
                .expect("profile renders")
        };
        let banner = json!({
            "login_name": "a", "display_name": "에이",
            "banner_image_filename": "abcdef.png", "banner_image_width": 200, "banner_image_height": 40,
        });
        let plain = json!({"login_name": "b", "display_name": "비", "banner_image_filename": null});

        let both = render(json!([banner, plain]));
        assert!(both.contains(r#"data-profile-tab="following">profile-following<span class="profile-tab-count">2</span>"#));
        assert!(both.contains(r#"data-profile-panel="following" hidden"#));
        assert_eq!(both.matches(r#"class="profile-follow""#).count(), 2);
        assert!(both.contains(r#"<a class="profile-follow" href="/@a""#));
        assert!(both.contains("/image/ab/abcdef.png"));
        assert!(both.contains(r#"<a class="profile-follow" href="/@b""#));
        assert!(both.contains(r#"profile-follow-blank" aria-hidden="true">비</span>"#));

        let nobody = render(json!([]));
        assert!(!nobody.contains("data-profile-tab"), "one grid, no switch");
        assert!(!nobody.contains("profile-follows"));
        assert!(nobody.contains(r#"<div class="profile-section-label">profile-tab-drawings</div>"#));
    }

    /// What a visitor can do about someone: follow them and sign their
    /// guestbook side by side, and report them from the menu after -- never
    /// a button at Follow's weight. Someone signed out gets the guestbook
    /// and nothing that needs an account.
    #[test]
    fn a_profile_keeps_reporting_behind_its_menu() {
        let env = test_support::env();
        let render = |current_user: serde_json::Value| {
            env.get_template("profile.jinja")
                .expect("profile loads")
                .render(context! {
                    user => json!({"id": "u1", "login_name": "oeee", "display_name": "오이", "created_at": "2024-03-05T12:00:00Z"}),
                    banner => json!({"image_filename": "abcdef.png", "width": 200, "height": 40}),
                    links => Vec::<serde_json::Value>::new(),
                    followings => Vec::<serde_json::Value>::new(),
                    achievements => Vec::<serde_json::Value>::new(),
                    public_community_posts => Vec::<serde_json::Value>::new(),
                    private_community_posts => Vec::<serde_json::Value>::new(),
                    domain => "oeee.cafe",
                    is_following => false,
                    r2_public_endpoint_url => "https://images.example",
                    current_user,
                    ..chrome()
                })
                .expect("profile renders")
        };
        let visitor = render(json!({"id": "u2", "login_name": "fan", "display_name": "Fan"}));
        let at = |needle: &str| visitor.find(needle).unwrap_or_else(|| panic!("no {needle}"));
        assert!(at("/@oeee/follow") < at("/@oeee/guestbook"));
        assert!(at("/@oeee/guestbook") < at(r#"<details class="toolbar-menu profile-more">"#));
        assert!(at("profile-more") < at("showProfileReportModal()"));
        // Their banner, not a link for someone who cannot redraw it.
        assert!(visitor.contains(r#"<span class="profile-banner">"#));

        let owner = render(json!({"id": "u1", "login_name": "oeee", "display_name": "오이"}));
        assert!(!owner.contains("profile-more"));
        assert!(owner.contains(r#"<a class="profile-banner" href="/banners/draw""#));
        assert!(owner.contains(r#"data-profile-tab="private""#));

        let signed_out = render(json!(null));
        assert!(signed_out.contains("/@oeee/guestbook"));
        assert!(!signed_out.contains("/@oeee/follow"));
        assert!(!signed_out.contains("profile-more"));
    }

    /// What the Steam app reads to tell friends what someone is doing: the
    /// painters, the collaborative room's head and any page on the base
    /// layout carry it when the handler passes one, and none of them when it
    /// does not.
    #[test]
    fn pages_say_what_their_reader_is_doing_for_steam() {
        let env = test_support::env();
        let presence = json!({
            "activity": "drawing",
            "community": "오이카페 \"모에화\" <b>",
            "group": null,
        });
        for template_name in [
            "draw_post_cucumber.jinja",
            "collaborate_chrome_head.jinja",
        ] {
            let render = |presence: serde_json::Value| {
                env.get_template(template_name)
                    .unwrap_or_else(|e| panic!("{template_name} loads: {e:#}"))
                    .render(context! {
                        presence,
                        painter_config => "{}",
                        parent_post => json!(null),
                        post => replay_post(),
                        post_id => "9c881320-2b43-4afa-b2bb-7128c8a3e985",
                        community_id => json!(null),
                        ..chrome()
                    })
                    .unwrap_or_else(|e| panic!("{template_name} renders: {e:#}"))
            };
            let with = render(presence.clone());
            assert!(
                with.contains(r#"<meta name="oeee-presence" content="drawing" data-community="오이카페 &quot;모에화&quot; &lt;b&gt;" />"#),
                "{template_name} should carry the presence tag, escaped"
            );
            // The tag, not the name: app_bridge.jinja's script reads it.
            assert!(!render(json!(null)).contains(r#"<meta name="oeee-presence""#), "{template_name}");
        }

        let room = env
            .get_template("presence_meta.jinja")
            .unwrap()
            .render(context! {
                presence => json!({"activity": "collaborating", "community": null, "group": "0123abcd"}),
            })
            .unwrap();
        assert!(room.contains(r#"content="collaborating" data-group="0123abcd" />"#));
    }

    #[test]
    fn drawing_pages_mount_the_offline_painter() {
        let env = test_support::env();
        let config = r##"{"width":640,"height":480,"communityId":"9c881320-2b43-4afa-b2bb-7128c8a3e985","mode":{"kind":"two-tone","backgroundColor":"#ffffff","foregroundColor":"#000000"}}"##;

        {
            let template_name = "draw_post_cucumber.jinja";
            let rendered = env
                .get_template(template_name)
                .unwrap_or_else(|e| panic!("{template_name} loads: {e:#}"))
                .render(context! {
                    painter_config => config,
                    parent_post => json!(null),
                    community_name => "Two Tone",
                    current_user => json!(null),
                    messages => Vec::<serde_json::Value>::new(),
                    draft_post_count => 0,
                    unread_notification_count => 0,
                    ftl_lang => "en",
                })
                .unwrap_or_else(|e| panic!("{template_name} renders: {e:#}"));

            assert!(rendered.contains("id=\"neo-cucumber-root\""));
            assert!(rendered.contains("/static/neo-cucumber/offline.js"));
            assert!(rendered.contains("/static/neo-cucumber/offline.css"));
            assert!(rendered.contains("\"kind\":\"two-tone\""));
            assert!(!rendered.contains("neo.js"));
            assert!(rendered.contains("html, body { width: 100%; height: 100%; margin: 0; }"));
            assert!(rendered.contains("body { overflow: hidden; }"));
            // The painter fills the element it is mounted into and nothing
            // more -- it used to pin itself to the viewport, which painted its
            // ground over anything a host drew above it. A page that is
            // nothing but the painter has to hand it the screen itself, and
            // without this the painter has no height at all.
            assert!(rendered.contains("#neo-cucumber-root {"));
            assert!(rendered.contains("height: 100dvh;"));
            // Saving leaves this page for good, so the adapter asks first --
            // and it asks in the page's words, because the page is the only
            // side of this that knows the reader's language. `entry.ts` reads
            // them off the button's dataset and falls back to English without
            // them, which nobody would notice until a Korean reader met an
            // English dialog. (Stubbed ftl_get_message echoes the id.)
            assert!(rendered.contains("data-confirm=\"draw-save-confirm\""));
            assert!(rendered.contains("data-cancel=\"cancel\""));
        }
    }

    #[test]
    fn banner_pages_mount_the_small_offline_painter() {
        let env = test_support::env();
        let config = r#"{"width":200,"height":40,"submission":{"kind":"banner","profileUrl":"/@artist"},"mode":{"kind":"standard"}}"#;

        {
            let template_name = "draw_banner.jinja";
            let rendered = env
                .get_template(template_name)
                .unwrap_or_else(|e| panic!("{template_name} loads: {e:#}"))
                .render(context! {
                    painter_config => config,
                    current_user => json!({ "login_name": "artist" }),
                    messages => Vec::<serde_json::Value>::new(),
                    draft_post_count => 0,
                    unread_notification_count => 0,
                    ftl_lang => "en",
                })
                .unwrap_or_else(|e| panic!("{template_name} renders: {e:#}"));

            assert!(rendered.contains("/static/neo-cucumber/offline.js"));
            assert!(rendered.contains("\"height\":40"));
            assert!(rendered.contains("\"kind\":\"banner\""));
            assert!(!rendered.contains("neo.js"));
            assert!(rendered.contains("data-confirm=\"draw-save-confirm\""));
            assert!(rendered.contains("data-cancel=\"cancel\""));
        }
    }

    #[test]
    fn the_toolbar_marks_the_language_in_use() {
        // The macro reads the page's context from inside the toolbar, and
        // `preferred_language` arrives as a string or as nothing, so this is
        // rendered with the shapes the handlers really pass.
        let render = |current_user: serde_json::Value, ftl_lang: &str| {
            test_support::env()
                .get_template("home.jinja")
                .unwrap_or_else(|e| panic!("home.jinja loads: {e:#}"))
                .render(context! {
                    feed => context! {
                        posts => Vec::<serde_json::Value>::new(),
                        has_more => false,
                        next_url => "",
                    },
                    current_user => current_user,
                    messages => Vec::<serde_json::Value>::new(),
                    draft_post_count => 0,
                    unread_notification_count => 0,
                    ftl_lang => ftl_lang,
                })
                .unwrap_or_else(|e| panic!("home.jinja renders: {e:#}"))
        };

        let chose = render(
            json!({ "login_name": "artist", "id": "u1", "preferred_language": "ja" }),
            "ja",
        );
        assert!(chose.contains(r#"<option value="ja" lang="ja" selected>"#));
        assert!(!chose.contains(r#"<option value="auto" selected>"#));
        assert!(!chose.contains("data-guest"));

        let auto = render(
            json!({ "login_name": "artist", "id": "u1", "preferred_language": null }),
            "ko",
        );
        assert!(auto.contains(r#"<option value="auto" selected>"#));

        // Signed out, the language in use; the page's script moves it to
        // Auto when no cookie chose it.
        let guest = render(serde_json::Value::Null, "zh");
        assert!(guest.contains(r#"<option value="zh" lang="zh" selected>"#));
        assert!(guest.contains("data-guest"));
        assert!(guest.contains(r#"action="/language" method="post" hx-boost="false""#));
    }

    #[test]
    fn nothing_in_the_boosted_nav_reaches_a_module_bundle() {
        // The nav carries hx-boost. A boosted navigation swaps the body and
        // re-runs its scripts by cloning the tags, which does *not* re-evaluate
        // a `<script type="module">` the browser has already loaded — the
        // module map is keyed on the URL and `cachebuster` holds it fixed for a
        // deploy. So a nav entry pointing at a page that mounts a painter would
        // work once per session and then quietly stop, with a blank canvas and
        // no error.
        //
        // Everything that mounts one is reached from a page body instead, where
        // boost does not apply. This test is what keeps that true: adding a
        // link to the nav fails here until its destination is listed, which is
        // the moment to check the destination does not load a bundle.
        let env = test_support::env();
        let rendered = env
            .get_template("home.jinja")
            .unwrap_or_else(|e| panic!("home.jinja loads: {e:#}"))
            .render(context! {
                feed => context! {
                    posts => Vec::<serde_json::Value>::new(),
                    has_more => false,
                    next_url => "",
                },
                current_user => json!({ "login_name": "artist", "id": "u1" }),
                messages => Vec::<serde_json::Value>::new(),
                draft_post_count => 0,
                unread_notification_count => 0,
                ftl_lang => "en",
            })
            .unwrap_or_else(|e| panic!("home.jinja renders: {e:#}"));

        let nav = rendered
            .split_once("<nav")
            .and_then(|(_, rest)| rest.split_once("</nav>"))
            .map(|(nav, _)| nav.to_string())
            .expect("base.jinja renders a nav");

        assert!(
            nav.contains("hx-boost:inherited=\"true\""),
            "the nav should still be the boosted scope"
        );
        assert!(
            !rendered.contains("<body hx-boost") && !rendered.contains("<main hx-boost"),
            "boost must stay scoped to the nav; widening it re-admits the drawing routes"
        );

        // Every destination reachable from the nav, and why it is safe.
        // Add here only after checking the page does not mount a bundle.
        let allowed = [
            // The toolbar's logo: home.jinja, which mounts nothing.
            "/",
            "/about",
            "/collaborate",
            "/communities",
            "/tags",
            // search.jinja: the shared post cards and the per-row control's
            // inline script, no bundle.
            "/search",
            "/following",
            "/joined",
            "/notifications",
            "/account",
            "/login",
            "/signup",
            "/posts/drafts",
            // Signing out answers with a redirect to "/", and its form opts
            // out of boost besides, so the signed out document is a fresh
            // one.
            "/logout",
            // The language choice: a redirect back to the page it was made
            // on, from a form that opts out of boost for the same reason.
            "/language",
        ];

        for (attr, _) in [("href=\"", 0), ("action=\"", 0)] {
            for piece in nav.split(attr).skip(1) {
                let target = piece.split('"').next().unwrap_or_default();
                // Profile links carry the viewer's own name.
                if target.starts_with("/@") {
                    continue;
                }
                // The draw form is the one nav entry that does reach a bundle,
                // and it says so by opting out of boost.
                if target == "/draw" {
                    assert!(
                        nav.contains("action=\"/draw\" method=\"post\" hx-boost=\"false\"")
                            || nav.contains("hx-boost=\"false\""),
                        "the draw form must opt out of boost"
                    );
                    continue;
                }
                assert!(
                    allowed.contains(&target),
                    "{target} is new in the boosted nav — confirm it does not load a \
                     <script type=\"module\"> before adding it to the list in this test"
                );
            }
        }
    }

    #[test]
    fn the_banner_grid_renders_the_shape_its_handler_passes() {
        // `list_user_banners` returns a real DateTime and a real bool, and the
        // grid pipes the first through `datetimeformat` and branches on the
        // second. Parsing sees none of that, and both buttons on this page now
        // swap this template in, so a render failure would take out activating
        // and deleting rather than one page load.
        let env = test_support::env();
        let template = env
            .get_template("banner_grid.jinja")
            .unwrap_or_else(|e| panic!("banner_grid.jinja loads: {e:#}"));

        let banner = |is_active: bool| {
            json!({
                "id": "6f2b4e4c-95f6-4d8a-9c47-1f2f3f4a5b6c",
                "image_filename": "abcd1234.png",
                "created_at": "2026-08-29T04:00:00Z",
                "is_active": is_active,
            })
        };
        let rendered = template
            .render(context! {
                banners => vec![
                    (banner(true), "https://img.example/image/ab/abcd1234.png"),
                    (banner(false), "https://img.example/image/ab/abcd1234.png"),
                ],
                ftl_lang => "en",
            })
            .unwrap_or_else(|e| panic!("banner_grid.jinja renders: {e:#}"));

        assert!(
            rendered.contains("id=\"banner-grid\""),
            "both buttons target this id; without it their swaps go nowhere"
        );
        // One card is active and shows no buttons, the other shows both.
        assert_eq!(
            rendered.matches("hx-target=\"#banner-grid\"").count(),
            2,
            "the inactive card should offer exactly activate and delete"
        );
        assert!(
            !rendered.contains("location.reload"),
            "these buttons stopped reloading the page"
        );
    }

    #[test]
    fn the_tag_results_render_for_both_the_page_and_the_search_box() {
        // Rendered inline by the page and standalone by /api/tags/cards.
        // The standalone call passes no `sort_by`, no `current_user` and no
        // chrome, so anything the fragment reaches for beyond its own three
        // keys would 500 the search box while the page stayed fine.
        let env = test_support::env();
        let template = env
            .get_template("tag_results.jinja")
            .unwrap_or_else(|e| panic!("tag_results.jinja loads: {e:#}"));

        let tags = vec![json!({
            "name": "oekaki",
            "display_name": "oekaki",
            "post_count": 12,
        })];

        let searched = template
            .render(context! {
                tags => tags.clone(),
                search_query => Some("oek"),
                ftl_lang => "en",
            })
            .unwrap_or_else(|e| panic!("renders a search: {e:#}"));
        assert!(searched.contains("tag-search-info"));
        assert!(searched.contains("/tags/oekaki"));

        let browsing = template
            .render(context! {
                tags => tags,
                search_query => None::<String>,
                ftl_lang => "en",
            })
            .unwrap_or_else(|e| panic!("renders while browsing: {e:#}"));
        assert!(
            !browsing.contains("tag-search-info"),
            "browsing is not a search and should not claim to be one"
        );

        let empty = template
            .render(context! {
                tags => Vec::<serde_json::Value>::new(),
                search_query => Some("zzzz"),
                ftl_lang => "en",
            })
            .unwrap_or_else(|e| panic!("renders no matches: {e:#}"));
        assert!(empty.contains("no-tags-found"));
    }

    #[test]
    fn the_tag_page_draws_the_shared_post_cards() {
        // This page used to write its own <img> tags. That is how sensitive
        // drawings came to be blurred everywhere except here, and how it came
        // to load every thumbnail eagerly with no way to reach the next batch.
        let env = test_support::env();
        let rendered = env
            .get_template("tag_view.jinja")
            .unwrap_or_else(|e| panic!("tag_view.jinja loads: {e:#}"))
            .render(context! {
                tag => json!({
                    "name": "oekaki",
                    "display_name": "Oekaki",
                    "post_count": 2,
                }),
                post_count => 2,
                feed => json!({
                    "posts": [{
                        "id": "9c881320-2b43-4afa-b2bb-7128c8a3e985",
                        "title": "Tandemaus",
                        "user_login_name": "someone",
                        "image_filename": "abcdef.png",
                        "image_width": 300,
                        "image_height": 300,
                        "is_sensitive": true,
                        "community_slug": null,
                        "community_name": null,
                        "published_at": "2026-08-01T00:00:00Z",
                    }],
                    "has_more": true,
                    "next_url": "/tags/oekaki/posts?offset=60&limit=60",
                }),
                ..chrome()
            })
            .unwrap_or_else(|e| panic!("tag_view.jinja renders: {e:#}"));

        assert!(
            rendered.contains(r#"class="sensitive""#),
            "a sensitive drawing has to be blurred here too"
        );
        assert!(rendered.contains(r#"loading="lazy""#));
        // The autoescaper writes `/` as `&#x2f;` in attributes.
        let links_in = rendered.replace("&#x2f;", "/").replace("&amp;", "&");
        assert!(
            links_in.contains("/tags/oekaki/posts?offset=60"),
            "the page needs the sentinel that loads the next batch"
        );
        // The count is passed separately from the tag now, because it is
        // counted over what this viewer can actually see.
        assert!(rendered.contains("tag-post-count(count=2)"));
    }

    #[test]
    fn the_tag_page_names_the_tag_in_its_link_preview() {
        let env = test_support::env();
        let rendered = env
            .get_template("tag_view.jinja")
            .unwrap_or_else(|e| panic!("tag_view.jinja loads: {e:#}"))
            .render(context! {
                // A tag in a non-Latin script has to survive being put in a URL.
                tag => json!({ "name": "그림", "display_name": "그림", "post_count": 0 }),
                post_count => 0,
                feed => json!({ "posts": [], "has_more": false, "next_url": "" }),
                ..chrome()
            })
            .unwrap_or_else(|e| panic!("tag_view.jinja renders empty: {e:#}"));

        assert!(
            rendered.contains(
                r#"content="https://oeee.test/tags/%EA%B7%B8%EB%A6%BC""#
            ),
            "og:url should be the escaped canonical name, got: {}",
            &rendered[..rendered.find("</head>").unwrap_or(400)]
        );
        assert!(rendered.contains("tag-no-posts"));
    }

    #[test]
    fn the_tag_suggestions_are_options_a_keyboard_can_reach() {
        // The menu used to be plain <li>s that only answered a click, inside a
        // container announcing itself as a listbox.
        let env = test_support::env();
        let rendered = env
            .get_template("tag_autocomplete.jinja")
            .unwrap_or_else(|e| panic!("tag_autocomplete.jinja loads: {e:#}"))
            .render(context! {
                tags => vec![json!({
                    "name": "oekaki",
                    "display_name": "Oekaki",
                    "post_count": 12,
                })],
                ftl_lang => "en",
            })
            .unwrap_or_else(|e| panic!("tag_autocomplete.jinja renders: {e:#}"));

        assert!(rendered.contains(r#"role="option""#));
        assert!(rendered.contains(r#"id="tag-option-0""#));
        assert!(rendered.contains(r#"aria-selected="false""#));
        assert!(rendered.contains("tag-post-count(count=12)"));
    }

    #[test]
    fn the_notification_chrome_renders_standalone() {
        // Both are swapped in by handlers as well as included by the page, so
        // they have to stand up with only the keys those handlers pass.
        let env = test_support::env();

        let nav = env
            .get_template("nav_notifications.jinja")
            .unwrap_or_else(|e| panic!("nav_notifications.jinja loads: {e:#}"));
        let with_count = nav
            .render(context! { unread_notification_count => 3, ftl_lang => "en" })
            .expect("nav renders with a count");
        let without = nav
            .render(context! { unread_notification_count => 0, ftl_lang => "en" })
            .expect("nav renders at zero");
        assert!(
            with_count.contains("toolbar-bell-unread") && with_count.contains("(3)"),
            "unread fills the bell and puts the count in its label"
        );
        assert!(
            !without.contains("toolbar-bell-unread") && !without.contains("(0)"),
            "zero unread is a plain bell"
        );
        assert!(
            with_count.contains("id=\"nav-notifications\""),
            "the partial targets this id, so it has to survive its own swap"
        );

        let header = env
            .get_template("notifications_header.jinja")
            .unwrap_or_else(|e| panic!("notifications_header.jinja loads: {e:#}"));
        let unread = header
            .render(context! { unread_notification_count => 2, ftl_lang => "en" })
            .expect("header renders with unread");
        let all_read = header
            .render(context! { unread_notification_count => 0, ftl_lang => "en" })
            .expect("header renders with none unread");
        assert!(unread.contains("mark-all-read"));
        assert!(
            !all_read.contains("mark-all-read"),
            "the button has to remove itself once there is nothing left to mark"
        );
    }

    #[test]
    fn the_report_result_looks_up_the_key_it_was_handed() {
        // The handler picks one of four Fluent keys and passes it in as a
        // value. That indirection is easy to get wrong in a way parsing
        // cannot see -- `ftl_get_message("message_key")` renders happily and
        // shows every reporter the same wrong string. The stub echoes ids
        // back, so the id that comes out is the id the template looked up.
        let env = test_support::env();
        let template = env
            .get_template("report_result.jinja")
            .unwrap_or_else(|e| panic!("report_result.jinja loads: {e:#}"));

        for key in [
            "post-report-success",
            "post-report-error",
            "profile-report-success",
            "profile-report-error",
        ] {
            let rendered = template
                .render(context! { message_key => key, ftl_lang => "en" })
                .unwrap_or_else(|e| panic!("report_result.jinja renders {key}: {e:#}"));

            assert!(
                rendered.contains(key),
                "{key} was not the id looked up; the variable is not being dereferenced"
            );
            assert!(
                !rendered.contains("message_key"),
                "the template looked up the literal name of the variable"
            );
            assert!(
                rendered.contains("report-result"),
                "the modal needs the wrapper to swap over the form"
            );
        }
    }

    #[test]
    fn every_locale_defines_the_keys_the_report_result_can_ask_for() {
        // The companion to the test above: that one proves the id reaches
        // Fluent, this one proves Fluent has something to say for it. Neither
        // catches the other's failure, because the render tests stub the
        // bundle out entirely.
        let locales = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("locales");
        for locale in ["en", "ko", "ja", "zh"] {
            let path = locales.join(format!("{locale}.ftl"));
            let text = std::fs::read_to_string(&path)
                .unwrap_or_else(|e| panic!("{} reads: {e:#}", path.display()));
            for key in [
                "post-report-success",
                "post-report-error",
                "profile-report-success",
                "profile-report-error",
                "close",
            ] {
                assert!(
                    text.lines()
                        .any(|line| line.starts_with(&format!("{key} = "))),
                    "{locale}.ftl has no {key}, so that modal would show its own key name"
                );
            }
        }
    }

    /// Rendered from the handler's own row types, so the fixture cannot drift
    /// from what `search_page` actually hands the template.
    fn render_search(
        search_query: Option<&str>,
        posts: Vec<crate::web::handlers::search::SearchPostRow>,
    ) -> String {
        test_support::env()
            .get_template("search.jinja")
            .unwrap_or_else(|e| panic!("search.jinja loads: {e:#}"))
            .render(context! {
                search_query,
                posts,
                ..chrome()
            })
            .unwrap_or_else(|e| panic!("search.jinja renders: {e:#}"))
    }

    #[test]
    fn search_page_renders_form_empty_state_and_results() {
        use crate::web::handlers::search::SearchPostRow;

        // Nothing asked yet: the form alone. Asserted on the page's own
        // form, `communities-filters` -- the toolbar in every page has a
        // form to /search of its own now, so `action="/search"` alone no
        // longer says this page has one.
        let blank = render_search(None, vec![]);
        assert!(blank.contains("communities-filters") && blank.contains(r#"name="q""#));
        assert!(!blank.contains("search-no-results"));

        // Asked, and nothing matched.
        let none = render_search(Some("zzz"), vec![]);
        assert!(none.contains("search-no-results"));
        assert!(none.contains(r#"value="zzz""#));

        let found = render_search(
            Some("그림"),
            vec![SearchPostRow {
                id: uuid::Uuid::nil(),
                title: Some("그림".into()),
                user_login_name: "someone".into(),
                image_filename: Some("abcdef0123.png".into()),
                image_width: Some(640),
                image_height: Some(480),
                is_sensitive: false,
                community_slug: None,
                community_name: None,
                published_at: Some(chrono::Utc::now()),
            }],
        );
        assert!(!found.contains("search-users"));
        assert!(found.contains(r#"href="/@someone/00000000-0000-0000-0000-000000000000""#));
        assert!(found.contains("/image/ab/abcdef0123.png"));
        assert!(!found.contains("search-no-results"));
    }

    /// Guests draw now, and keep what they drew in the browser. The pages that
    /// serve them render with no one signed in, and the words the painter's
    /// dialogs use arrive as JSON inside the page, which has to stay JSON once
    /// every message in it has been escaped for HTML.
    fn guest_chrome() -> minijinja::Value {
        context! {
            current_user => json!(null),
            messages => Vec::<serde_json::Value>::new(),
            draft_post_count => 0,
            unread_notification_count => 0,
            ftl_lang => "en",
        }
    }

    fn json_script(rendered: &str, id: &str) -> serde_json::Value {
        let open = format!(r#"<script id="{id}" type="application/json">"#);
        let start = rendered.find(&open).expect("page has the script") + open.len();
        let end = start + rendered[start..].find("</script>").expect("script is closed");
        serde_json::from_str(&rendered[start..end]).expect("the script holds JSON")
    }

    #[test]
    fn a_guest_painter_says_where_the_drawing_is_kept() {
        let env = test_support::env();
        let rendered = env
            .get_template("draw_post_cucumber.jinja")
            .expect("painter template loads")
            .render(context! {
                width => 300,
                height => 300,
                tool => "neo",
                painter_config => "{}",
                ..guest_chrome()
            })
            .expect("painter renders for a guest");
        assert!(rendered.contains("draw-guest-notice"), "the painter does not warn a guest");
        let words = json_script(&rendered, "oeee-painter-words");
        assert_eq!(words["guestSaved"], "draw-guest-saved");
        assert_eq!(words["downloadPng"], "draw-download-png");
    }

    #[test]
    fn a_signed_in_painter_has_no_guest_notice() {
        let env = test_support::env();
        let rendered = env
            .get_template("draw_post_cucumber.jinja")
            .expect("painter template loads")
            .render(context! {
                width => 300,
                height => 300,
                tool => "neo",
                painter_config => "{}",
                current_user => json!({
                    "id": "00000000-0000-0000-0000-000000000001",
                    "login_name": "someone",
                    "display_name": "Someone",
                    "email_verified_at": "2026-01-01T00:00:00Z",
                }),
                messages => Vec::<serde_json::Value>::new(),
                draft_post_count => 0,
                unread_notification_count => 0,
                ftl_lang => "en",
            })
            .expect("painter renders");
        assert!(!rendered.contains("draw-guest-notice"));
        assert!(rendered.contains(r#"data-user-id="00000000-0000-0000-0000-000000000001""#));
    }

    #[test]
    fn a_guest_can_start_a_drawing_and_find_its_drafts() {
        let env = test_support::env();
        let rendered = env
            .get_template("draft_posts.jinja")
            .expect("drafts template loads")
            .render(context! { posts => Vec::<serde_json::Value>::new(), ..guest_chrome() })
            .expect("drafts render for a guest");
        // The toolbar's draw button, signed out as well as in.
        assert!(rendered.contains(r#"id="nav-draw-button""#));
        // This browser's drafts, listed by the page's script, and the
        // guest's drafts square in the toolbar, which its script fills.
        assert!(rendered.contains(r#"id="local-drafts""#));
        assert!(rendered.contains(r#"data-user-id="""#));
        assert!(rendered.contains(r#"id="local-drafts-empty""#));
        assert!(rendered.contains("/static/neo-cucumber/drafts.js"));
        assert!(rendered.contains("toolbar-drafts\" href=\"/posts/drafts\""));
        // No server drafts to arrange for someone with none.
        assert!(!rendered.contains(r#"id="post-feed-grid""#));
        let words = json_script(&rendered, "local-drafts-words");
        assert_eq!(words["communityDenied"], "drafts-local-community-denied");
    }
}
