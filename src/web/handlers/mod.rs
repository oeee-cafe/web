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
pub mod hashtag;
pub mod home;
pub mod notifications;
pub mod password_reset;
pub mod policy;
pub mod post;
pub mod privacy;
pub mod profile;
pub mod report;
pub mod search;
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

    fn render_replay(template: &str) -> String {
        let env = test_support::env();
        env.get_template(template)
            .unwrap_or_else(|e| panic!("{template} loads: {e:#}"))
            .render(context! {
                post => replay_post(),
                post_id => "9c881320-2b43-4afa-b2bb-7128c8a3e985",
                community_id => json!(null),
                ..chrome()
            })
            .unwrap_or_else(|e| panic!("{template} renders: {e:#}"))
    }

    #[test]
    fn replay_pages_render_and_mount_the_viewer() {
        for template in [
            "post_replay_view_pch.jinja",
            "post_replay_view_pch_mobile.jinja",
        ] {
            let rendered = render_replay(template);

            assert!(
                rendered.contains("NeoCucumberReplay.mount"),
                "{template} should mount the viewer"
            );
            assert!(
                rendered.contains("/static/viewer/neo-cucumber-replay.js"),
                "{template} should load the viewer bundle"
            );
            // The dimensions reach the mount call as numbers, not as the
            // strings the context holds.
            assert!(
                rendered.contains("width: 640") && rendered.contains("height: 480"),
                "{template} should pass numeric dimensions, got: {}",
                rendered
                    .lines()
                    .filter(|l| l.contains("width") || l.contains("height"))
                    .collect::<Vec<_>>()
                    .join(" | ")
            );
            assert!(
                rendered.contains("/replay/30/30ca3f59"),
                "{template} should build the replay URL from the filename"
            );
        }
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
                hashtags => Vec::<serde_json::Value>::new(),
                child_posts => Vec::<serde_json::Value>::new(),
                post_community => json!(null),
                parent_post_data => json!(null),
                ..chrome()
            })
            .unwrap_or_else(|e| panic!("post_view.jinja renders: {e:#}"))
    }

    /// The replay switch is enforced in the handler; this is the other half of
    /// it -- the link a stranger is not supposed to be offered.
    #[test]
    fn a_closed_replay_is_linked_for_its_author_only() {
        let author = "b95e3d1e-5a25-4d0a-9d3a-3a0b0a9b1c2d";
        let stranger = json!({"id": "0d2a2b4c-7e8f-4a1b-8c9d-1e2f3a4b5c6d", "role": "user"});
        let link = "/9c881320-2b43-4afa-b2bb-7128c8a3e985/replay";

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
                    hashtags => "",
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

        let in_a_community = render(json!("Tegaki"), json!("tegaki"));
        assert!(
            in_a_community.contains("data-home=\"/communities/@tegaki\""),
            "a relay in a community should lead back to it"
        );
        assert!(
            in_a_community.contains("@ Tegaki"),
            "a relay in a community should be labelled with it"
        );

        let personal = render(json!(null), json!(null));
        assert!(
            personal.contains("data-home=\"/\""),
            "a personal relay has nowhere but home to lead back to, got: {}",
            personal
                .lines()
                .filter(|l| l.contains("data-home"))
                .collect::<Vec<_>>()
                .join(" | ")
        );
        assert!(
            !personal.contains("data-subtitle=\"@"),
            "a personal relay should name no community"
        );
    }

    #[test]
    fn replay_pages_do_not_load_the_retired_applet() {
        for template in [
            "post_replay_view_pch.jinja",
            "post_replay_view_pch_mobile.jinja",
        ] {
            let rendered = render_replay(template);
            assert!(
                !rendered.contains("neo.js"),
                "{template} still loads the NEO applet"
            );
        }
    }

    #[test]
    fn drawing_pages_mount_the_offline_painter() {
        let env = test_support::env();
        let config = r##"{"width":640,"height":480,"communityId":"9c881320-2b43-4afa-b2bb-7128c8a3e985","mode":{"kind":"two-tone","backgroundColor":"#ffffff","foregroundColor":"#000000"}}"##;

        for template_name in [
            "draw_post_cucumber.jinja",
            "draw_post_cucumber_mobile.jinja",
        ] {
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

        for template_name in ["draw_banner.jinja", "draw_banner_mobile.jinja"] {
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
    fn the_hashtag_results_render_for_both_the_page_and_the_search_box() {
        // Rendered inline by the page and standalone by /api/hashtags/cards.
        // The standalone call passes no `sort_by`, no `current_user` and no
        // chrome, so anything the fragment reaches for beyond its own three
        // keys would 500 the search box while the page stayed fine.
        let env = test_support::env();
        let template = env
            .get_template("hashtag_results.jinja")
            .unwrap_or_else(|e| panic!("hashtag_results.jinja loads: {e:#}"));

        let tags = vec![json!({
            "name": "oekaki",
            "display_name": "oekaki",
            "post_count": 12,
        })];

        let searched = template
            .render(context! {
                hashtags => tags.clone(),
                search_query => Some("oek"),
                ftl_lang => "en",
            })
            .unwrap_or_else(|e| panic!("renders a search: {e:#}"));
        assert!(searched.contains("hashtag-search-info"));
        assert!(searched.contains("/hashtags/oekaki"));

        let browsing = template
            .render(context! {
                hashtags => tags,
                search_query => None::<String>,
                ftl_lang => "en",
            })
            .unwrap_or_else(|e| panic!("renders while browsing: {e:#}"));
        assert!(
            !browsing.contains("hashtag-search-info"),
            "browsing is not a search and should not claim to be one"
        );

        let empty = template
            .render(context! {
                hashtags => Vec::<serde_json::Value>::new(),
                search_query => Some("zzzz"),
                ftl_lang => "en",
            })
            .unwrap_or_else(|e| panic!("renders no matches: {e:#}"));
        assert!(empty.contains("no-hashtags-found"));
    }

    #[test]
    fn the_hashtag_page_draws_the_shared_post_cards() {
        // This page used to write its own <img> tags. That is how sensitive
        // drawings came to be blurred everywhere except here, and how it came
        // to load every thumbnail eagerly with no way to reach the next batch.
        let env = test_support::env();
        let rendered = env
            .get_template("hashtag_view.jinja")
            .unwrap_or_else(|e| panic!("hashtag_view.jinja loads: {e:#}"))
            .render(context! {
                hashtag => json!({
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
                    "next_url": "/hashtags/oekaki/posts?offset=60&limit=60",
                }),
                ..chrome()
            })
            .unwrap_or_else(|e| panic!("hashtag_view.jinja renders: {e:#}"));

        assert!(
            rendered.contains(r#"class="sensitive""#),
            "a sensitive drawing has to be blurred here too"
        );
        assert!(rendered.contains(r#"loading="lazy""#));
        // The autoescaper writes `/` as `&#x2f;` in attributes.
        let links_in = rendered.replace("&#x2f;", "/").replace("&amp;", "&");
        assert!(
            links_in.contains("/hashtags/oekaki/posts?offset=60"),
            "the page needs the sentinel that loads the next batch"
        );
        // The count is passed separately from the tag now, because it is
        // counted over what this viewer can actually see.
        assert!(rendered.contains("hashtag-post-count(count=2)"));
    }

    #[test]
    fn the_hashtag_page_names_the_tag_in_its_link_preview() {
        let env = test_support::env();
        let rendered = env
            .get_template("hashtag_view.jinja")
            .unwrap_or_else(|e| panic!("hashtag_view.jinja loads: {e:#}"))
            .render(context! {
                // A tag in a non-Latin script has to survive being put in a URL.
                hashtag => json!({ "name": "그림", "display_name": "그림", "post_count": 0 }),
                post_count => 0,
                feed => json!({ "posts": [], "has_more": false, "next_url": "" }),
                ..chrome()
            })
            .unwrap_or_else(|e| panic!("hashtag_view.jinja renders empty: {e:#}"));

        assert!(
            rendered.contains(
                r#"content="https://oeee.test/hashtags/%EA%B7%B8%EB%A6%BC""#
            ),
            "og:url should be the escaped canonical name, got: {}",
            &rendered[..rendered.find("</head>").unwrap_or(400)]
        );
        assert!(rendered.contains("hashtag-no-posts"));
    }

    #[test]
    fn the_hashtag_suggestions_are_options_a_keyboard_can_reach() {
        // The menu used to be plain <li>s that only answered a click, inside a
        // container announcing itself as a listbox.
        let env = test_support::env();
        let rendered = env
            .get_template("hashtag_autocomplete.jinja")
            .unwrap_or_else(|e| panic!("hashtag_autocomplete.jinja loads: {e:#}"))
            .render(context! {
                hashtags => vec![json!({
                    "name": "oekaki",
                    "display_name": "Oekaki",
                    "post_count": 12,
                })],
                ftl_lang => "en",
            })
            .unwrap_or_else(|e| panic!("hashtag_autocomplete.jinja renders: {e:#}"));

        assert!(rendered.contains(r#"role="option""#));
        assert!(rendered.contains(r#"id="hashtag-option-0""#));
        assert!(rendered.contains(r#"aria-selected="false""#));
        assert!(rendered.contains("hashtag-post-count(count=12)"));
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
            with_count.contains("(3)"),
            "the badge should show the count"
        );
        assert!(
            !without.contains('('),
            "zero unread shows no parenthesised count"
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
}
