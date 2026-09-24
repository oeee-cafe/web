//! Asking the App Store what was bought.
//!
//! The iOS app sells the Supporter Pack with StoreKit and hands the site the
//! transaction id it got back. The site takes that id to Apple's App Store
//! Server API with a key of its own, and Apple answers with the transaction:
//! which app it was bought in, which product, for which Oeee Cafe account,
//! and whether it has since been refunded. Nothing the app says about a
//! purchase is taken on its word -- the same rule `steam.rs` keeps for
//! tickets.
//!
//! **Which account.** Whoever is signed in when the app hands the
//! transaction over. A transaction names no Apple ID and needs none here:
//! buying inside the app is not signing in with Apple, and an account that
//! signs in with a password keeps what it buys.
//!
//! **Restoring.** StoreKit can hand the same transaction over again -- a new
//! phone, a reinstall, someone tapping Restore Purchases -- and it comes
//! back here unchanged. `models::supporter` keys a purchase by the
//! transaction rather than by the account, so restoring updates the one row
//! that purchase has: it can give a pack back, and it can move it to the
//! account restoring it, but it can never make two.
//!
//! **Sandbox.** A build signed for TestFlight or Xcode buys in Apple's
//! sandbox, whose transactions the production API does not know. Apple's own
//! advice is to ask production first and to ask the sandbox when production
//! says it has never heard of the transaction, which is what [`look_up`]
//! does -- one configuration serves both, and a sandbox purchase grants
//! standing on a test server without granting it on the real one.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use anyhow::{anyhow, Result};
use chrono::Utc;
use jsonwebtoken::{decode, encode, Algorithm, DecodingKey, EncodingKey, Header, Validation};
use serde::Deserialize;
use serde_json::json;

use uuid::Uuid;

use crate::config::AppStoreConfig;
use crate::models::supporter::OwnedProduct;

/// Who the App Store Server API tokens are for, in every request Apple
/// accepts.
const TOKEN_AUDIENCE: &str = "appstoreconnect-v1";

/// A token is minted per request and lives only as long as the request
/// needs; Apple allows up to an hour.
const TOKEN_GOOD_FOR: i64 = 5 * 60;

fn http() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .build()
            .expect("reqwest client")
    })
}

/// A purchase the App Store has told us about.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Purchase {
    /// Which pack: the product and the year it supports.
    pub pack: OwnedProduct,
    /// What names this purchase from now on, and what a restore of it says.
    /// For a non-consumable Apple answers about the original transaction id
    /// for the life of the purchase.
    pub transaction: String,
    /// Whether it counts now: bought outright and not since refunded.
    pub owned: bool,
}

#[derive(Deserialize)]
struct TransactionResponse {
    #[serde(rename = "signedTransactionInfo")]
    signed_transaction_info: String,
}

/// The fields of Apple's signed transaction this cares about.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Transaction {
    bundle_id: String,
    product_id: String,
    original_transaction_id: String,
    /// When Apple refunded it, in milliseconds. Absent while it stands.
    revocation_date: Option<i64>,
    /// `PURCHASED` for the buyer; `FAMILY_SHARED` for somebody they share
    /// with, who bought nothing.
    in_app_ownership_type: Option<String>,
}

/// A JWT for the App Store Server API: ES256 over the key Apple issued
/// (App Store Connect > Users and Access > Integrations > In-App Purchase),
/// naming the issuer, the key and the app it is for.
fn api_token(config: &AppStoreConfig) -> Result<String> {
    let pem = std::fs::read(&config.private_key_path).map_err(|error| {
        anyhow!(
            "could not read the App Store key at {}: {error}",
            config.private_key_path
        )
    })?;
    let key = EncodingKey::from_ec_pem(&pem)?;
    let mut header = Header::new(Algorithm::ES256);
    header.kid = Some(config.key_id.clone());
    header.typ = Some("JWT".to_string());
    let now = Utc::now().timestamp();
    let claims = json!({
        "iss": config.issuer_id,
        "iat": now,
        "exp": now + TOKEN_GOOD_FOR,
        "aud": TOKEN_AUDIENCE,
        "bid": config.bundle_id,
    });
    Ok(encode(&header, &claims, &key)?)
}

/// Apple's answer, or `None` when that environment has never heard of the
/// transaction.
async fn signed_transaction(
    config: &AppStoreConfig,
    api_url: &str,
    transaction_id: &str,
) -> Result<Option<String>> {
    let url = format!(
        "{}/inApps/v1/transactions/{}",
        api_url.trim_end_matches('/'),
        urlencoding::encode(transaction_id)
    );
    let response = http()
        .get(url)
        .bearer_auth(api_token(config)?)
        .send()
        .await?;
    let status = response.status();
    if status == reqwest::StatusCode::NOT_FOUND {
        return Ok(None);
    }
    if status == reqwest::StatusCode::UNAUTHORIZED {
        // Ours to fix -- the key, the key id or the issuer -- and not
        // something the buyer can do anything about.
        return Err(anyhow!("the App Store refused our key (401)"));
    }
    if !status.is_success() {
        return Err(anyhow!("the App Store answered with {status}"));
    }
    let body: TransactionResponse = response.json().await?;
    Ok(Some(body.signed_transaction_info))
}

/// Reads Apple's signed transaction without checking its signature.
///
/// It is not the app's word being read: this JWS came back from Apple's own
/// API, over TLS, to a request signed with our key. Checking the signature
/// as well would mean walking the `x5c` chain to Apple's root, which proves
/// the same thing the connection already did.
fn read_transaction(jws: &str) -> Result<Transaction> {
    let mut validation = Validation::new(Algorithm::ES256);
    validation.insecure_disable_signature_validation();
    validation.validate_exp = false;
    validation.validate_aud = false;
    validation.required_spec_claims.clear();
    let data = decode::<Transaction>(jws, &DecodingKey::from_secret(&[]), &validation)?;
    Ok(data.claims)
}

/// What the App Store says about `transaction_id`, against `packs`: every
/// product the catalogue has for the App Store, on sale or not
/// (`store_product::packs`), since a pack taken off sale still counts for
/// whoever bought it.
///
/// `None` when nothing here sold it: a transaction Apple does not know, or
/// one from another app or of a product the catalogue does not have. A
/// refunded purchase, or one shared with the family rather than bought,
/// comes back as a [`Purchase`] that is not owned -- that is an answer, and
/// takes standing away. The year it counts for is the catalogue's.
pub async fn look_up(
    config: &AppStoreConfig,
    packs: &[OwnedProduct],
    transaction_id: &str,
) -> Result<Option<Purchase>> {
    if packs.is_empty() {
        return Ok(None);
    }
    let signed = match signed_transaction(config, &config.api_url, transaction_id).await? {
        Some(signed) => signed,
        // Bought in the sandbox, if anywhere.
        None => match signed_transaction(config, &config.sandbox_api_url, transaction_id).await? {
            Some(signed) => signed,
            None => return Ok(None),
        },
    };
    let transaction = read_transaction(&signed)?;

    if transaction.bundle_id != config.bundle_id {
        return Ok(None);
    }
    let Some(pack) = packs
        .iter()
        .find(|pack| pack.product == transaction.product_id)
    else {
        return Ok(None);
    };
    let owned = transaction.revocation_date.is_none()
        && transaction.in_app_ownership_type.as_deref() != Some("FAMILY_SHARED");
    Ok(Some(Purchase {
        pack: pack.clone(),
        transaction: transaction.original_transaction_id,
        owned,
    }))
}

/// How many times a minute one account may send us to Apple.
///
/// A purchase is posted once and a restore once more, so a handful a minute
/// is generous; past that it is a loop. Every one of them spends a request
/// against the App Store Server API's rate limit, and that limit is the
/// site's to lose rather than any one buyer's -- a signed-in account with a
/// script could otherwise leave everybody else's purchase unanswerable.
const ASKS_PER_MINUTE: usize = 6;

/// Whether `user_id` may have another transaction looked up.
///
/// Per process, which means per colour: both serve for a moment during a
/// deploy, and a limit that is twice as generous for that moment is still a
/// limit. Nothing here is worth a round trip to Redis.
pub fn may_ask(user_id: Uuid) -> bool {
    static ASKED: Asked = OnceLock::new();
    may_ask_of(&ASKED, user_id, ASKS_PER_MINUTE)
}

/// Who has asked a store what, and when: one per store, since each store's
/// rate limit is its own.
pub(crate) type Asked = OnceLock<Mutex<HashMap<Uuid, Vec<Instant>>>>;

/// Whether `user_id` may send us to the store `asked` counts for again, at
/// most `per_minute` times a minute.
pub(crate) fn may_ask_of(asked: &Asked, user_id: Uuid, per_minute: usize) -> bool {
    let mut asked = asked.get_or_init(Default::default).lock().unwrap();
    let now = Instant::now();
    // What has fallen out of the window is forgotten, and an account whose
    // asks all have is forgotten with it: this map would otherwise hold
    // every account that ever bought anything for the life of the process.
    asked.retain(|_, asks| {
        asks.retain(|at| now.duration_since(*at) < Duration::from_secs(60));
        !asks.is_empty()
    });
    let asks = asked.entry(user_id).or_default();
    if asks.len() >= per_minute {
        return false;
    }
    asks.push(now);
    true
}

/// A transaction id of the right shape that cannot be anybody's: Apple's
/// are numbers it issues in order, and it has not issued this one.
const NOBODYS_TRANSACTION: &str = "2000000000000000";

/// Whether the App Store will talk to us at all, for `cli check-app-store`.
///
/// It asks about a transaction that cannot exist, and the answer it wants is
/// "never heard of it": getting that means the key, the key id and the
/// issuer were all accepted. A key Apple refuses is a 401 and says so, which
/// is the failure worth finding before someone's money is involved rather
/// than after.
pub async fn check(config: &AppStoreConfig) -> Result<()> {
    match signed_transaction(config, &config.api_url, NOBODYS_TRANSACTION).await? {
        None => Ok(()),
        Some(_) => Err(anyhow!(
            "the App Store told us about transaction {NOBODYS_TRANSACTION}, which cannot be anyone's"
        )),
    }
}

/// Asks the App Store again, once a day, about every purchase it has told us
/// about, so a refund takes the mark away without anyone signing in. A check
/// Apple cannot answer changes nothing. The catalogue is read afresh each
/// time round, so a product added at /admin/store is known by the next one.
///
/// Both colours run this for a moment during a deploy, and asking twice is
/// harmless.
pub async fn recheck_supporters(db: sqlx::PgPool, config: AppStoreConfig) {
    use crate::models::store_product;
    use crate::models::supporter::{apple_purchases_due_for_check, record_recheck, Store};

    let mut every = tokio::time::interval(Duration::from_secs(10 * 60));
    every.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        every.tick().await;
        let due = match db.begin().await {
            Ok(mut tx) => apple_purchases_due_for_check(&mut tx, 100).await,
            Err(error) => Err(error.into()),
        };
        let due = match due {
            Ok(due) => due,
            Err(error) => {
                tracing::warn!("could not list App Store purchases to recheck: {error:#}");
                continue;
            }
        };
        if due.is_empty() {
            continue;
        }
        let packs = match store_product::packs_in(&db, Store::Apple).await {
            Ok(packs) => packs,
            Err(error) => {
                tracing::warn!("could not read the App Store's products: {error:#}");
                continue;
            }
        };
        // An empty catalogue is one nobody has filled in, not a refund of
        // everything: asking would answer every purchase with "not ours".
        if packs.is_empty() {
            continue;
        }
        for due in due {
            let owned = match look_up(&config, &packs, &due.transaction).await {
                Ok(Some(answer)) => answer.owned,
                // Apple no longer knows it, or it is no longer one of ours:
                // either way it supports nothing.
                Ok(None) => false,
                Err(error) => {
                    tracing::warn!("could not recheck an App Store purchase: {error:#}");
                    continue;
                }
            };
            let recorded = async {
                let mut tx = db.begin().await?;
                record_recheck(&mut tx, Store::Apple, &due.transaction, &due.product, owned)
                    .await?;
                tx.commit().await?;
                anyhow::Ok(())
            };
            if let Err(error) = recorded.await {
                tracing::warn!("could not record an App Store recheck: {error:#}");
            }
        }
    }
}

/// Against a stand-in for Apple's App Store Server API: our own token is
/// checked there, so a request Apple would turn away fails here too.
#[cfg(test)]
mod tests {
    use super::*;
    use axum::extract::Path;
    use axum::http::HeaderMap;
    use axum::response::IntoResponse;
    use axum::routing::get;
    use axum::{Json, Router};
    use serde_json::Value;

    /// A throwaway P-256 key, standing in for the one App Store Connect
    /// issues. Apple signs its transactions with its own; the site never
    /// sees that key, and never checks that signature (see
    /// [`read_transaction`]), so the fake signs with this one.
    const PRIVATE_KEY: &str = include_str!("testdata/app_store_test_key.p8");
    /// The same file, for the config to read the way the real one is read.
    const PRIVATE_KEY_PATH: &str = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/testdata/app_store_test_key.p8"
    );
    const KEY_ID: &str = "ABCD123456";
    const ISSUER_ID: &str = "57246542-96fe-1a63-e053-0824d011072a";
    const BUNDLE_ID: &str = "cafe.oeee";
    const PRODUCT_ID: &str = "cafe.oeee.supporter.2026";
    const PACK_YEAR: i32 = 2026;

    fn transaction(id: &str, overrides: Value) -> Value {
        let mut transaction = json!({
            "bundleId": BUNDLE_ID,
            "productId": PRODUCT_ID,
            "transactionId": id,
            "originalTransactionId": id,
            "type": "Non-Consumable",
            "inAppOwnershipType": "PURCHASED",
        });
        for (key, value) in overrides.as_object().expect("an object") {
            if value.is_null() {
                transaction.as_object_mut().unwrap().remove(key);
            } else {
                transaction[key] = value.clone();
            }
        }
        transaction
    }

    /// What Apple sells: the transactions each environment knows.
    fn known(environment: &str, id: &str) -> Option<Value> {
        let sandbox_only = id == "2000";
        if (environment == "sandbox") != sandbox_only {
            return None;
        }
        match id {
            "1000" | "2000" => Some(transaction(id, json!({}))),
            // Refunded.
            "1001" => Some(transaction(
                id,
                json!({"revocationDate": 1_790_000_000_000i64}),
            )),
            // Somebody in the family bought it; this account did not.
            "1002" => Some(transaction(
                id,
                json!({"inAppOwnershipType": "FAMILY_SHARED"}),
            )),
            "1003" => Some(transaction(id, json!({"bundleId": "com.example.other"}))),
            // Another year's pack, which this deployment does not sell.
            "1004" => Some(transaction(
                id,
                json!({"productId": "cafe.oeee.supporter.2019"}),
            )),
            _ => None,
        }
    }

    /// Reads the token the site sent the way Apple would: ours to sign, and
    /// naming the key, the issuer and the app.
    fn token_is_ours(headers: &HeaderMap) -> bool {
        let Some(bearer) = headers
            .get("authorization")
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.strip_prefix("Bearer "))
        else {
            return false;
        };
        let Ok(header) = jsonwebtoken::decode_header(bearer) else {
            return false;
        };
        if header.alg != Algorithm::ES256 || header.kid.as_deref() != Some(KEY_ID) {
            return false;
        }
        let mut validation = Validation::new(Algorithm::ES256);
        validation.insecure_disable_signature_validation();
        validation.validate_aud = false;
        validation.required_spec_claims.clear();
        let Ok(claims) = decode::<Value>(bearer, &DecodingKey::from_secret(&[]), &validation)
        else {
            return false;
        };
        claims.claims["iss"] == json!(ISSUER_ID)
            && claims.claims["aud"] == json!(TOKEN_AUDIENCE)
            && claims.claims["bid"] == json!(BUNDLE_ID)
    }

    fn signed(transaction: &Value) -> String {
        encode(
            &Header::new(Algorithm::ES256),
            transaction,
            &EncodingKey::from_ec_pem(PRIVATE_KEY.as_bytes()).unwrap(),
        )
        .unwrap()
    }

    async fn fake_app_store() -> AppStoreConfig {
        async fn answer(
            environment: &'static str,
            headers: HeaderMap,
            id: String,
        ) -> axum::response::Response {
            if !token_is_ours(&headers) {
                return axum::http::StatusCode::UNAUTHORIZED.into_response();
            }

            match known(environment, &id) {
                Some(transaction) => {
                    Json(json!({"signedTransactionInfo": signed(&transaction)})).into_response()
                }
                None => (
                    axum::http::StatusCode::NOT_FOUND,
                    Json(json!({"errorCode": 4_040_010i64, "errorMessage": "Transaction id not found."})),
                )
                    .into_response(),
            }
        }

        let app = Router::new()
            .route(
                "/production/inApps/v1/transactions/:id",
                get(|headers: HeaderMap, Path(id): Path<String>| answer("production", headers, id)),
            )
            .route(
                "/sandbox/inApps/v1/transactions/:id",
                get(|headers: HeaderMap, Path(id): Path<String>| answer("sandbox", headers, id)),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        AppStoreConfig {
            issuer_id: ISSUER_ID.to_string(),
            key_id: KEY_ID.to_string(),
            private_key_path: PRIVATE_KEY_PATH.to_string(),
            bundle_id: BUNDLE_ID.to_string(),
            api_url: format!("http://{addr}/production"),
            sandbox_api_url: format!("http://{addr}/sandbox"),
        }
    }

    /// The catalogue's App Store products, as `store_product::packs` gives
    /// them.
    fn packs() -> Vec<OwnedProduct> {
        vec![OwnedProduct {
            product: PRODUCT_ID.to_string(),
            year: PACK_YEAR,
        }]
    }

    #[tokio::test]
    async fn a_purchase_names_the_pack_and_the_year() {
        let config = fake_app_store().await;
        let purchase = look_up(&config, &packs(), "1000").await.unwrap().unwrap();
        assert_eq!(purchase.pack.product, PRODUCT_ID);
        assert_eq!(purchase.pack.year, PACK_YEAR, "the year it supports");
        assert_eq!(purchase.transaction, "1000", "what a restore names");
        assert!(purchase.owned);
    }

    /// A refund and a copy shared with the family are answers, not silence:
    /// they take standing away rather than leaving it as it was.
    #[tokio::test]
    async fn a_refund_and_a_shared_copy_are_not_purchases() {
        let config = fake_app_store().await;
        for id in ["1001", "1002"] {
            let purchase = look_up(&config, &packs(), id).await.unwrap().unwrap();
            assert!(!purchase.owned, "{id} bought nothing");
        }
    }

    #[tokio::test]
    async fn a_transaction_from_somewhere_else_buys_nothing_here() {
        let config = fake_app_store().await;
        for id in [
            // Another app's bundle, another year's pack that this
            // deployment does not sell, and one Apple has never heard of.
            "1003", "1004", "9999",
        ] {
            assert_eq!(look_up(&config, &packs(), id).await.unwrap(), None, "{id}");
        }
    }

    /// TestFlight and Xcode buy in the sandbox, which production has never
    /// heard of.
    #[tokio::test]
    async fn a_sandbox_purchase_is_asked_about_after_production() {
        let config = fake_app_store().await;
        let purchase = look_up(&config, &packs(), "2000").await.unwrap().unwrap();
        assert!(purchase.owned);
        assert_eq!(purchase.transaction, "2000");
    }

    #[tokio::test]
    async fn without_a_supporter_product_nothing_is_asked_about() {
        let config = fake_app_store().await;
        assert_eq!(look_up(&config, &[], "1000").await.unwrap(), None);
    }

    /// The year comes from the catalogue, whatever the product id says.
    #[tokio::test]
    async fn the_year_is_the_catalogues() {
        let config = fake_app_store().await;
        let packs = [OwnedProduct {
            product: PRODUCT_ID.to_string(),
            year: 2031,
        }];
        let purchase = look_up(&config, &packs, "1000").await.unwrap().unwrap();
        assert_eq!(purchase.pack.year, 2031);
    }

    /// The key, the key id and the issuer are all in the token, so a
    /// configuration Apple would turn away fails here rather than quietly
    /// answering no.
    /// A loop is stopped before it reaches Apple; somebody else's loop is
    /// not anyone's to answer for.
    #[test]
    fn an_account_may_only_ask_so_often() {
        let account = Uuid::new_v4();
        for ask in 0..ASKS_PER_MINUTE {
            assert!(may_ask(account), "ask {ask} of {ASKS_PER_MINUTE}");
        }
        assert!(!may_ask(account), "the one past the limit");
        assert!(may_ask(Uuid::new_v4()), "a different account is unaffected");
    }

    /// What `cli check-app-store` does: the App Store saying it has never
    /// heard of a transaction is the key being accepted.
    #[tokio::test]
    async fn a_check_passes_when_the_app_store_answers_at_all() {
        let config = fake_app_store().await;
        check(&config).await.expect("the key is accepted");

        let mut refused = fake_app_store().await;
        refused.issuer_id = "someone-else".to_string();
        assert!(check(&refused).await.is_err());
    }

    #[tokio::test]
    async fn a_token_apple_would_refuse_is_an_error_and_not_an_answer() {
        let mut config = fake_app_store().await;
        config.key_id = "WRONGKEY00".to_string();
        assert!(look_up(&config, &packs(), "1000").await.is_err());

        let mut config = fake_app_store().await;
        config.private_key_path = "/nowhere/app-store.p8".to_string();
        assert!(look_up(&config, &packs(), "1000").await.is_err());
    }
}
