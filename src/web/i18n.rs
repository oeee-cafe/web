//! Fluent bundles for a request, and the extractors that pick the language.
//!
//! Which language a request gets is decided in `web::language`, which makes
//! the `lang` cookie be the `Accept-Language` header before anything here
//! reads it; this module only turns that header (or the account's own choice)
//! into a bundle.

use axum::extract::FromRequestParts;
use axum::http::header::{HeaderValue, ACCEPT_LANGUAGE};
use axum::http::request::Parts;
use axum::http::StatusCode;
use fluent::bundle::FluentBundle;
use fluent::FluentResource;
use fluent_langneg::{
    convert_vec_str_to_langids_lossy, negotiate_languages, parse_accepted_languages,
    NegotiationStrategy,
};
use intl_memoizer::concurrent::IntlLangMemoizer;

use crate::locale::LOCALES;
use crate::models::user::{AuthSession, Language};

pub type Bundle = FluentBundle<&'static FluentResource, IntlLangMemoizer>;

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

pub struct ExtractAcceptLanguage(pub HeaderValue);

impl<S> FromRequestParts<S> for ExtractAcceptLanguage
where
    S: Send + Sync,
{
    type Rejection = (StatusCode, &'static str);

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        Ok(ExtractAcceptLanguage(accept_language(parts)))
    }
}

fn accept_language(parts: &Parts) -> HeaderValue {
    parts
        .headers
        .get(ACCEPT_LANGUAGE)
        .cloned()
        .unwrap_or_else(|| HeaderValue::from_static(""))
}

/// Extractor that provides the computed locale string for templates
pub struct ExtractFtlLang(pub String);

impl<S> FromRequestParts<S> for ExtractFtlLang
where
    S: Send + Sync,
{
    type Rejection = (StatusCode, &'static str);

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let accept_language = accept_language(parts);

        let auth_session = AuthSession::from_request_parts(parts, state)
            .await
            .map_err(|_| {
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Failed to extract auth session",
                )
            })?;

        let user_preferred_language = auth_session
            .user
            .as_ref()
            .and_then(|u| u.preferred_language.clone());

        Ok(ExtractFtlLang(ftl_lang(
            &accept_language,
            user_preferred_language,
        )))
    }
}

/// The locale a page is rendered in, as templates spell it (`"ko"`, ...).
pub fn ftl_lang(
    accept_language: &HeaderValue,
    user_preferred_language: Option<Language>,
) -> String {
    get_bundle(accept_language, user_preferred_language)
        .locales
        .first()
        .map(|l| l.to_string())
        .unwrap_or_else(|| "en".to_string())
}

pub(crate) fn get_bundle(
    accept_language: &HeaderValue,
    user_preferred_language: Option<Language>,
) -> Bundle {
    let lang_code = match user_preferred_language {
        Some(Language::Ko) => "ko",
        Some(Language::Ja) => "ja",
        Some(Language::En) => "en",
        Some(Language::Zh) => "zh",
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

            match supported.first().map(|l| l.language.as_str()) {
                Some("ko") => "ko",
                Some("ja") => "ja",
                Some("zh") => "zh",
                _ => "en",
            }
        }
    };

    let ftl = LOCALES
        .get(lang_code)
        .or_else(|| LOCALES.get("en"))
        .expect("English locale must exist");

    let lang_id = lang_code
        .parse()
        .expect("Hardcoded language string should parse");
    let mut bundle = FluentBundle::new_concurrent(vec![lang_id]);
    bundle.add_resource(ftl).expect("Failed to add a resource.");

    bundle
}

/// Helper function to safely get a Fluent message without panicking
/// Returns the translation key itself if the message is not found
pub fn safe_get_message(
    bundle: &FluentBundle<&FluentResource, IntlLangMemoizer>,
    key: &str,
) -> String {
    safe_format_message(bundle, key, None)
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

#[cfg(test)]
mod tests {
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
