//! The drawing pages, signed in and as a guest.

use crate::web::handlers::test_support;
use minijinja::context;
use serde_json::json;

use super::{chrome, replay_post};

/// The painter pages carry the site's toolbar, so every window has the
/// same title bar and the desktop app can seat its controls in it. It has
/// to come before the painter -- it is the page's first row -- and its
/// stylesheet after the painter's, whose reset would otherwise restyle it.
#[test]
fn the_painter_pages_carry_the_toolbar() {
    let env = test_support::env();
    let rendered = env
        .get_template("draw_post_cucumber.jinja")
        .expect("painter template loads")
        .render(context! {
            width => 300,
            height => 300,
            community_id => json!(null),
            painter_config => "{}",
            current_user => json!({"login_name": "someone", "display_name": "Someone"}),
            messages => Vec::<serde_json::Value>::new(),
            draft_post_count => 2,
            unread_notification_count => 3,
            ftl_lang => "en",
        })
        .expect("painter renders");
    let nav = rendered
        .find("<nav class=\"nav-bar\"")
        .expect("the painter page has the toolbar");
    assert!(rendered.contains("/static/ds.css"));
    assert!(rendered.contains("toolbar-bell-unread"), "unread bell");
    assert!(
        nav < rendered.find("id=\"neo-cucumber-root\"").unwrap(),
        "the toolbar is the painter page's first row"
    );
    if let Some(painter_css) = rendered.find("offline.css") {
        let ds_css = rendered.find("/static/ds.css").unwrap();
        assert!(
            painter_css < ds_css,
            "ds.css loads after the painter's reset"
        );
    }
}

/// What the Steam app reads to tell friends what someone is doing: the
/// painters, the collaborative room's head and any page on the base
/// layout carry it when the handler passes one, and none of them when it
/// does not.
#[test]
fn pages_say_what_their_reader_is_doing_for_steam() {
    let env = test_support::env();
    let presence = json!({
        "activity": "drawing",
        "community": "오이카페 \"모에화\" <b>",
        "group": null,
    });
    for template_name in ["draw_post_cucumber.jinja", "collaborate_chrome_head.jinja"] {
        let render = |presence: serde_json::Value| {
            env.get_template(template_name)
                .unwrap_or_else(|e| panic!("{template_name} loads: {e:#}"))
                .render(context! {
                    presence,
                    painter_config => "{}",
                    parent_post => json!(null),
                    post => replay_post(),
                    post_id => "9c881320-2b43-4afa-b2bb-7128c8a3e985",
                    community_id => json!(null),
                    ..chrome()
                })
                .unwrap_or_else(|e| panic!("{template_name} renders: {e:#}"))
        };
        let with = render(presence.clone());
        assert!(
            with.contains(r#"<meta name="oeee-presence" content="drawing" data-community="오이카페 &quot;모에화&quot; &lt;b&gt;" />"#),
            "{template_name} should carry the presence tag, escaped"
        );
        // The tag, not the name: app_bridge.jinja's script reads it.
        assert!(
            !render(json!(null)).contains(r#"<meta name="oeee-presence""#),
            "{template_name}"
        );
    }

    let room = env
        .get_template("presence_meta.jinja")
        .unwrap()
        .render(context! {
            presence => json!({"activity": "collaborating", "community": null, "group": "0123abcd"}),
        })
        .unwrap();
    assert!(room.contains(r#"content="collaborating" data-group="0123abcd" />"#));
}

#[test]
fn drawing_pages_mount_the_offline_painter() {
    let env = test_support::env();
    let config = r##"{"width":640,"height":480,"communityId":"9c881320-2b43-4afa-b2bb-7128c8a3e985","mode":{"kind":"two-tone","backgroundColor":"#ffffff","foregroundColor":"#000000"}}"##;

    {
        let template_name = "draw_post_cucumber.jinja";
        let rendered = env
            .get_template(template_name)
            .unwrap_or_else(|e| panic!("{template_name} loads: {e:#}"))
            .render(context! {
                painter_config => config,
                parent_post => json!(null),
                community_name => "Two Tone",
                current_user => json!(null),
                messages => Vec::<serde_json::Value>::new(),
                draft_post_count => 0,
                unread_notification_count => 0,
                ftl_lang => "en",
            })
            .unwrap_or_else(|e| panic!("{template_name} renders: {e:#}"));

        assert!(rendered.contains("id=\"neo-cucumber-root\""));
        assert!(rendered.contains("/static/neo-cucumber/offline.js"));
        assert!(rendered.contains("/static/neo-cucumber/offline.css"));
        // The error reporter is a script of its own; frontend/painter/sentry.ts.
        assert!(rendered.contains("/static/neo-cucumber/sentry.js"));
        assert!(rendered.contains("\"kind\":\"two-tone\""));
        assert!(!rendered.contains("neo.js"));
        assert!(rendered.contains("html, body { width: 100%; height: 100%; margin: 0; }"));
        assert!(rendered.contains("body { overflow: hidden; }"));
        // The painter fills the element it is mounted into and nothing
        // more -- it used to pin itself to the viewport, which painted its
        // ground over anything a host drew above it. A page that is
        // nothing but the painter has to hand it the screen itself, and
        // without this the painter has no height at all.
        assert!(rendered.contains("#neo-cucumber-root {"));
        assert!(rendered.contains("height: 100dvh;"));
        // Saving leaves this page for good, so the adapter asks first --
        // and it asks in the page's words, because the page is the only
        // side of this that knows the reader's language. `entry.ts` reads
        // them off the button's dataset and falls back to English without
        // them, which nobody would notice until a Korean reader met an
        // English dialog. (Stubbed ftl_get_message echoes the id.)
        assert!(rendered.contains("data-confirm=\"draw-save-confirm\""));
        assert!(rendered.contains("data-cancel=\"cancel\""));
    }
}

#[test]
fn banner_pages_mount_the_small_offline_painter() {
    let env = test_support::env();
    let config = r#"{"width":200,"height":40,"submission":{"kind":"banner","profileUrl":"/@artist"},"mode":{"kind":"standard"}}"#;

    {
        let template_name = "draw_banner.jinja";
        let rendered = env
            .get_template(template_name)
            .unwrap_or_else(|e| panic!("{template_name} loads: {e:#}"))
            .render(context! {
                painter_config => config,
                current_user => json!({ "login_name": "artist" }),
                messages => Vec::<serde_json::Value>::new(),
                draft_post_count => 0,
                unread_notification_count => 0,
                ftl_lang => "en",
            })
            .unwrap_or_else(|e| panic!("{template_name} renders: {e:#}"));

        assert!(rendered.contains("/static/neo-cucumber/offline.js"));
        assert!(rendered.contains("\"height\":40"));
        assert!(rendered.contains("\"kind\":\"banner\""));
        assert!(!rendered.contains("neo.js"));
        assert!(rendered.contains("data-confirm=\"draw-save-confirm\""));
        assert!(rendered.contains("data-cancel=\"cancel\""));
    }
}

/// Guests draw now, and keep what they drew in the browser. The pages that
/// serve them render with no one signed in, and the words the painter's
/// dialogs use arrive as JSON inside the page, which has to stay JSON once
/// every message in it has been escaped for HTML.
fn guest_chrome() -> minijinja::Value {
    context! {
        current_user => json!(null),
        messages => Vec::<serde_json::Value>::new(),
        draft_post_count => 0,
        unread_notification_count => 0,
        ftl_lang => "en",
    }
}

fn json_script(rendered: &str, id: &str) -> serde_json::Value {
    let open = format!(r#"<script id="{id}" type="application/json">"#);
    let start = rendered.find(&open).expect("page has the script") + open.len();
    let end = start
        + rendered[start..]
            .find("</script>")
            .expect("script is closed");
    serde_json::from_str(&rendered[start..end]).expect("the script holds JSON")
}

#[test]
fn a_guest_painter_says_where_the_drawing_is_kept() {
    let env = test_support::env();
    let rendered = env
        .get_template("draw_post_cucumber.jinja")
        .expect("painter template loads")
        .render(context! {
            width => 300,
            height => 300,
            tool => "neo",
            painter_config => "{}",
            ..guest_chrome()
        })
        .expect("painter renders for a guest");
    assert!(
        rendered.contains("draw-guest-notice"),
        "the painter does not warn a guest"
    );
    let words = json_script(&rendered, "oeee-painter-words");
    assert_eq!(words["guestSaved"], "draw-guest-saved");
    assert_eq!(words["downloadPng"], "draw-download-png");
}

#[test]
fn a_signed_in_painter_has_no_guest_notice() {
    let env = test_support::env();
    let rendered = env
        .get_template("draw_post_cucumber.jinja")
        .expect("painter template loads")
        .render(context! {
            width => 300,
            height => 300,
            tool => "neo",
            painter_config => "{}",
            current_user => json!({
                "id": "00000000-0000-0000-0000-000000000001",
                "login_name": "someone",
                "display_name": "Someone",
                "email_verified_at": "2026-01-01T00:00:00Z",
            }),
            messages => Vec::<serde_json::Value>::new(),
            draft_post_count => 0,
            unread_notification_count => 0,
            ftl_lang => "en",
        })
        .expect("painter renders");
    assert!(!rendered.contains("draw-guest-notice"));
    assert!(rendered.contains(r#"data-user-id="00000000-0000-0000-0000-000000000001""#));
    // Draw asks for a size on the painter page too.
    assert!(rendered.contains(r#"id="draw-dialog""#));
}

#[test]
fn a_guest_can_start_a_drawing_and_find_its_drafts() {
    let env = test_support::env();
    let rendered = env
        .get_template("draft_posts.jinja")
        .expect("drafts template loads")
        .render(context! { posts => Vec::<serde_json::Value>::new(), ..guest_chrome() })
        .expect("drafts render for a guest");
    // The toolbar's draw button, signed out as well as in, and the
    // dialog it opens to ask for the canvas size.
    assert!(rendered.contains(r#"id="nav-draw-button""#));
    let dialog = rendered
        .find(r#"id="draw-dialog""#)
        .expect("the size dialog");
    let size = &rendered[dialog..];
    assert!(size.contains(r#"<form action="/draw" method="post">"#));
    assert!(size.contains(r#"<select name="width""#));
    assert!(size.contains(r#"<select name="height""#));
    // This browser's drafts, listed by the page's script, and the
    // guest's drafts square in the toolbar, which its script fills.
    assert!(rendered.contains(r#"id="local-drafts""#));
    assert!(rendered.contains(r#"data-user-id="""#));
    assert!(rendered.contains(r#"id="local-drafts-empty""#));
    assert!(rendered.contains("/static/neo-cucumber/drafts.js"));
    assert!(rendered.contains("toolbar-drafts\" href=\"/posts/drafts\""));
    // No server drafts to arrange for someone with none.
    assert!(!rendered.contains(r#"id="post-feed-grid""#));
    let words = json_script(&rendered, "local-drafts-words");
    assert_eq!(words["communityDenied"], "drafts-local-community-denied");
}
