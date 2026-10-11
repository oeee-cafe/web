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
use serde::{Deserialize, Serialize};

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

/// What the desktop app asks to be traded for a Discord account's tokens
/// (`/api/discord/token`): the code its consent screen gave, with the
/// redirect and PKCE verifier it was asked with, or a refresh token.
///
/// The app connects the player's own Discord account with the Social SDK,
/// to list their friends and invite them into a room
/// (oeee-cafe-desktop's discord/linking.rs). This application is a
/// confidential client -- it signs people in here with its secret -- and
/// the SDK can trade codes itself only for public ones, so the app asks
/// here and the site adds the secret. Nothing is kept, and nothing about the
/// tokens is tied to an Oeee Cafe account: they are the app's to hold.
#[derive(Debug, Deserialize)]
#[serde(tag = "grant_type", rename_all = "snake_case")]
pub enum AppGrant {
    AuthorizationCode {
        code: String,
        redirect_uri: String,
        code_verifier: String,
    },
    RefreshToken {
        refresh_token: String,
    },
}

/// The tokens handed back to the app: Discord's own, and only these.
#[derive(Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct AppTokens {
    pub access_token: String,
    pub refresh_token: String,
    pub expires_in: i64,
}

/// Whether `redirect_uri` is where the SDK listens on the player's own
/// computer for the consent screen's answer: http on 127.0.0.1, any port,
/// at /callback. A code for any other redirect -- this site's own sign-in
/// among them -- is not the app's to trade.
pub fn is_apps_redirect(redirect_uri: &str) -> bool {
    url::Url::parse(redirect_uri).is_ok_and(|url| {
        url.scheme() == "http"
            && url.host_str() == Some("127.0.0.1")
            && url.path() == "/callback"
            && url.query().is_none()
            && url.username().is_empty()
    })
}

/// Trades what the app sent for tokens. `Ok(None)` when Discord will not:
/// a code used or stale, a refresh token revoked, or a redirect that is not
/// the app's.
pub async fn app_tokens(config: &DiscordConfig, grant: &AppGrant) -> Result<Option<AppTokens>> {
    let mut form = vec![
        ("client_id", config.client_id.as_str()),
        ("client_secret", config.client_secret.as_str()),
    ];
    match grant {
        AppGrant::AuthorizationCode {
            code,
            redirect_uri,
            code_verifier,
        } => {
            if !is_apps_redirect(redirect_uri) {
                return Ok(None);
            }
            form.extend([
                ("grant_type", "authorization_code"),
                ("code", code.as_str()),
                ("redirect_uri", redirect_uri.as_str()),
                ("code_verifier", code_verifier.as_str()),
            ]);
        }
        AppGrant::RefreshToken { refresh_token } => {
            form.extend([
                ("grant_type", "refresh_token"),
                ("refresh_token", refresh_token.as_str()),
            ]);
        }
    }
    let response = http().post(&config.token_url).form(&form).send().await?;
    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        tracing::info!("Discord would not trade the app's grant ({status}): {body}");
        return Ok(None);
    }
    Ok(Some(response.json::<AppTokens>().await?))
}

/// Tells Discord the app is done with a token, which takes the application
/// off the player's authorized apps. What Discord answers is not the app's
/// business: it forgets the token either way.
pub async fn revoke(config: &DiscordConfig, token: &str) -> Result<()> {
    http()
        .post(&config.revoke_url)
        .form(&[
            ("client_id", config.client_id.as_str()),
            ("client_secret", config.client_secret.as_str()),
            ("token", token),
            ("token_type_hint", "refresh_token"),
        ])
        .send()
        .await?;
    Ok(())
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
    const REFRESH: &str = "a-refresh-token";

    /// Discord's token endpoint and `/users/@me`, on a port of their own. The
    /// code `"good"` is traded for `TOKEN` -- with the verifier
    /// `"the-verifier"`, for a redirect to the app's -- and so is `REFRESH`;
    /// `TOKEN` names `user`.
    async fn fake_discord(user: Value) -> DiscordConfig {
        let app = Router::new()
            .route(
                "/token",
                post(|Form(form): Form<HashMap<String, String>>| async move {
                    let ours = form.get("client_id").map(String::as_str) == Some(CLIENT_ID)
                        && form.get("client_secret").map(String::as_str) == Some(SECRET);
                    let field = |name: &str| form.get(name).map(String::as_str);
                    let granted = match field("grant_type") {
                        Some("authorization_code") => {
                            field("code") == Some("good")
                                && (!field("redirect_uri")
                                    .unwrap_or("")
                                    .starts_with("http://127.0.0.1")
                                    || field("code_verifier") == Some("the-verifier"))
                        }
                        Some("refresh_token") => field("refresh_token") == Some(REFRESH),
                        _ => false,
                    };
                    if !ours || !granted {
                        return (
                            StatusCode::BAD_REQUEST,
                            Json(json!({"error": "invalid_grant"})),
                        );
                    }
                    (
                        StatusCode::OK,
                        Json(json!({
                            "access_token": TOKEN,
                            "token_type": "Bearer",
                            "refresh_token": REFRESH,
                            "expires_in": 604800,
                            "scope": "identify email",
                        })),
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
            revoke_url: format!("http://{addr}/revoke"),
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

    #[tokio::test]
    async fn the_apps_code_and_refresh_token_are_traded_for_tokens() {
        let config = fake_discord(user()).await;
        let traded = app_tokens(
            &config,
            &AppGrant::AuthorizationCode {
                code: "good".into(),
                redirect_uri: "http://127.0.0.1:54321/callback".into(),
                code_verifier: "the-verifier".into(),
            },
        )
        .await
        .unwrap();
        assert_eq!(
            traded,
            Some(AppTokens {
                access_token: TOKEN.into(),
                refresh_token: REFRESH.into(),
                expires_in: 604800,
            })
        );
        let refreshed = app_tokens(
            &config,
            &AppGrant::RefreshToken {
                refresh_token: REFRESH.into(),
            },
        )
        .await
        .unwrap();
        assert!(refreshed.is_some());

        // The wrong verifier, a stale refresh token: Discord says no, and
        // so does this.
        let wrong = AppGrant::AuthorizationCode {
            code: "good".into(),
            redirect_uri: "http://127.0.0.1:54321/callback".into(),
            code_verifier: "another".into(),
        };
        assert_eq!(app_tokens(&config, &wrong).await.unwrap(), None);
        let stale = AppGrant::RefreshToken {
            refresh_token: "revoked".into(),
        };
        assert_eq!(app_tokens(&config, &stale).await.unwrap(), None);
    }

    #[tokio::test]
    async fn only_a_code_given_to_the_app_is_traded_for_it() {
        let config = fake_discord(user()).await;
        // A code given to this site's sign-in, which Discord would trade,
        // is not the app's: it is never asked.
        let sites = AppGrant::AuthorizationCode {
            code: "good".into(),
            redirect_uri: CALLBACK.into(),
            code_verifier: "the-verifier".into(),
        };
        assert_eq!(app_tokens(&config, &sites).await.unwrap(), None);

        assert!(is_apps_redirect("http://127.0.0.1/callback"));
        assert!(is_apps_redirect("http://127.0.0.1:61234/callback"));
        for redirect in [
            "https://127.0.0.1/callback",
            "http://localhost/callback",
            "http://127.0.0.1.example.com/callback",
            "http://evil@127.0.0.1/callback",
            "http://127.0.0.1/callback?next=x",
            "http://127.0.0.1/other",
            "https://oeee.cafe/auth/discord/callback",
            "",
        ] {
            assert!(!is_apps_redirect(redirect), "{redirect}");
        }
    }

    #[test]
    fn the_app_says_which_grant_it_is_asking_for() {
        let code: AppGrant = serde_json::from_value(json!({
            "grant_type": "authorization_code",
            "code": "c",
            "redirect_uri": "http://127.0.0.1/callback",
            "code_verifier": "v",
        }))
        .unwrap();
        assert!(matches!(code, AppGrant::AuthorizationCode { .. }));
        let refresh: AppGrant =
            serde_json::from_value(json!({"grant_type": "refresh_token", "refresh_token": "r"}))
                .unwrap();
        assert!(matches!(refresh, AppGrant::RefreshToken { .. }));
        assert!(
            serde_json::from_value::<AppGrant>(json!({"grant_type": "client_credentials"}))
                .is_err()
        );
    }

    #[test]
    fn the_browser_is_sent_to_discord_with_what_comes_back() {
        let config = DiscordConfig {
            client_id: CLIENT_ID.to_string(),
            client_secret: SECRET.to_string(),
            token_url: String::new(),
            user_url: String::new(),
            revoke_url: String::new(),
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
