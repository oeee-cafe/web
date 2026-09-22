//! Asking Apple who signed in.
//!
//! Sign in with Apple on the web: the site sends the browser to Apple with a
//! `state` and a `nonce` it keeps in the session, and Apple posts back an ID
//! token -- a JWT it signed, naming the person by `sub` and carrying the
//! nonce. The site checks the signature against Apple's published keys, that
//! the token was made for this site (`aud`) and for this sign-in (`nonce`),
//! and takes nothing else Apple's post says on its word.
//!
//! The iOS app signs in natively instead (`ASAuthorizationAppleIDProvider`),
//! with a state and nonce it asks the site for from inside its web view, and
//! posts the token it gets to the same place. Its token names the app's
//! bundle ID as audience; the rest is checked the same way.
//!
//! Apple says nothing here about what anyone has bought -- an ID token names
//! a person and nothing else, which is why `purchased` is always `None`
//! below. What the app sells goes through `app_store`, which asks a
//! different Apple with a different key.

use anyhow::{anyhow, Result};
use jsonwebtoken::{decode, decode_header, Algorithm, Validation};
use serde::Deserialize;

use crate::config::AppleConfig;
use crate::jwks;
use crate::models::identity::{Provider, VerifiedIdentity};

/// Who signs Apple's ID tokens, and where a browser is sent to sign in.
pub const ISSUER: &str = "https://appleid.apple.com";
const AUTHORIZE_URL: &str = "https://appleid.apple.com/auth/authorize";

/// Where to send the browser to sign in. Apple posts the answer to
/// `redirect_uri` (`response_mode=form_post`, which it requires when asked
/// for a name or an address).
pub fn authorize_url(config: &AppleConfig, redirect_uri: &str, state: &str, nonce: &str) -> String {
    let mut url = url::Url::parse(AUTHORIZE_URL).expect("Apple's authorize URL");
    url.query_pairs_mut()
        .append_pair("client_id", &config.client_id)
        .append_pair("redirect_uri", redirect_uri)
        .append_pair("response_type", "code id_token")
        .append_pair("response_mode", "form_post")
        .append_pair("scope", "name email")
        .append_pair("state", state)
        .append_pair("nonce", nonce);
    url.into()
}

#[derive(Deserialize)]
struct Claims {
    sub: String,
    nonce: Option<String>,
    email: Option<String>,
    /// `true`, or `"true"`: Apple has sent both.
    email_verified: Option<serde_json::Value>,
}

fn is_true(value: Option<&serde_json::Value>) -> bool {
    match value {
        Some(serde_json::Value::Bool(b)) => *b,
        Some(serde_json::Value::String(s)) => s == "true",
        _ => false,
    }
}

/// The `user` field Apple posts alongside the token, the first time a person
/// signs in to this site and never again. Not signed, so only ever a
/// suggestion for a new account's name.
#[derive(Deserialize)]
struct AppleUser {
    name: Option<AppleName>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct AppleName {
    first_name: Option<String>,
    last_name: Option<String>,
}

/// Whether a name is written family name first, without a space: Korean,
/// Japanese and Chinese names are.
fn family_name_first(name: &str) -> bool {
    name.chars().any(|c| {
        matches!(c,
            '\u{1100}'..='\u{11FF}'   // Hangul Jamo
            | '\u{3040}'..='\u{30FF}' // Hiragana, Katakana
            | '\u{3400}'..='\u{4DBF}' // CJK Extension A
            | '\u{4E00}'..='\u{9FFF}' // CJK Unified Ideographs
            | '\u{AC00}'..='\u{D7AF}' // Hangul Syllables
        )
    })
}

/// A display name from the `user` field, if it holds one.
fn name_from_user(user: Option<&str>) -> Option<String> {
    let user: AppleUser = serde_json::from_str(user?).ok()?;
    let name = user.name?;
    let first = name.first_name.unwrap_or_default().trim().to_string();
    let last = name.last_name.unwrap_or_default().trim().to_string();
    let joined = match (first.is_empty(), last.is_empty()) {
        (true, true) => return None,
        (false, true) => first,
        (true, false) => last,
        (false, false) if family_name_first(&first) || family_name_first(&last) => {
            format!("{last}{first}")
        }
        (false, false) => format!("{first} {last}"),
    };
    Some(joined.chars().take(255).collect())
}

/// Checks an ID token Apple posted back and says whose it is.
///
/// `nonce` is the one this sign-in was started with, kept in the session;
/// `user` is the `user` field of Apple's post, when there was one.
/// `Ok(None)` means the token is not one to accept: not Apple's, not for this
/// site, expired, or from another sign-in.
pub async fn verify_id_token(
    config: &AppleConfig,
    id_token: &str,
    nonce: &str,
    user: Option<&str>,
) -> Result<Option<VerifiedIdentity>> {
    let Ok(header) = decode_header(id_token) else {
        return Ok(None);
    };
    if header.alg != Algorithm::RS256 {
        return Ok(None);
    }
    let Some(kid) = header.kid else {
        return Ok(None);
    };
    let Some(key) = jwks::decoding_key(&config.keys_url, &kid).await? else {
        return Ok(None);
    };

    let mut validation = Validation::new(Algorithm::RS256);
    validation.set_issuer(&[ISSUER]);
    let audiences: Vec<&str> = std::iter::once(config.client_id.as_str())
        .chain(config.app_ids.iter().map(String::as_str))
        .collect();
    validation.set_audience(&audiences);
    validation.set_required_spec_claims(&["exp", "iss", "aud", "sub"]);
    let claims = match decode::<Claims>(id_token, &key, &validation) {
        Ok(data) => data.claims,
        Err(error) => {
            tracing::info!("Apple ID token turned away: {error}");
            return Ok(None);
        }
    };
    if claims.nonce.as_deref() != Some(nonce) || nonce.is_empty() {
        return Ok(None);
    }
    if claims.sub.is_empty() {
        return Err(anyhow!("Apple signed a token with an empty sub"));
    }

    // Only an address Apple has checked; a private relay address counts, and
    // mail sent to it reaches the person.
    let email = claims
        .email
        .filter(|_| is_true(claims.email_verified.as_ref()))
        .map(|email| email.trim().to_string())
        .filter(|email| !email.is_empty() && email.len() <= 320);

    Ok(Some(VerifiedIdentity {
        provider: Provider::Apple,
        subject: claims.sub,
        name: name_from_user(user),
        email,
        purchased: None,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    use axum::routing::get;
    use axum::{Json, Router};
    use jsonwebtoken::{encode, EncodingKey, Header};
    use serde_json::{json, Value};

    /// A throwaway RSA key, standing in for Apple's.
    const PRIVATE_KEY: &str = include_str!("testdata/apple_test_key.pem");
    const MODULUS: &str = include_str!("testdata/apple_test_key.n");
    const KID: &str = "test-key";
    const CLIENT_ID: &str = "cafe.oeee.web";
    const APP_ID: &str = "cafe.oeee";

    async fn fake_apple() -> AppleConfig {
        let app = Router::new().route(
            "/auth/keys",
            get(|| async {
                Json::<Value>(json!({"keys": [{
                    "kty": "RSA",
                    "kid": KID,
                    "use": "sig",
                    "alg": "RS256",
                    "n": MODULUS.trim(),
                    "e": "AQAB",
                }]}))
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        AppleConfig {
            client_id: CLIENT_ID.to_string(),
            app_ids: vec![APP_ID.to_string()],
            keys_url: format!("http://{addr}/auth/keys"),
        }
    }

    fn token(claims: Value) -> String {
        token_with(KID, claims)
    }

    fn token_with(kid: &str, claims: Value) -> String {
        let mut header = Header::new(Algorithm::RS256);
        header.kid = Some(kid.to_string());
        encode(
            &header,
            &claims,
            &EncodingKey::from_rsa_pem(PRIVATE_KEY.as_bytes()).unwrap(),
        )
        .unwrap()
    }

    fn claims() -> Value {
        json!({
            "iss": ISSUER,
            "aud": CLIENT_ID,
            "exp": chrono::Utc::now().timestamp() + 600,
            "iat": chrono::Utc::now().timestamp(),
            "sub": "001234.abcdef.0123",
            "nonce": "the-nonce",
            "email": "oeee@privaterelay.appleid.com",
            "email_verified": "true",
            "is_private_email": "true",
        })
    }

    fn with(mut claims: Value, key: &str, value: Value) -> Value {
        claims[key] = value;
        claims
    }

    #[tokio::test]
    async fn a_good_token_names_its_apple_account() {
        let config = fake_apple().await;
        let user = r#"{"name":{"firstName":"Oeee","lastName":"Cafe"},"email":"x@example.test"}"#;
        let identity = verify_id_token(&config, &token(claims()), "the-nonce", Some(user))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(identity.provider, Provider::Apple);
        assert_eq!(identity.subject, "001234.abcdef.0123");
        assert_eq!(identity.name.as_deref(), Some("Oeee Cafe"));
        // The token's address, not the unsigned `user` field's.
        assert_eq!(
            identity.email.as_deref(),
            Some("oeee@privaterelay.appleid.com")
        );
        assert_eq!(identity.purchased, None);
    }

    #[tokio::test]
    async fn the_ios_apps_token_names_the_app_and_is_accepted() {
        let config = fake_apple().await;
        let identity = verify_id_token(
            &config,
            &token(with(claims(), "aud", json!(APP_ID))),
            "the-nonce",
            Some(r#"{"name":{"firstName":"지혁","lastName":"서"}}"#),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(identity.subject, "001234.abcdef.0123");
        assert_eq!(identity.name.as_deref(), Some("서지혁"));
    }

    #[tokio::test]
    async fn an_unverified_address_is_not_carried() {
        let config = fake_apple().await;
        let identity = verify_id_token(
            &config,
            &token(with(claims(), "email_verified", json!(false))),
            "the-nonce",
            None,
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(identity.email, None);
        assert_eq!(identity.name, None);
    }

    #[tokio::test]
    async fn a_token_for_another_sign_in_or_site_is_turned_away() {
        let config = fake_apple().await;
        let turned_away = [
            // Another sign-in's nonce, or none.
            (token(claims()), "another-nonce"),
            (token(with(claims(), "nonce", Value::Null)), "the-nonce"),
            // Made for another site.
            (
                token(with(claims(), "aud", json!("com.example.other"))),
                "the-nonce",
            ),
            // Not Apple's.
            (
                token(with(claims(), "iss", json!("https://evil.test"))),
                "the-nonce",
            ),
            // Expired.
            (
                token(with(
                    claims(),
                    "exp",
                    json!(chrono::Utc::now().timestamp() - 3600),
                )),
                "the-nonce",
            ),
            // Signed with a key Apple does not have.
            (token_with("no-such-key", claims()), "the-nonce"),
            // Not a token.
            ("not.a.token".to_string(), "the-nonce"),
        ];
        for (id_token, nonce) in turned_away {
            assert!(
                verify_id_token(&config, &id_token, nonce, None)
                    .await
                    .unwrap()
                    .is_none(),
                "{id_token}"
            );
        }
    }

    #[tokio::test]
    async fn a_token_whose_signature_does_not_match_is_turned_away() {
        let config = fake_apple().await;
        // Someone else's claims under this token's signature.
        let good = token(claims());
        let other = token(with(claims(), "sub", json!("someone.else")));
        let (header, rest) = good.split_once('.').unwrap();
        let signature = rest.split_once('.').unwrap().1;
        let payload = other.split('.').nth(1).unwrap();
        let forged = format!("{header}.{payload}.{signature}");
        assert!(verify_id_token(&config, &forged, "the-nonce", None)
            .await
            .unwrap()
            .is_none());
    }

    #[test]
    fn a_name_is_written_the_way_its_language_writes_it() {
        let name = |first: &str, last: &str| {
            name_from_user(Some(
                &json!({"name": {"firstName": first, "lastName": last}}).to_string(),
            ))
        };
        assert_eq!(name("Oeee", "Cafe").as_deref(), Some("Oeee Cafe"));
        assert_eq!(name("지혁", "서").as_deref(), Some("서지혁"));
        assert_eq!(name("太郎", "山田").as_deref(), Some("山田太郎"));
        assert_eq!(name(" 오이 ", "").as_deref(), Some("오이"));
        assert_eq!(name("", " "), None);
        assert_eq!(name_from_user(Some("{}")), None);
        assert_eq!(name_from_user(Some("not json")), None);
        assert_eq!(name_from_user(None), None);
    }

    #[test]
    fn the_browser_is_sent_to_apple_with_what_comes_back() {
        let config = AppleConfig {
            client_id: CLIENT_ID.to_string(),
            app_ids: Vec::new(),
            keys_url: String::new(),
        };
        let url = url::Url::parse(&authorize_url(
            &config,
            "https://oeee.cafe/auth/apple/callback",
            "the-state",
            "the-nonce",
        ))
        .unwrap();
        let query: HashMap<_, _> = url.query_pairs().into_owned().collect();
        assert_eq!(url.host_str(), Some("appleid.apple.com"));
        assert_eq!(query["client_id"], CLIENT_ID);
        assert_eq!(
            query["redirect_uri"],
            "https://oeee.cafe/auth/apple/callback"
        );
        assert_eq!(query["response_mode"], "form_post");
        assert_eq!(query["state"], "the-state");
        assert_eq!(query["nonce"], "the-nonce");
    }
}
