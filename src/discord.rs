//! Asking Discord who signed in.
//!
//! Discord's OAuth2 is the plain authorization code flow, as Google's is,
//! but without OpenID Connect: it hands out no ID token, so there is no
//! nonce and nothing signed to check. The site sends the browser to Discord
//! with a `state` it keeps in the session, Discord sends it back to
//! `/auth/discord/callback` with a code, the site trades the code for an
//! access token over its own connection, with the client secret, and asks
//! Discord whose token it is (`/users/@me`). That answer, from Discord
//! itself over TLS, is what says who signed in; the code is good once, for
//! this site's client and redirect URI, and the state ties it to the browser
//! that set out. The token is used for that one question and kept nowhere.
//!
//! Discord comes back by a GET, which a SameSite=Lax cookie is sent with, so
//! nothing has to be bounced off a page of this site (see
//! `web::handlers::identity`).
//!
//! Most of the people who sign in this way have been asked into a
//! collaborative room from Discord (the desktop app's invitations,
//! oeee-cafe-desktop's discord.rs), and arrive with no account here: this is
//! the shortest way to one.

use anyhow::Result;
use serde::Deserialize;

use crate::config::DiscordConfig;
use crate::models::identity::{Provider, VerifiedIdentity};

const AUTHORIZE_URL: &str = "https://discord.com/oauth2/authorize";

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

/// Where to send the browser to sign in. `identify` for who they are and
/// `email` for an address Discord vouches for, nothing more. `prompt=none`
/// so someone who has let the site in before is not asked again.
pub fn authorize_url(config: &DiscordConfig, redirect_uri: &str, state: &str) -> String {
    let mut url = url::Url::parse(AUTHORIZE_URL).expect("Discord's authorize URL");
    url.query_pairs_mut()
        .append_pair("client_id", &config.client_id)
        .append_pair("redirect_uri", redirect_uri)
        .append_pair("response_type", "code")
        .append_pair("scope", "identify email")
        .append_pair("state", state)
        .append_pair("prompt", "none");
    url.into()
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: Option<String>,
}

/// Trades the code Discord sent the browser back with for an access token.
/// `Ok(None)` when Discord will not trade it: used already, expired, or not
/// this site's.
pub async fn exchange_code(
    config: &DiscordConfig,
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
        tracing::info!("Discord would not trade the code ({status}): {body}");
        return Ok(None);
    }
    Ok(response
        .json::<TokenResponse>()
        .await?
        .access_token
        .filter(|token| !token.is_empty()))
}

/// The parts of Discord's user object read here.
#[derive(Deserialize)]
struct User {
    id: String,
    username: Option<String>,
    global_name: Option<String>,
    email: Option<String>,
    verified: Option<bool>,
}

/// Asks Discord whose `access_token` this is. `Ok(None)` when Discord says
/// the token is no good.
pub async fn identify(
    config: &DiscordConfig,
    access_token: &str,
) -> Result<Option<VerifiedIdentity>> {
    let response = http()
        .get(&config.user_url)
        .bearer_auth(access_token)
        .send()
        .await?;
    if !response.status().is_success() {
        tracing::info!(
            "Discord would not say whose token it was ({})",
            response.status()
        );
        return Ok(None);
    }
    let user = response.json::<User>().await?;
    Ok(identity(user))
}

fn identity(user: User) -> Option<VerifiedIdentity> {
    if user.id.is_empty() {
        return None;
    }
    // The name they show, or failing that their username: either is only
    // offered for the display name of a new account.
    let name = user
        .global_name
        .or(user.username)
        .map(|name| name.trim().chars().take(255).collect::<String>())
        .filter(|name| !name.is_empty());
    // An address is trusted to sign into the account that has it (see
    // `VerifiedIdentity::email`), so only one Discord says was verified.
    let email = user
        .email
        .filter(|_| user.verified == Some(true))
        .map(|email| email.trim().chars().take(320).collect::<String>())
        .filter(|email| !email.is_empty());
    Some(VerifiedIdentity {
        provider: Provider::Discord,
        subject: user.id,
        name,
        email,
        purchased: None,
        bought_app: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    use axum::http::{HeaderMap, StatusCode};
    use axum::routing::{get, post};
    use axum::{Form, Json, Router};
    use serde_json::{json, Value};

    const CLIENT_ID: &str = "1558602531133722646";
    const SECRET: &str = "a-client-secret";
    const TOKEN: &str = "an-access-token";

    /// Discord's token endpoint and `/users/@me`, on a port of their own. The
    /// code `"good"` is traded for `TOKEN`, and `TOKEN` names `user`.
    async fn fake_discord(user: Value) -> DiscordConfig {
        let app = Router::new()
            .route(
                "/token",
                post(|Form(form): Form<HashMap<String, String>>| async move {
                    let ours = form.get("client_id").map(String::as_str) == Some(CLIENT_ID)
                        && form.get("client_secret").map(String::as_str) == Some(SECRET);
                    if !ours || form.get("code").map(String::as_str) != Some("good") {
                        return (
                            StatusCode::BAD_REQUEST,
                            Json(json!({"error": "invalid_grant"})),
                        );
                    }
                    (
                        StatusCode::OK,
                        Json(json!({"access_token": TOKEN, "token_type": "Bearer"})),
                    )
                }),
            )
            .route(
                "/users/@me",
                get(|headers: HeaderMap| async move {
                    let bearer = headers.get("authorization").and_then(|v| v.to_str().ok());
                    if bearer != Some(&format!("Bearer {TOKEN}")) {
                        return (StatusCode::UNAUTHORIZED, Json(json!({"code": 0})));
                    }
                    (StatusCode::OK, Json(user.clone()))
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        DiscordConfig {
            client_id: CLIENT_ID.to_string(),
            client_secret: SECRET.to_string(),
            token_url: format!("http://{addr}/token"),
            user_url: format!("http://{addr}/users/@me"),
        }
    }

    fn user() -> Value {
        json!({
            "id": "80351110224678912",
            "username": "nelly",
            "global_name": "넬리",
            "email": "nelly@example.test",
            "verified": true,
        })
    }

    const CALLBACK: &str = "https://oeee.cafe/auth/discord/callback";

    #[tokio::test]
    async fn a_code_is_traded_for_whoever_it_stands_for() {
        let config = fake_discord(user()).await;
        let token = exchange_code(&config, CALLBACK, "good")
            .await
            .unwrap()
            .expect("a token");
        assert_eq!(
            identify(&config, &token).await.unwrap(),
            Some(VerifiedIdentity {
                provider: Provider::Discord,
                subject: "80351110224678912".into(),
                name: Some("넬리".into()),
                email: Some("nelly@example.test".into()),
                purchased: None,
                bought_app: None,
            })
        );
        // A code Discord will not trade, or a token it does not know, is not
        // an error to report, only a sign-in that did not happen.
        assert_eq!(
            exchange_code(&config, CALLBACK, "stale").await.unwrap(),
            None
        );
        assert_eq!(identify(&config, "someone-elses").await.unwrap(), None);
    }

    #[tokio::test]
    async fn an_unverified_address_is_not_carried() {
        let mut unverified = user();
        unverified["verified"] = json!(false);
        let config = fake_discord(unverified).await;
        let identity = identify(&config, TOKEN).await.unwrap().unwrap();
        assert_eq!(identity.email, None);
        assert_eq!(identity.subject, "80351110224678912");
    }

    #[test]
    fn a_username_stands_in_for_a_name_never_shown() {
        let identity = identity(User {
            id: "1".into(),
            username: Some("nelly".into()),
            global_name: None,
            email: None,
            verified: None,
        })
        .unwrap();
        assert_eq!(identity.name.as_deref(), Some("nelly"));
        assert!(super::identity(User {
            id: String::new(),
            username: Some("nelly".into()),
            global_name: None,
            email: None,
            verified: None,
        })
        .is_none());
    }

    #[test]
    fn the_browser_is_sent_to_discord_with_what_comes_back() {
        let config = DiscordConfig {
            client_id: CLIENT_ID.to_string(),
            client_secret: SECRET.to_string(),
            token_url: String::new(),
            user_url: String::new(),
        };
        let url = url::Url::parse(&authorize_url(&config, CALLBACK, "the-state")).unwrap();
        let query: HashMap<_, _> = url.query_pairs().into_owned().collect();
        assert_eq!(url.host_str(), Some("discord.com"));
        assert_eq!(query["client_id"], CLIENT_ID);
        assert_eq!(query["redirect_uri"], CALLBACK);
        assert_eq!(query["response_type"], "code");
        assert_eq!(query["scope"], "identify email");
        assert_eq!(query["state"], "the-state");
        // The secret is only ever sent to Discord's token endpoint.
        assert!(!url.as_str().contains(SECRET));
    }
}
