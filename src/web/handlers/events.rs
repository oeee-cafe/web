//! `/events`: the one stream an open page holds (crate::live, live.jinja).
//!
//! Every event is one line of JSON under an event name, so the page reads it
//! with `JSON.parse` and no framing of its own. A signed-out page hears the
//! events any page may; a signed-in one hears its reader's own as well --
//! the bell, rendered here in the reader's language, since the event that
//! caused it carries only a number.
//!
//! The stream ends when the process begins to shut down. Axum's graceful
//! shutdown waits for every response to finish, and this one otherwise never
//! would; the page's `EventSource` reconnects by itself, to whichever colour
//! is serving by then.

use axum::extract::State;
use axum::http::{header, HeaderValue};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use futures_util::stream::{self, Stream};
use minijinja::context;
use serde_json::json;
use std::convert::Infallible;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::broadcast::error::RecvError;
use uuid::Uuid;

use crate::live::LiveEvent;
use crate::models::user::AuthSession;
use crate::web::handlers::ExtractFtlLang;
use crate::web::state::AppState;

/// Often enough that nothing between here and the browser -- Caddy, the
/// tunnel, Cloudflare's idle timeout of 100s -- takes the quiet for a dead
/// connection.
const KEEP_ALIVE: Duration = Duration::from_secs(20);

pub async fn events(
    auth_session: AuthSession,
    ExtractFtlLang(ftl_lang): ExtractFtlLang,
    State(state): State<AppState>,
) -> Response {
    let reader = auth_session.user.as_ref().map(|user| user.id);
    // Spread over a few seconds, so a deploy's worth of pages losing their
    // colour at once do not all come back in the same instant.
    let retry = Duration::from_millis(2000 + u64::from(Uuid::new_v4().as_bytes()[0]) * 16);

    let first = Event::default().retry(retry).comment("hello");
    let heard = heard(state, reader, ftl_lang);
    let stream = stream::once(async move { Ok::<_, Infallible>(first) });
    let stream = futures_util::StreamExt::chain(stream, heard);

    let mut response = Sse::new(stream)
        .keep_alive(KeepAlive::new().interval(KEEP_ALIVE))
        .into_response();
    // For anything in between that would otherwise hold the stream back to
    // fill a buffer.
    response
        .headers_mut()
        .insert("X-Accel-Buffering", HeaderValue::from_static("no"));
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("no-cache, no-transform"),
    );
    response
}

struct Listening {
    state: AppState,
    receiver: tokio::sync::broadcast::Receiver<Arc<LiveEvent>>,
    reader: Option<Uuid>,
    ftl_lang: String,
}

fn heard(
    state: AppState,
    reader: Option<Uuid>,
    ftl_lang: String,
) -> impl Stream<Item = Result<Event, Infallible>> {
    let receiver = state.live.subscribe();
    let listening = Listening {
        state,
        receiver,
        reader,
        ftl_lang,
    };
    stream::unfold(listening, |mut listening| async move {
        loop {
            let shutdown = listening.state.shutdown.clone();
            let received = tokio::select! {
                received = listening.receiver.recv() => received,
                _ = shutdown.signalled() => return None,
            };
            let live_event = match received {
                Ok(live_event) => live_event,
                // A page this far behind has missed what it missed; what
                // comes next is still worth sending.
                Err(RecvError::Lagged(skipped)) => {
                    tracing::debug!("a page's live events fell {skipped} behind");
                    continue;
                }
                Err(RecvError::Closed) => return None,
            };
            if let Some(event) = for_reader(&listening, &live_event) {
                return Some((Ok(event), listening));
            }
        }
    })
}

/// The event as this page should hear it, or `None` if it is someone else's.
fn for_reader(listening: &Listening, live_event: &LiveEvent) -> Option<Event> {
    let (name, data) = heard_as(
        &listening.state.env,
        listening.reader,
        &listening.ftl_lang,
        live_event,
    )?;
    Some(Event::default().event(name).data(data.to_string()))
}

/// The event's name and its JSON, as `reader` hears it.
fn heard_as(
    env: &crate::web::templates::Templates,
    reader: Option<Uuid>,
    ftl_lang: &str,
    live_event: &LiveEvent,
) -> Option<(&'static str, serde_json::Value)> {
    if let Some(recipient) = live_event.recipient() {
        if reader != Some(recipient) {
            return None;
        }
    }
    Some(match live_event {
        LiveEvent::Unread { count, .. } => {
            let bell = env.render_without_people(
                "nav_notifications.jinja",
                context! {
                    unread_notification_count => count,
                    ftl_lang => ftl_lang,
                },
            );
            match bell {
                Ok(bell) => ("unread", json!({ "count": count, "html": bell })),
                Err(e) => {
                    tracing::error!("the bell did not render for a live event: {e:#}");
                    return None;
                }
            }
        }
        LiveEvent::Notification {
            title,
            body,
            url,
            quoted,
            ..
        } => (
            "notification",
            json!({ "title": title, "body": body, "url": url, "quoted": quoted }),
        ),
        LiveEvent::Comments { post_id, by } => {
            ("comments", json!({ "post_id": post_id, "by": by }))
        }
        LiveEvent::Post {
            community_id,
            recent,
            by,
        } => (
            "post",
            json!({ "community_id": community_id, "recent": recent, "by": by }),
        ),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::web::handlers::test_support;

    #[test]
    fn a_reader_hears_their_own_bell_and_nobody_elses() {
        let env = crate::web::templates::Templates::new(test_support::env());
        let (me, them) = (Uuid::new_v4(), Uuid::new_v4());
        let mine = LiveEvent::Unread {
            user_id: me,
            count: 3,
        };
        let (name, data) = heard_as(&env, Some(me), "en", &mine).expect("mine is heard");
        assert_eq!(name, "unread");
        let html = data["html"].as_str().unwrap();
        assert!(html.contains("toolbar-bell-unread") && html.contains("(3)"));

        assert!(heard_as(&env, Some(them), "en", &mine).is_none());
        assert!(
            heard_as(&env, None, "en", &mine).is_none(),
            "nor a signed-out page"
        );
    }

    #[test]
    fn a_notification_says_whether_its_body_is_a_quote() {
        let env = crate::web::templates::Templates::new(test_support::env());
        let me = Uuid::new_v4();
        let event = LiveEvent::Notification {
            user_id: me,
            title: "oeee reacted to your post".into(),
            body: "oeee reacted with ❤️".into(),
            url: "/@artist/1".into(),
            quoted: false,
        };
        let (name, data) = heard_as(&env, Some(me), "en", &event).expect("heard");
        assert_eq!(name, "notification");
        assert_eq!(data["quoted"], false);
        // An event from a release that did not say is not a quote.
        let older: LiveEvent = serde_json::from_str(&format!(
            r#"{{"type":"notification","user_id":"{me}","title":"t","body":"b","url":"/"}}"#
        ))
        .unwrap();
        assert!(matches!(
            older,
            LiveEvent::Notification { quoted: false, .. }
        ));
    }

    #[test]
    fn anyone_hears_that_a_post_has_new_comments() {
        let env = crate::web::templates::Templates::new(test_support::env());
        let post_id = Uuid::new_v4();
        let event = LiveEvent::Comments { post_id, by: None };
        let (name, data) = heard_as(&env, None, "en", &event).expect("heard");
        assert_eq!(name, "comments");
        assert_eq!(data["post_id"], post_id.to_string());
    }
}
