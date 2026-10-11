//! Asking Steam who signed in, in a browser: Steam's OpenID 2.0 sign-in,
//! for everywhere but the Oeee Cafe app on Steam, which has a ticket
//! (`super::verify_ticket`).
//!
//! The site sends the browser to Steam's sign-in page with a `return_to` of
//! its own, holding a state it keeps in the session, and Steam sends the
//! browser back there with a signed assertion naming the account
//! (`https://steamcommunity.com/openid/id/<SteamID64>`). Nothing in that
//! assertion is taken on the browser's word: every field it came back with
//! is posted back to Steam (`check_authentication`), over the site's own
//! connection, and only Steam's `is_valid:true` makes it good. Steam
//! refuses to vouch for an assertion twice, and the state ties it to the
//! browser that set out.
//!
//! A sign-in here says who someone is and nothing more. It never asks what
//! they own, so it earns no `STEAM_SUPPORTER` and records no Supporter Pack:
//! those are for a sign-in in the Steam app, and for the site's own
//! rechecks. Nor does it say whether Oeee Cafe has banned the account on
//! Steam, which only a ticket answers.

use std::collections::HashMap;

use anyhow::{anyhow, Result};

use crate::config::SteamConfig;
use crate::models::identity::{Provider, VerifiedIdentity};

const NS: &str = "http://specs.openid.net/auth/2.0";
const IDENTIFIER_SELECT: &str = "http://specs.openid.net/auth/2.0/identifier_select";
const CLAIMED_ID_PREFIX: &str = "https://steamcommunity.com/openid/id/";

/// Where to send the browser to sign in. `realm` is the site, which Steam's
/// page names to the person; `return_to` is where Steam sends them back,
/// and has to be within it.
pub fn authorize_url(config: &SteamConfig, realm: &str, return_to: &str) -> String {
    let mut url = url::Url::parse(&config.openid_url).expect("Steam's OpenID URL");
    url.query_pairs_mut()
        .append_pair("openid.ns", NS)
        .append_pair("openid.mode", "checkid_setup")
        .append_pair("openid.return_to", return_to)
        .append_pair("openid.realm", realm)
        .append_pair("openid.identity", IDENTIFIER_SELECT)
        .append_pair("openid.claimed_id", IDENTIFIER_SELECT);
    url.into()
}

/// The SteamID64 an assertion claims, if it is shaped like one of Steam's:
/// a 64-bit number after Steam's own prefix, and nothing else.
fn steam_id_of(claimed_id: &str) -> Option<&str> {
    let id = claimed_id.strip_prefix(CLAIMED_ID_PREFIX)?;
    (!id.is_empty() && id.len() <= 20 && id.bytes().all(|b| b.is_ascii_digit()))
        .then_some(id)
        .filter(|id| id.parse::<u64>().is_ok())
}

/// Checks what Steam sent the browser back with -- `answer`, the whole
/// query of the callback -- and says whose account it names.
///
/// `return_to` is the one this sign-in set out with, state and all: an
/// assertion made for any other is not this one. `Ok(None)` when the answer
/// is not a good one: put away, for another site, or not vouched for by
/// Steam. An `Err` is Steam not answering.
///
/// Steam gives no email address, so the identity never carries one; nor
/// does it say what the account owns (see the module's notes).
pub async fn verify(
    config: &SteamConfig,
    return_to: &str,
    answer: &HashMap<String, String>,
) -> Result<Option<VerifiedIdentity>> {
    let field = |name: &str| answer.get(name).map(String::as_str);
    if field("openid.ns") != Some(NS)
        || field("openid.mode") != Some("id_res")
        || field("openid.op_endpoint") != Some(config.openid_url.as_str())
        || field("openid.return_to") != Some(return_to)
    {
        return Ok(None);
    }
    let Some(claimed_id) = field("openid.claimed_id") else {
        return Ok(None);
    };
    if field("openid.identity") != Some(claimed_id) {
        return Ok(None);
    }
    let Some(steam_id) = steam_id_of(claimed_id) else {
        return Ok(None);
    };
    // Steam signs the fields it names in `signed`; the ones that say who and
    // for whom have to be among them, or the signature vouches for nothing
    // that matters.
    let signed: Vec<&str> = field("openid.signed").unwrap_or("").split(',').collect();
    for needed in [
        "op_endpoint",
        "claimed_id",
        "identity",
        "return_to",
        "response_nonce",
    ] {
        if !signed.contains(&needed) {
            return Ok(None);
        }
    }

    // Everything Steam sent, back to Steam, asking whether it said it.
    let mut form: Vec<(&str, &str)> = answer
        .iter()
        .filter(|(name, _)| name.starts_with("openid.") && name.as_str() != "openid.mode")
        .map(|(name, value)| (name.as_str(), value.as_str()))
        .collect();
    form.push(("openid.mode", "check_authentication"));
    let response = super::http()
        .post(&config.openid_url)
        .form(&form)
        .send()
        .await?;
    let status = response.status();
    if !status.is_success() {
        return Err(anyhow!("Steam answered check_authentication with {status}"));
    }
    let body = response.text().await?;
    let valid = body.lines().any(|line| line.trim() == "is_valid:true");
    if !valid {
        tracing::info!("Steam did not vouch for a sign-in's assertion");
        return Ok(None);
    }

    let name = super::persona_name(config, steam_id)
        .await
        .unwrap_or_else(|error| {
            tracing::warn!("could not look up a Steam persona name: {error:#}");
            None
        });
    Ok(Some(VerifiedIdentity {
        provider: Provider::Steam,
        subject: steam_id.to_string(),
        name,
        email: None,
        // Never asked here: a sign-in in a browser earns no STEAM_SUPPORTER
        // and records no Supporter Pack (models::identity::refresh_standing).
        purchased: None,
        bought_app: None,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    use axum::extract::Query;
    use axum::routing::{get, post};
    use axum::{Form, Json, Router};
    use serde_json::json;

    const STEAM_ID: &str = "76561197960287930";
    const RETURN_TO: &str = "https://oeee.cafe/auth/steam/callback?state=the-state";

    /// Steam's OpenID endpoint and the Web API, on a port of their own.
    /// Steam vouches for an assertion whose nonce is `"good"`, once.
    async fn fake_steam() -> SteamConfig {
        let spent = std::sync::Arc::new(std::sync::Mutex::new(false));
        let app = Router::new()
            .route(
                "/openid/login",
                post(move |Form(form): Form<HashMap<String, String>>| {
                    let spent = spent.clone();
                    async move {
                        let asked = form.get("openid.mode").map(String::as_str)
                            == Some("check_authentication");
                        let good =
                            form.get("openid.response_nonce").map(String::as_str) == Some("good");
                        let mut spent = spent.lock().unwrap();
                        let valid = asked && good && !*spent;
                        if valid {
                            *spent = true;
                        }
                        format!("ns:{NS}\nis_valid:{valid}\n")
                    }
                }),
            )
            .route(
                "/ISteamUser/GetPlayerSummaries/v2/",
                get(|Query(_): Query<HashMap<String, String>>| async {
                    Json(json!({"response": {"players": [
                        {"steamid": STEAM_ID, "personaname": " 오이 "}
                    ]}}))
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        SteamConfig {
            app_id: 480,
            web_api_key: "a-publisher-key".to_string(),
            web_api_url: format!("http://{addr}"),
            openid_url: format!("http://{addr}/openid/login"),
        }
    }

    fn answer(config: &SteamConfig) -> HashMap<String, String> {
        let claimed = format!("{CLAIMED_ID_PREFIX}{STEAM_ID}");
        [
            ("openid.ns", NS),
            ("openid.mode", "id_res"),
            ("openid.op_endpoint", config.openid_url.as_str()),
            ("openid.claimed_id", claimed.as_str()),
            ("openid.identity", claimed.as_str()),
            ("openid.return_to", RETURN_TO),
            ("openid.response_nonce", "good"),
            ("openid.assoc_handle", "1234567890"),
            (
                "openid.signed",
                "signed,op_endpoint,claimed_id,identity,return_to,response_nonce,assoc_handle",
            ),
            ("openid.sig", "c2lnbmVk"),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
    }

    #[tokio::test]
    async fn steam_vouches_for_whoever_it_names_once() {
        let config = fake_steam().await;
        let answer = answer(&config);
        assert_eq!(
            verify(&config, RETURN_TO, &answer).await.unwrap(),
            Some(VerifiedIdentity {
                provider: Provider::Steam,
                subject: STEAM_ID.into(),
                name: Some("오이".into()),
                email: None,
                purchased: None,
                bought_app: None,
            })
        );
        // The same assertion again is one Steam has already vouched for.
        assert_eq!(verify(&config, RETURN_TO, &answer).await.unwrap(), None);
    }

    #[tokio::test]
    async fn an_assertion_for_another_sign_in_is_not_this_one() {
        let config = fake_steam().await;
        let answer = answer(&config);
        let other = "https://oeee.cafe/auth/steam/callback?state=another";
        assert_eq!(verify(&config, other, &answer).await.unwrap(), None);
    }

    #[tokio::test]
    async fn only_steams_own_shape_of_account_is_taken() {
        let config = fake_steam().await;
        for claimed in [
            "https://steamcommunity.com/openid/id/".to_string(),
            "https://steamcommunity.com/openid/id/7656119x".to_string(),
            format!("https://evil.example/openid/id/{STEAM_ID}"),
            format!("http://steamcommunity.com/openid/id/{STEAM_ID}"),
        ] {
            let mut answer = answer(&config);
            answer.insert("openid.claimed_id".into(), claimed.clone());
            answer.insert("openid.identity".into(), claimed);
            assert_eq!(verify(&config, RETURN_TO, &answer).await.unwrap(), None);
        }
        // Claimed and identity have to agree.
        let mut answer = answer(&config);
        answer.insert(
            "openid.identity".into(),
            format!("{CLAIMED_ID_PREFIX}76561197960287931"),
        );
        assert_eq!(verify(&config, RETURN_TO, &answer).await.unwrap(), None);
    }

    #[tokio::test]
    async fn what_says_who_has_to_be_signed() {
        let config = fake_steam().await;
        let mut answer = answer(&config);
        answer.insert(
            "openid.signed".into(),
            "signed,op_endpoint,identity,return_to,response_nonce".into(),
        );
        assert_eq!(verify(&config, RETURN_TO, &answer).await.unwrap(), None);
    }

    #[tokio::test]
    async fn a_cancelled_sign_in_is_not_an_error() {
        let config = fake_steam().await;
        let mut answer = answer(&config);
        answer.insert("openid.mode".into(), "cancel".into());
        assert_eq!(verify(&config, RETURN_TO, &answer).await.unwrap(), None);
    }

    #[test]
    fn the_browser_is_sent_to_steam_with_where_to_come_back() {
        let config = SteamConfig {
            app_id: 480,
            web_api_key: "a-publisher-key".to_string(),
            web_api_url: String::new(),
            openid_url: "https://steamcommunity.com/openid/login".to_string(),
        };
        let url = url::Url::parse(&authorize_url(&config, "https://oeee.cafe", RETURN_TO)).unwrap();
        let query: HashMap<_, _> = url.query_pairs().into_owned().collect();
        assert_eq!(url.host_str(), Some("steamcommunity.com"));
        assert_eq!(query["openid.mode"], "checkid_setup");
        assert_eq!(query["openid.return_to"], RETURN_TO);
        assert_eq!(query["openid.realm"], "https://oeee.cafe");
        assert_eq!(query["openid.claimed_id"], IDENTIFIER_SELECT);
        // The publisher key is only ever sent to the Web API.
        assert!(!url.as_str().contains("a-publisher-key"));
    }
}
