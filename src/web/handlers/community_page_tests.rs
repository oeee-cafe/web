use super::test_support;
use minijinja::context;
use minijinja::value::Serde;
use serde_json::json;

/// Saving or cancelling the edit form asks for the header block alone, and
/// those handlers pass no feed. Reaching a block still walks the template
/// around it, so the drawing grid below has to survive the missing value —
/// the swap 500s if the grid reaches into `feed` without checking.
#[test]
fn the_header_block_renders_without_a_feed() {
    let env = test_support::env();
    let rendered = env
        .get_template("community.jinja")
        .expect("community template loads")
        .render_captured_to(
            context! {
                current_user => Serde(json!(null)),
                community => Serde(json!({
                    "id": "00000000-0000-0000-0000-000000000001",
                    "name": "Open Studio",
                    "description": "Draw with us",
                    "slug": "open",
                    "visibility": "public",
                    "owner_id": "00000000-0000-0000-0000-000000000002",
                })),
                community_id => "00000000-0000-0000-0000-000000000001",
                domain => "oeee.test",
                ftl_lang => "en",
            },
            std::io::sink(),
        )
        .expect("template evaluates")
        .with_state_mut(|state| state.render_block("community_edit_block"))
        .expect("the header block renders on its own");

    assert!(rendered.contains("Open Studio"));
    assert!(
        !rendered.contains("posts-grid"),
        "the block is the header only"
    );
}

/// Under the card, a pill between the drawings and the comments, each
/// its own address; the comments page is the same page with the other
/// one chosen, its comments as cards in a grid.
#[test]
fn the_drawings_and_the_comments_are_a_pill_apart() {
    let env = test_support::env();
    let base = || {
        context! {
            current_user => Serde(json!(null)),
            messages => Serde(Vec::<serde_json::Value>::new()),
            draft_post_count => 0,
            unread_notification_count => 0,
            community => Serde(json!({
                "id": "00000000-0000-0000-0000-000000000001",
                "name": "Open Studio",
                "description": "Draw with us",
                "slug": "open",
                "visibility": "public",
                "owner_id": "00000000-0000-0000-0000-000000000002",
            })),
            community_id => "00000000-0000-0000-0000-000000000001",
            domain => "oeee.test",
            ftl_lang => "en",
        }
    };
    let pill = |html: &str| {
        let html = html.replace("&#x2f;", "/");
        let start = html.find(r#"<nav class="ds-segmented""#).expect("a pill");
        let end = start + html[start..].find("</nav>").unwrap();
        html[start..end].to_string()
    };

    let drawings = env
        .get_template("community.jinja")
        .expect("community loads")
        .render(base())
        .expect("community renders");
    let on_drawings = pill(&drawings);
    assert!(on_drawings.contains(r#"<a href="/@open" aria-current="page">feed-view-drawings</a>"#));
    assert!(on_drawings.contains(r#"<a href="/communities/@open/comments">feed-view-comments</a>"#));
    assert!(
        on_drawings.contains("hx-boost:inherited"),
        "switched in place"
    );

    let comments = env
        .get_template("community_comments.jinja")
        .expect("comments loads")
        .render(context! {
            comments => Serde(json!({"rows": [{
                "id": "0c8f0000-0000-0000-0000-000000000001",
                "post_id": "0c8f0000-0000-0000-0000-000000000002",
                "actor_id": "0c8f0000-0000-0000-0000-000000000003",
                "content": "멋져요",
                "content_html": null,
                "iri": null,
                "actor_name": "오이",
                "handle": {"login_name": "oeee", "name": null, "host": null},
                "actor_url": "https://oeee.cafe/@oeee",
                "updated_at": "2026-09-22T00:00:00Z",
                "created_at": "2026-09-22T00:00:00Z",
                "post_title": "고양이",
                "post_author_login_name": "cat",
                "post_image_filename": "abcdef.png",
                "post_image_width": 300,
                "post_image_height": 300,
            }], "next_url": null})),
            ..base()
        })
        .expect("comments render");
    let on_comments = pill(&comments);
    assert!(on_comments.contains(r#"<a href="/@open">feed-view-drawings</a>"#));
    assert!(on_comments.contains(
        r#"<a href="/communities/@open/comments" aria-current="page">feed-view-comments</a>"#
    ));
    assert!(comments.contains("Draw with us"), "under the same card");
    assert!(comments.contains(r#"<div class="comment-grid">"#));
    assert!(comments.contains("멋져요"));
    assert!(
        !comments.contains(r#"id="post-feed-grid""#),
        "no drawings under it"
    );
}

/// The header says who keeps the community and how much is in it, and
/// drawing is its primary action: the painter's choices are asked for in
/// a dialog when it is pressed, rather than sitting open on the page.
#[test]
fn the_header_credits_the_owner_and_drawing_opens_a_dialog() {
    let env = test_support::env();
    let community = |background: serde_json::Value| {
        json!({
            "id": "00000000-0000-0000-0000-000000000001",
            "name": "Open Studio",
            "description": "Draw with us",
            "slug": "open",
            "visibility": "public",
            "owner_id": "00000000-0000-0000-0000-000000000002",
            "background_color": background,
            "foreground_color": "#000000",
        })
    };
    let render = |community: serde_json::Value| {
        env.get_template("community.jinja")
            .expect("community template loads")
            .render(context! {
                current_user => Serde(json!({"id": "00000000-0000-0000-0000-000000000003"})),
                messages => Serde(Vec::<serde_json::Value>::new()),
                draft_post_count => 0,
                unread_notification_count => 0,
                community => Serde(community),
                header => Serde(json!({
                    "owner": {"login_name": "keeper", "display_name": "The Keeper"},
                    "posts_count": 12,
                    "contributors_count": 4,
                })),
                community_id => "00000000-0000-0000-0000-000000000001",
                domain => "oeee.test",
                feed => Serde(json!({"posts": [], "has_more": false})),
                ftl_lang => "en",
            })
            .expect("community renders")
    };

    let rendered = render(community(json!(null)));
    assert!(rendered.contains("The Keeper"));
    assert!(rendered.contains("12 community-stats-posts"));
    let dialog = rendered
        .find("id=\"community-draw-modal\"")
        .expect("the drawing dialog");
    let size = rendered.find("community-draw-size").expect("size choice");
    assert!(size > dialog, "the drawing form is back on the page");
    assert!(rendered.contains("/collaborate?community=open"));

    // Two-tone: the dialog asks for an orientation, and there is no
    // drawing together to offer.
    let rendered = render(community(json!("#ffffff")));
    assert!(rendered.contains("name=\"orientation\""));
    assert!(rendered.contains("community-colors"));
    assert!(!rendered.contains("/collaborate?community=open"));
}

/// Cancel and Save swap the edit card back out for the header. The
/// delete area was a sibling of the form, so it outlived the swap and
/// stayed on the page under the restored header; now the one element
/// that is swapped holds all of it.
#[test]
fn the_edit_card_is_one_swappable_element() {
    let env = test_support::env();
    for visibility in ["public", "private"] {
        let rendered = env
            .get_template("community_edit.jinja")
            .expect("edit template loads")
            .render(context! {
                community => Serde(json!({
                    "name": "Open Studio",
                    "slug": "open",
                    "description": "Draw with us",
                    "visibility": visibility,
                })),
                community_id => "00000000-0000-0000-0000-000000000001",
                ftl_lang => "en",
            })
            .expect("edit form renders");
        let rendered = rendered.trim();
        assert!(
            rendered.ends_with("</section>"),
            "something follows the card"
        );
        assert_eq!(rendered.matches("<section").count(), 1);
        assert!(rendered.contains("hx-target:inherited=\"this\""));
        assert!(rendered.contains("delete-community-btn"));
        assert_eq!(
            rendered.contains("name=\"visibility\" value=\"private\""),
            visibility == "private"
        );
    }
}

/// Drafts are drawn in the same grid, with the same Per row, as every
/// other page of drawings; each leads to publishing it, and says how
/// long ago it was last touched rather than printing a timestamp.
#[test]
fn drafts_share_the_grid_and_its_control() {
    let env = test_support::env();
    let updated = (chrono::Utc::now() - chrono::Duration::hours(3)).to_rfc3339();
    let rendered = env
        .get_template("draft_posts.jinja")
        .expect("drafts template loads")
        .render(context! {
            current_user => Serde(json!({"login_name": "someone"})),
            messages => Serde(Vec::<serde_json::Value>::new()),
            draft_post_count => 1,
            unread_notification_count => 0,
            ftl_lang => "en",
            r2_public_endpoint_url => "https://example.test",
            posts => Serde(vec![json!({
                "id": "00000000-0000-0000-0000-000000000001",
                "title": null,
                "content": null,
                "community_id": null,
                "community_name": null,
                "image_filename": "abcdef.png",
                "image_width": 300,
                "image_height": 300,
                "updated_at": updated,
            })]),
        })
        .expect("drafts render");
    assert!(rendered.contains("class=\"posts-grid\" id=\"post-feed-grid\""));
    assert!(rendered.contains("data-per-row"));
    assert!(
        rendered.contains("&#x2f;posts&#x2f;00000000-0000-0000-0000-000000000001&#x2f;publish")
            || rendered.contains("/posts/00000000-0000-0000-0000-000000000001/publish")
    );
    assert!(rendered.contains(">3h<"), "not a relative time");
}

/// A guestbook's list and its empty note are one or the other by CSS
/// (`:empty`), so the list has to be empty to the letter when it has no
/// entries, and an entry has to bring no whitespace around it -- or the
/// last one deleted would leave a blank card and no note.
#[test]
fn a_guestbook_list_is_empty_to_the_letter() {
    let env = test_support::env();
    let render = |entries: Vec<serde_json::Value>| {
        env.get_template("guestbook.jinja")
            .expect("loads")
            .render(context! {
                user => Serde(json!({"login_name": "oeee", "display_name": "오이", "id": "u1"})),
                current_user => Serde(json!(null)),
                messages => Serde(Vec::<serde_json::Value>::new()),
                draft_post_count => 0,
                unread_notification_count => 0,
                ftl_lang => "en",
                guestbook_entries => Serde(entries),
            })
            .expect("renders")
    };
    let empty = render(vec![]);
    assert!(
        empty.contains(r#"id="guestbook-entries"></div>"#),
        "the list is not :empty"
    );
    assert!(empty.contains("guestbook-empty"));
    let entry = json!({
        "id": "e1", "author_id": "u2", "recipient_id": "u1",
        "author_login_name": "someone", "author_display_name": "Someone",
        "content": "hi", "reply": null,
        "created_at": chrono::Utc::now().to_rfc3339(),
    });
    let one = render(vec![entry.clone()]);
    assert!(one.contains(r#"id="guestbook-entries"><div class="guestbook-entry">"#));
    let alone = env
        .get_template("guestbook_entry.jinja")
        .expect("loads")
        .render(context! { entry => Serde(entry), user => Serde(json!({"login_name": "oeee"})), current_user => Serde(json!(null)), ftl_lang => "en" })
        .expect("renders");
    assert!(
        alone.starts_with("<div") && alone.ends_with("</div>"),
        "{alone:?}"
    );
}

/// Throwing a draft away answers with what else changes, out of band:
/// the count always, and at the last one the empty state in the grid's
/// place and the per-row control gone. Ids the drafts page carries.
#[test]
fn a_thrown_away_draft_updates_the_count_and_empties_the_page() {
    let env = test_support::env();
    let oob = env.get_template("draft_delete_oob.jinja").expect("loads");
    let some = oob
        .render(context! { remaining => 2, ftl_lang => "en" })
        .expect("renders");
    assert!(some.contains(r#"<span id="drafts-count" hx-swap-oob="true">2</span>"#));
    assert!(!some.contains("drafts-body"), "drafts left, the grid stays");
    let none = oob
        .render(context! { remaining => 0, ftl_lang => "en" })
        .expect("renders");
    assert!(none.contains(r#"<div id="drafts-body" hx-swap-oob="true">"#));
    assert!(none.contains("draft-empty"));
    assert!(
        none.contains(r#"<div id="drafts-tools" class="drafts-tools" hx-swap-oob="true"></div>"#)
    );

    let page = env
        .get_template("draft_posts.jinja")
        .expect("loads")
        .render(context! {
            current_user => Serde(json!({"login_name": "someone"})),
            messages => Serde(Vec::<serde_json::Value>::new()),
            draft_post_count => 0,
            unread_notification_count => 0,
            ftl_lang => "en",
            posts => Serde(Vec::<serde_json::Value>::new()),
        })
        .expect("renders");
    for id in ["drafts-count", "drafts-tools", "drafts-body"] {
        assert!(
            page.contains(&format!(r#"id="{id}""#)),
            "the page has no #{id}"
        );
    }
}

/// The grid is the shared feed fragment, so its sentinel points wherever
/// the handler said — and it must be this community's endpoint rather than
/// the home feed's, or scrolling a community page loads the front page.
#[test]
fn the_grid_continues_from_the_communitys_own_endpoint() {
    use crate::models::post::SerializablePostForHome;
    use crate::web::handlers::home::{feed_context, HOME_POSTS_PER_BATCH};

    // A full batch, because that is what tells the feed there is more.
    let posts = (0..HOME_POSTS_PER_BATCH)
        .map(|i| SerializablePostForHome {
            id: uuid::Uuid::from_u128(i as u128 + 1),
            title: Some(format!("Drawing {i}")),
            author_id: uuid::Uuid::from_u128(999),
            user_login_name: "artist".into(),
            paint_duration: "0".to_string(),
            stroke_count: 1,
            viewer_count: 0,
            image_filename: "abcdef.png".to_string(),
            image_width: 300,
            image_height: 300,
            replay_filename: None,
            is_sensitive: false,
            community_slug: Some("open".to_string()),
            community_name: Some("Open Studio".to_string()),
            published_at: Some(chrono::Utc::now()),
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        })
        .collect();

    let env = test_support::env();
    let rendered = env
        .get_template("community.jinja")
        .expect("community template loads")
        .render(context! {
            current_user => Serde(json!(null)),
            messages => Serde(Vec::<serde_json::Value>::new()),
            draft_post_count => 0,
            unread_notification_count => 0,
            community => Serde(json!({
                "id": "00000000-0000-0000-0000-000000000001",
                "name": "Open Studio",
                "description": "Draw with us",
                "slug": "open",
                "visibility": "public",
                "owner_id": "00000000-0000-0000-0000-000000000002",
            })),
            community_id => "00000000-0000-0000-0000-000000000001",
            domain => "oeee.test",
            feed => feed_context(posts, "/api/communities/@open/posts", 0, None),
            comments => Serde(json!({
                "rows": [{
                    "post_id": "00000000-0000-0000-0000-000000000001",
                    "post_author_login_name": "artist",
                    "post_title": "Drawing 0",
                    "actor_name": "Commenter",
                    "handle": {"login_name": "commenter", "name": null, "host": null},
                    "content": "Lovely colours",
                    "created_at": "2026-01-02T03:04:05Z",
                }],
                "next_url": "/api/communities/@open/comments?after=00000000-0000-0000-0000-000000000009",
            })),
            ftl_lang => "en",
        })
        .expect("community renders");

    // What is said on its drawings goes beside them, loads on from this
    // community's own endpoint, and -- where a phone shows only the
    // first few -- goes on to the community's own page of comments.
    assert!(rendered.contains(r#"<aside class="feed-comments" aria-labelledby"#));
    assert!(rendered.contains(
        r#"hx-get="&#x2f;api&#x2f;communities&#x2f;@open&#x2f;comments?after=00000000-0000-0000-0000-000000000009""#
    ));
    assert!(rendered.contains(
        r#"<a class="feed-comments-more" href="&#x2f;communities&#x2f;@open&#x2f;comments">"#
    ));

    // Minijinja escapes the slashes and the ampersand in an attribute; the
    // browser reads them back as the URL, so assert against that.
    // Every drawing is this month's, so the next batch is told the grid
    // is still in it, and does not head its first drawing again.
    let month = crate::feed_period::period(chrono::Utc::now(), chrono::Utc::now()).key;
    let links_in = rendered.replace("&#x2f;", "/").replace("&amp;", "&");
    assert!(
        links_in.contains(&format!(
            r#"hx-get="/api/communities/@open/posts?offset={}&limit={}&period={}""#,
            HOME_POSTS_PER_BATCH, HOME_POSTS_PER_BATCH, month
        )),
        "the sentinel should ask this community for the next batch"
    );
    assert!(
        rendered.contains(r#"id="post-feed-grid""#),
        "the column control drives the grid by id"
    );
}
