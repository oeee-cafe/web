//! `/jump`: the quick switcher's list (jump.jinja), as a fragment.
//!
//! Places, not drawings: communities, tags, the site's own pages, and
//! people -- a handle typed exactly, or the start of a nearby person's
//! name, as on /search (models::people) -- each a link, in one list the reader
//! moves through with the arrow keys. Nothing typed is the reader's own communities and the pages; with
//! something typed, what matches it, the reader's communities first, and a
//! last row that searches drawings for it -- the one thing this is not for.
//! An address of the site's own, pasted whole, is the one thing it names
//! instead, if the reader may open it (link.rs).

mod link;

use axum::extract::{Query, State};
use axum::http::HeaderMap;
use axum::response::{Html, IntoResponse};
use minijinja::context;
use serde::{Deserialize, Serialize};
use sqlx::{Postgres, Transaction};
use uuid::Uuid;

use crate::app_error::AppError;
use crate::models::community::choice::{choose_communities, ChoiceQuery, ChoiceRule};
use crate::models::tag::search_tags;
use crate::models::user::AuthSession;
use crate::web::handlers::search::search_people;
use crate::web::i18n::ExtractFtlLang;
use crate::web::rate_limit;
use crate::web::state::AppState;
use minijinja::value::Serde;
use tower_sessions::Session;

/// Rows of each kind: enough to find a place by a few letters of its name,
/// few enough that the list fits a window without scrolling.
const PER_KIND: i64 = 5;

/// Longer than any name or handle, so a pasted paragraph is not a query.
const MAX_QUERY: usize = 100;

/// Longer than any address of ours, query string and all, which is read
/// before the query is cut to `MAX_QUERY`.
const MAX_LINK: usize = 2000;

#[derive(Deserialize)]
pub struct JumpQuery {
    #[serde(default)]
    q: String,
}

#[derive(Serialize)]
struct JumpCommunity {
    name: String,
    slug: String,
    is_member: bool,
}

pub async fn jump(
    auth_session: AuthSession,
    session: Session,
    headers: HeaderMap,
    ExtractFtlLang(ftl_lang): ExtractFtlLang,
    State(state): State<AppState>,
    Query(query): Query<JumpQuery>,
) -> Result<impl IntoResponse, AppError> {
    let pasted = query.q.trim();
    let permalink = if pasted.len() <= MAX_LINK {
        link::parse(pasted, &state.config.domain)
    } else {
        None
    };
    let mut tx = state.db_pool.begin().await?;
    if let Some(permalink) = permalink {
        let reader = link::Reader {
            user: auth_session.user.as_ref(),
            session: &session,
            ip: rate_limit::client_ip(&headers),
        };
        let found = link::resolve(&mut tx, &state.db_pool, permalink, &reader).await?;
        tx.commit().await?;
        let rendered = state
            .render(
                "jump_results.jinja",
                context! {
                    is_link => true,
                    link => Serde(found),
                    current_user => Serde(auth_session.user),
                    ftl_lang,
                },
            )
            .await?;
        return Ok(Html(rendered));
    }

    let q: String = pasted.chars().take(MAX_QUERY).collect();
    let reader = auth_session.user.as_ref().map(|user| user.id);
    let communities = communities(&mut tx, &q, reader).await?;
    let (people, tags) = if q.is_empty() {
        (Vec::new(), Vec::new())
    } else {
        (
            search_people(&mut tx, reader, &q, PER_KIND).await?,
            search_tags(&mut tx, &q, PER_KIND).await?,
        )
    };
    tx.commit().await?;

    let rendered = state
        .render(
            "jump_results.jinja",
            context! {
                q,
                current_user => Serde(auth_session.user),
                communities => Serde(communities),
                people => Serde(people),
                tags => Serde(tags),
                ftl_lang,
            },
        )
        .await?;
    Ok(Html(rendered))
}

/// The reader's own communities, and with something typed, public ones too.
/// Never a private community the reader is not in, not even its name.
async fn communities(
    tx: &mut Transaction<'_, Postgres>,
    q: &str,
    reader: Option<Uuid>,
) -> Result<Vec<JumpCommunity>, AppError> {
    // Nothing typed lists the reader's communities, so it can be longer.
    let limit = if q.is_empty() { PER_KIND * 2 } else { PER_KIND };
    let found =
        choose_communities(tx, &ChoiceQuery::new(ChoiceRule::Jump, reader, q, limit)).await?;
    Ok(found
        .into_iter()
        .take(limit as usize)
        .map(|community| JumpCommunity {
            name: community.name,
            slug: community.slug,
            is_member: community.is_member,
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::tag::Tag;
    use crate::web::handlers::test_support;
    use serde_json::json;

    fn render(q: &str, signed_in: bool, communities: Vec<JumpCommunity>) -> String {
        let tags: Vec<Tag> = Vec::new();
        test_support::env()
            .get_template("jump_results.jinja")
            .expect("jump_results.jinja loads")
            .render(context! {
                q,
                current_user => Serde(if signed_in { json!({"id": "u", "login_name": "tandemaus"}) } else { json!(null) }),
                communities => Serde(communities),
                people => Serde(vec![crate::web::handlers::search::SearchPersonRow {
                    login_name: "neo".into(),
                    display_name: "Neo <b>".into(),
                    banner_image_filename: None,
                }]),
                tags => Serde(tags),
                ftl_lang => "en",
            })
            .unwrap_or_else(|e| panic!("jump_results.jinja renders: {e:#}"))
            // Autoescape writes a slash in a value as an entity, which the
            // browser reads back as a slash.
            .replace("&#x2f;", "/")
    }

    fn club() -> JumpCommunity {
        JumpCommunity {
            name: "drawing club".into(),
            slug: "club".into(),
            is_member: true,
        }
    }

    #[test]
    fn every_row_is_a_link_and_a_name_is_text() {
        let rendered = render("", true, vec![club()]);
        assert!(rendered.contains(r#"href="/@club""#));
        assert!(rendered.contains(r#"href="/@neo""#));
        assert!(
            rendered.contains("Neo &lt;b&gt;"),
            "a name is text, not markup"
        );
        assert!(
            rendered.contains(r#"href="/@tandemaus""#),
            "a signed-in reader's pages include their profile"
        );
        assert!(rendered.contains(r#"data-command="new-drawing""#));
        assert!(
            !rendered.contains("/search?q="),
            "nothing typed, nothing to search for"
        );
    }

    #[test]
    fn typing_narrows_the_pages_and_offers_to_search_drawings() {
        let rendered = render("notif", true, Vec::new());
        assert!(
            rendered.contains(r#"href="/notifications""#),
            "got: {rendered}"
        );
        assert!(!rendered.contains(r#"href="/account""#), "got: {rendered}");
        assert!(rendered.contains(r#"href="/search?q=notif""#));
    }

    #[test]
    fn a_signed_out_reader_is_offered_no_pages_of_their_own() {
        let rendered = render("", false, Vec::new());
        assert!(!rendered.contains("/notifications"));
        assert!(!rendered.contains("new-drawing"));
        assert!(rendered.contains(r#"href="/communities""#));
    }

    fn render_link(link: serde_json::Value) -> String {
        test_support::env()
            .get_template("jump_results.jinja")
            .expect("jump_results.jinja loads")
            .render(context! {
                is_link => true,
                link => Serde(link),
                current_user => Serde(json!(null)),
                ftl_lang => "en",
            })
            .unwrap_or_else(|e| panic!("jump_results.jinja renders: {e:#}"))
            .replace("&#x2f;", "/")
    }

    #[test]
    fn a_pasted_link_is_the_one_row_it_names() {
        let rendered = render_link(json!({
            "kind": "post",
            "href": "/@club/0199a0e2-7c1d-7b3a-9f00-0123456789ab",
            "name": null,
            "login_name": "neo",
            "detail": "drawing club",
        }));
        assert!(rendered.contains(r#"href="/@club/0199a0e2-7c1d-7b3a-9f00-0123456789ab""#));
        assert!(rendered.contains("post-untitled"), "got: {rendered}");
        assert!(rendered.contains("@neo") && rendered.contains("drawing club"));
        assert_eq!(rendered.matches("jump-item").count(), 1, "got: {rendered}");
        assert!(
            !rendered.contains("/search?q="),
            "a link is not searched for"
        );
    }

    #[test]
    fn a_room_without_a_community_says_it_is_one() {
        let rendered = render_link(json!({
            "kind": "room",
            "href": "/collaborate/0199a0e2-7c1d-7b3a-9f00-0123456789ab",
            "name": "Doodle <i>",
            "login_name": null,
            "detail": null,
        }));
        assert!(rendered.contains("Doodle &lt;i&gt;"));
        assert!(rendered.contains(">collaborate<"), "got: {rendered}");
    }

    #[test]
    fn a_link_the_reader_may_not_open_names_nothing() {
        let rendered = render_link(json!(null));
        assert!(!rendered.contains("jump-item"), "got: {rendered}");
        assert!(rendered.contains("jump-empty"));
    }
}
