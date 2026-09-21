use fluent::bundle::FluentBundle;
use fluent::{FluentArgs, FluentValue};
use minijinja::{path_loader, AutoEscape, Environment, State};
use oeee_cafe::locale::LOCALES;
use oeee_cafe::push::PushService;
use oeee_cafe::web::app::App;
use oeee_cafe::web::handlers::collaborate::redis_state::RedisStateManager;
use oeee_cafe::web::handlers::collaborate::room_fanout::RoomFanout;
use oeee_cafe::web::state::{AppState, Shutdown};
use oeee_cafe::AppConfig;
use std::collections::HashMap;
use std::env::args;
use std::path::PathBuf;
use std::process::exit;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tracing_subscriber::EnvFilter;

fn main() {
    // Initialize rustls crypto provider for push notifications
    let _ = rustls::crypto::ring::default_provider().install_default();

    let args: Vec<String> = args().collect();
    if args.len() < 2 {
        println!("usage: {} CFG", args.first().unwrap_or(&"oeee".to_string()));
        exit(1);
    }

    // Config is loaded before the runtime starts because both Sentry and the
    // tracing subscriber are configured from it, and Sentry's guard has to
    // outlive everything it might report on.
    let cfg: AppConfig = AppConfig::new_from_file_and_env(args[1].as_ref()).unwrap_or_else(|e| {
        eprintln!("error: {}", e);
        exit(1);
    });

    // RUST_LOG wins when set, so an operator can turn up detail on a running
    // container without editing config; otherwise fall back to `log_level`.
    let subscriber = tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(&cfg.log_level)),
        )
        .finish();
    let _ = tracing::subscriber::set_global_default(subscriber);

    let _guard = cfg
        .sentry_dsn
        .as_deref()
        .filter(|dsn| !dsn.is_empty())
        .map(|dsn| {
            sentry::init((
                dsn,
                sentry::ClientOptions {
                    release: sentry::release_name!(),
                    environment: Some(cfg.env.clone().into()),
                    // Capture user IPs and potentially sensitive headers when using HTTP server integrations
                    // see https://docs.sentry.io/platforms/rust/data-management/data-collected for more info
                    send_default_pii: true,
                    ..Default::default()
                },
            ))
        });
    if _guard.is_none() {
        tracing::warn!("sentry_dsn not configured, error reporting is disabled");
    }

    // Said out loud at boot because the failure mode is silence: a deployment
    // with nowhere private to keep recordings keeps none, and there is nothing
    // else about a running server that would tell you which it is.
    match oeee_cafe::web::handlers::collaborate::archive::bucket(&cfg) {
        Some(bucket) => tracing::info!(
            "Collaborative sessions are recorded to bucket {}",
            bucket
        ),
        None => tracing::warn!(
            "archive_s3_bucket not configured, collaborative sessions are not recorded"
        ),
    }

    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("Failed to build tokio runtime")
        .block_on(async {
            tracing::debug!("config: {:?}", cfg);

            let template_path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("templates");
            let mut env = Environment::new();
            // minijinja picks escaping from the file extension by default, and
            // only .html/.htm/.xml get it — every template here is .jinja, so
            // without this nothing was ever escaped and any user-supplied `<`
            // or `"` was injected raw into the page. Force HTML escaping for all
            // templates; the places that intentionally emit markup (markdown
            // output, *_html columns, asset URLs) already say `| safe`.
            env.set_auto_escape_callback(|_| AutoEscape::Html);
            minijinja_contrib::add_to_environment(&mut env);

            // In production the asset URL has to be *stable*, or browsers and
            // the CDN re-fetch every file on every page view — which is what a
            // fresh `now()` per render used to do. Versioning by build id means
            // assets cache indefinitely and a deploy invalidates them all at
            // once. In development we want the opposite: a new value per render,
            // so editing style.css shows up without restarting the server.
            let asset_version: Option<String> =
                (cfg.env == "production").then(|| oeee_cafe::build_info::build_id().to_string());
            env.add_filter("cachebuster", move |value: String| match &asset_version {
                Some(version) => format!("{}?{}", value, version),
                None => {
                    let timestamp = SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .expect("System time is before UNIX_EPOCH")
                        .as_secs();
                    format!("{}?{}", value, timestamp)
                }
            });

            fn markdown_to_html(value: String) -> String {
                oeee_cafe::markdown_utils::process_markdown_content(&value)
            }
            env.add_filter("markdown", markdown_to_html);
            env.add_filter("ago", oeee_cafe::relative_time::ago_filter);

            fn ftl_get_message(state: &State, message_id: String) -> String {
                // Get the current language from template context
                let lang = match state.lookup("ftl_lang") {
                    Some(lang_val) => lang_val.as_str().unwrap_or("ko").to_string(),
                    None => "ko".to_string(),
                };

                // Get the appropriate Fluent resource
                let ftl = LOCALES
                    .get(&lang)
                    .unwrap_or_else(|| LOCALES.get("ko").expect("Korean locale must exist"));

                // Create bundle
                let mut bundle = FluentBundle::new_concurrent(vec![lang
                    .parse()
                    .expect("Language string should be valid")]);
                bundle.add_resource(ftl).expect("Failed to add a resource.");

                // Get and format the message
                match bundle.get_message(&message_id) {
                    Some(message) => match message.value() {
                        Some(pattern) => bundle
                            .format_pattern(pattern, None, &mut vec![])
                            .to_string(),
                        None => message_id,
                    },
                    None => message_id,
                }
            }
            env.add_function("ftl_get_message", ftl_get_message);

            fn ftl_format_pattern(
                state: &State,
                message_id: String,
                params: minijinja::Value,
            ) -> Result<String, minijinja::Error> {
                // Get the current language from template context
                let lang = match state.lookup("ftl_lang") {
                    Some(lang_val) => lang_val.as_str().unwrap_or("ko").to_string(),
                    None => "ko".to_string(),
                };

                // Get the appropriate Fluent resource
                let ftl = LOCALES
                    .get(&lang)
                    .unwrap_or_else(|| LOCALES.get("ko").expect("Korean locale must exist"));

                // Create bundle
                let mut bundle = FluentBundle::new_concurrent(vec![lang
                    .parse()
                    .expect("Language string should be valid")]);
                bundle.add_resource(ftl).expect("Failed to add a resource.");

                // Convert minijinja values to FluentArgs by deserializing to HashMap
                let mut args = FluentArgs::new();

                // Deserialize the params Value to a HashMap
                if let Ok(map) = serde_json::from_value::<HashMap<String, serde_json::Value>>(
                    serde_json::to_value(&params).map_err(|e| {
                        minijinja::Error::new(
                            minijinja::ErrorKind::InvalidOperation,
                            format!("Failed to serialize params: {}", e),
                        )
                    })?,
                ) {
                    for (key, value) in map {
                        let fluent_value = match value {
                            serde_json::Value::String(s) => FluentValue::from(s),
                            serde_json::Value::Number(n) => {
                                if let Some(i) = n.as_i64() {
                                    FluentValue::from(i)
                                } else if let Some(f) = n.as_f64() {
                                    FluentValue::from(f)
                                } else {
                                    FluentValue::from(n.to_string())
                                }
                            }
                            _ => FluentValue::from(value.to_string()),
                        };
                        args.set(key, fluent_value);
                    }
                }

                // Get and format the message
                match bundle.get_message(&message_id) {
                    Some(message) => match message.value() {
                        Some(pattern) => Ok(bundle
                            .format_pattern(pattern, Some(&args), &mut vec![])
                            .to_string()),
                        None => Ok(message_id),
                    },
                    None => Ok(message_id),
                }
            }
            env.add_function("ftl_format_pattern", ftl_format_pattern);

            // Add global variables
            env.add_global("r2_public_endpoint_url", cfg.r2_public_endpoint_url.clone());
            // Needed by base.jinja to build absolute og:url/og:image values on
            // every page. Handlers that pass their own `base_url` still win,
            // since context takes precedence over globals.
            env.add_global("base_url", cfg.base_url.trim_end_matches('/').to_string());

            env.set_loader(path_loader(&template_path));

            let db_pool = cfg.connect_database().await.unwrap_or_else(|e| {
                eprintln!("error connecting to database: {}", e);
                exit(1);
            });

            let redis_pool = cfg.connect_redis().await.unwrap_or_else(|e| {
                eprintln!("error connecting to redis: {}", e);
                exit(1);
            });

            let redis_state = RedisStateManager::new(redis_pool.clone());

            // Push is a nice-to-have: an expired APNs key or unreadable FCM
            // service account must not stop the site from serving pages.
            let push_service = match PushService::new(&cfg, db_pool.clone()).await {
                Ok(service) => service,
                Err(e) => {
                    tracing::error!(
                        "Failed to initialize push service, continuing without push \
                         notifications: {:?}",
                        e
                    );
                    PushService::disabled(db_pool.clone())
                }
            };

            let state = AppState {
                config: cfg.clone(),
                env,
                db_pool,
                redis_pool,
                redis_state,
                room_fanout: RoomFanout::new(&cfg.redis_url),
                push_service: Arc::new(push_service),
                shutdown: Shutdown::new(),
            };

            App::new(state)
                .await
                .expect("Failed to create app")
                .serve()
                .await
                .expect("Failed to serve app")
        });
}
