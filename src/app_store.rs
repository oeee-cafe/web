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
//!
//! **Refunds.** Apple tells the site itself when a purchase is refunded, a
//! refund is reversed, or Family Sharing is taken away: App Store Server
//! Notifications, which it POSTs to `/store/apple/notifications`, signed as
//! its transactions are ([`read_notification`]). A notification is only
//! ever a cue. What it says happened is not recorded; the transaction it
//! names is looked up again and whatever Apple says of it *now* is, so two
//! notifications arriving out of order cannot leave the older one standing.
//! A notification that never arrived -- the site was down, or answered with
//! an error -- is found by [`sweep_notifications`], which asks Apple once an
//! hour for the ones it could not deliver. Nothing else asks Apple about a
//! purchase once it has been recorded.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use anyhow::{anyhow, Result};
use chrono::Utc;
use data_encoding::BASE64;
use jsonwebtoken::{encode, Algorithm, DecodingKey, EncodingKey, Header, Validation};
use rustls_pki_types::{CertificateDer, UnixTime};
use serde::Deserialize;
use serde_json::json;

use uuid::Uuid;

use crate::config::{AppStoreConfig, KeyFile};
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

impl AppStoreConfig {
    /// Reads the key Apple issued from `private_key_path`, once, when the
    /// config is loaded (`KeyFile`).
    pub fn load_key(&mut self) -> Result<()> {
        let pem = std::fs::read(&self.private_key_path).map_err(|error| {
            anyhow!(
                "could not read the App Store key at {}: {error}",
                self.private_key_path
            )
        })?;
        let key = EncodingKey::from_ec_pem(&pem).map_err(|error| {
            anyhow!(
                "the App Store key at {} is not a P-256 private key: {error}",
                self.private_key_path
            )
        })?;
        self.private_key = KeyFile::new(key);
        Ok(())
    }
}

/// A JWT for the App Store Server API: ES256 over the key Apple issued
/// (App Store Connect > Users and Access > Integrations > In-App Purchase),
/// naming the issuer, the key and the app it is for.
fn api_token(config: &AppStoreConfig) -> Result<String> {
    let key = config.private_key.get().ok_or_else(|| {
        anyhow!(
            "the App Store key at {} was never loaded",
            config.private_key_path
        )
    })?;
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
    Ok(encode(&header, &claims, key)?)
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

/// Apple Root CA - G3, from <https://www.apple.com/certificateauthority/>.
/// SHA-256 `63:34:3A:BF:B8:9A:6A:03:EB:B5:7E:9B:3F:5F:A7:BE:7C:4F:5C:75:6F:30:17:B3:A8:C4:88:C3:65:3E:91:79`.
const APPLE_ROOT_CA_G3: &[u8] = include_bytes!("certs/AppleRootCA-G3.cer");

/// Apple's marker on a Worldwide Developer Relations intermediate.
const WWDR_INTERMEDIATE_MARKER: &str = "1.2.840.113635.100.6.2.1";
/// Apple's marker on the leaf that signs App Store receipts and
/// transactions. It is what says what the leaf is for: the leaf carries no
/// extended key usage.
const RECEIPT_SIGNING_MARKER: &str = "1.2.840.113635.100.6.11.1";

/// The root a signed transaction has to chain to. Apple's, except in tests.
#[derive(Clone, Copy)]
pub struct TrustedRoot(pub &'static [u8]);

impl Default for TrustedRoot {
    fn default() -> Self {
        Self(APPLE_ROOT_CA_G3)
    }
}

impl std::fmt::Debug for TrustedRoot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("TrustedRoot(..)")
    }
}

/// Reads something Apple signed -- a transaction, a notification -- having
/// checked that Apple signed it.
///
/// The JWS names its signer in `x5c`: the leaf, Apple's WWDR intermediate
/// and Apple's root. The leaf and the intermediate have to chain to *our*
/// copy of the root -- the one in the header is only Apple's say-so -- be
/// valid at `now`, and each carry Apple's marker for what it is; then the
/// signature has to be the leaf's. This is what Apple's own App Store Server
/// Library checks, less the online revocation check.
///
/// A transaction comes from Apple's API over TLS, so this is not the only
/// thing standing between a forged purchase and a Supporter Pack. But TLS
/// only says which server answered; this says Apple signed what it said. A
/// notification comes from anyone who can reach the site, and this is all
/// that says it came from Apple.
fn read_signed<T: serde::de::DeserializeOwned>(
    jws: &str,
    root: TrustedRoot,
    now: UnixTime,
) -> Result<T> {
    let header = jsonwebtoken::decode_header(jws)?;
    if header.alg != Algorithm::ES256 {
        return Err(anyhow!(
            "the payload is signed with {:?}, not ES256",
            header.alg
        ));
    }
    let chain = header
        .x5c
        .ok_or_else(|| anyhow!("the payload names no certificate chain"))?;
    let [leaf, intermediate, _root] = chain.as_slice() else {
        return Err(anyhow!(
            "the payload's chain has {} certificates, not 3",
            chain.len()
        ));
    };
    let leaf = BASE64.decode(leaf.as_bytes())?;
    let intermediate = BASE64.decode(intermediate.as_bytes())?;
    verify_chain(&leaf, &intermediate, root, now)?;

    let (_, leaf_certificate) = x509_parser::parse_x509_certificate(&leaf)?;
    let key = DecodingKey::from_ec_der(&leaf_certificate.public_key().subject_public_key.data);
    let mut validation = Validation::new(Algorithm::ES256);
    // A transaction or a notification is a record, not a token: it has no
    // expiry, audience or subject to check.
    validation.validate_exp = false;
    validation.validate_aud = false;
    validation.required_spec_claims.clear();
    Ok(jsonwebtoken::decode::<T>(jws, &key, &validation)?.claims)
}

/// Apple's signed transaction.
fn read_transaction(jws: &str, root: TrustedRoot, now: UnixTime) -> Result<Transaction> {
    read_signed(jws, root, now)
}

/// That `leaf` and `intermediate` chain to `root` at `now`, and are the
/// certificates Apple says they are.
fn verify_chain(leaf: &[u8], intermediate: &[u8], root: TrustedRoot, now: UnixTime) -> Result<()> {
    let root = CertificateDer::from(root.0);
    let anchor = webpki::anchor_from_trusted_cert(&root)
        .map_err(|error| anyhow!("the trusted root does not parse: {error}"))?;
    let leaf_der = CertificateDer::from(leaf);
    let end_entity = webpki::EndEntityCert::try_from(&leaf_der)
        .map_err(|error| anyhow!("the signing certificate does not parse: {error}"))?;
    end_entity
        .verify_for_usage(
            &[
                webpki::ring::ECDSA_P256_SHA256,
                webpki::ring::ECDSA_P256_SHA384,
                webpki::ring::ECDSA_P384_SHA256,
                webpki::ring::ECDSA_P384_SHA384,
            ],
            &[anchor],
            &[CertificateDer::from(intermediate)],
            now,
            AnyExtendedKeyUsage,
            None,
            None,
        )
        .map_err(|error| anyhow!("the transaction's chain does not lead to Apple: {error}"))?;

    if !has_extension(intermediate, WWDR_INTERMEDIATE_MARKER)? {
        return Err(anyhow!("the intermediate is not Apple's WWDR certificate"));
    }
    if !has_extension(leaf, RECEIPT_SIGNING_MARKER)? {
        return Err(anyhow!(
            "the signing certificate is not for App Store receipts"
        ));
    }
    Ok(())
}

fn has_extension(certificate: &[u8], oid: &str) -> Result<bool> {
    let (_, certificate) = x509_parser::parse_x509_certificate(certificate)?;
    Ok(certificate
        .extensions()
        .iter()
        .any(|extension| extension.oid.to_id_string() == oid))
}

/// Apple's receipt-signing leaf carries no extended key usage, so none is
/// asked for; its marker extension is checked instead.
struct AnyExtendedKeyUsage;

impl webpki::ExtendedKeyUsageValidator for AnyExtendedKeyUsage {
    fn validate(&self, _: webpki::KeyPurposeIdIter<'_, '_>) -> Result<(), webpki::Error> {
        Ok(())
    }
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
    let transaction = read_transaction(&signed, config.trusted_root, UnixTime::now())?;

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

/// The fields of a notification's payload (`responseBodyV2DecodedPayload`)
/// this cares about.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct NotificationPayload {
    notification_type: String,
    subtype: Option<String>,
    data: Option<NotificationData>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct NotificationData {
    bundle_id: Option<String>,
    /// The transaction it is about, signed on its own.
    signed_transaction_info: Option<String>,
}

/// What a notification from Apple is a cue to do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Notice {
    /// Look the transaction up again and record whatever Apple says of it
    /// now: a refund, a refund reversed, Family Sharing taken away, or
    /// anything else about a purchase of ours.
    Recheck {
        kind: String,
        transaction: String,
        product: String,
    },
    /// Apple's test, asked for with `cli test-app-store-notifications`.
    Test,
    /// Signed by Apple, and nothing to do: about another app, or a kind
    /// with no transaction in it.
    Nothing { kind: String },
}

/// Reads a notification's `signedPayload`, having checked that Apple signed
/// it and the transaction inside it. An error is a payload Apple did not
/// sign, or one that is not a notification at all.
///
/// Which kind of notification it is decides nothing beyond whether there
/// is a transaction to look at: REFUND, REFUND_REVERSED, REVOKE and the rest
/// are all answered by asking Apple about the transaction again, so a kind
/// Apple adds later is handled the day it arrives.
pub fn read_notification(
    signed_payload: &str,
    config: &AppStoreConfig,
    now: UnixTime,
) -> Result<Notice> {
    let payload: NotificationPayload = read_signed(signed_payload, config.trusted_root, now)?;
    let kind = match &payload.subtype {
        Some(subtype) => format!("{}/{subtype}", payload.notification_type),
        None => payload.notification_type.clone(),
    };
    if payload.notification_type == "TEST" {
        return Ok(Notice::Test);
    }
    let Some(data) = payload.data else {
        return Ok(Notice::Nothing { kind });
    };
    if data.bundle_id.as_deref() != Some(config.bundle_id.as_str()) {
        return Ok(Notice::Nothing { kind });
    }
    let Some(signed) = data.signed_transaction_info else {
        return Ok(Notice::Nothing { kind });
    };
    let transaction = read_transaction(&signed, config.trusted_root, now)?;
    if transaction.bundle_id != config.bundle_id {
        return Ok(Notice::Nothing { kind });
    }
    Ok(Notice::Recheck {
        kind,
        transaction: transaction.original_transaction_id,
        product: transaction.product_id,
    })
}

/// What came of a [`Notice::Recheck`].
#[derive(Debug, PartialEq, Eq)]
pub enum Heeded {
    /// The purchase stands, or does not, as Apple now says.
    Recorded { owned: bool },
    /// Not a pack the catalogue has, or a transaction Apple no longer
    /// knows: nothing here to change.
    NotOurs,
}

/// Looks the transaction a notification names up again and records what
/// Apple says of it now against the purchase it already is. Only a purchase
/// some account has handed over is changed; the notification makes none.
/// An error is Apple out of reach, which Apple answers by sending the
/// notification again.
pub async fn heed(
    db: &sqlx::PgPool,
    config: &AppStoreConfig,
    transaction: &str,
    product: &str,
) -> Result<Heeded> {
    use crate::models::store_product;
    use crate::models::supporter::{record_recheck, Store};

    let packs = store_product::packs_in(db, Store::Apple).await?;
    if !packs.iter().any(|pack| pack.product == product) {
        return Ok(Heeded::NotOurs);
    }
    let Some(purchase) = look_up(config, &packs, transaction).await? else {
        return Ok(Heeded::NotOurs);
    };
    let mut tx = db.begin().await?;
    record_recheck(
        &mut tx,
        Store::Apple,
        &purchase.transaction,
        &purchase.pack.product,
        purchase.owned,
    )
    .await?;
    tx.commit().await?;
    Ok(Heeded::Recorded {
        owned: purchase.owned,
    })
}

#[derive(Deserialize)]
struct TestNotificationResponse {
    #[serde(rename = "testNotificationToken")]
    test_notification_token: String,
}

/// Asks Apple to send the site a TEST notification, for `cli
/// test-app-store-notifications`: Apple POSTs it to whatever URL App Store
/// Connect has for production, and the site logs it when it arrives.
/// Answers with the token Apple names it by.
pub async fn request_test_notification(config: &AppStoreConfig) -> Result<String> {
    let url = format!(
        "{}/inApps/v1/notifications/test",
        config.api_url.trim_end_matches('/')
    );
    let response = http()
        .post(url)
        .bearer_auth(api_token(config)?)
        .send()
        .await?;
    let status = response.status();
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        return Err(anyhow!("the App Store answered with {status}: {body}"));
    }
    let answer: TestNotificationResponse = response.json().await?;
    Ok(answer.test_notification_token)
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

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct HistoryPage {
    #[serde(default)]
    notification_history: Vec<HistoryItem>,
    #[serde(default)]
    has_more: bool,
    pagination_token: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct HistoryItem {
    signed_payload: String,
}

/// How far back the sweep looks: past the three days Apple spends sending a
/// notification again, so one that fails late in its life is still found.
/// The sandbox keeps thirty days of history and production 180.
const SWEEP_WINDOW: chrono::Duration = chrono::Duration::days(7);

/// Pages of history the sweep reads before giving up for the hour: twenty a
/// page, and a notification that never arrives is the exception.
const SWEEP_MAX_PAGES: usize = 50;

/// Every notification from the last [`SWEEP_WINDOW`] that has not reached
/// the site, or that Apple is still retrying, as its signed payload: Get
/// Notification History, with `onlyFailures`.
async fn undelivered(config: &AppStoreConfig, api_url: &str) -> Result<Vec<String>> {
    let until = Utc::now();
    let body = json!({
        "startDate": (until - SWEEP_WINDOW).timestamp_millis(),
        "endDate": until.timestamp_millis(),
        "onlyFailures": true,
    });
    let url = format!(
        "{}/inApps/v1/notifications/history",
        api_url.trim_end_matches('/')
    );
    let mut payloads = Vec::new();
    let mut token: Option<String> = None;
    for _ in 0..SWEEP_MAX_PAGES {
        let mut request = http()
            .post(&url)
            .bearer_auth(api_token(config)?)
            .json(&body);
        if let Some(token) = &token {
            request = request.query(&[("paginationToken", token)]);
        }
        let response = request.send().await?;
        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(anyhow!(
                "the App Store answered the notification history with {status}: {body}"
            ));
        }
        let page: HistoryPage = response.json().await?;
        payloads.extend(
            page.notification_history
                .into_iter()
                .map(|item| item.signed_payload),
        );
        match page.pagination_token.filter(|_| page.has_more) {
            Some(next) => token = Some(next),
            None => return Ok(payloads),
        }
    }
    Err(anyhow!(
        "the App Store's notification history went on past {SWEEP_MAX_PAGES} pages"
    ))
}

/// Heeds every notification that has not reached the site, in production
/// and in the sandbox, as it would have been heeded had it arrived.
/// `heeded` is the payloads already dealt with, which are passed over: a
/// notification that never arrives stays in the history as a failure for
/// the whole window, and is only worth asking about once. Returns how many
/// were dealt with this time.
pub async fn sweep_once(
    db: &sqlx::PgPool,
    config: &AppStoreConfig,
    heeded: &mut std::collections::HashSet<String>,
) -> Result<usize> {
    let mut dealt_with = 0;
    for api_url in [&config.api_url, &config.sandbox_api_url] {
        for payload in undelivered(config, api_url).await? {
            if heeded.contains(&payload) {
                continue;
            }
            let done = match read_notification(&payload, config, UnixTime::now()) {
                Ok(Notice::Recheck {
                    kind,
                    transaction,
                    product,
                }) => match heed(db, config, &transaction, &product).await {
                    Ok(heeded) => {
                        tracing::info!(
                            kind,
                            ?heeded,
                            "heeded an App Store notification that never arrived"
                        );
                        true
                    }
                    // Left for the next sweep.
                    Err(error) => {
                        tracing::warn!(
                            kind,
                            "could not heed a missed App Store notification: {error:#}"
                        );
                        false
                    }
                },
                Ok(Notice::Test | Notice::Nothing { .. }) => true,
                // Apple's history is Apple's, so this is a payload that has
                // stopped checking out -- a certificate expired, say -- and
                // asking again next hour will not change that.
                Err(error) => {
                    tracing::warn!("a missed App Store notification does not check out: {error:#}");
                    true
                }
            };
            if done {
                heeded.insert(payload);
                dealt_with += 1;
            }
        }
    }
    Ok(dealt_with)
}

/// Once an hour, the notifications Apple could not deliver: its answer to a
/// refund the site was down for, or answered with an error. A notification
/// that arrived was heeded as it came, so this asks Apple about nothing
/// else -- one request an hour for each environment, however many packs
/// have been sold, where a daily recheck of every purchase would grow with
/// them.
///
/// What has been heeded is kept for the life of the process: a restart
/// heeds the failures of the last week once more, which changes nothing.
/// Both colours sweep for a moment during a deploy, which is harmless too.
pub async fn sweep_notifications(db: sqlx::PgPool, config: AppStoreConfig) {
    let mut heeded = std::collections::HashSet::new();
    let mut every = tokio::time::interval(Duration::from_secs(60 * 60));
    every.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        every.tick().await;
        if let Err(error) = sweep_once(&db, &config, &mut heeded).await {
            tracing::warn!("could not read the App Store's notification history: {error:#}");
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
    /// issues -- and, since the fake App Store signs its transactions with
    /// it, for Apple's receipt-signing key too: `testdata/app_store_x5c`'s
    /// leaf certificates are for this key.
    const PRIVATE_KEY: &str = include_str!("testdata/app_store_test_key.p8");
    /// A P-256 key that is not the leaf's.
    const IMPOSTOR_KEY: &str = include_str!("testdata/app_store_x5c/impostor_key.p8");

    /// The chains the fake signs under, shaped like Apple's (see
    /// `testdata/app_store_x5c/generate.sh`), and the root they lead to.
    const TEST_ROOT: TrustedRoot = TrustedRoot(include_bytes!("testdata/app_store_x5c/root.der"));
    const LEAF: &[u8] = include_bytes!("testdata/app_store_x5c/leaf.der");
    const INTERMEDIATE: &[u8] = include_bytes!("testdata/app_store_x5c/intermediate.der");
    const UNMARKED_LEAF: &[u8] = include_bytes!("testdata/app_store_x5c/leaf_unmarked.der");
    const UNMARKED_INTERMEDIATE: &[u8] =
        include_bytes!("testdata/app_store_x5c/intermediate_unmarked.der");
    const LEAF_UNDER_UNMARKED: &[u8] =
        include_bytes!("testdata/app_store_x5c/leaf_under_unmarked.der");

    /// Apple's own intermediate and current receipt-signing leaf, as they
    /// arrive in every production transaction's `x5c`: copied from the
    /// tests of Apple's app-store-server-library-python, which check them
    /// at the moment below.
    const APPLE_INTERMEDIATE: &[u8] = include_bytes!("testdata/app_store_x5c/apple_wwdr_g6.der");
    const APPLE_LEAF: &[u8] = include_bytes!("testdata/app_store_x5c/apple_receipt_signing.der");
    const WHEN_APPLE_CHECKS_THEM: u64 = 1_761_962_975;
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
        let Ok(claims) = jsonwebtoken::dangerous::insecure_decode::<Value>(bearer) else {
            return false;
        };
        claims.claims["iss"] == json!(ISSUER_ID)
            && claims.claims["aud"] == json!(TOKEN_AUDIENCE)
            && claims.claims["bid"] == json!(BUNDLE_ID)
    }

    /// Signed the way Apple signs: ES256, naming the chain in `x5c`.
    fn signed(transaction: &Value) -> String {
        signed_with(transaction, PRIVATE_KEY, &[LEAF, INTERMEDIATE, TEST_ROOT.0])
    }

    fn signed_with(transaction: &Value, key: &str, chain: &[&[u8]]) -> String {
        let mut header = Header::new(Algorithm::ES256);
        header.x5c = Some(chain.iter().map(|der| BASE64.encode(der)).collect());
        encode(
            &header,
            transaction,
            &EncodingKey::from_ec_pem(key.as_bytes()).unwrap(),
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
                "/production/inApps/v1/transactions/{id}",
                get(|headers: HeaderMap, Path(id): Path<String>| answer("production", headers, id)),
            )
            .route(
                "/sandbox/inApps/v1/transactions/{id}",
                get(|headers: HeaderMap, Path(id): Path<String>| answer("sandbox", headers, id)),
            )
            .route(
                "/production/inApps/v1/notifications/test",
                axum::routing::post(|headers: HeaderMap| async move {
                    if !token_is_ours(&headers) {
                        return axum::http::StatusCode::UNAUTHORIZED.into_response();
                    }
                    Json(json!({"testNotificationToken": "test-token"})).into_response()
                }),
            )
            .route(
                "/{environment}/inApps/v1/notifications/history",
                axum::routing::post(
                    |headers: HeaderMap,
                     Path(environment): Path<String>,
                     axum::extract::Query(query): axum::extract::Query<HashMap<String, String>>,
                     Json(body): Json<Value>| async move {
                        if !token_is_ours(&headers) {
                            return axum::http::StatusCode::UNAUTHORIZED.into_response();
                        }
                        // Apple's own limits on the request.
                        let (Some(start), Some(end)) =
                            (body["startDate"].as_i64(), body["endDate"].as_i64())
                        else {
                            return axum::http::StatusCode::BAD_REQUEST.into_response();
                        };
                        let now = Utc::now().timestamp_millis();
                        if start >= end || now - start > 30 * 24 * 60 * 60 * 1000 {
                            return axum::http::StatusCode::BAD_REQUEST.into_response();
                        }
                        if body["onlyFailures"] != json!(true) || environment == "sandbox" {
                            return Json(json!({"notificationHistory": [], "hasMore": false}))
                                .into_response();
                        }
                        // Two pages: the refunds Apple could not deliver, then
                        // its test. 1000's refund has since been reversed, so
                        // its record no longer says what is true of it.
                        let refund = |id: &str, revoked: bool| {
                            let revocation = if revoked {
                                json!({"revocationDate": 1_790_000_000_000i64})
                            } else {
                                json!({})
                            };
                            json!({
                                "signedPayload": signed(&notification(
                                    "REFUND",
                                    None,
                                    Some(signed(&transaction(id, revocation))),
                                )),
                                "sendAttempts": [{"attemptDate": 1_790_000_000_000i64,
                                                  "sendAttemptResult": "TIMED_OUT"}],
                            })
                        };
                        match query.get("paginationToken").map(String::as_str) {
                            None => Json(json!({
                                "notificationHistory": [refund("1001", true), refund("1000", true)],
                                "hasMore": true,
                                "paginationToken": "page-2",
                            }))
                            .into_response(),
                            Some("page-2") => Json(json!({
                                "notificationHistory": [
                                    {"signedPayload": signed(&json!({"notificationType": "TEST"}))}
                                ],
                                "hasMore": false,
                            }))
                            .into_response(),
                            Some(_) => axum::http::StatusCode::BAD_REQUEST.into_response(),
                        }
                    },
                ),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let mut config = AppStoreConfig {
            issuer_id: ISSUER_ID.to_string(),
            key_id: KEY_ID.to_string(),
            private_key_path: PRIVATE_KEY_PATH.to_string(),
            private_key: KeyFile::default(),
            bundle_id: BUNDLE_ID.to_string(),
            api_url: format!("http://{addr}/production"),
            sandbox_api_url: format!("http://{addr}/sandbox"),
            trusted_root: TEST_ROOT,
        };
        config.load_key().expect("the test key loads");
        config
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

        // A key that was never loaded is an error too, and not a request
        // without one.
        let mut config = fake_app_store().await;
        config.private_key = KeyFile::default();
        assert!(look_up(&config, &packs(), "1000").await.is_err());
    }

    /// A key file that is not there, or is not a key, is found when the
    /// config is loaded: the server does not boot, rather than failing the
    /// first purchase after it passed its health check.
    #[test]
    fn a_key_that_will_not_load_is_found_at_load() {
        let mut config = test_config();
        config.private_key_path = "/nowhere/app-store.p8".to_string();
        let error = config.load_key().unwrap_err().to_string();
        assert!(error.contains("/nowhere/app-store.p8"), "{error}");

        let mut config = test_config();
        config.private_key_path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/testdata/app_store_x5c/root.der"
        )
        .to_string();
        assert!(config.load_key().is_err());
    }

    fn read(jws: &str) -> Result<Transaction> {
        read_transaction(jws, TEST_ROOT, UnixTime::now())
    }

    /// A notification as Apple sends it: the payload signed, with the
    /// transaction it is about signed again inside it.
    fn notification(kind: &str, subtype: Option<&str>, transaction: Option<String>) -> Value {
        let mut payload = json!({
            "notificationType": kind,
            "notificationUUID": "002e14d5-51f5-4503-b5a8-c3a1af68eb20",
            "data": {
                "appAppleId": 1234567890,
                "bundleId": BUNDLE_ID,
                "environment": "Production",
                "signedTransactionInfo": transaction,
            },
            "version": "2.0",
            "signedDate": 1_790_000_000_000i64,
        });
        if let Some(subtype) = subtype {
            payload["subtype"] = json!(subtype);
        }
        payload
    }

    fn test_config() -> AppStoreConfig {
        let mut config = AppStoreConfig {
            issuer_id: ISSUER_ID.to_string(),
            key_id: KEY_ID.to_string(),
            private_key_path: PRIVATE_KEY_PATH.to_string(),
            private_key: KeyFile::default(),
            bundle_id: BUNDLE_ID.to_string(),
            api_url: "http://127.0.0.1:9/production".to_string(),
            sandbox_api_url: "http://127.0.0.1:9/sandbox".to_string(),
            trusted_root: TEST_ROOT,
        };
        config.load_key().expect("the test key loads");
        config
    }

    fn notice(payload: &Value) -> Result<Notice> {
        read_notification(&signed(payload), &test_config(), UnixTime::now())
    }

    /// A refund, a reversal, Family Sharing taken away: each is a cue to
    /// look the transaction up again, whatever it is called.
    #[test]
    fn a_notification_names_the_purchase_to_look_at_again() {
        let refunded = signed(&transaction(
            "1001",
            json!({"revocationDate": 1_790_000_000_000i64}),
        ));
        for (kind, subtype, called) in [
            ("REFUND", None, "REFUND"),
            ("REFUND_REVERSED", None, "REFUND_REVERSED"),
            ("REVOKE", None, "REVOKE"),
            (
                "SOMETHING_NEW",
                Some("AND_SO_ON"),
                "SOMETHING_NEW/AND_SO_ON",
            ),
        ] {
            assert_eq!(
                notice(&notification(kind, subtype, Some(refunded.clone()))).unwrap(),
                Notice::Recheck {
                    kind: called.to_string(),
                    transaction: "1001".to_string(),
                    product: PRODUCT_ID.to_string(),
                }
            );
        }
    }

    #[test]
    fn a_test_notification_is_a_test() {
        assert_eq!(
            notice(&json!({"notificationType": "TEST"})).unwrap(),
            Notice::Test
        );
    }

    #[test]
    fn a_notification_about_another_app_or_no_purchase_is_nothing() {
        let mut elsewhere = notification(
            "REFUND",
            None,
            Some(signed(&transaction("1000", json!({})))),
        );
        elsewhere["data"]["bundleId"] = json!("com.example.other");
        assert!(matches!(
            notice(&elsewhere).unwrap(),
            Notice::Nothing { .. }
        ));

        // The payload says ours, the transaction inside it says otherwise.
        let other = signed(&transaction(
            "1003",
            json!({"bundleId": "com.example.other"}),
        ));
        assert!(matches!(
            notice(&notification("REFUND", None, Some(other))).unwrap(),
            Notice::Nothing { .. }
        ));

        assert_eq!(
            notice(&notification("EXTERNAL_PURCHASE_TOKEN", None, None)).unwrap(),
            Notice::Nothing {
                kind: "EXTERNAL_PURCHASE_TOKEN".to_string()
            }
        );
    }

    /// Anyone can post to the site. A payload Apple did not sign, or one
    /// that carries a transaction Apple did not sign, is refused.
    #[test]
    fn a_notification_apple_did_not_sign_is_refused() {
        let genuine = signed(&transaction("1000", json!({})));
        let forged = signed_with(
            &notification("REFUND", None, Some(genuine)),
            IMPOSTOR_KEY,
            &[LEAF, INTERMEDIATE, TEST_ROOT.0],
        );
        assert!(read_notification(&forged, &test_config(), UnixTime::now()).is_err());

        let forged_inside = signed_with(
            &transaction("1000", json!({})),
            IMPOSTOR_KEY,
            &[LEAF, INTERMEDIATE, TEST_ROOT.0],
        );
        assert!(notice(&notification("REFUND", None, Some(forged_inside))).is_err());

        assert!(read_notification("not a jws", &test_config(), UnixTime::now()).is_err());
        let under_another_root = read_notification(
            &signed(&json!({"notificationType": "TEST"})),
            &{
                let mut config = test_config();
                config.trusted_root = TrustedRoot::default();
                config
            },
            UnixTime::now(),
        );
        assert!(
            under_another_root.is_err(),
            "only the configured root is trusted"
        );
    }

    /// The whole of a refund arriving: the notification is a cue, Apple is
    /// asked, and the purchase an account handed over is revoked -- and a
    /// reversal, asked about the same way, would give it back. Against the
    /// database `DATABASE_URL` names, with what it adds taken away again.
    /// The fake App Store's transaction ids are the database tests' purchase
    /// owners, and a purchase is one row whoever records it, so those tests
    /// take turns.
    static PURCHASE_ROWS: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    #[tokio::test]
    async fn a_refund_notification_revokes_the_purchase_it_names() {
        use crate::models::supporter::{record_purchase, Store};
        let _turn = PURCHASE_ROWS.lock().await;
        let Ok(url) = std::env::var("DATABASE_URL") else {
            return;
        };
        let Ok(db) = sqlx::PgPool::connect(&url).await else {
            return;
        };
        let config = fake_app_store().await;

        let added_product = sqlx::query(
            "INSERT INTO store_products (store, product, year) VALUES ('apple', $1, $2)
             ON CONFLICT DO NOTHING",
        )
        .bind(PRODUCT_ID)
        .bind(PACK_YEAR)
        .execute(&db)
        .await
        .unwrap()
        .rows_affected()
            == 1;
        let login = format!("notify_{}", &Uuid::new_v4().simple().to_string()[..12]);
        let buyer: Uuid = sqlx::query_scalar(
            "INSERT INTO users (login_name, display_name, password_hash) VALUES ($1, $1, 'x') RETURNING id",
        )
        .bind(&login)
        .fetch_one(&db)
        .await
        .unwrap();
        // "1001" is refunded at the fake App Store; "1000" is not.
        let mut tx = db.begin().await.unwrap();
        for id in ["1001", "1000"] {
            let pack = OwnedProduct {
                product: PRODUCT_ID.to_string(),
                year: PACK_YEAR,
            };
            record_purchase(&mut tx, buyer, Store::Apple, id, &pack, true)
                .await
                .unwrap();
        }
        tx.commit().await.unwrap();

        let refunded = heed(&db, &config, "1001", PRODUCT_ID).await;
        let standing = heed(&db, &config, "1000", PRODUCT_ID).await;
        let unknown = heed(&db, &config, "9999", PRODUCT_ID).await;
        let elsewhere = heed(&db, &config, "1000", "cafe.oeee.not.in.the.catalogue").await;
        let revoked: Vec<(String, bool)> = sqlx::query_as(
            "SELECT owner, revoked_at IS NOT NULL FROM supporter_purchases
             WHERE user_id = $1 ORDER BY owner",
        )
        .bind(buyer)
        .fetch_all(&db)
        .await
        .unwrap();

        sqlx::query("DELETE FROM users WHERE id = $1")
            .bind(buyer)
            .execute(&db)
            .await
            .unwrap();
        if added_product {
            sqlx::query("DELETE FROM store_products WHERE store = 'apple' AND product = $1")
                .bind(PRODUCT_ID)
                .execute(&db)
                .await
                .unwrap();
        }

        assert_eq!(refunded.unwrap(), Heeded::Recorded { owned: false });
        assert_eq!(standing.unwrap(), Heeded::Recorded { owned: true });
        assert_eq!(unknown.unwrap(), Heeded::NotOurs);
        assert_eq!(elsewhere.unwrap(), Heeded::NotOurs);
        assert_eq!(
            revoked,
            [("1000".to_string(), false), ("1001".to_string(), true)]
        );
    }

    /// The notifications Apple could not deliver, found in its history and
    /// heeded as they would have been: the refund revokes its purchase, and
    /// a refund since reversed -- whose record in the history still says
    /// refunded -- is answered by what Apple says of it now, and stands.
    /// Each is dealt with once, however many sweeps see it.
    #[tokio::test]
    async fn a_sweep_heeds_what_never_arrived_by_what_is_true_now() {
        use crate::models::supporter::{record_purchase, Store};
        let _turn = PURCHASE_ROWS.lock().await;
        let Ok(url) = std::env::var("DATABASE_URL") else {
            return;
        };
        let Ok(db) = sqlx::PgPool::connect(&url).await else {
            return;
        };
        let config = fake_app_store().await;

        let added_product = sqlx::query(
            "INSERT INTO store_products (store, product, year) VALUES ('apple', $1, $2)
             ON CONFLICT DO NOTHING",
        )
        .bind(PRODUCT_ID)
        .bind(PACK_YEAR)
        .execute(&db)
        .await
        .unwrap()
        .rows_affected()
            == 1;
        let login = format!("sweep_{}", &Uuid::new_v4().simple().to_string()[..12]);
        let buyer: Uuid = sqlx::query_scalar(
            "INSERT INTO users (login_name, display_name, password_hash) VALUES ($1, $1, 'x') RETURNING id",
        )
        .bind(&login)
        .fetch_one(&db)
        .await
        .unwrap();
        let mut tx = db.begin().await.unwrap();
        for id in ["1001", "1000"] {
            let pack = OwnedProduct {
                product: PRODUCT_ID.to_string(),
                year: PACK_YEAR,
            };
            record_purchase(&mut tx, buyer, Store::Apple, id, &pack, true)
                .await
                .unwrap();
        }
        tx.commit().await.unwrap();

        let mut heeded = std::collections::HashSet::new();
        let first = sweep_once(&db, &config, &mut heeded).await;
        let second = sweep_once(&db, &config, &mut heeded).await;
        let revoked: Vec<(String, bool)> = sqlx::query_as(
            "SELECT owner, revoked_at IS NOT NULL FROM supporter_purchases
             WHERE user_id = $1 ORDER BY owner",
        )
        .bind(buyer)
        .fetch_all(&db)
        .await
        .unwrap();

        sqlx::query("DELETE FROM users WHERE id = $1")
            .bind(buyer)
            .execute(&db)
            .await
            .unwrap();
        if added_product {
            sqlx::query("DELETE FROM store_products WHERE store = 'apple' AND product = $1")
                .bind(PRODUCT_ID)
                .execute(&db)
                .await
                .unwrap();
        }

        assert_eq!(first.unwrap(), 3, "two refunds and a test, over two pages");
        assert_eq!(second.unwrap(), 0, "each once");
        assert_eq!(
            revoked,
            [("1000".to_string(), false), ("1001".to_string(), true)],
            "1000's record says refunded; Apple says it stands"
        );
    }

    #[tokio::test]
    async fn apple_is_asked_for_a_test_notification_with_our_key() {
        let config = fake_app_store().await;
        assert_eq!(
            request_test_notification(&config).await.unwrap(),
            "test-token"
        );

        let mut refused = fake_app_store().await;
        refused.key_id = "WRONGKEY00".to_string();
        assert!(request_test_notification(&refused).await.is_err());
    }

    #[test]
    fn a_transaction_signed_under_the_chain_reads() {
        let transaction = read(&signed(&transaction("1000", json!({})))).unwrap();
        assert_eq!(transaction.product_id, PRODUCT_ID);
    }

    /// The production setting: a chain to any root but Apple's is refused,
    /// whatever root the header itself carries.
    #[test]
    fn only_apples_root_is_trusted() {
        let jws = signed(&transaction("1000", json!({})));
        let error = read_transaction(&jws, TrustedRoot::default(), UnixTime::now())
            .err()
            .expect("our test root is not Apple's");
        assert!(
            error.to_string().contains("does not lead to Apple"),
            "{error}"
        );
    }

    /// The chain is right and the signature is somebody else's.
    #[test]
    fn the_signature_has_to_be_the_leafs() {
        let jws = signed_with(
            &transaction("1000", json!({})),
            IMPOSTOR_KEY,
            &[LEAF, INTERMEDIATE, TEST_ROOT.0],
        );
        assert!(read(&jws).is_err());
    }

    /// A certificate that chains to the root is not enough: each has to be
    /// the one Apple's marker says it is.
    #[test]
    fn each_certificate_has_to_carry_apples_marker() {
        for (what, chain) in [
            ("leaf", [UNMARKED_LEAF, INTERMEDIATE, TEST_ROOT.0]),
            (
                "intermediate",
                [LEAF_UNDER_UNMARKED, UNMARKED_INTERMEDIATE, TEST_ROOT.0],
            ),
        ] {
            let jws = signed_with(&transaction("1000", json!({})), PRIVATE_KEY, &chain);
            let error = read(&jws)
                .err()
                .unwrap_or_else(|| panic!("an unmarked {what}"));
            assert!(error.to_string().contains("is not"), "{what}: {error}");
        }
    }

    #[test]
    fn a_transaction_without_its_chain_is_refused() {
        let unchained = encode(
            &Header::new(Algorithm::ES256),
            &transaction("1000", json!({})),
            &EncodingKey::from_ec_pem(PRIVATE_KEY.as_bytes()).unwrap(),
        )
        .unwrap();
        assert!(read(&unchained).is_err(), "no x5c");

        let short = signed_with(
            &transaction("1000", json!({})),
            PRIVATE_KEY,
            &[LEAF, INTERMEDIATE],
        );
        assert!(read(&short).is_err(), "two certificates");
    }

    /// Apple's real chain, against the root built in: what every production
    /// transaction is checked against, and what the fixtures above only
    /// imitate. Its leaf carries no extended key usage, which is why none is
    /// asked for.
    #[test]
    fn apples_own_chain_leads_to_the_built_in_root() {
        let when = UnixTime::since_unix_epoch(Duration::from_secs(WHEN_APPLE_CHECKS_THEM));
        verify_chain(APPLE_LEAF, APPLE_INTERMEDIATE, TrustedRoot::default(), when).unwrap();
    }

    #[test]
    fn apples_chain_is_refused_once_its_leaf_expires() {
        // The leaf is good until October 2027.
        let later = UnixTime::since_unix_epoch(Duration::from_secs(1_830_000_000));
        assert!(verify_chain(
            APPLE_LEAF,
            APPLE_INTERMEDIATE,
            TrustedRoot::default(),
            later
        )
        .is_err());
    }

    #[test]
    fn apples_chain_is_refused_under_another_root() {
        let when = UnixTime::since_unix_epoch(Duration::from_secs(WHEN_APPLE_CHECKS_THEM));
        assert!(verify_chain(APPLE_LEAF, APPLE_INTERMEDIATE, TEST_ROOT, when).is_err());
    }
}
