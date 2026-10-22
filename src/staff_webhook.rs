//! A line in the staff's Discord channel when something happens that staff
//! would want to know about: a Supporter Pack bought or refunded, a report,
//! a room opened or lost, a signup.
//!
//! Off unless `staff_webhook_url` is set. Nothing here can hold up or fail
//! what it is told about: [`notify`] puts the line on a queue and returns,
//! one task posts the queue in order, and a line that would not fit on the
//! queue, or that Discord would not take, is dropped with a warning. A
//! purchase that went through is worth more than the message saying so.
//!
//! Some lines are sent from inside a database transaction, where the change
//! is noticed (`models::supporter`), so a transaction that then fails to
//! commit has announced something that did not happen. That is rare and is
//! a line in a channel, not a record anybody acts on without the admin page
//! the line links to.
//!
//! What goes in a line goes into a Discord channel: never an email address,
//! a purchase token or transaction, or a push token. Text people typed --
//! names, titles, report reasons -- is escaped, mentions are switched off
//! for the whole message, and links in it are not unfurled. A guest's name
//! is whatever they typed and may be an account's, so it is written as a
//! guest's and never as a handle.

use std::sync::OnceLock;
use std::time::Duration;

use serde_json::json;
use tokio::sync::mpsc;
use uuid::Uuid;

use crate::config::AppConfig;

/// Lines waiting to be posted. Past this many something is wrong with
/// Discord or with whatever is producing them, and the rest are dropped.
const QUEUE: usize = 256;
/// Discord's limit on a message's content.
const MAX_CONTENT: usize = 2000;
/// How much of something somebody typed goes into a line.
const MAX_QUOTE: usize = 300;
/// How much of a report's reason does: it is what staff act on, and the
/// channel is the only place a report goes.
const MAX_REASON: usize = 1500;
/// How many times one line is tried, Discord's rate limit included.
const ATTEMPTS: u32 = 3;
/// SUPPRESS_EMBEDS: a link in a report reason is not unfurled in the channel.
const SUPPRESS_EMBEDS: u32 = 1 << 2;

struct Notifier {
    base_url: String,
    queue: mpsc::Sender<String>,
}

static NOTIFIER: OnceLock<Notifier> = OnceLock::new();

/// Starts posting, if the config names a webhook. Called once, at boot,
/// inside the runtime; anywhere it has not been, [`notify`] does nothing.
pub fn start(config: &AppConfig) {
    let Some(url) = config
        .staff_webhook_url
        .as_ref()
        .map(|url| url.expose().to_string())
        .filter(|url| !url.trim().is_empty())
    else {
        tracing::info!("staff_webhook_url not configured, staff are not told of events");
        return;
    };
    let (queue, lines) = mpsc::channel(QUEUE);
    let notifier = Notifier {
        base_url: config.base_url.trim_end_matches('/').to_string(),
        queue,
    };
    if NOTIFIER.set(notifier).is_err() {
        return;
    }
    tokio::spawn(post_all(url, lines));
    tracing::info!("Staff are told of events through a Discord webhook");
}

/// Posts each line in turn. A line Discord would not take is tried again
/// after the wait it asks for, and dropped after [`ATTEMPTS`].
async fn post_all(url: String, mut lines: mpsc::Receiver<String>) {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .expect("a reqwest client builds");
    while let Some(line) = lines.recv().await {
        let body = body(&line);
        for attempt in 1..=ATTEMPTS {
            // Never the error's URL, which is the webhook's secret.
            let wait = match client.post(&url).json(&body).send().await {
                Ok(response) if response.status().is_success() => break,
                Ok(response) if response.status() == reqwest::StatusCode::TOO_MANY_REQUESTS => {
                    let retry_after = response
                        .json::<serde_json::Value>()
                        .await
                        .ok()
                        .and_then(|body| body.get("retry_after")?.as_f64())
                        .unwrap_or(1.0);
                    Duration::from_secs_f64(retry_after.clamp(0.0, 60.0))
                }
                Ok(response) => {
                    tracing::warn!("The staff webhook answered {}", response.status());
                    if response.status().is_client_error() {
                        break;
                    }
                    Duration::from_secs(2)
                }
                Err(error) => {
                    tracing::warn!(
                        "The staff webhook could not be reached: {}",
                        error.without_url()
                    );
                    Duration::from_secs(2)
                }
            };
            if attempt == ATTEMPTS {
                tracing::warn!("Gave up telling staff of an event");
                break;
            }
            tokio::time::sleep(wait).await;
        }
    }
}

/// The webhook's body for one line: no mentions, no unfurled links.
fn body(line: &str) -> serde_json::Value {
    json!({
        "content": truncate(line, MAX_CONTENT),
        "allowed_mentions": { "parse": [] },
        "flags": SUPPRESS_EMBEDS,
    })
}

/// Whether there is a webhook to tell staff through.
pub fn is_on() -> bool {
    NOTIFIER.get().is_some()
}

/// Tells staff of `event`, if there is a webhook. Never waits and never
/// fails.
pub fn notify(event: Event) {
    let Some(notifier) = NOTIFIER.get() else {
        return;
    };
    let line = event.line(&notifier.base_url);
    if notifier.queue.try_send(line).is_err() {
        tracing::warn!("The staff webhook's queue is full; an event was dropped");
    }
}

/// Somebody, as a line names them.
#[derive(Clone, Debug)]
pub enum Who {
    /// An account, by its login name.
    Account(String),
    /// A guest in a room: a name they typed, never a handle.
    Guest(String),
}

impl Who {
    fn render(&self, base_url: &str) -> String {
        match self {
            Who::Account(login_name) => account(base_url, login_name),
            Who::Guest(name) => format!("guest “{}”", quote(name)),
        }
    }
}

/// Something staff are told of.
#[derive(Clone, Debug)]
pub enum Event {
    /// A Supporter Pack owned that was not before: bought, or bought again
    /// after a refund.
    SupporterPurchased {
        login_name: String,
        store: &'static str,
        product: String,
        year: i32,
    },
    /// A Supporter Pack that was owned and is not any more: refunded or
    /// revoked by the store.
    SupporterRevoked {
        login_name: String,
        store: &'static str,
        product: String,
        year: i32,
    },
    PostReported {
        reporter: String,
        author: String,
        post_id: Uuid,
        title: Option<String>,
        reason: String,
    },
    ProfileReported {
        reporter: String,
        reported: String,
        reason: String,
    },
    RoomOpened {
        room: Uuid,
        owner: Who,
        title: Option<String>,
        width: i32,
        height: i32,
        community: Option<String>,
    },
    /// A participant's canvas went wrong and they sent the report for it.
    SyncReport { room: Uuid, reporter: Who },
    /// A paused room nobody came back to, ended with its drawing unsaved.
    RoomAbandoned { room: Uuid },
    Signup {
        login_name: String,
        /// The sign-in the account was made with, or `None` for a password.
        provider: Option<&'static str>,
    },
    /// Staff marked a post explicit, or took the mark off.
    PostFlagged {
        post_id: Uuid,
        by: String,
        explicit: bool,
    },
    /// Staff set or took off an account's address from its admin page. The
    /// address itself stays out of the channel.
    EmailSet {
        login_name: String,
        by: String,
        removed: bool,
        verified: bool,
    },
}

impl Event {
    fn line(&self, base: &str) -> String {
        match self {
            Event::SupporterPurchased {
                login_name,
                store,
                product,
                year,
            } => format!(
                "💖 {} bought the {year} Supporter Pack on {} ({})",
                account(base, login_name),
                store_name(store),
                quote(product),
            ),
            Event::SupporterRevoked {
                login_name,
                store,
                product,
                year,
            } => format!(
                "💔 {}'s {year} Supporter Pack on {} was refunded or revoked ({})",
                account(base, login_name),
                store_name(store),
                quote(product),
            ),
            Event::PostReported {
                reporter,
                author,
                post_id,
                title,
                reason,
            } => format!(
                "🚩 {} reported [{}]({base}/admin/posts/{post_id}) by {}: {}",
                account(base, reporter),
                title
                    .as_deref()
                    .filter(|title| !title.trim().is_empty())
                    .map(quote)
                    .unwrap_or_else(|| "a post".to_string()),
                account(base, author),
                quote_at_most(reason, MAX_REASON),
            ),
            Event::ProfileReported {
                reporter,
                reported,
                reason,
            } => format!(
                "🚩 {} reported the profile of {}: {}",
                account(base, reporter),
                account(base, reported),
                quote_at_most(reason, MAX_REASON),
            ),
            Event::RoomOpened {
                room,
                owner,
                title,
                width,
                height,
                community,
            } => format!(
                "🎨 {} opened [{}]({base}/admin/collaborative-sessions/{room}) ({width}×{height}{})",
                owner.render(base),
                title
                    .as_deref()
                    .filter(|title| !title.trim().is_empty())
                    .map(quote)
                    .unwrap_or_else(|| "a room".to_string()),
                community
                    .as_deref()
                    .map(|name| format!(", in {}", quote(name)))
                    .unwrap_or_default(),
            ),
            Event::SyncReport { room, reporter } => format!(
                "⚠️ {} sent a synchronisation report from [a room]({base}/admin/collaborative-sessions/{room})",
                reporter.render(base),
            ),
            Event::RoomAbandoned { room } => format!(
                "🕳️ [A room]({base}/admin/collaborative-sessions/{room}) was paused a week with nobody back, and ended unsaved",
            ),
            Event::Signup {
                login_name,
                provider,
            } => format!(
                "👋 {} signed up{}",
                account(base, login_name),
                provider
                    .map(|provider| format!(" with {provider}"))
                    .unwrap_or_default(),
            ),
            Event::PostFlagged {
                post_id,
                by,
                explicit,
            } => format!(
                "🔞 {} {} [a post]({base}/admin/posts/{post_id})",
                account(base, by),
                if *explicit {
                    "marked explicit"
                } else {
                    "took the explicit mark off"
                },
            ),
            Event::EmailSet {
                login_name,
                by,
                removed,
                verified,
            } => {
                let whose = account(base, login_name);
                format!(
                    "✉️ {} {}",
                    account(base, by),
                    match (removed, verified) {
                        (true, _) => format!("took {whose}'s email off"),
                        (false, true) => format!("set {whose}'s email, verified"),
                        (false, false) => format!("set {whose}'s email, unverified"),
                    },
                )
            }
        }
    }
}

/// An account, linked to its admin page. A login name is letters, digits
/// and underscores, but it is escaped all the same.
fn account(base: &str, login_name: &str) -> String {
    format!(
        "[@{}]({base}/admin/users/{})",
        quote(login_name),
        urlencoding::encode(login_name)
    )
}

fn store_name(store: &str) -> &str {
    match store {
        "apple" => "the App Store",
        "google" => "Google Play",
        "microsoft" => "the Microsoft Store",
        "steam" => "Steam",
        other => other,
    }
}

/// Something somebody typed, made safe to put in a line: on one line,
/// shortened, with Discord's markdown escaped.
fn quote(text: &str) -> String {
    quote_at_most(text, MAX_QUOTE)
}

fn quote_at_most(text: &str, max: usize) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let short = truncate(&flat, max);
    let mut escaped = String::with_capacity(short.len());
    for c in short.chars() {
        if matches!(
            c,
            '\\' | '*'
                | '_'
                | '~'
                | '`'
                | '|'
                | '>'
                | '#'
                | '-'
                | '['
                | ']'
                | '('
                | ')'
                | '<'
                | '@'
                | ':'
        ) {
            escaped.push('\\');
        }
        escaped.push(c);
    }
    escaped
}

/// At most `max` characters, with an ellipsis where it was cut.
fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let mut cut: String = text.chars().take(max - 1).collect();
    cut.push('…');
    cut
}

#[cfg(test)]
mod tests {
    use super::*;

    const BASE: &str = "https://oeee.cafe";

    #[test]
    fn a_guest_is_never_written_as_a_handle() {
        let line = Event::RoomOpened {
            room: Uuid::nil(),
            owner: Who::Guest("@admin".to_string()),
            title: None,
            width: 300,
            height: 300,
            community: None,
        }
        .line(BASE);
        assert!(line.contains("guest “\\@admin”"), "{line}");
        assert!(!line.contains("/admin/users/"), "{line}");
        assert!(line.contains(
            "https://oeee.cafe/admin/collaborative-sessions/00000000-0000-0000-0000-000000000000"
        ));
    }

    #[test]
    fn what_somebody_typed_cannot_break_out_of_its_place() {
        let line = Event::PostReported {
            reporter: "oi".to_string(),
            author: "cucumber".to_string(),
            post_id: Uuid::nil(),
            title: Some("[click](https://evil.example) @everyone".to_string()),
            reason: "line one\n# heading\n".repeat(200),
        }
        .line(BASE);
        assert!(line.contains("\\[click\\]\\(https\\:"), "{line}");
        assert!(line.contains("\\@everyone"), "{line}");
        assert!(!line.contains('\n'), "{line}");
        assert!(line.contains('…'));
        assert!(line.contains("[@oi](https://oeee.cafe/admin/users/oi)"));
    }

    #[test]
    fn a_purchase_names_the_pack_and_not_the_token() {
        let line = Event::SupporterPurchased {
            login_name: "oi".to_string(),
            store: "google",
            product: "supporter_2026".to_string(),
            year: 2026,
        }
        .line(BASE);
        assert_eq!(
            line,
            "💖 [@oi](https://oeee.cafe/admin/users/oi) bought the 2026 Supporter Pack on Google Play (supporter\\_2026)"
        );
    }

    #[test]
    fn a_set_email_names_both_accounts_and_not_the_address() {
        let line = Event::EmailSet {
            login_name: "fan".to_string(),
            by: "oi".to_string(),
            removed: false,
            verified: true,
        }
        .line(BASE);
        assert_eq!(
            line,
            "✉️ [@oi](https://oeee.cafe/admin/users/oi) set [@fan](https://oeee.cafe/admin/users/fan)'s email, verified"
        );
    }

    #[test]
    fn the_body_turns_mentions_and_embeds_off() {
        let body = body(&"x".repeat(5000));
        assert_eq!(body["allowed_mentions"]["parse"], json!([]));
        assert_eq!(body["flags"], json!(4));
        assert_eq!(body["content"].as_str().unwrap().chars().count(), 2000);
    }
}
