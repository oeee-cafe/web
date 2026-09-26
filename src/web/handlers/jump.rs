//! `/jump`: the quick switcher's list (jump.jinja), as a fragment.
//!
//! Places, not drawings: communities, tags, the site's own pages, and the
//! person whose handle was typed exactly -- people are not found by part of
//! a name, as on /search (search.rs) -- each a link, in one list the reader
//! moves through with the arrow keys. Nothing typed is the reader's own communities and the pages; with
//! something typed, what matches it, the reader's communities first, and a
//! last row that searches drawings for it -- the one thing this is not for.

use axum::extract::{Query, State};
use axum::response::{Html, IntoResponse};
use minijinja::context;
use serde::{Deserialize, Serialize};
use sqlx::{Postgres, Transaction};
use uuid::Uuid;

use crate::app_error::AppError;
use crate::models::tag::{escape_like, search_tags};
use crate::models::user::AuthSession;
use crate::web::handlers::search::search_people;
use crate::web::handlers::ExtractFtlLang;
use crate::web::state::AppState;

/// Rows of each kind: enough to find a place by a few letters of its name,
/// few enough that the list fits a window without scrolling.
const PER_KIND: i64 = 5;

/// Longer than any name or handle, so a pasted paragraph is not a query.
const MAX_QUERY: usize = 100;

#[derive(Deserialize)]
pub struct JumpQuery {
    #[serde(default)]
    q: String,
}

#[derive(Serialize)]
struct JumpCommunity {
    name: String,
    slug: String,
    foreground_color: Option<String>,
    background_color: Option<String>,
    is_member: bool,
}

pub async fn jump(
    auth_session: AuthSession,
    ExtractFtlLang(ftl_lang): ExtractFtlLang,
    State(state): State<AppState>,
    Query(query): Query<JumpQuery>,
) -> Result<impl IntoResponse, AppError> {
    let q: String = query.q.trim().chars().take(MAX_QUERY).collect();
    let reader = auth_session.user.as_ref().map(|user| user.id);

    let mut tx = state.db_pool.begin().await?;
    let communities = communities(&mut tx, &q, reader).await?;
    let (people, tags) = if q.is_empty() {
        (Vec::new(), Vec::new())
    } else {
        (
            search_people(&mut tx, &q, PER_KIND).await?,
            search_tags(&mut tx, &q, PER_KIND).await?,
        )
    };
    tx.commit().await?;

    let rendered = state
        .env
        .get_template("jump_results.jinja")?
        .render(context! {
            q,
            current_user => auth_session.user,
            communities,
            people,
            tags,
            ftl_lang,
        })?;
    Ok(Html(rendered))
}

/// The reader's own communities, and with something typed, public ones too.
/// Never a private community the reader is not in, not even its name.
async fn communities(
    tx: &mut Transaction<'_, Postgres>,
    q: &str,
    reader: Option<Uuid>,
) -> Result<Vec<JumpCommunity>, AppError> {
    let pattern = format!("%{}%", escape_like(q));
    let rows = sqlx::query_as!(
        JumpCommunity,
        r#"
        SELECT
            c.name,
            c.slug,
            c.foreground_color,
            c.background_color,
            (m.user_id IS NOT NULL) AS "is_member!"
        FROM communities c
        LEFT JOIN community_members m ON m.community_id = c.id AND m.user_id = $2
        WHERE c.deleted_at IS NULL
          AND (m.user_id IS NOT NULL OR ($1 <> '' AND c.visibility = 'public'))
          AND ($1 = '' OR c.name ILIKE $3 ESCAPE '\' OR c.slug ILIKE $3 ESCAPE '\')
        ORDER BY
          (m.user_id IS NOT NULL) DESC,
          (c.name ILIKE $4 || '%' ESCAPE '\' OR c.slug ILIKE $4 || '%' ESCAPE '\') DESC,
          c.updated_at DESC
        LIMIT $5
        "#,
        q,
        reader,
        pattern,
        escape_like(q),
        // Nothing typed lists the reader's communities, so it can be longer.
        if q.is_empty() { PER_KIND * 2 } else { PER_KIND },
    )
    .fetch_all(&mut **tx)
    .await?;
    // The chip is coloured by a style attribute, so only a colour goes in.
    Ok(rows
        .into_iter()
        .map(|mut row| {
            if !(is_hex_colour(&row.foreground_color) && is_hex_colour(&row.background_color)) {
                row.foreground_color = None;
                row.background_color = None;
            }
            row
        })
        .collect())
}

fn is_hex_colour(colour: &Option<String>) -> bool {
    colour.as_deref().is_some_and(|c| {
        c.len() == 7 && c.starts_with('#') && c[1..].chars().all(|d| d.is_ascii_hexdigit())
    })
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
                current_user => if signed_in { json!({"id": "u", "login_name": "tandemaus"}) } else { json!(null) },
                communities,
                people => vec![crate::web::handlers::search::SearchPersonRow {
                    login_name: "neo".into(),
                    display_name: "Neo <b>".into(),
                    banner_image_filename: None,
                }],
                tags,
                ftl_lang => "en",
            })
            .unwrap_or_else(|e| panic!("jump_results.jinja renders: {e:#}"))
            // Autoescape writes a slash in a value as an entity, which the
            // browser reads back as a slash.
            .replace("&#x2f;", "/")
    }

    fn club(colour: Option<&str>) -> JumpCommunity {
        JumpCommunity {
            name: "drawing club".into(),
            slug: "club".into(),
            foreground_color: colour.map(Into::into),
            background_color: colour.map(Into::into),
            is_member: true,
        }
    }

    #[test]
    fn every_row_is_a_link_and_a_name_is_text() {
        let rendered = render("", true, vec![club(Some("#112233"))]);
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
        assert!(rendered.contains("background-color: #112233"));
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

    #[test]
    fn only_a_colour_goes_into_a_style() {
        assert!(is_hex_colour(&Some("#a0B1c2".into())));
        assert!(!is_hex_colour(&Some("red;x:y".into())));
        assert!(!is_hex_colour(&None));
    }
}
