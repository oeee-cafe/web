use std::time::Duration;

use config::{Config, ConfigError, Environment, File};
use serde::{Deserialize, Serialize};
use serde_with::{serde_as, DurationSeconds};

#[serde_as]
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct AppConfig {
    pub env: String,
    pub base_url: String,
    pub domain: String,
    pub port: u16,

    /// `tracing_subscriber` env-filter directive, e.g. "info" or
    /// "warn,oeee_cafe=debug". Overridden by `RUST_LOG` when that is set.
    #[serde(default = "default_log_level")]
    pub log_level: String,
    /// Error reporting is disabled when unset or empty.
    #[serde(default)]
    pub sentry_dsn: Option<String>,

    pub db_url: String,
    pub db_max_connections: u32,
    #[serde_as(as = "DurationSeconds<u64>")]
    pub db_acquire_timeout: Duration,

    pub redis_url: String,
    pub redis_max_connections: u32,
    #[serde_as(as = "DurationSeconds<u64>")]
    pub redis_acquire_timeout: Duration,

    pub official_account_login_name: String,

    pub aws_access_key_id: String,
    pub aws_secret_access_key: String,
    pub aws_region: String,
    pub aws_s3_bucket: String,
    pub r2_endpoint_url: String,
    pub r2_public_endpoint_url: String,

    /// Where collaborative session recordings and the reports that go with
    /// them are kept.
    ///
    /// A bucket of its own, and one that is **not** served publicly.
    /// `aws_s3_bucket` is: every image on the site is fetched straight from
    /// `r2_public_endpoint_url`, so anything written there is readable by
    /// anyone holding the URL. A recording is the whole of a private drawing
    /// session and is meant for staff alone.
    ///
    /// Unset means no recording at all, which is the safe way round: a
    /// deployment that has not been given somewhere private to put these
    /// should keep none rather than publish them.
    #[serde(default)]
    pub archive_s3_bucket: Option<String>,

    pub smtp_host: String,
    pub smtp_port: u16,
    pub smtp_user: String,
    pub smtp_password: String,

    // APNs configuration
    pub apns_key_id: String,
    pub apns_team_id: String,
    pub apns_key_path: String,
    pub apns_environment: String, // "production" or "sandbox"
    pub apns_topic: String,       // App bundle ID

    // FCM configuration (V1 API)
    pub fcm_service_account_path: String,
    pub fcm_project_id: String,

    /// Steam sign-in, as a `[steam]` table. Unset means the site does not
    /// offer it, and `/auth/steam` turns every ticket away.
    #[serde(default)]
    pub steam: Option<SteamConfig>,

    /// Sign in with Apple, as an `[apple]` table. Unset means the site does
    /// not offer it.
    #[serde(default)]
    pub apple: Option<AppleConfig>,

    /// The App Store's side of the iOS and macOS apps, as an `[app_store]`
    /// table: the key that asks Apple about a purchase. Separate from
    /// `[apple]`, which signs people in with different credentials entirely.
    /// What the Supporter Pack is sold as is the catalogue's to say
    /// (`models::store_product`). Unset means `/store/apple/purchases`
    /// answers every transaction with a 404.
    #[serde(default)]
    pub app_store: Option<AppStoreConfig>,

    /// The Microsoft Store's side of the Windows app, as a
    /// `[microsoft_store]` table: the Microsoft Entra app the site asks the
    /// Microsoft Store's collections API with (`crate::microsoft_store`).
    /// Unset means `/store/microsoft/tickets` and
    /// `/store/microsoft/purchases` answer 404.
    #[serde(default)]
    pub microsoft_store: Option<MicrosoftStoreConfig>,

    /// Sign in with Google, as a `[google]` table. Unset means the site does
    /// not offer it.
    #[serde(default)]
    pub google: Option<GoogleConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct AppStoreConfig {
    /// The issuer of the In-App Purchase key, a UUID App Store Connect
    /// prints beside it (Users and Access > Integrations > In-App Purchase).
    pub issuer_id: String,
    /// That key's id, which every request names in its token's header.
    pub key_id: String,
    /// Where the `.p8` private key file is. Kept off the repository, like
    /// the APNs key beside it.
    pub private_key_path: String,
    /// The app the purchase has to have been made in: `cafe.oeee` for the
    /// iOS app. A transaction from any other bundle buys nothing here.
    pub bundle_id: String,
    /// Deprecated: the packs live in the `store_products` table now, and
    /// /admin/store changes them. Still read so a config that lists them
    /// boots, and imported into the table on boot -- added where missing,
    /// never changing a row already there (`store_product::import_configured`).
    /// Nothing else reads it.
    ///
    /// A Supporter Pack per year, as `[[app_store.supporter_products]]`
    /// tables of `year` and `product_id`.
    #[serde(default)]
    pub supporter_products: Vec<SupporterProduct>,
    /// Where the App Store Server API is, and where its sandbox is: a
    /// transaction production has never heard of is asked about there, so
    /// TestFlight and Xcode builds work without a second deployment. Only a
    /// test changes them.
    #[serde(default = "default_app_store_api_url")]
    pub api_url: String,
    #[serde(default = "default_app_store_sandbox_api_url")]
    pub sandbox_api_url: String,
}

/// A year's Supporter Pack in the App Store.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct SupporterProduct {
    pub year: i32,
    pub product_id: String,
}

fn default_app_store_api_url() -> String {
    "https://api.storekit.itunes.apple.com".to_string()
}

fn default_app_store_sandbox_api_url() -> String {
    "https://api.storekit-sandbox.itunes.apple.com".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct AppleConfig {
    /// The Services ID registered for Sign in with Apple (Certificates,
    /// Identifiers & Profiles > Identifiers > Services IDs), whose return URL
    /// is `{base_url}/auth/apple/callback`. Apple's ID tokens name it as
    /// their audience.
    pub client_id: String,
    /// Bundle IDs of the apps that sign in with Apple natively (the iOS app
    /// is `cafe.oeee`). Their ID tokens name the bundle ID as audience rather
    /// than the Services ID.
    #[serde(default)]
    pub app_ids: Vec<String>,
    /// Where Apple publishes the keys it signs ID tokens with. Only a test
    /// changes it.
    #[serde(default = "default_apple_keys_url")]
    pub keys_url: String,
}

fn default_apple_keys_url() -> String {
    "https://appleid.apple.com/auth/keys".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct GoogleConfig {
    /// The OAuth client id of the **Web application** client (Google Cloud
    /// console > APIs & Services > Credentials), whose authorised redirect
    /// URI is `{base_url}/auth/google/callback`. Google's ID tokens name it
    /// as their audience, and so do the Android app's: Credential Manager is
    /// given this as its server client id.
    pub client_id: String,
    /// That client's secret. Only ever sent to Google's token endpoint, to
    /// trade a code for an ID token.
    pub client_secret: String,
    /// Where Google publishes the keys it signs ID tokens with. Only a test
    /// changes it.
    #[serde(default = "default_google_keys_url")]
    pub keys_url: String,
    /// Where a code is traded for a token. Only a test changes it.
    #[serde(default = "default_google_token_url")]
    pub token_url: String,
}

fn default_google_keys_url() -> String {
    "https://www.googleapis.com/oauth2/v3/certs".to_string()
}

fn default_google_token_url() -> String {
    "https://oauth2.googleapis.com/token".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct SteamConfig {
    /// The app id of Oeee Cafe on Steam.
    pub app_id: u32,
    /// A publisher Web API key (Steamworks > Users & Permissions > Manage
    /// Groups), not a user's key: AuthenticateUserTicket accepts no other.
    pub web_api_key: String,
    /// Deprecated, as `app_store.supporter_products` is: the packs live in
    /// the `store_products` table, this is imported into it on boot where
    /// missing, and nothing else reads it. The app's own `app_id` is left
    /// out of the import however it is listed: buying Oeee Cafe is not
    /// supporting it.
    ///
    /// A Supporter Pack DLC per year, as `[[steam.supporter_apps]]` tables
    /// of `year` and `app_id`.
    #[serde(default)]
    pub supporter_apps: Vec<SupporterApp>,
    /// Where the partner Web API is. Only a test changes it.
    #[serde(default = "default_steam_web_api_url")]
    pub web_api_url: String,
}

/// A year's Supporter Pack on Steam.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct SupporterApp {
    pub year: i32,
    pub app_id: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct MicrosoftStoreConfig {
    /// The Microsoft Entra tenant the app below is registered in, whose id
    /// is associated with the Partner Center account that sells the Windows
    /// app (Partner Center > Account settings > Tenants).
    pub tenant_id: String,
    /// The application (client) id of a web app registered in that tenant
    /// and added to the Windows app's product in Partner Center (Product
    /// management > Product collections and purchases). Its tokens are what
    /// the Microsoft Store is asked with.
    pub client_id: String,
    /// A client secret of that app. Only ever sent to Microsoft's token
    /// endpoint.
    pub client_secret: String,
    /// Where tokens come from. Unset is Microsoft Entra's v1 endpoint for
    /// the tenant (see `microsoft_store::token_url`); only a test changes it.
    #[serde(default)]
    pub token_url: Option<String>,
    /// Where the collections API is. Only a test changes it.
    #[serde(default = "default_microsoft_collections_url")]
    pub collections_url: String,
}

fn default_microsoft_collections_url() -> String {
    "https://purchase.mp.microsoft.com/v8.0/b2b/collections/query".to_string()
}

fn default_steam_web_api_url() -> String {
    "https://partner.steam-api.com".to_string()
}

fn default_log_level() -> String {
    "info".to_string()
}

impl AppConfig {
    pub fn new_from_file_and_env(path: &str) -> Result<Self, ConfigError> {
        Config::builder()
            .add_source(File::with_name(path))
            .add_source(Environment::with_prefix("oeee"))
            .build()
            .and_then(|cfg| cfg.try_deserialize::<Self>())
    }

    /// Determines whether to use ActivityPub message queueing.
    /// Returns true in production for reliability (automatic retries on failure),
    /// false in development for easier debugging (immediate sending).
    pub fn use_activitypub_queue(&self) -> bool {
        self.env == "production"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The sample is the only written account of what a config looks like,
    /// and a table shaped wrong is a server that does not boot. The packs
    /// are the part with shape to get wrong: a table array inside a table,
    /// one entry per year.
    ///
    /// Only the tables this reads are deserialized, not the whole config:
    /// the sample leaves some required fields out entirely, which is its own
    /// problem and not this one's.
    #[test]
    fn the_sample_configs_packs_parse_when_uncommented() {
        #[derive(Deserialize)]
        struct Sample {
            steam: Option<SteamConfig>,
            apple: Option<AppleConfig>,
            app_store: Option<AppStoreConfig>,
            microsoft_store: Option<MicrosoftStoreConfig>,
        }

        let sample =
            std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/config/sample.toml"))
                .unwrap();
        // A commented line that is config rather than prose -- a table
        // heading, or a key with a value -- is a line somebody is meant to
        // be able to uncomment, so that is how it is read here.
        let uncommented: String = sample
            .lines()
            .filter_map(|line| match line.strip_prefix("# ") {
                Some(rest)
                    if (rest.starts_with('[') && rest.ends_with(']') && !rest.contains(' '))
                        || rest
                            .split_once(" = ")
                            .is_some_and(|(key, _)| !key.contains(' ')) =>
                {
                    Some(rest)
                }
                Some(_) => None,
                None => Some(line).filter(|line| !line.starts_with('#')),
            })
            .collect::<Vec<_>>()
            .join("\n");
        let parsed: Sample = ::config::Config::builder()
            .add_source(::config::File::from_str(
                &uncommented,
                ::config::FileFormat::Toml,
            ))
            .build()
            .expect("the sample builds")
            .try_deserialize()
            .expect("the sample's tables deserialize");

        let steam = parsed.steam.expect("a [steam] table");
        assert_eq!(steam.supporter_apps.len(), 1);
        assert_eq!(steam.supporter_apps[0].year, 2026);
        assert_eq!(
            steam.web_api_url, "https://partner.steam-api.com",
            "the default is the real one"
        );
        assert!(parsed.apple.is_some_and(|apple| !apple.app_ids.is_empty()));

        let store = parsed.app_store.expect("an [app_store] table");
        assert_eq!(store.bundle_id, "cafe.oeee");
        assert_eq!(store.supporter_products.len(), 1);
        assert_eq!(store.supporter_products[0].year, 2026);
        assert_eq!(
            store.supporter_products[0].product_id,
            "cafe.oeee.supporter.2026"
        );
        assert!(store.api_url.contains("api.storekit."));
        assert!(store.sandbox_api_url.contains("sandbox"));

        let microsoft = parsed.microsoft_store.expect("a [microsoft_store] table");
        assert_eq!(microsoft.token_url, None, "the tenant's own, unless a test says");
        assert_eq!(
            microsoft.collections_url,
            "https://purchase.mp.microsoft.com/v8.0/b2b/collections/query"
        );
    }
}
