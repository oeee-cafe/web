//! Asking the Microsoft Store what was bought.
//!
//! The Microsoft Store build of the Windows app sells the Supporter Pack as
//! a durable add-on, with `StoreContext.RequestPurchaseAsync`. What proves a
//! purchase to anyone but the app is a **Microsoft Store ID key**: a token
//! the app asks the Store for on the site's behalf, which names the
//! Microsoft account signed into the Store, and which the site then shows
//! the Store's collections API to ask what that account owns. It goes:
//!
//! 1. The page asks the site for a ticket (`POST /store/microsoft/tickets`,
//!    `oeeeApp.store.ticket()` in app_store.jinja). The ticket is a Microsoft
//!    Entra access token the site gets for itself with its client secret,
//!    made out to [`KEY_AUDIENCE`] -- what the Store asks for before it
//!    makes a collections key -- and it comes back with `user`, an id for
//!    whoever is signed in here.
//! 2. The app passes both to `StoreContext.GetCustomerCollectionsIdAsync
//!    (ticket, user)`, and the Store answers with the Store ID key.
//! 3. The app hands the key to the page as its proof, which posts it to
//!    `/store/microsoft/purchases` like any other store's, and the site asks
//!    the collections API ([`look_up`]) what the key's account owns, with a
//!    second token of its own made out to [`API_AUDIENCE`].
//!
//! Nothing the app says about a purchase is taken on its word -- the rule
//! `app_store.rs` and `steam.rs` keep. The key says only whose purchases to
//! ask about; the answer is the Store's.
//!
//! **Which account.** Whoever is signed in here, as with the App Store: the
//! key names a Microsoft account, which signs nobody in here, and buying
//! inside the app is not linking anything.
//!
//! **Which purchase.** A durable add-on is bought once per Microsoft
//! account, and the Store names that purchase by its `orderId`, which is
//! what `supporter_purchases.owner` holds. Handing the same key over again
//! -- a reinstall, another Oeee Cafe account signed in on the same PC --
//! answers with the same order, so it updates the one row that purchase has
//! and never makes a second one, exactly as restoring does in the App Store.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use anyhow::{anyhow, Result};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::app_store::{may_ask_of, Asked};
use crate::config::MicrosoftStoreConfig;
use crate::models::supporter::OwnedProduct;

/// What a ticket is made out to: the Store makes a collections key only for
/// a token with this audience. `GetCustomerCollectionsIdAsync` is the call
/// that takes it; a purchase key (`GetCustomerPurchaseIdAsync`) is for the
/// Store's purchase API, which the site does not use, and wants a token of
/// its own.
pub const KEY_AUDIENCE: &str = "https://onestore.microsoft.com/b2b/keys/create/collections";

/// What the site's own requests to the collections API are made out to.
pub const API_AUDIENCE: &str = "https://onestore.microsoft.com";

/// Pages of a collection the site will read before giving up on one. A
/// hundred items a page, and an account owns a handful of add-ons.
const MAX_PAGES: usize = 20;

fn http() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .build()
            .expect("reqwest client")
    })
}

/// Where tokens come from: Microsoft Entra's v1 endpoint for the tenant.
///
/// v1 rather than v2 because v1 is what the Store documents, and because
/// its `resource` takes an audience as it is written. The key audience has
/// a path (`/b2b/keys/create/collections`), and v2 would have it spelled as
/// a scope -- that path with `/.default` after it -- which the Store does
/// not document accepting. The token v1 gives names the audience exactly as
/// `resource` did, which is what the Store checks.
pub fn token_url(config: &MicrosoftStoreConfig) -> String {
    config.token_url.clone().unwrap_or_else(|| {
        format!(
            "https://login.microsoftonline.com/{}/oauth2/token",
            urlencoding::encode(&config.tenant_id)
        )
    })
}

/// The client-credentials form for a token made out to `resource`.
fn token_form(config: &MicrosoftStoreConfig, resource: &str) -> Vec<(&'static str, String)> {
    vec![
        ("grant_type", "client_credentials".to_string()),
        ("client_id", config.client_id.clone()),
        ("client_secret", config.client_secret.clone()),
        ("resource", resource.to_string()),
    ]
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    /// Seconds. v1 writes it as a string ("3599") and v2 as a number, so
    /// either is read.
    #[serde(default)]
    expires_in: Option<Value>,
}

impl TokenResponse {
    fn lasts(&self) -> Duration {
        let seconds = match &self.expires_in {
            Some(Value::Number(n)) => n.as_u64(),
            Some(Value::String(s)) => s.parse().ok(),
            _ => None,
        };
        Duration::from_secs(seconds.unwrap_or(0))
    }
}

/// A token is kept until five minutes before it runs out, so every ticket
/// and every purchase is not also a round trip to Microsoft Entra.
const TOKEN_MARGIN: Duration = Duration::from_secs(5 * 60);

/// A token made out to `resource`, from the cache while it lasts.
async fn access_token(config: &MicrosoftStoreConfig, resource: &str) -> Result<String> {
    // Keyed by everything that decides the token, the secret included, so
    // a config that changes any of it is never answered with the old one.
    type Tokens = Mutex<HashMap<[String; 4], (String, Instant)>>;
    static TOKENS: OnceLock<Tokens> = OnceLock::new();
    let url = token_url(config);
    let key = [
        url.clone(),
        config.client_id.clone(),
        config.client_secret.clone(),
        resource.to_string(),
    ];
    let tokens = TOKENS.get_or_init(Default::default);
    if let Some((token, until)) = tokens.lock().unwrap().get(&key) {
        if Instant::now() < *until {
            return Ok(token.clone());
        }
    }

    let response = http()
        .post(url)
        .form(&token_form(config, resource))
        .send()
        .await?;
    let status = response.status();
    if !status.is_success() {
        // A wrong secret, a client id the tenant does not know, a secret
        // that has expired: ours to fix, never the buyer's.
        let body = response.text().await.unwrap_or_default();
        return Err(anyhow!(
            "Microsoft Entra would not give us a token ({status}): {body}"
        ));
    }
    let answer: TokenResponse = response.json().await?;
    let lasts = answer.lasts();
    if lasts > TOKEN_MARGIN {
        tokens.lock().unwrap().insert(
            key,
            (
                answer.access_token.clone(),
                Instant::now() + lasts - TOKEN_MARGIN,
            ),
        );
    }
    Ok(answer.access_token)
}

/// A ticket for the app to ask the Store for a collections key with.
pub async fn ticket(config: &MicrosoftStoreConfig) -> Result<String> {
    access_token(config, KEY_AUDIENCE).await
}

/// The id a ticket is handed out with, which the app passes to the Store as
/// its `publisherUserId` and the site passes back as the collections
/// query's `localTicketReference`: the account's own id, which stays the
/// same however its names change.
pub fn user_reference(user_id: Uuid) -> String {
    user_id.to_string()
}

/// One page of the collections query: every durable add-on the key's
/// account has, refunded ones included -- a refund is an answer, and takes
/// the pack back -- with the page before's continuation token after the
/// first.
fn collections_query(key: &str, user: &str, continuation: Option<&str>) -> Value {
    let mut query = json!({
        "beneficiaries": [{
            "identityType": "b2b",
            "identityValue": key,
            "localTicketReference": user,
        }],
        "productTypes": ["Durable"],
        "validityType": "All",
        "maxPageSize": 100,
    });
    if let Some(continuation) = continuation {
        query["continuationToken"] = json!(continuation);
    }
    query
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CollectionsPage {
    #[serde(default)]
    items: Vec<CollectionItem>,
    continuation_token: Option<String>,
}

/// The fields of a collection item this cares about.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CollectionItem {
    /// The add-on's Store ID, as the catalogue lists it.
    product_id: String,
    /// The purchase. Absent for an item nobody paid for, which is no
    /// purchase to record.
    order_id: Option<String>,
    /// "Active" while it stands; "Revoked" once refunded, "Banned" and
    /// "Expired" otherwise.
    status: Option<String>,
    /// "OwnedByBeneficiary" for the buyer. Anything shared with them rather
    /// than bought is not theirs to support with.
    ownership_type: Option<String>,
}

/// A purchase the Microsoft Store has told us about.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Purchase {
    /// Which pack: the catalogue's product and the year it counts for.
    pub pack: OwnedProduct,
    /// The order it was bought in, which names it from now on.
    pub order: String,
    /// Whether it counts now: active, and the account's own.
    pub owned: bool,
}

/// The catalogue's purchases among what the Store listed. Anything else --
/// another publisher's product, one of ours that is not a pack, an item
/// with no order behind it -- is passed over.
fn purchases_in(items: &[CollectionItem], packs: &[OwnedProduct]) -> Vec<Purchase> {
    let mut purchases: Vec<Purchase> = Vec::new();
    for item in items {
        // Store IDs are upper case in the Store and may not be as typed at
        // /admin/store; the catalogue's spelling is what is recorded.
        let Some(pack) = packs
            .iter()
            .find(|pack| pack.product.eq_ignore_ascii_case(&item.product_id))
        else {
            continue;
        };
        let Some(order) = item.order_id.as_deref().filter(|order| !order.is_empty()) else {
            continue;
        };
        let owned = item.status.as_deref() == Some("Active")
            && !item
                .ownership_type
                .as_deref()
                .is_some_and(|ownership| ownership.contains("Shared"));
        let purchase = Purchase {
            pack: pack.clone(),
            order: order.to_string(),
            owned,
        };
        // One order, one product, one row: a page that lists it twice
        // counts it once, owned if either says so.
        match purchases
            .iter_mut()
            .find(|seen| seen.order == purchase.order && seen.pack == purchase.pack)
        {
            Some(seen) => seen.owned |= purchase.owned,
            None => purchases.push(purchase),
        }
    }
    purchases
}

/// Why a key was turned away.
#[derive(Debug, PartialEq, Eq)]
pub enum KeyRejected {
    /// Not a Store ID key, or one the Store did not accept: expired, or
    /// made for another publisher's app.
    Invalid,
}

/// Whether `key` is worth sending the Store at all. A Store ID key is a JWT,
/// and nothing with whitespace in it or of this length is one.
fn looks_like_a_key(key: &str) -> bool {
    !key.is_empty() && key.len() <= 16 * 1024 && key.bytes().all(|b| b.is_ascii_graphic())
}

/// What the Microsoft Store says the account behind `key` bought, among
/// `packs`: every product the catalogue has for the Microsoft Store, on sale
/// or not (`store_product::packs`), since a pack taken off sale still counts
/// for whoever bought it. `user` is the id the ticket was handed out with.
///
/// Refunded purchases come back as not owned -- that is an answer, and
/// takes standing away. An empty list is an account that bought none of
/// them.
pub async fn look_up(
    config: &MicrosoftStoreConfig,
    packs: &[OwnedProduct],
    key: &str,
    user: &str,
) -> Result<std::result::Result<Vec<Purchase>, KeyRejected>> {
    let key = key.trim();
    if !looks_like_a_key(key) {
        return Ok(Err(KeyRejected::Invalid));
    }
    let token = access_token(config, API_AUDIENCE).await?;
    let mut items = Vec::new();
    let mut continuation: Option<String> = None;
    for _ in 0..MAX_PAGES {
        let response = http()
            .post(&config.collections_url)
            .bearer_auth(&token)
            .json(&collections_query(key, user, continuation.as_deref()))
            .send()
            .await?;
        let status = response.status();
        if status == reqwest::StatusCode::BAD_REQUEST {
            let body = response.text().await.unwrap_or_default();
            tracing::info!("the Microsoft Store turned a key away: {body}");
            return Ok(Err(KeyRejected::Invalid));
        }
        if !status.is_success() {
            // 401 is our token, or a key made for an app whose client id is
            // not ours; either is ours to fix. Anything else is the Store.
            let body = response.text().await.unwrap_or_default();
            return Err(anyhow!(
                "the Microsoft Store answered the collections query with {status}: {body}"
            ));
        }
        let page: CollectionsPage = response.json().await?;
        items.extend(page.items);
        match page.continuation_token.filter(|token| !token.is_empty()) {
            Some(next) => continuation = Some(next),
            None => return Ok(Ok(purchases_in(&items, packs))),
        }
    }
    Err(anyhow!(
        "the Microsoft Store's collection went on past {MAX_PAGES} pages"
    ))
}

/// How many times a minute one account may send us to the Microsoft Store,
/// tickets and purchases together: a ticket, a key and a purchase are one
/// sale, and a handful more is a restore. Past that it is a loop, against
/// a rate limit the whole site shares.
const ASKS_PER_MINUTE: usize = 10;

pub fn may_ask(user_id: Uuid) -> bool {
    static ASKED: Asked = OnceLock::new();
    may_ask_of(&ASKED, user_id, ASKS_PER_MINUTE)
}

/// Request building and response reading, against fixtures shaped like the
/// ones Microsoft documents, and the whole of [`look_up`] against a stand-in
/// on localhost. Nothing here reaches Microsoft.
#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderMap;
    use axum::routing::post;
    use axum::{Form, Json, Router};

    const KEY: &str = "eyJ0eXAiOiJKV1QiLCJhbGciOiJSUzI1NiJ9.eyJ1c2VySWQiOiJ4In0.c2lnbmF0dXJl";
    const USER: &str = "5f0b6a2e-1d3c-4b7a-9e8f-0a1b2c3d4e5f";

    fn config() -> MicrosoftStoreConfig {
        MicrosoftStoreConfig {
            tenant_id: "00000000-1111-2222-3333-444444444444".to_string(),
            client_id: "client".to_string(),
            client_secret: "secret".to_string(),
            token_url: None,
            collections_url: "https://purchase.mp.microsoft.com/v8.0/b2b/collections/query"
                .to_string(),
        }
    }

    fn packs() -> Vec<OwnedProduct> {
        vec![
            OwnedProduct {
                product: "9NSUPPORT2026".to_string(),
                year: 2026,
            },
            OwnedProduct {
                product: "9nsupport2027".to_string(),
                year: 2027,
            },
        ]
    }

    /// An item as the collections API lists one, with `overrides` laid over
    /// it; a null takes the field away.
    fn item(overrides: Value) -> Value {
        let mut item = json!({
            "acquiredDate": "2026-03-01T00:00:00.0000000+00:00",
            "devOfferId": "",
            "endDate": "9999-12-31T23:59:59.9999999+00:00",
            "fulfillmentData": [],
            "inAppOfferToken": "supporter-2026",
            "itemId": "2c28a42fb0e5470a8ab0e8b5c7d6c21a",
            "localTicketReference": USER,
            "modifiedDate": "2026-03-01T00:00:00.0000000+00:00",
            "orderId": "b340ef8d-7cfa-4a4d-8e0e-4f5c3ca3a8a0",
            "orderLineItemId": "e11e6bd8-d3a3-4c2a-9ad4-9d7f5e6c2b1a",
            "ownershipType": "OwnedByBeneficiary",
            "productId": "9NSUPPORT2026",
            "productType": "Durable",
            "purchasedCountry": "KR",
            "purchaser": {"identityType": "pub", "identityValue": "anonymous"},
            "quantity": 1,
            "skuId": "0010",
            "skuType": "Full",
            "startDate": "2026-03-01T00:00:00.0000000+00:00",
            "status": "Active",
            "tags": [],
            "transactionId": "7b4d1a3f-5e6c-4c2b-8a9d-0e1f2a3b4c5d",
        });
        for (key, value) in overrides.as_object().expect("an object") {
            if value.is_null() {
                item.as_object_mut().unwrap().remove(key);
            } else {
                item[key] = value.clone();
            }
        }
        item
    }

    fn read(page: Value) -> CollectionsPage {
        serde_json::from_value(page).expect("a page reads")
    }

    #[test]
    fn a_token_is_asked_for_by_client_credentials_from_the_tenants_v1_endpoint() {
        let config = config();
        assert_eq!(
            token_url(&config),
            "https://login.microsoftonline.com/00000000-1111-2222-3333-444444444444/oauth2/token"
        );
        let form: HashMap<_, _> = token_form(&config, KEY_AUDIENCE).into_iter().collect();
        assert_eq!(form["grant_type"], "client_credentials");
        assert_eq!(form["client_id"], "client");
        assert_eq!(form["client_secret"], "secret");
        assert_eq!(
            form["resource"],
            "https://onestore.microsoft.com/b2b/keys/create/collections"
        );
        assert_eq!(form.len(), 4, "nothing else goes to Microsoft Entra");
    }

    /// v1 says how long a token lasts as a string, v2 as a number.
    #[test]
    fn a_tokens_life_is_read_either_way_it_is_written() {
        let v1: TokenResponse = serde_json::from_value(json!({
            "token_type": "Bearer", "expires_in": "3599", "ext_expires_in": "3599",
            "expires_on": "1790000000", "not_before": "1789996100",
            "resource": API_AUDIENCE, "access_token": "eyJ0",
        }))
        .unwrap();
        assert_eq!(v1.lasts(), Duration::from_secs(3599));
        let v2: TokenResponse =
            serde_json::from_value(json!({"access_token": "eyJ0", "expires_in": 3599})).unwrap();
        assert_eq!(v2.lasts(), Duration::from_secs(3599));
        let none: TokenResponse = serde_json::from_value(json!({"access_token": "eyJ0"})).unwrap();
        assert_eq!(none.lasts(), Duration::ZERO, "not kept at all");
    }

    #[test]
    fn the_collections_query_names_the_key_and_the_user_and_asks_for_add_ons() {
        let first = collections_query(KEY, USER, None);
        assert_eq!(
            first,
            json!({
                "beneficiaries": [{
                    "identityType": "b2b",
                    "identityValue": KEY,
                    "localTicketReference": USER,
                }],
                "productTypes": ["Durable"],
                "validityType": "All",
                "maxPageSize": 100,
            })
        );
        let next = collections_query(KEY, USER, Some("next-page"));
        assert_eq!(next["continuationToken"], "next-page");
    }

    #[test]
    fn an_active_pack_is_a_purchase_named_by_its_order_and_credited_with_the_catalogues_year() {
        let page = read(json!({"items": [item(json!({}))]}));
        assert_eq!(page.continuation_token, None);
        assert_eq!(
            purchases_in(&page.items, &packs()),
            [Purchase {
                pack: OwnedProduct {
                    product: "9NSUPPORT2026".to_string(),
                    year: 2026,
                },
                order: "b340ef8d-7cfa-4a4d-8e0e-4f5c3ca3a8a0".to_string(),
                owned: true,
            }]
        );
    }

    /// A refund, a ban and a copy shared rather than bought are answers,
    /// not silence: they take standing away.
    #[test]
    fn a_refunded_or_shared_pack_is_not_owned() {
        for overrides in [
            json!({"status": "Revoked"}),
            json!({"status": "Banned"}),
            json!({"status": "Expired"}),
            json!({"status": null}),
            json!({"ownershipType": "SharedByBeneficiary"}),
        ] {
            let page = read(json!({"items": [item(overrides.clone())]}));
            let purchases = purchases_in(&page.items, &packs());
            assert_eq!(purchases.len(), 1, "{overrides}");
            assert!(!purchases[0].owned, "{overrides}");
        }
    }

    #[test]
    fn what_is_not_a_pack_or_was_never_ordered_is_passed_over() {
        let page = read(json!({"items": [
            item(json!({"productId": "9NOTAPACK000"})),
            item(json!({"orderId": null})),
            item(json!({"orderId": ""})),
        ]}));
        assert_eq!(purchases_in(&page.items, &packs()), []);
        assert_eq!(purchases_in(&read(json!({})).items, &packs()), []);
    }

    /// The catalogue's spelling is recorded, whatever case the Store uses.
    #[test]
    fn a_store_id_matches_whatever_its_case() {
        let page = read(json!({"items": [
            item(json!({"productId": "9NSUPPORT2027", "orderId": "order-2027"})),
        ]}));
        let purchases = purchases_in(&page.items, &packs());
        assert_eq!(purchases[0].pack.product, "9nsupport2027");
        assert_eq!(purchases[0].pack.year, 2027);
    }

    #[test]
    fn one_order_listed_twice_is_one_purchase() {
        let page = read(json!({"items": [
            item(json!({"status": "Revoked"})),
            item(json!({})),
        ]}));
        let purchases = purchases_in(&page.items, &packs());
        assert_eq!(purchases.len(), 1);
        assert!(purchases[0].owned);
    }

    #[test]
    fn only_something_shaped_like_a_key_is_sent() {
        assert!(looks_like_a_key(KEY));
        for key in ["", "a key", "a\nkey", &"x".repeat(16 * 1024 + 1)] {
            assert!(!looks_like_a_key(key), "{key:?}");
        }
    }

    /// Microsoft Entra and the collections API on localhost, each checking
    /// what it is sent the way the real one would: the token is ours and
    /// made out to the collections API, and the key is the one handed over.
    async fn fake_microsoft() -> MicrosoftStoreConfig {
        let app = Router::new()
            .route(
                "/token",
                post(|Form(form): Form<HashMap<String, String>>| async move {
                    if form.get("client_secret").map(String::as_str) != Some("secret") {
                        return (
                            axum::http::StatusCode::UNAUTHORIZED,
                            Json(json!({"error": "invalid_client"})),
                        );
                    }
                    let resource = form.get("resource").cloned().unwrap_or_default();
                    (
                        axum::http::StatusCode::OK,
                        Json(json!({
                            "token_type": "Bearer",
                            "expires_in": "3599",
                            "resource": resource,
                            "access_token": format!("token-for {resource}"),
                        })),
                    )
                }),
            )
            .route(
                "/collections",
                post(|headers: HeaderMap, Json(query): Json<Value>| async move {
                    let bearer = headers
                        .get("authorization")
                        .and_then(|value| value.to_str().ok());
                    if bearer != Some(&format!("Bearer token-for {API_AUDIENCE}")) {
                        return (axum::http::StatusCode::UNAUTHORIZED, Json(json!({})));
                    }
                    let beneficiary = &query["beneficiaries"][0];
                    if beneficiary["identityValue"] != json!(KEY) {
                        return (
                            axum::http::StatusCode::BAD_REQUEST,
                            Json(json!({"code": "BadRequest", "message": "Invalid key"})),
                        );
                    }
                    // Two pages: this year's pack, then last year's refund.
                    let page = match query.get("continuationToken") {
                        None => json!({
                            "continuationToken": "page-2",
                            "items": [item(json!({}))],
                        }),
                        Some(_) => json!({
                            "items": [item(json!({
                                "productId": "9NSUPPORT2027",
                                "orderId": "order-2027",
                                "status": "Revoked",
                            }))],
                        }),
                    };
                    (axum::http::StatusCode::OK, Json(page))
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        MicrosoftStoreConfig {
            token_url: Some(format!("http://{addr}/token")),
            collections_url: format!("http://{addr}/collections"),
            ..config()
        }
    }

    #[tokio::test]
    async fn every_page_of_the_collection_is_read() {
        let config = fake_microsoft().await;
        let purchases = look_up(&config, &packs(), KEY, USER)
            .await
            .unwrap()
            .unwrap();
        let summary: Vec<_> = purchases
            .iter()
            .map(|purchase| (purchase.pack.year, purchase.owned))
            .collect();
        assert_eq!(summary, [(2026, true), (2027, false)]);
    }

    #[tokio::test]
    async fn a_ticket_is_made_out_to_the_key_audience() {
        let config = fake_microsoft().await;
        assert_eq!(
            ticket(&config).await.unwrap(),
            format!("token-for {KEY_AUDIENCE}")
        );
    }

    #[tokio::test]
    async fn a_key_the_store_turns_away_is_invalid_and_our_own_failure_is_an_error() {
        let config = fake_microsoft().await;
        assert_eq!(
            look_up(&config, &packs(), "someone-elses-key", USER)
                .await
                .unwrap(),
            Err(KeyRejected::Invalid)
        );
        assert_eq!(
            look_up(&config, &packs(), "not a key", USER).await.unwrap(),
            Err(KeyRejected::Invalid),
            "never sent"
        );

        let refused = MicrosoftStoreConfig {
            client_secret: "wrong".to_string(),
            ..config
        };
        assert!(look_up(&refused, &packs(), KEY, USER).await.is_err());
        assert!(ticket(&refused).await.is_err());
    }
}
