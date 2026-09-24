//! Asking Google who signed in.
//!
//! Sign in with Google on the web is the plain authorization code flow: the
//! site sends the browser to Google with a `state` and a `nonce` it keeps in
//! the session, Google sends it back to `/auth/google/callback` with a code,
//! and the site trades the code for an ID token over its own connection to
//! Google, with the client secret. That token -- a JWT Google signed, naming
//! the person by `sub` and carrying the nonce -- is what says who signed in.
//!
//! Google comes back by a GET, which a SameSite=Lax cookie is sent with, so
//! nothing has to be bounced off a page of this site the way Apple's
//! `form_post` answer is (see `web::handlers::identity`).
//!
//! Google refuses this flow in an embedded web view, which is what the apps
//! are. The iOS, macOS and Windows apps run this same flow in a browser of
//! the system's instead and hand the sign-in back (`handoff`). The Android
//! app signs in with Credential Manager: it asks the site for a nonce from
//! inside the page and posts the ID token it ends up with to `/auth/google`,
//! where it is checked exactly as one traded for a code is. Credential
//! Manager is given the site's own client id as its server client id, so its
//! audience is the same one, and the only one a token is accepted for.

use anyhow::{anyhow, Result};
use jsonwebtoken::{decode, decode_header, Algorithm, Validation};
use serde::Deserialize;

use crate::config::GoogleConfig;
use crate::jwks;
use crate::models::identity::{Provider, VerifiedIdentity};

/// Who signs Google's ID tokens. Google has issued both spellings for years
/// and its own documentation says to accept either.
pub const ISSUERS: [&str; 2] = ["https://accounts.google.com", "accounts.google.com"];

const AUTHORIZE_URL: &str = "https://accounts.google.com/o/oauth2/v2/auth";

fn http() -> &'static reqwest::Client {
    use std::sync::OnceLock;
    use std::time::Duration;
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .build()
            .expect("reqwest client")
    })
}

/// Where to send the browser to sign in. `prompt=select_account` so someone
/// signed in to several Google accounts is asked which, rather than being
/// given whichever the browser is holding.
pub fn authorize_url(
    config: &GoogleConfig,
    redirect_uri: &str,
    state: &str,
    nonce: &str,
) -> String {
    let mut url = url::Url::parse(AUTHORIZE_URL).expect("Google's authorize URL");
    url.query_pairs_mut()
        .append_pair("client_id", &config.client_id)
        .append_pair("redirect_uri", redirect_uri)
        .append_pair("response_type", "code")
        .append_pair("scope", "openid email profile")
        .append_pair("state", state)
        .append_pair("nonce", nonce)
        .append_pair("prompt", "select_account");
    url.into()
}

#[derive(Deserialize)]
struct TokenResponse {
    id_token: Option<String>,
}

/// Trades the code Google sent the browser back with for the ID token it
/// stands for. `Ok(None)` when Google will not trade it: used already,
/// expired, or not this site's.
pub async fn exchange_code(
    config: &GoogleConfig,
    redirect_uri: &str,
    code: &str,
) -> Result<Option<String>> {
    let response = http()
        .post(&config.token_url)
        .form(&[
            ("client_id", config.client_id.as_str()),
            ("client_secret", config.client_secret.as_str()),
            ("code", code),
            ("grant_type", "authorization_code"),
            ("redirect_uri", redirect_uri),
        ])
        .send()
        .await?;
    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        tracing::info!("Google would not trade the code ({status}): {body}");
        return Ok(None);
    }
    Ok(response.json::<TokenResponse>().await?.id_token)
}

#[derive(Deserialize)]
struct Claims {
    sub: String,
    nonce: Option<String>,
    email: Option<String>,
    email_verified: Option<bool>,
    /// The person's full name, written as their own locale writes it.
    name: Option<String>,
}

/// Checks an ID token of Google's and says whose it is.
///
/// `nonce` is the one this sign-in was started with, kept in the session.
/// `Ok(None)` means the token is not one to accept: not Google's, not for
/// this site, expired, or from another sign-in.
pub async fn verify_id_token(
    config: &GoogleConfig,
    id_token: &str,
    nonce: &str,
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
    validation.set_issuer(&ISSUERS);
    validation.set_audience(&[config.client_id.as_str()]);
    validation.set_required_spec_claims(&["exp", "iss", "aud", "sub"]);
    let claims = match decode::<Claims>(id_token, &key, &validation) {
        Ok(data) => data.claims,
        Err(error) => {
            tracing::info!("Google ID token turned away: {error}");
            return Ok(None);
        }
    };
    if claims.nonce.as_deref() != Some(nonce) || nonce.is_empty() {
        return Ok(None);
    }
    if claims.sub.is_empty() {
        return Err(anyhow!("Google signed a token with an empty sub"));
    }

    // Only an address Google has checked. A Workspace account's address is
    // its domain's to give and take away, which is no different from a
    // personal one being closed and reissued; what it says now is what the
    // site goes by, as it does for Apple.
    let email = claims
        .email
        .filter(|_| claims.email_verified == Some(true))
        .map(|email| email.trim().to_string())
        .filter(|email| !email.is_empty() && email.len() <= 320);

    let name = claims
        .name
        .map(|name| name.trim().chars().take(255).collect::<String>())
        .filter(|name| !name.is_empty());

    Ok(Some(VerifiedIdentity {
        provider: Provider::Google,
        subject: claims.sub,
        name,
        email,
        purchased: None,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    use axum::routing::{get, post};
    use axum::{Form, Json, Router};
    use jsonwebtoken::{encode, EncodingKey, Header};
    use serde_json::{json, Value};

    /// A throwaway RSA key, standing in for Google's.
    const PRIVATE_KEY: &str = include_str!("testdata/apple_test_key.pem");
    const MODULUS: &str = include_str!("testdata/apple_test_key.n");
    const KID: &str = "test-key";
    const CLIENT_ID: &str = "123.apps.googleusercontent.com";
    const SECRET: &str = "a-client-secret";

    /// Google's key set and token endpoint, on a port of their own. The
    /// token endpoint trades the code `"good"` for a token of `claims`, and
    /// nothing else for anything.
    async fn fake_google(traded: Value) -> GoogleConfig {
        let app = Router::new()
            .route(
                "/certs",
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
            )
            .route(
                "/token",
                post(|Form(form): Form<HashMap<String, String>>| async move {
                    let ours = form.get("client_id").map(String::as_str) == Some(CLIENT_ID)
                        && form.get("client_secret").map(String::as_str) == Some(SECRET);
                    if !ours || form.get("code").map(String::as_str) != Some("good") {
                        return (
                            axum::http::StatusCode::BAD_REQUEST,
                            Json(json!({"error": "invalid_grant"})),
                        );
                    }
                    (
                        axum::http::StatusCode::OK,
                        Json(json!({"id_token": token(traded.clone())})),
                    )
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        GoogleConfig {
            client_id: CLIENT_ID.to_string(),
            client_secret: SECRET.to_string(),
            keys_url: format!("http://{addr}/certs"),
            token_url: format!("http://{addr}/token"),
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
            "iss": "https://accounts.google.com",
            "aud": CLIENT_ID,
            "exp": chrono::Utc::now().timestamp() + 600,
            "iat": chrono::Utc::now().timestamp(),
            "sub": "110169484474386276334",
            "nonce": "the-nonce",
            "email": "oeee@example.test",
            "email_verified": true,
            "name": "서지혁",
        })
    }

    fn with(mut claims: Value, key: &str, value: Value) -> Value {
        claims[key] = value;
        claims
    }

    #[tokio::test]
    async fn a_good_token_names_its_google_account() {
        let config = fake_google(claims()).await;
        let identity = verify_id_token(&config, &token(claims()), "the-nonce")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(identity.provider, Provider::Google);
        assert_eq!(identity.subject, "110169484474386276334");
        assert_eq!(identity.name.as_deref(), Some("서지혁"));
        assert_eq!(identity.email.as_deref(), Some("oeee@example.test"));
        assert_eq!(identity.purchased, None);
    }

    /// Google issues `iss` both with and without the scheme.
    #[tokio::test]
    async fn either_spelling_of_the_issuer_is_googles() {
        let config = fake_google(claims()).await;
        let bare = token(with(claims(), "iss", json!("accounts.google.com")));
        assert!(verify_id_token(&config, &bare, "the-nonce")
            .await
            .unwrap()
            .is_some());
    }

    #[tokio::test]
    async fn an_unverified_address_is_not_carried() {
        let config = fake_google(claims()).await;
        let identity = verify_id_token(
            &config,
            &token(with(claims(), "email_verified", json!(false))),
            "the-nonce",
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(identity.email, None);
        // The name is the token's own, and stays.
        assert_eq!(identity.name.as_deref(), Some("서지혁"));
    }

    #[tokio::test]
    async fn a_token_for_another_sign_in_or_site_is_turned_away() {
        let config = fake_google(claims()).await;
        let turned_away = [
            // Another sign-in's nonce, or none.
            (token(claims()), "another-nonce"),
            (token(with(claims(), "nonce", Value::Null)), "the-nonce"),
            // Made for another site -- another Google client, such as
            // somebody else's app asking the same person to sign in.
            (
                token(with(claims(), "aud", json!("999.apps.googleusercontent.com"))),
                "the-nonce",
            ),
            // Not Google's.
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
            // Signed with a key Google does not have.
            (token_with("no-such-key", claims()), "the-nonce"),
            // Not a token.
            ("not.a.token".to_string(), "the-nonce"),
        ];
        for (id_token, nonce) in turned_away {
            assert!(
                verify_id_token(&config, &id_token, nonce)
                    .await
                    .unwrap()
                    .is_none(),
                "{id_token}"
            );
        }
    }

    #[tokio::test]
    async fn a_token_whose_signature_does_not_match_is_turned_away() {
        let config = fake_google(claims()).await;
        // Someone else's claims under this token's signature.
        let good = token(claims());
        let other = token(with(claims(), "sub", json!("someone.else")));
        let (header, rest) = good.split_once('.').unwrap();
        let signature = rest.split_once('.').unwrap().1;
        let payload = other.split('.').nth(1).unwrap();
        let forged = format!("{header}.{payload}.{signature}");
        assert!(verify_id_token(&config, &forged, "the-nonce")
            .await
            .unwrap()
            .is_none());
    }

    #[tokio::test]
    async fn a_code_is_traded_for_the_token_it_stands_for() {
        let config = fake_google(claims()).await;
        let id_token = exchange_code(&config, "https://oeee.cafe/auth/google/callback", "good")
            .await
            .unwrap()
            .expect("an ID token");
        let identity = verify_id_token(&config, &id_token, "the-nonce")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(identity.subject, "110169484474386276334");

        // A code Google will not trade is not an error to report, only a
        // sign-in that did not happen.
        assert_eq!(
            exchange_code(&config, "https://oeee.cafe/auth/google/callback", "stale")
                .await
                .unwrap(),
            None
        );
    }

    #[test]
    fn the_browser_is_sent_to_google_with_what_comes_back() {
        let config = GoogleConfig {
            client_id: CLIENT_ID.to_string(),
            client_secret: SECRET.to_string(),
            keys_url: String::new(),
            token_url: String::new(),
        };
        let url = url::Url::parse(&authorize_url(
            &config,
            "https://oeee.cafe/auth/google/callback",
            "the-state",
            "the-nonce",
        ))
        .unwrap();
        let query: HashMap<_, _> = url.query_pairs().into_owned().collect();
        assert_eq!(url.host_str(), Some("accounts.google.com"));
        assert_eq!(query["client_id"], CLIENT_ID);
        assert_eq!(
            query["redirect_uri"],
            "https://oeee.cafe/auth/google/callback"
        );
        assert_eq!(query["response_type"], "code");
        assert_eq!(query["state"], "the-state");
        assert_eq!(query["nonce"], "the-nonce");
        // The secret is only ever sent to Google's token endpoint.
        assert!(!url.as_str().contains(SECRET));
    }
}
