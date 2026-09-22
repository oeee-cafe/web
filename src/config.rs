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
pub struct SteamConfig {
    /// The app id of Oeee Cafe on Steam.
    pub app_id: u32,
    /// A publisher Web API key (Steamworks > Users & Permissions > Manage
    /// Groups), not a user's key: AuthenticateUserTicket accepts no other.
    pub web_api_key: String,
    /// The Steam apps whose owners are supporters: a badge beside their
    /// name and a line in the credits on /about. Owning any one of them outright
    /// counts. The Supporter Pack DLC today; were the app itself ever sold,
    /// its id would go beside the DLC's -- a delisted DLC stays owned, so
    /// the DLC's id stays too. Empty means nobody's standing changes.
    #[serde(default)]
    pub supporter_app_ids: Vec<u32>,
    /// Where the partner Web API is. Only a test changes it.
    #[serde(default = "default_steam_web_api_url")]
    pub web_api_url: String,
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
