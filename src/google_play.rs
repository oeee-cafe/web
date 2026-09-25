//! Asking Google Play what was bought.
//!
//! The Android app sells the Supporter Pack as a one-time product with Play
//! Billing and hands the site the **purchase token** Play gave it. The site
//! takes the token to the Google Play Developer API with a service account
//! of its own, and Google answers with the purchase: which product, whether
//! it went through, whether it has since been refunded. Nothing the app says
//! about a purchase is taken on its word -- the rule `app_store.rs` keeps
//! for transactions.
//!
//! **Which account.** Whoever is signed in when the app hands the token
//! over, as with the App Store. A token names no Google account here:
//! buying on Google Play is not signing in with Google, and an account that
//! signs in with a password keeps what it buys.
//!
//! **Restoring.** Play lists what the device's Google account owns every
//! time the app asks, and the app hands those tokens over again -- a new
//! phone, a reinstall, someone pressing Restore. `models::supporter` keys a
//! purchase by its token rather than by the account, so that updates the
//! one row the purchase has and never makes a second.
//!
//! **Acknowledging.** Play refunds a purchase nobody acknowledges within
//! three days. The site acknowledges it here, once it has recorded it
//! ([`acknowledge`]), rather than leaving that to the app: a purchase the
//! site never took is then one Play gives back by itself, and the app has
//! nothing to finish. Until it is acknowledged the app keeps handing it
//! over whenever /supporter opens, so a failed acknowledgement is tried
//! again.
//!
//! **Test purchases.** A licence tester's purchase is answered like any
//! other, as a sandbox purchase is by `app_store::look_up`: there is one
//! deployment, and trying the sale on it has to work.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use anyhow::{anyhow, Result};
use chrono::Utc;
use jsonwebtoken::{encode, Algorithm, EncodingKey, Header};
use serde::Deserialize;
use serde_json::json;
use uuid::Uuid;

use crate::app_store::{may_ask_of, Asked};
use crate::config::{GooglePlayConfig, KeyFile};
use crate::models::supporter::OwnedProduct;

/// What the site's tokens are good for: the Google Play Developer API.
const SCOPE: &str = "https://www.googleapis.com/auth/androidpublisher";

/// Where a service account trades its signed assertion for a token when its
/// key file does not say.
const DEFAULT_TOKEN_URI: &str = "https://oauth2.googleapis.com/token";

/// How long an assertion asks for its token to last: Google's maximum.
const ASSERTION_GOOD_FOR: i64 = 60 * 60;

/// A token is kept until five minutes before it runs out, so every purchase
/// is not also a round trip to Google's token endpoint.
const TOKEN_MARGIN: Duration = Duration::from_secs(5 * 60);

fn http() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .build()
            .expect("reqwest client")
    })
}

/// The fields of a service account's JSON key this uses.
#[derive(Deserialize)]
pub struct ServiceAccountKey {
    client_email: String,
    /// PKCS#8, as Google issues it.
    private_key: String,
    #[serde(default)]
    private_key_id: Option<String>,
    #[serde(default)]
    token_uri: Option<String>,
}

impl GooglePlayConfig {
    /// Reads the service account's key from `service_account_path`, once,
    /// when the config is loaded (`KeyFile`). The private key inside it is
    /// parsed here too, so a key Google would never have issued stops the
    /// server booting rather than the first token request.
    pub fn load_key(&mut self) -> Result<()> {
        let text = std::fs::read_to_string(&self.service_account_path).map_err(|error| {
            anyhow!(
                "could not read the Google Play service account key at {}: {error}",
                self.service_account_path
            )
        })?;
        let key: ServiceAccountKey = serde_json::from_str(&text).map_err(|error| {
            anyhow!(
                "the Google Play service account key at {} is not one: {error}",
                self.service_account_path
            )
        })?;
        EncodingKey::from_rsa_pem(key.private_key.as_bytes()).map_err(|error| {
            anyhow!(
                "the private key in the Google Play service account key at {} does not parse: {error}",
                self.service_account_path
            )
        })?;
        self.service_account = KeyFile::new(key);
        Ok(())
    }
}

/// The signed assertion a service account asks for a token with (RFC 7523):
/// RS256 over its own key, naming itself, the token endpoint and the scope.
fn assertion(key: &ServiceAccountKey, token_uri: &str) -> Result<String> {
    let signing_key = EncodingKey::from_rsa_pem(key.private_key.as_bytes())
        .map_err(|error| anyhow!("the service account's private key does not parse: {error}"))?;
    let mut header = Header::new(Algorithm::RS256);
    header.kid = key.private_key_id.clone();
    let now = Utc::now().timestamp();
    let claims = json!({
        "iss": key.client_email,
        "scope": SCOPE,
        "aud": token_uri,
        "iat": now,
        "exp": now + ASSERTION_GOOD_FOR,
    });
    Ok(encode(&header, &claims, &signing_key)?)
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    #[serde(default)]
    expires_in: Option<u64>,
}

/// A token for the Developer API, from the cache while it lasts.
async fn access_token(config: &GooglePlayConfig) -> Result<String> {
    // Keyed by everything that decides the token, so one account is never
    // answered with another's.
    type Tokens = Mutex<HashMap<[String; 3], (String, Instant)>>;
    static TOKENS: OnceLock<Tokens> = OnceLock::new();
    let key = config.service_account.get().ok_or_else(|| {
        anyhow!(
            "the Google Play service account key at {} was never loaded",
            config.service_account_path
        )
    })?;
    let token_uri = key
        .token_uri
        .clone()
        .unwrap_or_else(|| DEFAULT_TOKEN_URI.to_string());
    let cache_key = [
        key.client_email.clone(),
        key.private_key.clone(),
        token_uri.clone(),
    ];
    let tokens = TOKENS.get_or_init(Default::default);
    if let Some((token, until)) = tokens.lock().unwrap().get(&cache_key) {
        if Instant::now() < *until {
            return Ok(token.clone());
        }
    }

    let response = http()
        .post(&token_uri)
        .form(&[
            (
                "grant_type",
                "urn:ietf:params:oauth:grant-type:jwt-bearer".to_string(),
            ),
            ("assertion", assertion(key, &token_uri)?),
        ])
        .send()
        .await?;
    let status = response.status();
    if !status.is_success() {
        // A key that has been deleted, or an account that has been
        // disabled: ours to fix, never the buyer's.
        let body = response.text().await.unwrap_or_default();
        return Err(anyhow!(
            "Google would not give the service account a token ({status}): {body}"
        ));
    }
    let answer: TokenResponse = response.json().await?;
    let lasts = Duration::from_secs(answer.expires_in.unwrap_or(0));
    if lasts > TOKEN_MARGIN {
        tokens.lock().unwrap().insert(
            cache_key,
            (
                answer.access_token.clone(),
                Instant::now() + lasts - TOKEN_MARGIN,
            ),
        );
    }
    Ok(answer.access_token)
}

/// The fields of Google's `ProductPurchaseV2` this cares about.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ProductPurchase {
    #[serde(default)]
    product_line_item: Vec<LineItem>,
    purchase_state_context: Option<PurchaseStateContext>,
    /// `ACKNOWLEDGEMENT_STATE_PENDING` until someone acknowledges it.
    acknowledgement_state: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct LineItem {
    product_id: String,
    product_offer_details: Option<OfferDetails>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct OfferDetails {
    /// How many of it have not been refunded: 0 once all of it has.
    refundable_quantity: Option<i64>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PurchaseStateContext {
    /// `PURCHASED`, `PENDING` (waiting on cash, or on a parent) or
    /// `CANCELLED`.
    purchase_state: Option<String>,
}

/// Whether `token` is worth sending Google at all. Play's tokens are a few
/// hundred characters of letters, digits and punctuation, and nothing with
/// whitespace in it or of this length is one.
fn looks_like_a_token(token: &str) -> bool {
    !token.is_empty() && token.len() <= 4096 && token.bytes().all(|b| b.is_ascii_graphic())
}

/// What Google says about `token`, or `None` when it has never heard of it
/// in this app.
async fn product_purchase(
    config: &GooglePlayConfig,
    token: &str,
) -> Result<Option<ProductPurchase>> {
    let url = format!(
        "{}/androidpublisher/v3/applications/{}/purchases/productsv2/tokens/{}",
        config.api_url.trim_end_matches('/'),
        urlencoding::encode(&config.package_name),
        urlencoding::encode(token)
    );
    let response = http()
        .get(url)
        .bearer_auth(access_token(config).await?)
        .send()
        .await?;
    let status = response.status();
    // A token Google does not recognise is a 400 ("Invalid Value"), one it
    // no longer has is a 404 or a 410: none of them a purchase of ours.
    if matches!(status.as_u16(), 400 | 404 | 410) {
        return Ok(None);
    }
    if matches!(status.as_u16(), 401 | 403) {
        // The service account has not been invited in Play Console, or not
        // with the permissions it needs: ours to fix.
        let body = response.text().await.unwrap_or_default();
        return Err(anyhow!(
            "Google Play refused our service account ({status}): {body}"
        ));
    }
    if !status.is_success() {
        return Err(anyhow!("Google Play answered with {status}"));
    }
    Ok(Some(response.json().await?))
}

/// A purchase Google Play has told us about.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Purchase {
    /// Which pack: the product and the year it supports.
    pub pack: OwnedProduct,
    /// The purchase token, which names this purchase from now on and is
    /// what a restore of it hands over again.
    pub token: String,
    /// Whether it counts now: paid for, and not since refunded.
    pub owned: bool,
    /// Whether it has been acknowledged, so that Play will not refund it by
    /// itself.
    pub acknowledged: bool,
}

/// What Google Play says about `token`, against `packs`: every product the
/// catalogue has for Google Play, on sale or not (`store_product::packs`),
/// since a pack taken off sale still counts for whoever bought it.
///
/// `None` when nothing here sold it: a token Google does not know, one from
/// another app, or one for a product the catalogue does not have. A
/// refunded purchase, a cancelled one, or one still waiting to be paid for
/// comes back as a [`Purchase`] that is not owned -- that is an answer, and
/// takes standing away. The year it counts for is the catalogue's.
pub async fn look_up(
    config: &GooglePlayConfig,
    packs: &[OwnedProduct],
    token: &str,
) -> Result<Option<Purchase>> {
    let token = token.trim();
    if packs.is_empty() || !looks_like_a_token(token) {
        return Ok(None);
    }
    let Some(purchase) = product_purchase(config, token).await? else {
        return Ok(None);
    };
    let Some((pack, item)) = purchase.product_line_item.iter().find_map(|item| {
        packs
            .iter()
            .find(|pack| pack.product == item.product_id)
            .map(|pack| (pack, item))
    }) else {
        return Ok(None);
    };
    let purchased = purchase
        .purchase_state_context
        .as_ref()
        .and_then(|context| context.purchase_state.as_deref())
        == Some("PURCHASED");
    let refunded = item
        .product_offer_details
        .as_ref()
        .and_then(|details| details.refundable_quantity)
        == Some(0);
    Ok(Some(Purchase {
        pack: pack.clone(),
        token: token.to_string(),
        owned: purchased && !refunded,
        acknowledged: purchase.acknowledgement_state.as_deref()
            == Some("ACKNOWLEDGEMENT_STATE_ACKNOWLEDGED"),
    }))
}

/// Tells Google Play the purchase has been delivered, so that it is not
/// refunded three days on. Acknowledging one that already is does no harm.
pub async fn acknowledge(config: &GooglePlayConfig, purchase: &Purchase) -> Result<()> {
    let url = format!(
        "{}/androidpublisher/v3/applications/{}/purchases/products/{}/tokens/{}:acknowledge",
        config.api_url.trim_end_matches('/'),
        urlencoding::encode(&config.package_name),
        urlencoding::encode(&purchase.pack.product),
        urlencoding::encode(&purchase.token)
    );
    let response = http()
        .post(url)
        .bearer_auth(access_token(config).await?)
        .json(&json!({}))
        .send()
        .await?;
    let status = response.status();
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        return Err(anyhow!(
            "Google Play would not acknowledge a purchase ({status}): {body}"
        ));
    }
    Ok(())
}

/// How many times a minute one account may send us to Google Play. A
/// purchase is handed over once and a restore once more, and each is a
/// lookup and perhaps an acknowledgement; past a handful it is a loop,
/// against a quota the whole site shares.
const ASKS_PER_MINUTE: usize = 10;

/// Whether `user_id` may have another purchase token looked up.
pub fn may_ask(user_id: Uuid) -> bool {
    static ASKED: Asked = OnceLock::new();
    may_ask_of(&ASKED, user_id, ASKS_PER_MINUTE)
}

/// A token of the right shape that cannot be anybody's.
const NOBODYS_TOKEN: &str = "oeee-cafe-check-no-such-purchase";

/// Whether Google Play will talk to us at all, for `cli check-google-play`.
///
/// It asks about a token that cannot exist, and the answer it wants is
/// "never heard of it": getting that means the key was accepted, the
/// service account was given a token, and Play Console lets it see the app.
/// A service account that has not been invited is a 401 or a 403 and says
/// so, which is the failure worth finding before someone's money is
/// involved rather than after.
pub async fn check(config: &GooglePlayConfig) -> Result<()> {
    match product_purchase(config, NOBODYS_TOKEN).await? {
        None => Ok(()),
        Some(_) => Err(anyhow!(
            "Google Play told us about token {NOBODYS_TOKEN}, which cannot be anyone's"
        )),
    }
}

/// Asks Google Play again, once a day, about every purchase it has told us
/// about, so a refund takes the mark away without anyone signing in. A check
/// Google cannot answer changes nothing. The catalogue is read afresh each
/// time round, so a product added at /admin/store is known by the next one.
///
/// Both colours run this for a moment during a deploy, and asking twice is
/// harmless.
pub async fn recheck_supporters(db: sqlx::PgPool, config: GooglePlayConfig) {
    use crate::models::store_product;
    use crate::models::supporter::{purchases_due_for_check, record_recheck, Store};

    let mut every = tokio::time::interval(Duration::from_secs(10 * 60));
    every.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        every.tick().await;
        let due = match db.begin().await {
            Ok(mut tx) => purchases_due_for_check(&mut tx, Store::Google, 100).await,
            Err(error) => Err(error.into()),
        };
        let due = match due {
            Ok(due) => due,
            Err(error) => {
                tracing::warn!("could not list Google Play purchases to recheck: {error:#}");
                continue;
            }
        };
        if due.is_empty() {
            continue;
        }
        let packs = match store_product::packs_in(&db, Store::Google).await {
            Ok(packs) => packs,
            Err(error) => {
                tracing::warn!("could not read Google Play's products: {error:#}");
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
                // Google no longer knows it, or it is no longer one of ours:
                // either way it supports nothing.
                Ok(None) => false,
                Err(error) => {
                    tracing::warn!("could not recheck a Google Play purchase: {error:#}");
                    continue;
                }
            };
            let recorded = async {
                let mut tx = db.begin().await?;
                record_recheck(
                    &mut tx,
                    Store::Google,
                    &due.transaction,
                    &due.product,
                    owned,
                )
                .await?;
                tx.commit().await?;
                anyhow::Ok(())
            };
            if let Err(error) = recorded.await {
                tracing::warn!("could not record a Google Play recheck: {error:#}");
            }
        }
    }
}

/// Against a stand-in for Google's token endpoint and the Developer API, on
/// localhost: the assertion is checked there with the key's public half, so
/// a request Google would turn away fails here too. Nothing here reaches
/// Google.
#[cfg(test)]
mod tests {
    use super::*;
    use axum::extract::{Path, State};
    use axum::http::{HeaderMap, StatusCode};
    use axum::response::IntoResponse;
    use axum::routing::{get, post};
    use axum::{Form, Json, Router};
    use jsonwebtoken::{decode, DecodingKey, Validation};
    use serde_json::Value;
    use std::sync::Arc;

    /// A throwaway RSA key, standing in for a service account's.
    const PRIVATE_KEY: &str = include_str!("testdata/google_play_test_key.pem");
    const PUBLIC_KEY: &str = include_str!("testdata/google_play_test_key.pub.pem");
    const CLIENT_EMAIL: &str = "oeee-cafe@example.iam.gserviceaccount.com";
    const PACKAGE: &str = "cafe.oeee";
    const PRODUCT_ID: &str = "supporter_pack_2026";
    const PACK_YEAR: i32 = 2026;
    /// What the fake token endpoint hands out, and the fake API accepts.
    const ACCESS_TOKEN: &str = "ya29.fake";

    fn purchase(product: &str, state: &str, refundable: i64, acknowledged: bool) -> Value {
        json!({
            "kind": "androidpublisher#productPurchaseV2",
            "productLineItem": [{
                "productId": product,
                "productOfferDetails": {
                    "quantity": 1,
                    "refundableQuantity": refundable,
                    "consumptionState": "CONSUMPTION_STATE_YET_TO_BE_CONSUMED",
                },
            }],
            "purchaseStateContext": {"purchaseState": state},
            "acknowledgementState": if acknowledged {
                "ACKNOWLEDGEMENT_STATE_ACKNOWLEDGED"
            } else {
                "ACKNOWLEDGEMENT_STATE_PENDING"
            },
            "orderId": "GPA.0000-0000-0000-00000",
        })
    }

    /// What Google knows, by token.
    fn known(token: &str) -> Option<Value> {
        match token {
            "bought" => Some(purchase(PRODUCT_ID, "PURCHASED", 1, false)),
            "acknowledged" => Some(purchase(PRODUCT_ID, "PURCHASED", 1, true)),
            "refunded" => Some(purchase(PRODUCT_ID, "PURCHASED", 0, true)),
            "cancelled" => Some(purchase(PRODUCT_ID, "CANCELLED", 1, false)),
            "pending" => Some(purchase(PRODUCT_ID, "PENDING", 1, false)),
            // Another year's pack, which this deployment does not sell.
            "elsewhere" => Some(purchase("supporter_pack_2019", "PURCHASED", 1, true)),
            _ => None,
        }
    }

    #[derive(Default)]
    struct Seen {
        acknowledged: Mutex<Vec<(String, String)>>,
        tokens_given: Mutex<usize>,
    }

    struct Fake {
        config: GooglePlayConfig,
        seen: Arc<Seen>,
        key_file: std::path::PathBuf,
    }

    impl Drop for Fake {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.key_file);
        }
    }

    fn bearer_is_ours(headers: &HeaderMap) -> bool {
        headers
            .get("authorization")
            .and_then(|value| value.to_str().ok())
            == Some(&format!("Bearer {ACCESS_TOKEN}"))
    }

    async fn fake_google_play() -> Fake {
        fake_google_play_with(CLIENT_EMAIL).await
    }

    /// Google, which accepts assertions only from `accepted`, signed with
    /// the test key and made out to its own token endpoint.
    async fn fake_google_play_with(accepted: &'static str) -> Fake {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let token_uri = format!("http://{addr}/token");
        let seen = Arc::new(Seen::default());

        let audience = token_uri.clone();
        let app = Router::new()
            .route(
                "/token",
                post(
                    move |State(seen): State<Arc<Seen>>, Form(form): Form<HashMap<String, String>>| {
                        let audience = audience.clone();
                        async move {
                            if form.get("grant_type").map(String::as_str)
                                != Some("urn:ietf:params:oauth:grant-type:jwt-bearer")
                            {
                                return StatusCode::BAD_REQUEST.into_response();
                            }
                            let mut validation = Validation::new(Algorithm::RS256);
                            validation.set_audience(&[&audience]);
                            validation.set_issuer(&[accepted]);
                            let Ok(claims) = decode::<Value>(
                                form.get("assertion").map(String::as_str).unwrap_or(""),
                                &DecodingKey::from_rsa_pem(PUBLIC_KEY.as_bytes()).unwrap(),
                                &validation,
                            ) else {
                                return (
                                    StatusCode::BAD_REQUEST,
                                    Json(json!({"error": "invalid_grant"})),
                                )
                                    .into_response();
                            };
                            if claims.claims["scope"] != json!(SCOPE) {
                                return StatusCode::BAD_REQUEST.into_response();
                            }
                            *seen.tokens_given.lock().unwrap() += 1;
                            Json(json!({
                                "access_token": ACCESS_TOKEN,
                                "expires_in": 3599,
                                "token_type": "Bearer",
                            }))
                            .into_response()
                        }
                    },
                ),
            )
            .route(
                "/androidpublisher/v3/applications/{package}/purchases/productsv2/tokens/{token}",
                get(
                    |headers: HeaderMap, Path((package, token)): Path<(String, String)>| async move {
                        if !bearer_is_ours(&headers) {
                            return StatusCode::UNAUTHORIZED.into_response();
                        }
                        // Another app's purchases are not this account's to see.
                        if package != PACKAGE {
                            return StatusCode::FORBIDDEN.into_response();
                        }
                        match known(&token) {
                            Some(purchase) => Json(purchase).into_response(),
                            None => (
                                StatusCode::BAD_REQUEST,
                                Json(json!({"error": {"code": 400, "message": "Invalid Value"}})),
                            )
                                .into_response(),
                        }
                    },
                ),
            )
            .route(
                "/androidpublisher/v3/applications/{package}/purchases/products/{product}/tokens/{acknowledge}",
                post(
                    |State(seen): State<Arc<Seen>>,
                     headers: HeaderMap,
                     Path((package, product, acknowledge)): Path<(String, String, String)>| async move {
                        if !bearer_is_ours(&headers) {
                            return StatusCode::UNAUTHORIZED;
                        }
                        let Some(token) = acknowledge.strip_suffix(":acknowledge") else {
                            return StatusCode::NOT_FOUND;
                        };
                        if package != PACKAGE || known(token).is_none() {
                            return StatusCode::BAD_REQUEST;
                        }
                        seen.acknowledged
                            .lock()
                            .unwrap()
                            .push((product, token.to_string()));
                        StatusCode::NO_CONTENT
                    },
                ),
            )
            .with_state(seen.clone());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let key_file =
            std::env::temp_dir().join(format!("oeee-cafe-google-play-{}.json", Uuid::new_v4()));
        std::fs::write(
            &key_file,
            json!({
                "type": "service_account",
                "project_id": "oeee-cafe",
                "private_key_id": "0123456789abcdef",
                "private_key": PRIVATE_KEY,
                "client_email": CLIENT_EMAIL,
                "token_uri": token_uri,
            })
            .to_string(),
        )
        .unwrap();

        let mut config = GooglePlayConfig {
            package_name: PACKAGE.to_string(),
            service_account_path: key_file.to_string_lossy().into_owned(),
            service_account: KeyFile::default(),
            api_url: format!("http://{addr}"),
        };
        config.load_key().expect("the test key loads");
        Fake {
            config,
            seen,
            key_file,
        }
    }

    fn packs() -> Vec<OwnedProduct> {
        vec![OwnedProduct {
            product: PRODUCT_ID.to_string(),
            year: PACK_YEAR,
        }]
    }

    #[tokio::test]
    async fn a_purchase_names_the_pack_and_the_year() {
        let fake = fake_google_play().await;
        let purchase = look_up(&fake.config, &packs(), "bought")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(purchase.pack.product, PRODUCT_ID);
        assert_eq!(purchase.pack.year, PACK_YEAR, "the year it supports");
        assert_eq!(purchase.token, "bought", "what a restore names");
        assert!(purchase.owned);
        assert!(!purchase.acknowledged);

        let again = look_up(&fake.config, &packs(), "acknowledged")
            .await
            .unwrap()
            .unwrap();
        assert!(again.owned && again.acknowledged);
    }

    /// A refund, a cancellation and a purchase still waiting on its money
    /// are answers, not silence: they take standing away rather than
    /// leaving it as it was.
    #[tokio::test]
    async fn a_refund_a_cancellation_and_a_pending_purchase_are_not_owned() {
        let fake = fake_google_play().await;
        for token in ["refunded", "cancelled", "pending"] {
            let purchase = look_up(&fake.config, &packs(), token)
                .await
                .unwrap()
                .unwrap();
            assert!(!purchase.owned, "{token} supports nothing");
        }
    }

    #[tokio::test]
    async fn a_token_from_somewhere_else_buys_nothing_here() {
        let fake = fake_google_play().await;
        for token in [
            // Another year's pack that this deployment does not sell, and
            // one Google has never heard of.
            "elsewhere",
            "nobody",
        ] {
            assert_eq!(
                look_up(&fake.config, &packs(), token).await.unwrap(),
                None,
                "{token}"
            );
        }
        // Not worth asking about at all.
        for token in ["", "two words", &"x".repeat(5000)] {
            assert_eq!(look_up(&fake.config, &packs(), token).await.unwrap(), None);
        }
        assert_eq!(
            *fake.seen.tokens_given.lock().unwrap(),
            1,
            "one token, kept"
        );
    }

    #[tokio::test]
    async fn without_a_supporter_product_nothing_is_asked_about() {
        let fake = fake_google_play().await;
        assert_eq!(look_up(&fake.config, &[], "bought").await.unwrap(), None);
        assert_eq!(*fake.seen.tokens_given.lock().unwrap(), 0);
    }

    /// The year comes from the catalogue, whatever the product id says.
    #[tokio::test]
    async fn the_year_is_the_catalogues() {
        let fake = fake_google_play().await;
        let packs = [OwnedProduct {
            product: PRODUCT_ID.to_string(),
            year: 2031,
        }];
        let purchase = look_up(&fake.config, &packs, "bought")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(purchase.pack.year, 2031);
    }

    #[tokio::test]
    async fn a_purchase_is_acknowledged_by_its_product_and_token() {
        let fake = fake_google_play().await;
        let purchase = look_up(&fake.config, &packs(), "bought")
            .await
            .unwrap()
            .unwrap();
        acknowledge(&fake.config, &purchase).await.unwrap();
        assert_eq!(
            *fake.seen.acknowledged.lock().unwrap(),
            [(PRODUCT_ID.to_string(), "bought".to_string())]
        );
    }

    /// A service account Google would turn away, or one Play Console has
    /// not let see the app, is an error rather than a quiet "not ours".
    #[tokio::test]
    async fn a_service_account_google_refuses_is_an_error_and_not_an_answer() {
        let refused = fake_google_play_with("someone-else@example.iam.gserviceaccount.com").await;
        assert!(look_up(&refused.config, &packs(), "bought").await.is_err());

        let mut another_app = fake_google_play().await;
        another_app.config.package_name = "com.example.other".to_string();
        assert!(look_up(&another_app.config, &packs(), "bought")
            .await
            .is_err());

        let mut unloaded = fake_google_play().await;
        unloaded.config.service_account = KeyFile::default();
        assert!(look_up(&unloaded.config, &packs(), "bought").await.is_err());
    }

    /// A key file that is not there, is not a service account's key, or
    /// holds a private key that does not parse, is found when the config is
    /// loaded: the server does not boot, rather than failing the first
    /// purchase after it passed its health check.
    #[tokio::test]
    async fn a_key_that_will_not_load_is_found_at_load() {
        let mut fake = fake_google_play().await;
        fake.config.service_account_path = "/nowhere/google-play.json".to_string();
        let error = fake.config.load_key().unwrap_err().to_string();
        assert!(error.contains("/nowhere/google-play.json"), "{error}");

        std::fs::write(&fake.key_file, "{}").unwrap();
        fake.config.service_account_path = fake.key_file.to_string_lossy().into_owned();
        assert!(fake.config.load_key().is_err());

        std::fs::write(
            &fake.key_file,
            json!({"client_email": CLIENT_EMAIL, "private_key": "not a key"}).to_string(),
        )
        .unwrap();
        assert!(fake.config.load_key().is_err());
    }

    /// What `cli check-google-play` does: Google saying it has never heard
    /// of a token is the service account being accepted.
    #[tokio::test]
    async fn a_check_passes_when_google_play_answers_at_all() {
        let fake = fake_google_play().await;
        check(&fake.config)
            .await
            .expect("the service account is accepted");

        let refused = fake_google_play_with("someone-else@example.iam.gserviceaccount.com").await;
        assert!(check(&refused.config).await.is_err());
    }

    #[tokio::test]
    async fn a_token_is_asked_for_once_and_kept() {
        let fake = fake_google_play().await;
        for _ in 0..3 {
            look_up(&fake.config, &packs(), "bought").await.unwrap();
        }
        assert_eq!(*fake.seen.tokens_given.lock().unwrap(), 1);
    }

    #[test]
    fn an_account_may_only_ask_so_often() {
        let account = Uuid::new_v4();
        for ask in 0..ASKS_PER_MINUTE {
            assert!(may_ask(account), "ask {ask} of {ASKS_PER_MINUTE}");
        }
        assert!(!may_ask(account), "the one past the limit");
        assert!(may_ask(Uuid::new_v4()), "a different account is unaffected");
    }
}
