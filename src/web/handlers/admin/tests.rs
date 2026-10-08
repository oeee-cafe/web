//! `cargo check` validates the handlers but not the Jinja, so render every
//! admin template against representative context. Catches syntax errors,
//! broken inheritance and unknown filters.

use minijinja::value::Serde;
use minijinja::{context, Environment};
use serde_json::json;

fn test_env() -> Environment<'static> {
    crate::web::handlers::test_support::env()
}

fn sample_post() -> serde_json::Value {
    json!({
        "id": "00000000-0000-0000-0000-000000000001",
        "title": "A drawing",
        "is_sensitive": false,
        "viewer_count": 12,
        "author_id": "00000000-0000-0000-0000-000000000002",
        "author_login_name": "someone",
        "author_display_name": "Some One",
        "community_id": "00000000-0000-0000-0000-000000000003",
        "community_slug": "secret",
        "community_name": "Secret Club",
        "community_visibility": "private",
        "image_filename": "abcdef.png",
        "image_width": 300,
        "image_height": 300,
        "published_at": "2026-01-02T03:04:05Z",
        "created_at": "2026-01-01T03:04:05Z",
        "deleted_at": "2026-02-01T03:04:05Z",
        "deletion_reason": "Moderation",
        "is_sensitive_by_author": false,
        "is_explicit": true,
        "explicit_flagged_at": "2026-06-01T00:00:00Z",
        "explicit_flagged_by_login_name": "admin",
    })
}

fn sample_communities() -> serde_json::Value {
    json!([{
        "id": "00000000-0000-0000-0000-000000000003",
        "slug": "secret",
        "name": "Secret Club",
        "visibility": "private",
        "created_at": "2026-01-01T00:00:00Z",
        "deleted_at": null,
        "post_count": 7,
        "last_active_at": "2026-03-01T00:00:00Z",
    }])
}

fn current_user() -> serde_json::Value {
    json!({
        "id": "00000000-0000-0000-0000-000000000009",
        "login_name": "admin",
        "display_name": "Admin",
        "email_verified_at": "2026-01-01T00:00:00Z",
        "role": "admin",
    })
}

#[test]
fn renders_posts_list() {
    let env = test_env();
    let template = env
        .get_template("admin/posts.jinja")
        .expect("template loads");
    let rendered = template
        .render(context! {
            current_user => Serde(current_user()),
            posts => Serde(vec![sample_post()]),
            communities => Serde(sample_communities()),
            total => 1,
            has_more => true,
            next_url => "/admin/posts-fragment?offset=60",
            filter_author => "someone",
            filter_community => "secret",
            include_drafts => true,
            include_deleted => true,
            draft_post_count => 0,
            unread_notification_count => 0,
            ftl_lang => "en",
        })
        .expect("posts.jinja renders");
    // The column control drives --admin-cols; the grid must opt in via
    // `adjustable` or the slider changes nothing.
    assert!(rendered.contains("admin-grid adjustable"));
    assert!(rendered.contains("id=\"admin-cols\""));
    // The toolbar is the same on every page — admin does not get a header
    // of its own to match its full-bleed grid. It spans the window, with
    // no page column to widen.
    // Matched on the class rather than the whole tag: the nav also carries
    // the boost attribute, and this assertion is about its layout.
    assert!(rendered.contains("<nav class=\"nav-bar\""));
    assert!(rendered.contains("<div id=\"menubar\">"));
    // The page's `set admin_section` reaches the layout, which marks
    // its section in the pill and no other.
    assert!(rendered.contains("href=\"/admin/posts\" aria-current=\"page\""));
    assert!(!rendered.contains("href=\"/admin/users\" aria-current"));
}

#[test]
fn renders_posts_list_when_empty() {
    let env = test_env();
    let template = env
        .get_template("admin/posts.jinja")
        .expect("template loads");
    let rendered = template
        .render(context! {
            current_user => Serde(current_user()),
            posts => Serde(Vec::<serde_json::Value>::new()),
            communities => Serde(sample_communities()),
            total => 0,
            has_more => false,
            next_url => "/admin/posts-fragment?offset=60",
            filter_author => None::<String>,
            filter_community => None::<String>,
            include_drafts => false,
            include_deleted => false,
            draft_post_count => 0,
            unread_notification_count => 0,
            ftl_lang => "en",
        })
        .expect("posts.jinja renders with no results");
    assert!(rendered.contains("No posts match this filter."));
}

#[test]
fn post_titles_are_html_escaped() {
    // Regression: a real post titled `><//` unbalanced the card markup
    // because .jinja templates were not autoescaped, and a title carrying a
    // quote could break out of the alt attribute entirely.
    let env = test_env();
    let template = env
        .get_template("admin/posts_fragment.jinja")
        .expect("template loads");
    let mut post = sample_post();
    post["title"] = json!(r#"><//" onerror="alert(1)"#);
    let rendered = template
        .render(context! {
            posts => Serde(vec![post]),
            has_more => false,
            next_url => "",
            r2_public_endpoint_url => "https://example.test",
        })
        .expect("renders");

    // The raw title must not survive anywhere in the output.
    assert!(!rendered.contains(r#"><//" onerror="#));
    assert!(!rendered.contains("onerror=\"alert"));
    // ...and the div holding it must still close, so cards do not nest.
    assert_eq!(
        rendered.matches("<div").count(),
        rendered.matches("</div>").count()
    );
}

#[test]
fn shared_card_switches_targets_for_admin() {
    // The public feed and the admin grid render the same macro; `admin`
    // decides the link targets and the moderation tags. If that flag stops
    // working, admin cards quietly start linking readers into /admin.
    let env = test_env();
    let template = env
        .get_template("admin/posts_fragment.jinja")
        .expect("template loads");
    let rendered = template
        .render(context! {
            posts => Serde(vec![sample_post()]),
            has_more => false,
            next_url => "",
            r2_public_endpoint_url => "https://example.test",
        })
        .expect("renders");

    // Admin targets, not public ones.
    assert!(rendered.contains("/admin/posts/"));
    assert!(rendered.contains("/admin/users/someone/posts"));
    assert!(rendered.contains("/admin/communities/secret/posts"));
    assert!(!rendered.contains("/communities/@secret"));
    // Moderation tags are admin-only.
    assert!(rendered.contains("admin-tag"));
    assert!(rendered.contains("explicit"));
    // The handle fallback resolves author_login_name for admin rows.
    assert!(rendered.contains("@someone"));
    // Staff see sensitive content unblurred.
    assert!(!rendered.contains("class=\"sensitive\""));
    // ...and the shared attribution block is present either way.
    assert!(rendered.contains("post-card-byline"));
}

#[test]
fn renders_posts_fragment_standalone() {
    // The fragment handler passes a strictly smaller context than the full
    // page, so render it with only those keys.
    let env = test_env();
    let template = env
        .get_template("admin/posts_fragment.jinja")
        .expect("template loads");
    let rendered = template
        .render(context! {
            posts => Serde(vec![sample_post()]),
            has_more => true,
            next_url => "/admin/posts-fragment?offset=60&author=some%20one",
            r2_public_endpoint_url => "https://example.test",
        })
        .expect("posts_fragment.jinja renders standalone");
    assert!(rendered.contains("hx-trigger=\"revealed\""));
    // `&` is entity-encoded in the attribute now that escaping is on. That
    // is correct HTML: the parser decodes it, so getAttribute() hands htmx
    // back a plain `&`. Pinned so double-escaping would be caught.
    // Escaping encodes `/` and `&` as entities. Harmless in an attribute —
    // the HTML parser decodes them, so getAttribute() hands htmx back the
    // plain URL. Pinned so double-escaping would be caught.
    assert!(rendered.contains("&#x2f;admin&#x2f;posts-fragment?offset=60&amp;author=some%20one"));
}

#[test]
fn fragment_omits_sentinel_on_last_batch() {
    let env = test_env();
    let template = env
        .get_template("admin/posts_fragment.jinja")
        .expect("template loads");
    let rendered = template
        .render(context! {
            posts => Serde(vec![sample_post()]),
            has_more => false,
            next_url => "",
            r2_public_endpoint_url => "https://example.test",
        })
        .expect("posts_fragment.jinja renders");
    assert!(!rendered.contains("hx-trigger"));
}

#[test]
fn renders_post_detail() {
    let env = test_env();
    let template = env
        .get_template("admin/post_detail.jinja")
        .expect("template loads");
    template
        .render(context! {
            current_user => Serde(current_user()),
            post => Serde(sample_post()),
            draft_post_count => 0,
            unread_notification_count => 0,
            ftl_lang => "en",
        })
        .expect("post_detail.jinja renders");
}

/// Staff watch a NEO replay on the post's own admin page, removed post or
/// not -- the sample is deleted -- and the drawing waits under the viewer as
/// its poster. Tegaki's player is a page of its own, so that one is a link.
#[test]
fn post_detail_plays_the_replay() {
    let env = test_env();
    let template = env
        .get_template("admin/post_detail.jinja")
        .expect("template loads");
    let render = |post: serde_json::Value| {
        template
            .render(context! {
                current_user => Serde(current_user()),
                post => Serde(post),
                draft_post_count => 0,
                unread_notification_count => 0,
                ftl_lang => "ko",
                r2_public_endpoint_url => "https://r2.example.test",
            })
            .expect("post_detail.jinja renders")
    };

    let mut neo = sample_post();
    neo["replay_filename"] = json!("ab12.pch");
    let rendered = render(neo);
    assert!(rendered.contains("id=\"admin-post-replay\""));
    assert!(rendered.contains("data-replay=\"https://r2.example.test/replay/ab/ab12.pch\""));
    assert!(rendered.contains("data-lang=\"ko\""));
    assert!(rendered.contains("neo-cucumber-replay-controls"));
    assert!(rendered.contains("neo-cucumber-replay.js"));

    let mut tegaki = sample_post();
    tegaki["replay_filename"] = json!("cd34.tgkr");
    tegaki["deleted_at"] = json!(null);
    tegaki["deletion_reason"] = json!(null);
    let rendered = render(tegaki);
    assert!(!rendered.contains("admin-post-replay"));
    assert!(rendered.contains("/replay\">watch</a>"));

    let rendered = render(sample_post());
    assert!(!rendered.contains("admin-post-replay"));
    assert!(!rendered.contains("neo-cucumber-replay.js"));
}

fn sample_banner(is_explicit: bool) -> serde_json::Value {
    json!({
        "id": "00000000-0000-0000-0000-00000000000b",
        "author_id": "00000000-0000-0000-0000-000000000002",
        "author_login_name": "someone",
        "image_filename": "bannerfile.png",
        "image_width": 300,
        "image_height": 100,
        "is_explicit": is_explicit,
        "flagged_at": if is_explicit { Some("2026-05-01T00:00:00Z") } else { None },
        "flagged_by_login_name": if is_explicit { Some("admin") } else { None },
        "is_active": true,
        "created_at": "2026-01-01T00:00:00Z",
    })
}

#[test]
fn post_flag_panel_renders_standalone_for_htmx_swap() {
    let env = test_env();
    let template = env
        .get_template("admin/post_flag_panel.jinja")
        .expect("template loads");

    let rendered = template
        .render(context! { post => Serde(sample_post()) })
        .expect("flagged panel renders");
    assert!(rendered.contains("flagged explicit by staff"));
    assert!(rendered.contains("Remove explicit flag"));
    // Flipping back must post the opposite state, not a blind toggle.
    assert!(rendered.contains("value=\"false\""));

    let mut unflagged = sample_post();
    unflagged["is_explicit"] = json!(false);
    unflagged["explicit_flagged_at"] = json!(null);
    unflagged["explicit_flagged_by_login_name"] = json!(null);
    let rendered = template
        .render(context! { post => Serde(unflagged) })
        .expect("unflagged panel renders");
    assert!(rendered.contains("Flag as explicit"));
    assert!(rendered.contains("value=\"true\""));
}

#[test]
fn renders_banner_queue() {
    let env = test_env();
    let template = env
        .get_template("admin/banners.jinja")
        .expect("template loads");
    let rendered = template
        .render(context! {
            current_user => Serde(current_user()),
            banners => Serde(vec![sample_banner(false), sample_banner(true)]),
            only_explicit => false,
            has_more => true,
            next_url => "/admin/banners-fragment?offset=60",
            draft_post_count => 0,
            unread_notification_count => 0,
            ftl_lang => "en",
        })
        .expect("banners.jinja renders");
    assert!(rendered.contains("Flag as explicit"));
    assert!(rendered.contains("Unflag"));
}

#[test]
fn renders_banners_fragment_standalone() {
    let env = test_env();
    let template = env
        .get_template("admin/banners_fragment.jinja")
        .expect("template loads");
    let rendered = template
        .render(context! {
            banners => Serde(vec![sample_banner(false)]),
            has_more => true,
            next_url => "/admin/banners-fragment?offset=60&explicit=on",
            r2_public_endpoint_url => "https://example.test",
        })
        .expect("banners_fragment.jinja renders standalone");
    assert!(rendered.contains("hx-trigger=\"revealed\""));
    assert!(rendered.contains("&#x2f;admin&#x2f;banners-fragment?offset=60&amp;explicit=on"));
    // The nested card must still render inside the fragment.
    assert!(rendered.contains("Flag as explicit"));
}

#[test]
fn banner_card_renders_standalone_for_htmx_swap() {
    // The flag endpoint returns this template alone, so it must not depend
    // on anything the queue page supplies.
    let env = test_env();
    let template = env
        .get_template("admin/banner_card.jinja")
        .expect("template loads");
    let rendered = template
        .render(context! {
            banner => Serde(sample_banner(true)),
            r2_public_endpoint_url => "https://example.test",
        })
        .expect("banner_card.jinja renders standalone");
    assert!(rendered.contains("hidden from /about"));
    // Flipping back must post the opposite state, not a blind toggle.
    assert!(rendered.contains("value=\"false\""));
}

/// A session card's admin context. Defaults describe a live, link-only,
/// personal session with a preview; tests override the field under test.
fn sample_session(overrides: serde_json::Value) -> serde_json::Value {
    let mut session = json!({
        "id": "00000000-0000-0000-0000-000000000009",
        "title": "Doodle",
        "owner_login_name": "someone",
        "width": 1024,
        "height": 768,
        "max_participants": 4,
        "active_participant_count": 2,
        "total_participant_count": 5,
        "is_public": false,
        "community_slug": null,
        "community_name": null,
        "community_visibility": null,
        "created_at": "2026-01-02T03:04:05",
        "last_activity": "2026-01-02T05:06:07",
        "ended_at": null,
        "saved_post_id": null,
        "preview_version": 1_700_000_000_123u64,
    });
    for (key, value) in overrides.as_object().expect("object") {
        session[key] = value.clone();
    }
    session
}

fn sessions_context(sessions: Vec<serde_json::Value>) -> minijinja::Value {
    context! {
        current_user => Serde(current_user()),
        sessions => Serde(sessions),
        page => 1,
        sort => "active",
        status => "all",
        has_next => false,
        draft_post_count => 0,
        unread_notification_count => 0,
        ftl_lang => "en",
    }
}

fn render_sessions(sessions: Vec<serde_json::Value>) -> String {
    let env = test_env();
    env.get_template("admin/collaborative_sessions.jinja")
        .expect("template loads")
        .render(sessions_context(sessions))
        .expect("collaborative_sessions.jinja renders")
}

/// The reason this page exists: a room the lobby will not list for
/// anyone who is not already in it.
#[test]
fn session_list_marks_the_ones_the_lobby_hides() {
    let rendered = render_sessions(vec![
        sample_session(json!({})),
        sample_session(json!({
            "id": "00000000-0000-0000-0000-00000000000a",
            "is_public": true,
            "community_slug": "secret",
            "community_name": "Back Room",
            "community_visibility": "private",
        })),
    ]);
    assert!(rendered.contains("link only"));
    assert!(rendered.contains("admin-tag private"));
    assert!(rendered.contains("Back Room"));
}

/// The canvas itself, which is the thing an admin cannot otherwise see
/// without taking a seat in the room.
#[test]
fn session_list_shows_the_live_canvas() {
    let rendered = render_sessions(vec![sample_session(json!({}))]);
    assert!(rendered
        .contains("/collaborate/00000000-0000-0000-0000-000000000009/preview?v=1700000000123"));
}

/// An ended session's Redis state is deleted with it, so there is nothing
/// to point an `<img>` at and a row must not try.
#[test]
fn session_list_omits_the_canvas_once_a_session_is_over() {
    let rendered = render_sessions(vec![sample_session(json!({
        "ended_at": "2026-01-03T00:00:00Z",
        "preview_version": null,
        "saved_post_id": "00000000-0000-0000-0000-00000000000b",
    }))]);
    assert!(!rendered.contains("/preview?v="));
    // The canvas is a post by then, and that is where it is reachable.
    assert!(rendered.contains("/@someone/00000000-0000-0000-0000-00000000000b"));
    // Nor is there a live room left to open.
    assert!(!rendered.contains("/collaborate/00000000-0000-0000-0000-000000000009\""));
}

/// The recording is offered for every row, ended ones included -- an
/// ended session is exactly the one worth examining, and the archive
/// outlives the room it came from.
#[test]
fn session_rows_link_to_their_recording() {
    let rendered = render_sessions(vec![
        sample_session(json!({})),
        sample_session(json!({
            "id": "00000000-0000-0000-0000-00000000000c",
            "ended_at": "2026-01-03T00:00:00Z",
            "preview_version": null,
        })),
    ]);
    // One place for everything kept about a session -- the replay, the
    // log, the conversation and the reports are read against each other.
    assert!(rendered
        .contains("href=\"/admin/collaborative-sessions/00000000-0000-0000-0000-000000000009\""));
    assert!(rendered
        .contains("href=\"/admin/collaborative-sessions/00000000-0000-0000-0000-00000000000c\""));
    // And the raw file beside it.
    assert!(rendered
        .contains("/admin/collaborative-sessions/00000000-0000-0000-0000-000000000009/archive"));
}

/// The inspector is a built page, and the design system reaches it only
/// through what the server adds to its head -- after the painter's reset,
/// or the reset would undo it.
#[test]
fn the_inspector_is_given_the_design_system_after_its_own_sheet() {
    let head = test_env()
        .get_template("admin/replay_head.jinja")
        .expect("template loads")
        .render(context! {})
        .expect("renders");
    assert!(head.contains("/static/ds.css"));
    assert!(head.contains("/static/admin.css"));
    let page = super::sessions::with_head(
        "<html><head><link rel=\"stylesheet\" href=\"/static/replay/replay.css\"></head><body></body></html>",
        &head,
    );
    assert!(page.find("replay.css").unwrap() < page.find("/static/ds.css").unwrap());
    assert!(page.find("/static/ds.css").unwrap() < page.find("</head>").unwrap());
}

/// What was kept about a session reads on its row, and what wants a
/// look -- a report, a replay that differs -- says so there.
#[test]
fn session_rows_say_what_their_recording_holds() {
    let rendered = render_sessions(vec![
        sample_session(json!({ "recording": null })),
        sample_session(json!({
            "id": "00000000-0000-0000-0000-00000000000d",
            "recording": {
                "first_seq": 1,
                "last_seq": 5194,
                "messages": 5194,
                "sealed": true,
                "chat_lines": 4,
                "reports": 2,
                "check_outcome": "differs",
                "check_differing_pixels": 1234,
                "check_total_pixels": 60000,
                "check_note": null,
                "checked_at": "2026-09-24T09:00:00Z",
            },
        })),
        sample_session(json!({
            "id": "00000000-0000-0000-0000-00000000000e",
            "recording": {
                "first_seq": 3310,
                "last_seq": 4000,
                "messages": 690,
                "sealed": false,
                "chat_lines": 0,
                "reports": 0,
                "check_outcome": "match",
                "check_differing_pixels": 0,
                "check_total_pixels": 60000,
                "check_note": null,
                "checked_at": null,
            },
        })),
    ]);
    assert!(rendered.contains("5194 msgs"));
    assert!(rendered.contains("4 chat"));
    assert!(rendered.contains("2 reports"));
    assert!(rendered.contains("replay ✗ 1234 px"));
    assert!(rendered.contains("replay ✓"));
    assert!(rendered.contains(">partial<"));
    assert!(rendered.contains("status=flagged"));
}

/// Sorting and filtering have to survive each other: a status link that
/// dropped the sort would silently reset the page under the admin.
#[test]
fn session_list_filters_keep_the_current_sort() {
    let rendered = render_sessions(vec![sample_session(json!({}))]);
    assert!(rendered.contains("?sort=active&status=live"));
    assert!(rendered.contains("?status=all&sort=created"));
}

#[test]
fn renders_users_list() {
    let env = test_env();
    let template = env
        .get_template("admin/users.jinja")
        .expect("template loads");
    template
        .render(context! {
            current_user => Serde(current_user()),
            users => Serde(json!([{
                "id": "00000000-0000-0000-0000-000000000002",
                "login_name": "someone",
                "display_name": "Some One",
                "role": "user",
                "post_count": 4,
                "created_at": "2026-01-01T00:00:00Z",
                "deleted_at": null,
                "last_active_at": "2026-04-01T00:00:00Z",
            }])),
            page => 1,
            sort => "active",
            has_next => false,
            draft_post_count => 0,
            unread_notification_count => 0,
            ftl_lang => "en",
        })
        .expect("users.jinja renders");
}

#[test]
fn renders_communities_list() {
    let env = test_env();
    let template = env
        .get_template("admin/communities.jinja")
        .expect("template loads");
    template
        .render(context! {
            current_user => Serde(current_user()),
            communities => Serde(sample_communities()),
            sort => "active",
            draft_post_count => 0,
            unread_notification_count => 0,
            ftl_lang => "en",
        })
        .expect("communities.jinja renders");
}

/// The catalogue as the handler hands it over: `StoreProduct`s grouped
/// under a `Store`, a year as a number, a label or null, and the add
/// form echoed back as the strings it was sent.
fn render_store(error: serde_json::Value, form: serde_json::Value) -> String {
    test_env()
        .get_template("admin/store.jinja")
        .expect("template loads")
        .render(context! {
            current_user => Serde(current_user()),
            groups => Serde(json!([
                {"store": "apple", "products": [
                    {"store": "apple", "product": "cafe.oeee.supporter.2026", "year": 2026,
                     "label": null, "on_sale": true, "selling_now": false,
                     "sale_starts_at": "2026-01-01T00:00:00Z", "sale_ends_at": null,
                     "sale_starts_local": "2026-01-01T09:00", "sale_ends_local": "",
                     "purchases": 3, "created_at": "2026-01-01T00:00:00Z"},
                    {"store": "apple", "product": "cafe.oeee.supporter.2025", "year": 2025,
                     "label": "Last year's", "on_sale": false, "created_at": "2025-01-01T00:00:00Z"},
                ]},
                {"store": "google", "products": []},
                {"store": "microsoft", "products": []},
                {"store": "steam", "products": [
                    {"store": "steam", "product": "481", "year": 2026,
                     "label": null, "on_sale": true, "created_at": "2026-01-01T00:00:00Z"},
                ]},
            ])),
            stores => Serde(json!(["apple", "google", "microsoft", "steam"])),
            this_year => 2026,
            microsoft_configured => false,
            google_play_configured => false,
            error => Serde(error),
            form => Serde(form),
            draft_post_count => 0,
            unread_notification_count => 0,
            ftl_lang => "en",
        })
        .expect("store.jinja renders")
}

#[test]
fn renders_store_catalogue() {
    let rendered = render_store(
        json!(null),
        json!({"store": "", "product": "", "year": "", "label": ""}),
    );
    // A toggle per product, saying what pressing it will do.
    assert!(rendered.contains(r#"action="/admin/store/apple/cafe.oeee.supporter.2026/on-sale""#));
    assert!(rendered.contains("Take off sale"));
    assert!(rendered.contains("Put on sale"));
    // Its window, as the inputs that change it want it.
    assert!(
        rendered.contains(r#"action="/admin/store/apple/cafe.oeee.supporter.2026/sale-window""#)
    );
    assert!(rendered.contains(r#"value="2026-01-01T09:00""#));
    assert!(rendered.contains("outside its window"));
    assert!(rendered.contains("Last year&#x27;s") || rendered.contains("Last year's"));
    assert!(rendered.contains(r#"href="/admin/store""#), "in the nav");
    assert!(
        rendered.contains("[microsoft_store]"),
        "says the store cannot be asked"
    );
    assert!(rendered.contains("[google_play]"), "and this one");
    // The year and the words, changed together, and what a new year moves.
    assert!(rendered.contains(r#"action="/admin/store/apple/cafe.oeee.supporter.2026/details""#));
    assert!(
        rendered.contains("admin-product-this-year"),
        "this year stands out"
    );
    assert!(
        rendered.contains(r#"value="Last year&#x27;s""#)
            || rendered.contains(r#"value="Last year's""#),
        "the words are there to change"
    );
    assert!(
        rendered.contains("3 purchases"),
        "says what a new year would move"
    );
    assert!(
        rendered.contains(r#"name="year" value="2026""#),
        "this year by default"
    );
    assert!(!rendered.contains("Not added"));
}

#[test]
fn a_refused_product_comes_back_with_why_and_what_was_typed() {
    let rendered = render_store(
        json!("A product id is one word, with no spaces."),
        json!({"store": "steam", "product": "4 81", "year": "2027", "label": "Hi"}),
    );
    assert!(rendered.contains("Not added"));
    assert!(rendered.contains("A product id is one word, with no spaces."));
    assert!(rendered.contains(r#"<option value="steam" selected>"#));
    assert!(rendered.contains(r#"name="product" value="4 81""#));
    assert!(rendered.contains(r#"name="year" value="2027""#));
}

#[test]
fn a_sale_window_is_read_in_seoul_time() {
    use super::store::{parse_sale_window, to_local_input};
    let (starts, ends) = parse_sale_window("2026-12-01T09:00", " ").unwrap();
    assert_eq!(starts.unwrap().to_rfc3339(), "2026-12-01T00:00:00+00:00");
    assert_eq!(ends, None);
    assert_eq!(
        to_local_input(starts),
        "2026-12-01T09:00",
        "and written back the same"
    );
    assert_eq!(parse_sale_window("", "").unwrap(), (None, None));
    assert!(
        parse_sale_window("2026-12-01T09:00:30", "").is_ok(),
        "seconds"
    );
    for (starts, ends, why) in [
        (
            "2026-12-02T00:00",
            "2026-12-01T00:00",
            "ends before it starts",
        ),
        ("2026-12-01T00:00", "2026-12-01T00:00", "ends as it starts"),
        ("tomorrow", "", "not a date"),
    ] {
        assert!(parse_sale_window(starts, ends).is_err(), "{why}");
    }
}

#[test]
fn a_store_product_is_checked_before_it_is_added() {
    use super::store::{validate_store_product, AddStoreProductForm};
    use crate::models::supporter::{current_year, Store};
    let form = |store: &str, product: &str, year: &str, label: &str| AddStoreProductForm {
        store: store.to_string(),
        product: product.to_string(),
        year: year.to_string(),
        label: label.to_string(),
        ..Default::default()
    };
    let year = current_year().to_string();
    assert_eq!(
        validate_store_product(&form("microsoft", " 9NBLGGH4R315 ", &year, "  "), Some(480)),
        Ok((
            Store::Microsoft,
            "9NBLGGH4R315".to_string(),
            current_year(),
            None
        ))
    );
    assert_eq!(
        validate_store_product(&form("google", "supporter_pack_2026", &year, ""), None)
            .unwrap()
            .1,
        "supporter_pack_2026"
    );
    assert_eq!(
        validate_store_product(&form("apple", "cafe.oeee.x", &year, " Buy "), None)
            .unwrap()
            .3
            .as_deref(),
        Some("Buy")
    );
    for (refused, why) in [
        (form("google_play", "x", &year, ""), "an unknown store"),
        (
            form("google", "Supporter_2026", &year, ""),
            "a Google Play id with capitals",
        ),
        (
            form("google", "_supporter", &year, ""),
            "a Google Play id starting with _",
        ),
        (form("", "x", &year, ""), "no store"),
        (form("apple", "  ", &year, ""), "no product"),
        (form("apple", "cafe oeee", &year, ""), "whitespace"),
        (
            form("apple", "cafe\u{3000}oeee", &year, ""),
            "an ideographic space",
        ),
        (
            form("steam", "abc", &year, ""),
            "a Steam product that is not an app id",
        ),
        (form("steam", "480", &year, ""), "the app itself"),
        (form("apple", "x", "1999", ""), "too early"),
        (form("apple", "x", "20226", ""), "a typo"),
        (form("apple", "x", "", ""), "no year"),
        (form("apple", "x", &year, &"가".repeat(101)), "a long label"),
    ] {
        assert!(
            validate_store_product(&refused, Some(480)).is_err(),
            "{why}"
        );
    }
}
