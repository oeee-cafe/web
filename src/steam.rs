//! Asking Steam who signed in.
//!
//! The Oeee Cafe app on Steam asks the Steam client for a Web API ticket
//! (`GetAuthTicketForWebApi`) and hands it to the site. The site takes it to
//! Steam's partner Web API with the publisher key, and Steam answers with the
//! SteamID it was issued to. Nothing the app says about who is signed in is
//! taken on its word.

use std::sync::OnceLock;
use std::time::Duration;

use anyhow::{anyhow, Result};
use serde::Deserialize;

use crate::config::SteamConfig;
use crate::models::identity::{Provider, VerifiedIdentity};

/// What the app names when it asks for a ticket, and what the ticket is
/// checked against: a ticket made for some other service is not accepted
/// here. The desktop app's `STEAM_TICKET_IDENTITY` has to match.
pub const TICKET_IDENTITY: &str = "oeee-cafe";

fn http() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .build()
            .expect("reqwest client")
    })
}

#[derive(Deserialize)]
struct AuthenticateResponse {
    response: AuthenticateBody,
}

#[derive(Deserialize)]
struct AuthenticateBody {
    params: Option<AuthenticateParams>,
    error: Option<SteamError>,
}

#[derive(Deserialize)]
struct AuthenticateParams {
    result: String,
    steamid: String,
    // `vacbanned` is left unread: a VAC ban is for cheating in games and says
    // nothing about drawing.
    #[serde(default)]
    publisherbanned: bool,
}

#[derive(Deserialize, Debug)]
struct SteamError {
    errorcode: i64,
    errordesc: String,
}

#[derive(Deserialize)]
struct PlayerSummaries {
    response: PlayerSummariesBody,
}

#[derive(Deserialize)]
struct PlayerSummariesBody {
    #[serde(default)]
    players: Vec<PlayerSummary>,
}

#[derive(Deserialize)]
struct PlayerSummary {
    steamid: String,
    personaname: Option<String>,
}

/// Why a ticket was turned away, in the terms a person can act on.
#[derive(Debug, PartialEq, Eq)]
pub enum TicketRejected {
    /// Steam did not accept it: expired, for another app or identity, or not
    /// a ticket at all.
    Invalid,
    /// Oeee Cafe has banned the account on Steam.
    Banned,
}

/// Checks a hex-encoded Web API ticket with Steam and says whose it is.
///
/// Steam gives no email address, so the identity never carries one.
pub async fn verify_ticket(
    config: &SteamConfig,
    ticket: &str,
) -> Result<std::result::Result<VerifiedIdentity, TicketRejected>> {
    let ticket = ticket.trim();
    if ticket.is_empty() || ticket.len() > 8192 || !ticket.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Ok(Err(TicketRejected::Invalid));
    }

    let url = format!(
        "{}/ISteamUserAuth/AuthenticateUserTicket/v1/",
        config.web_api_url.trim_end_matches('/')
    );
    let response = http()
        .get(url)
        .query(&[
            ("key", config.web_api_key.as_str()),
            ("appid", &config.app_id.to_string()),
            ("ticket", ticket),
            ("identity", TICKET_IDENTITY),
        ])
        .send()
        .await?;
    let status = response.status();
    if status.is_server_error() {
        return Err(anyhow!(
            "Steam answered AuthenticateUserTicket with {status}"
        ));
    }
    if !status.is_success() {
        // 400 for a malformed ticket; 403 for a key that is not a
        // publisher's, which is ours to fix, not the player's.
        if status == reqwest::StatusCode::FORBIDDEN {
            return Err(anyhow!("Steam refused the Web API key (403)"));
        }
        return Ok(Err(TicketRejected::Invalid));
    }

    let body: AuthenticateResponse = response.json().await?;
    let params = match (body.response.params, body.response.error) {
        (Some(params), _) => params,
        (None, Some(error)) => {
            tracing::info!(
                code = error.errorcode,
                desc = %error.errordesc,
                "Steam rejected a sign-in ticket"
            );
            return Ok(Err(TicketRejected::Invalid));
        }
        (None, None) => {
            return Err(anyhow!(
                "AuthenticateUserTicket answered with neither params nor error"
            ))
        }
    };

    if params.result != "OK" || params.steamid.parse::<u64>().is_err() {
        return Ok(Err(TicketRejected::Invalid));
    }
    if params.publisherbanned {
        return Ok(Err(TicketRejected::Banned));
    }
    let purchased = owns_outright(config, &params.steamid)
        .await
        .unwrap_or_else(|error| {
            // Asked again at the next sign-in; the achievement waits.
            tracing::warn!("could not check Steam ownership: {error:#}");
            false
        });

    let name = persona_name(config, &params.steamid)
        .await
        .unwrap_or_else(|error| {
            tracing::warn!("could not look up a Steam persona name: {error:#}");
            None
        });

    Ok(Ok(VerifiedIdentity {
        provider: Provider::Steam,
        subject: params.steamid,
        name,
        email: None,
        purchased,
    }))
}

#[derive(Deserialize)]
struct OwnershipResponse {
    appownership: Ownership,
}

#[derive(Deserialize)]
struct Ownership {
    #[serde(default)]
    ownsapp: bool,
    #[serde(default)]
    permanent: bool,
    #[serde(default)]
    sitelicense: bool,
    #[serde(default)]
    timedtrial: bool,
    ownersteamid: Option<String>,
    result: Option<String>,
}

/// Whether `steam_id` bought Oeee Cafe on Steam: owns it for good, as itself.
/// Not a copy borrowed through Family Sharing (owned by someone else), a
/// free weekend or a timed trial (not permanent), or a cafe's site licence.
async fn owns_outright(config: &SteamConfig, steam_id: &str) -> Result<bool> {
    let url = format!(
        "{}/ISteamUser/CheckAppOwnership/v2/",
        config.web_api_url.trim_end_matches('/')
    );
    let response: OwnershipResponse = http()
        .get(url)
        .query(&[
            ("key", config.web_api_key.as_str()),
            ("steamid", steam_id),
            ("appid", &config.app_id.to_string()),
        ])
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let o = response.appownership;
    Ok(o.result.as_deref().unwrap_or("OK") == "OK"
        && o.ownsapp
        && o.permanent
        && !o.sitelicense
        && !o.timedtrial
        && o.ownersteamid.as_deref() == Some(steam_id))
}

/// The name the player goes by on Steam, offered as a new account's display
/// name. A convenience: sign-in goes ahead without it.
async fn persona_name(config: &SteamConfig, steam_id: &str) -> Result<Option<String>> {
    let url = format!(
        "{}/ISteamUser/GetPlayerSummaries/v2/",
        config.web_api_url.trim_end_matches('/')
    );
    let summaries: PlayerSummaries = http()
        .get(url)
        .query(&[("key", config.web_api_key.as_str()), ("steamids", steam_id)])
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    Ok(summaries
        .response
        .players
        .into_iter()
        .find(|player| player.steamid == steam_id)
        .and_then(|player| player.personaname)
        .map(|name| name.trim().to_string())
        .filter(|name| !name.is_empty()))
}

/// Unlocks `achievements` (their API names in Steamworks) for `steam_id`.
///
/// Server-side, with the publisher key, through `SetUserStatsForGame`: the
/// achievements are set to be unlocked by the game server only, so the app
/// cannot grant them and an account earns them however it drew -- in the
/// app, in a browser or on a phone.
pub async fn set_achievements(
    config: &SteamConfig,
    steam_id: &str,
    achievements: &[String],
) -> Result<()> {
    let url = format!(
        "{}/ISteamUserStats/SetUserStatsForGame/v1/",
        config.web_api_url.trim_end_matches('/')
    );
    let mut form = vec![
        ("key".to_string(), config.web_api_key.clone()),
        ("steamid".to_string(), steam_id.to_string()),
        ("appid".to_string(), config.app_id.to_string()),
        ("count".to_string(), achievements.len().to_string()),
    ];
    for (i, name) in achievements.iter().enumerate() {
        form.push((format!("name[{i}]"), name.clone()));
        form.push((format!("value[{i}]"), "1".to_string()));
    }
    let response = http().post(url).form(&form).send().await?;
    let status = response.status();
    let body = response.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(anyhow!("SetUserStatsForGame answered {status}: {body}"));
    }
    // Steam answers 200 with a result code in the body; 1 is success.
    if let Ok(json) = serde_json::from_str::<serde_json::Value>(&body) {
        let result = json
            .pointer("/result/result")
            .or_else(|| json.pointer("/response/result"))
            .and_then(serde_json::Value::as_i64);
        if let Some(code) = result.filter(|code| *code != 1) {
            return Err(anyhow!(
                "SetUserStatsForGame answered result {code}: {body}"
            ));
        }
    }
    Ok(())
}

/// Tells Steam about achievements it has not yet accepted, for as long as
/// the server runs. An achievement Steam turns away or cannot be reached for
/// is tried again next time round; nothing is lost by waiting.
///
/// Both colours run this for a moment during a deploy. Unlocking an
/// achievement twice is unlocking it once, so they do no harm.
pub async fn sync_achievements(db: sqlx::PgPool, config: SteamConfig) {
    use crate::models::achievement::{mark_synced, unsynced_achievements};

    let mut every = tokio::time::interval(Duration::from_secs(30));
    every.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        every.tick().await;
        let waiting = match db.begin().await {
            Ok(mut tx) => unsynced_achievements(&mut tx, 50).await,
            Err(error) => Err(error.into()),
        };
        let waiting = match waiting {
            Ok(waiting) => waiting,
            Err(error) => {
                tracing::warn!("could not list achievements for Steam: {error:#}");
                continue;
            }
        };
        for account in waiting {
            if let Err(error) =
                set_achievements(&config, &account.steam_id, &account.achievements).await
            {
                tracing::warn!(
                    user_id = %account.user_id,
                    "Steam did not take achievements: {error:#}"
                );
                continue;
            }
            let marked = async {
                let mut tx = db.begin().await?;
                mark_synced(&mut tx, account.user_id, &account.achievements).await?;
                tx.commit().await?;
                anyhow::Ok(())
            };
            if let Err(error) = marked.await {
                tracing::warn!("could not record achievements Steam took: {error:#}");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::extract::Query;
    use axum::routing::get;
    use axum::{Json, Router};
    use serde_json::{json, Value};
    use std::collections::HashMap;

    const STEAM_ID: &str = "76561197960287930";

    /// A stand-in for Steam's partner Web API that accepts one ticket.
    async fn fake_steam(publisher_banned: bool) -> SteamConfig {
        fake_steam_owned_by(publisher_banned, STEAM_ID).await
    }

    async fn fake_steam_owned_by(publisher_banned: bool, owner: &'static str) -> SteamConfig {
        let app = Router::new()
            .route(
                "/ISteamUserAuth/AuthenticateUserTicket/v1/",
                get(move |Query(q): Query<HashMap<String, String>>| async move {
                    let ok = q.get("key").map(String::as_str) == Some("publisher-key")
                        && q.get("appid").map(String::as_str) == Some("480")
                        && q.get("identity").map(String::as_str) == Some(TICKET_IDENTITY)
                        && q.get("ticket").map(String::as_str) == Some("14000000abcdef");
                    Json(if ok {
                        json!({"response": {"params": {
                            "result": "OK",
                            "steamid": STEAM_ID,
                            "ownersteamid": STEAM_ID,
                            "vacbanned": false,
                            "publisherbanned": publisher_banned,
                        }}})
                    } else {
                        json!({"response": {"error": {"errorcode": 101, "errordesc": "Invalid ticket"}}})
                    })
                }),
            )
            .route(
                "/ISteamUser/CheckAppOwnership/v2/",
                get(move || async move {
                    Json::<Value>(json!({"appownership": {
                        "ownsapp": true,
                        "permanent": true,
                        "timestamp": "2026-09-22T00:00:00Z",
                        "ownersteamid": owner,
                        "sitelicense": false,
                        "timedtrial": false,
                        "usercanceled": false,
                        "result": "OK",
                    }}))
                }),
            )
            .route(
                "/ISteamUser/GetPlayerSummaries/v2/",
                get(|| async {
                    Json::<Value>(json!({"response": {"players": [
                        {"steamid": STEAM_ID, "personaname": "  오이 "}
                    ]}}))
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        SteamConfig {
            app_id: 480,
            web_api_key: "publisher-key".to_string(),
            web_api_url: format!("http://{addr}"),
        }
    }

    #[tokio::test]
    async fn a_good_ticket_names_its_steam_account() {
        let config = fake_steam(false).await;
        let identity = verify_ticket(&config, "14000000abcdef")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(identity.provider, Provider::Steam);
        assert_eq!(identity.subject, STEAM_ID);
        assert_eq!(identity.name.as_deref(), Some("오이"));
        assert_eq!(identity.email, None);
        assert!(identity.purchased);
    }

    #[tokio::test]
    async fn a_borrowed_copy_is_not_a_purchase() {
        // Family Sharing: the app is owned, by somebody else.
        let config = fake_steam_owned_by(false, "76561197960287931").await;
        let identity = verify_ticket(&config, "14000000abcdef")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(identity.subject, STEAM_ID);
        assert!(!identity.purchased);
    }

    #[tokio::test]
    async fn a_ticket_steam_rejects_is_invalid() {
        let config = fake_steam(false).await;
        let result = verify_ticket(&config, "deadbeef").await.unwrap();
        assert_eq!(result.unwrap_err(), TicketRejected::Invalid);
    }

    #[tokio::test]
    async fn something_that_is_not_hex_never_reaches_steam() {
        let config = SteamConfig {
            app_id: 480,
            web_api_key: "k".to_string(),
            // Nothing listens here; reaching it would be an error, not Invalid.
            web_api_url: "http://127.0.0.1:9".to_string(),
        };
        for ticket in ["", "   ", "not-hex", "14000000&key=x"] {
            let result = verify_ticket(&config, ticket).await.unwrap();
            assert_eq!(result.unwrap_err(), TicketRejected::Invalid, "{ticket:?}");
        }
    }

    #[tokio::test]
    async fn achievements_go_to_steam_as_one_form() {
        use axum::extract::Form;
        use axum::routing::post;
        let seen = std::sync::Arc::new(std::sync::Mutex::new(None));
        let app = Router::new().route(
            "/ISteamUserStats/SetUserStatsForGame/v1/",
            post({
                let seen = seen.clone();
                move |Form(form): Form<HashMap<String, String>>| async move {
                    let ok = form.get("key").map(String::as_str) == Some("publisher-key");
                    *seen.lock().unwrap() = Some(form);
                    Json(json!({"result": {"result": if ok { 1 } else { 8 }}}))
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let mut config = SteamConfig {
            app_id: 480,
            web_api_key: "publisher-key".to_string(),
            web_api_url: format!("http://{addr}"),
        };

        let achievements = ["FIRST_DRAWING".to_string(), "FIRST_RELAY".to_string()];
        set_achievements(&config, STEAM_ID, &achievements)
            .await
            .unwrap();
        let form = seen.lock().unwrap().take().unwrap();
        assert_eq!(form["steamid"], STEAM_ID);
        assert_eq!(form["appid"], "480");
        assert_eq!(form["count"], "2");
        assert_eq!(form["name[0]"], "FIRST_DRAWING");
        assert_eq!(form["value[0]"], "1");
        assert_eq!(form["name[1]"], "FIRST_RELAY");
        assert_eq!(form["value[1]"], "1");

        config.web_api_key = "wrong".to_string();
        assert!(set_achievements(&config, STEAM_ID, &achievements)
            .await
            .is_err());
    }

    #[tokio::test]
    async fn a_publisher_ban_turns_the_ticket_away() {
        let config = fake_steam(true).await;
        let result = verify_ticket(&config, "14000000abcdef").await.unwrap();
        assert_eq!(result.unwrap_err(), TicketRejected::Banned);
    }
}
