use anyhow::Result;
use clap::{Parser, Subcommand, ValueEnum};
use oeee_cafe::{
    models::{
        actor::{backfill_actors_for_existing_communities, backfill_actors_for_existing_users},
        community::get_communities,
        device::get_user_devices,
        user::{
            find_user_by_id, find_user_by_login_name, update_password, update_user_role, UserRole,
        },
    },
    push::PushService,
    AppConfig,
};
use std::process::exit;
use tracing::Level;
use uuid::Uuid;

#[derive(Parser)]
#[command(version, about, long_about = None)]
#[command(propagate_version = true)]
struct Cli {
    #[arg(short, long)]
    config: Option<String>,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// List all communities
    ListCommunities,
    /// Reset a user's password
    ResetPassword { user_id: Uuid },
    /// Get user information by login name
    GetUser { login_name: String },
    /// Create actors for existing users that don't have them
    BackfillActors,
    /// Create actors for existing communities that don't have them
    BackfillCommunityActors,
    /// Send a test push notification to a user
    SendTestPush { login_name: String },
    /// Grant or revoke site-wide staff access (user, moderator, admin)
    SetRole { login_name: String, role: RoleArg },
    /// Check that the App Store will talk to us about purchases
    CheckAppStore,
}

#[derive(Clone, Copy, ValueEnum)]
enum RoleArg {
    User,
    Moderator,
    Admin,
}

impl From<RoleArg> for UserRole {
    fn from(role: RoleArg) -> Self {
        match role {
            RoleArg::User => UserRole::User,
            RoleArg::Moderator => UserRole::Moderator,
            RoleArg::Admin => UserRole::Admin,
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize rustls crypto provider
    let _ = rustls::crypto::ring::default_provider().install_default();

    // Initialize tracing/logging
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(Level::WARN)
        .finish();
    let _ = tracing::subscriber::set_global_default(subscriber);

    let cli = Cli::parse();

    let config_path = cli
        .config
        .ok_or_else(|| anyhow::anyhow!("Config file path required"))?;
    let cfg = AppConfig::new_from_file_and_env(&config_path).unwrap_or_else(|e| {
        eprintln!("error: {}", e);
        exit(1);
    });

    // Asking Apple about a key needs no database, and a check that first
    // opens a pool would fail for the wrong reason on a server whose
    // Postgres is down.
    if matches!(cli.command, Commands::CheckAppStore) {
        // What the config says, then whether Apple agrees: a wrong issuer or
        // a key Apple has revoked is a 401 here rather than a purchase that
        // quietly grants nothing.
        let Some(store) = cfg.app_store.as_ref() else {
            println!("no [app_store] table in the config: the app sells nothing");
            exit(1);
        };
        println!(
            "issuer {}, key {}, app {}",
            store.issuer_id, store.key_id, store.bundle_id
        );
        if store.supporter_products.is_empty() {
            println!("no packs: [[app_store.supporter_products]] is empty");
        }
        for pack in &store.supporter_products {
            println!("  {} -> {}", pack.year, pack.product_id);
        }
        match oeee_cafe::app_store::check(store).await {
            Ok(()) => println!("the App Store accepted the key"),
            Err(error) => {
                println!("the App Store did not: {error:#}");
                exit(1);
            }
        }
        return Ok(());
    }

    let db = match cfg.connect_database().await {
        Ok(db) => db,
        Err(e) => {
            eprintln!("error connecting to database: {}", e);
            exit(1);
        }
    };
    let mut tx = db.begin().await?;

    // You can check for the existence of subcommands, and if found use their
    // matches just as you would the top level cmd
    match &cli.command {
        // Answered above, before the database was opened.
        Commands::CheckAppStore => unreachable!(),
        Commands::ListCommunities => {
            let communities = get_communities(&mut tx).await?;
            for community in communities {
                println!("Name: {}", community.name);
                println!("Description: {}", community.description);
                println!("Visibility: {:?}", community.visibility);
                println!("URL: {}{}", cfg.base_url, community.get_url());
                println!();
            }
        }
        Commands::GetUser { login_name } => {
            let user = find_user_by_login_name(&mut tx, login_name).await;
            match user {
                Ok(Some(user)) => {
                    print_user_info(user);
                }
                Ok(None) => {
                    println!("User not found");
                }
                Err(e) => {
                    eprintln!("error: {}", e);
                    exit(1);
                }
            }
        }
        Commands::ResetPassword { user_id } => {
            let user = find_user_by_id(&mut tx, *user_id).await;
            match user {
                Ok(Some(user)) => {
                    print_user_info(user);
                    println!();
                    let password = rpassword::prompt_password("New password: ")
                        .map_err(|e| anyhow::anyhow!("Failed to read password: {}", e))?;
                    let password2 = rpassword::prompt_password("New password (again): ")
                        .map_err(|e| anyhow::anyhow!("Failed to read password: {}", e))?;

                    if password != password2 {
                        eprintln!("Passwords do not match");
                        exit(1);
                    }

                    update_password(&mut tx, *user_id, password).await?;
                    tx.commit().await?;

                    println!("Password updated");
                }
                Ok(None) => {
                    println!("User not found");
                }
                Err(e) => {
                    eprintln!("error: {}", e);
                    exit(1);
                }
            }
        }
        Commands::BackfillActors => {
            println!("Starting actor backfill for existing users...");
            let created_count = backfill_actors_for_existing_users(&mut tx, &cfg).await?;
            tx.commit().await?;
            println!("✅ Created {} actors for existing users", created_count);
        }
        Commands::BackfillCommunityActors => {
            println!("Starting actor backfill for existing communities...");
            let created_count = backfill_actors_for_existing_communities(&mut tx, &cfg).await?;
            tx.commit().await?;
            println!(
                "✅ Created {} actors for existing communities",
                created_count
            );
        }
        Commands::SetRole { login_name, role } => {
            let user = find_user_by_login_name(&mut tx, login_name).await?;
            match user {
                Some(user) => {
                    let previous = user.role;
                    let updated = update_user_role(&mut tx, user.id, (*role).into()).await?;
                    tx.commit().await?;
                    println!(
                        "✅ @{}: {:?} -> {:?}",
                        updated.login_name, previous, updated.role
                    );
                }
                None => {
                    eprintln!("User '{}' not found", login_name);
                    exit(1);
                }
            }
        }
        Commands::SendTestPush { login_name } => {
            println!("Looking up user '{}'...", login_name);
            let user = find_user_by_login_name(&mut tx, login_name).await?;

            match user {
                Some(user) => {
                    println!("Found user: {} ({})", user.display_name, user.id);

                    // Get devices for this user
                    let devices = get_user_devices(&mut tx, user.id).await?;
                    println!("User has {} device(s) registered:", devices.len());
                    for token in &devices {
                        println!(
                            "  - {:?}: {}...",
                            token.platform,
                            &token.device_token[..token.device_token.len().min(20)]
                        );
                    }

                    if devices.is_empty() {
                        println!("❌ No devices registered for this user");
                        exit(1);
                    }

                    // Initialize push service
                    println!("\nInitializing push service...");
                    let db_pool = cfg.connect_database().await?;
                    let push_service = PushService::new(&cfg, db_pool).await?;

                    // Send test notification
                    println!("Sending test push notification...");
                    match push_service
                        .send_notification_to_user(
                            user.id,
                            "Test Push Notification",
                            "This is a test notification from the CLI",
                            Some(1),
                            None,
                        )
                        .await
                    {
                        Ok(_) => {
                            println!("✅ Test notification sent successfully!");
                        }
                        Err(e) => {
                            println!("❌ Failed to send test notification: {:?}", e);
                            exit(1);
                        }
                    }
                }
                None => {
                    println!("❌ User not found");
                    exit(1);
                }
            }
        }
    }

    Ok(())
}

fn print_user_info(user: oeee_cafe::models::user::User) {
    println!("ID: {}", user.id);
    println!("Login Name: {}", user.login_name);
    println!("Display Name: {}", user.display_name);
    println!("Email: {:?}", user.email);
    println!("Preferred Language: {:?}", user.preferred_language);
    println!("Signup Date: {:?}", user.created_at);
}
