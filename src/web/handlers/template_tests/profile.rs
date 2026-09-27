//! A person's profile: their drawings, their words, who they follow.

use crate::web::handlers::test_support;
use minijinja::context;
use serde_json::json;

use super::chrome;

/// Achievements along the foot of the profile card, a badge each, named,
/// with what it was for in its tooltip; no strip at all for someone with
/// none.
#[test]
fn the_profile_shows_what_its_owner_has_achieved() {
    let env = test_support::env();
    let render = |achievements: serde_json::Value| {
        env.get_template("profile.jinja")
            .expect("profile loads")
            .render(context! {
                user => json!({
                    "id": "b95e3d1e-5a25-4d0a-9d3a-3a0b0a9b1c2d",
                    "login_name": "oeee",
                    "display_name": "오이",
                    "created_at": "2024-03-05T12:00:00Z",
                }),
                banner => json!(null),
                links => Vec::<serde_json::Value>::new(),
                followings => json!([{
                    "login_name": "a", "display_name": "에이",
                    "banner_image_filename": "abcdef.png",
                    "banner_image_width": 200, "banner_image_height": 40,
                }]),
                achievements,
                public_count => 0,
                public_feed => json!({"posts": [], "headings": [], "has_more": false, "next_url": ""}),
                private_count => 0,
                private_feed => json!({"posts": [], "headings": [], "has_more": false, "next_url": ""}),
                domain => "oeee.cafe",
                is_following => false,
                ..chrome()
            })
            .expect("profile renders")
    };
    let with = render(json!([
        {"achievement": "FIRST_DRAWING", "key": "first-drawing", "earned_at": "2026-09-22T00:00:00Z"},
        {"achievement": "STEAM_SUPPORTER", "key": "steam-supporter", "earned_at": "2026-09-22T01:00:00Z"},
    ]));
    assert!(with.contains("profile-achievements"));
    assert!(with.contains(r#"title="achievement-first-drawing-description"#));
    assert!(with.contains("achievement-steam-supporter"));
    // Each with its own Material Symbols icon: the brush for a first
    // drawing, the game controller for buying on Steam.
    assert_eq!(with.matches(r#"class="achievement-icon""#).count(), 2);
    assert!(with.contains(r#"d="M6 21q-1.125 0-2.225-.55T2 19"#));
    assert!(with.contains(r#"d="M4.55 19q-1.275 0-1.975-.888"#));
    assert!(!render(json!([])).contains("profile-achievements"));
    // In the card; those they follow come after it, and the drawings
    // after them.
    let at = |needle: &str| with.find(needle).unwrap_or_else(|| panic!("no {needle}"));
    assert!(at("profile-achievements") < at("profile-follows"));
    assert!(at("profile-follows") < at("data-profile-panel=\"public\""));
}

/// A profile's drawings are the feeds' grid and cards, rendered here from
/// what the handler hands them: a sensitive drawing blurred as it is
/// everywhere else (the bare grid this replaced showed it plainly),
/// headed by month, loading on from the profile's own endpoint, and
/// without whose drawing each is, since every one is theirs.
#[test]
fn a_profiles_drawings_are_the_shared_grid() {
    use crate::models::post::SerializablePostForHome;
    use crate::web::handlers::home::{feed_context, HOME_POSTS_PER_BATCH};

    let now = chrono::Utc::now();
    let posts = (0..HOME_POSTS_PER_BATCH as u128)
        .map(|i| SerializablePostForHome {
            id: uuid::Uuid::from_u128(i + 1),
            title: Some(format!("Drawing {i}")),
            author_id: uuid::Uuid::from_u128(999),
            user_login_name: "oeee".into(),
            paint_duration: "0".to_string(),
            stroke_count: 1,
            viewer_count: 0,
            image_filename: "abcdef.png".to_string(),
            image_width: 300,
            image_height: 300,
            replay_filename: None,
            is_sensitive: i == 0,
            community_slug: Some("open".to_string()),
            community_name: Some("Open Studio".to_string()),
            published_at: Some(now),
            created_at: now,
            updated_at: now,
        })
        .collect();
    let empty = json!({"posts": [], "headings": [], "has_more": false, "next_url": ""});
    let rendered = test_support::env()
        .get_template("profile.jinja")
        .expect("profile loads")
        .render(context! {
            user => json!({"id": "u1", "login_name": "oeee", "display_name": "오이", "created_at": "2024-03-05T12:00:00Z"}),
            banner => json!(null),
            links => Vec::<serde_json::Value>::new(),
            followings => Vec::<serde_json::Value>::new(),
            achievements => Vec::<serde_json::Value>::new(),
            comments => json!({"rows": [], "next_url": null, "by_drawing": true}),
            comment_count => 0,
            public_count => 75,
            public_feed => feed_context(posts, "/api/profiles/@oeee/posts", 0, None),
            private_count => 0,
            private_feed => empty,
            domain => "oeee.cafe",
            is_following => false,
            ..chrome()
        })
        .expect("profile renders");

    assert!(rendered.contains(r#"<div class="profile-drawings" data-profile-panel="public">"#));
    assert!(rendered.contains("post-card-byline"), "the shared card");
    assert!(
        rendered.contains(r#"class="sensitive""#),
        "blurred, as everywhere else"
    );
    // The heading element: the toolbar's skeleton script carries the
    // class too.
    assert_eq!(
        rendered.matches(r#"<h3 class="feed-period">"#).count(),
        1,
        "headed by month"
    );
    let links_in = rendered.replace("&#x2f;", "/").replace("&amp;", "&");
    assert!(links_in.contains(&format!(
        r#"hx-get="/api/profiles/@oeee/posts?offset={0}&limit={0}&period="#,
        HOME_POSTS_PER_BATCH
    )));
}

/// What they have said gets its own tab, counted in full, a row per
/// comment leading to the drawing it is on, and a sentinel that scrolls
/// in the next batch while there is one; someone who has said nothing
/// gets no tab.
#[test]
fn the_profile_lists_what_its_owner_has_said() {
    let env = test_support::env();
    // As profile.rs's comments_batch hands them to comments_fragment.jinja.
    let render = |rows: serde_json::Value, comment_count: i64, next_url: Option<&str>| {
        env.get_template("profile.jinja")
            .expect("profile loads")
            .render(context! {
                user => json!({
                    "id": "b95e3d1e-5a25-4d0a-9d3a-3a0b0a9b1c2d",
                    "login_name": "oeee",
                    "display_name": "오이",
                    "created_at": "2024-03-05T12:00:00Z",
                }),
                banner => json!(null),
                links => Vec::<serde_json::Value>::new(),
                followings => Vec::<serde_json::Value>::new(),
                achievements => Vec::<serde_json::Value>::new(),
                comments => json!({"rows": rows, "next_url": next_url, "by_drawing": true}),
                comment_count,
                public_count => 0,
                public_feed => json!({"posts": [], "headings": [], "has_more": false, "next_url": ""}),
                private_count => 0,
                private_feed => json!({"posts": [], "headings": [], "has_more": false, "next_url": ""}),
                domain => "oeee.cafe",
                is_following => false,
                ..chrome()
            })
            .expect("profile renders")
    };
    // As `NotificationComment` serialises.
    let with = render(
        json!([{
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
        }]),
        45,
        Some("/@oeee/comments?after=0c8f0000-0000-0000-0000-000000000001"),
    );
    assert!(with.contains(r#"data-profile-tab="comments""#));
    assert!(with.contains(r#"data-profile-panel="comments""#));
    assert!(with.contains(r#"href="/@cat/0c8f0000-0000-0000-0000-000000000002""#));
    assert!(with.contains("멋져요"));
    // Headed by the drawing, not by its owner's own name on every row.
    assert!(with.contains(r#"href="/@cat/0c8f0000-0000-0000-0000-000000000002">고양이</a>"#));
    assert!(with.contains(r#"<span class="ds-handle">@cat</span> · "#));
    assert!(!with.contains("comment-row-post"));
    assert!(with.contains(r#"<span class="profile-tab-count">45</span>"#));
    // Minijinja escapes the slashes in an attribute; the browser reads
    // them back as the URL.
    assert!(with
        .replace("&#x2f;", "/")
        .contains(r#"hx-get="/@oeee/comments?after=0c8f0000-0000-0000-0000-000000000001""#));

    // The scrolled batches come from the fragment alone, which has to
    // stand without the profile's context.
    let last_batch = env
        .get_template("comments_fragment.jinja")
        .expect("fragment loads")
        .render(context! {
            comments => json!({"rows": [], "next_url": null, "by_drawing": true}),
            ftl_lang => "en",
        })
        .expect("fragment renders");
    assert!(!last_batch.contains("infinite-scroll-sentinel"));

    // Each tab is a link to its own address, and the address picks the
    // one showing: /@oeee shows the drawings, /@oeee/comments the
    // comments, with the column control that only drawings have hidden.
    let links = with.replace("&#x2f;", "/");
    assert!(links.contains(r#"<a href="/@oeee" data-profile-tab="public" aria-current="page">"#));
    assert!(links.contains(r#"<a href="/@oeee/comments" data-profile-tab="comments">"#));
    assert!(with.contains(r#"<div data-profile-panel="comments" hidden>"#));
    let on_comments = env
        .get_template("profile.jinja")
        .unwrap()
        .render(context! {
            user => json!({"id": "u1", "login_name": "oeee", "display_name": "오이", "created_at": "2024-03-05T12:00:00Z"}),
            banner => json!(null),
            links => Vec::<serde_json::Value>::new(),
            followings => Vec::<serde_json::Value>::new(),
            achievements => Vec::<serde_json::Value>::new(),
            comments => json!({"rows": [], "next_url": null, "by_drawing": true}),
            comment_count => 3,
            tab => "comments",
            public_count => 1,
            public_feed => json!({"posts": [{"id": "p1", "title": "t", "user_login_name": "oeee", "image_filename": "abcdef.png", "image_width": 300, "image_height": 300, "is_sensitive": false, "published_at": "2026-08-01T00:00:00Z"}], "headings": [], "has_more": false, "next_url": ""}),
            private_count => 0,
            private_feed => json!({"posts": [], "headings": [], "has_more": false, "next_url": ""}),
            domain => "oeee.cafe",
            is_following => false,
            ..chrome()
        })
        .unwrap()
        .replace("&#x2f;", "/");
    assert!(on_comments
        .contains(r#"<a href="/@oeee/comments" data-profile-tab="comments" aria-current="page">"#));
    assert!(on_comments.contains(r#"<div data-profile-panel="comments">"#));
    assert!(on_comments
        .contains(r#"<div class="profile-drawings" data-profile-panel="public" hidden>"#));
    assert!(on_comments.contains("<div data-profile-per-row hidden>"));

    let without = render(json!([]), 0, None);
    assert!(!without.contains("data-profile-tab=\"comments\""));
    assert!(!without.contains("data-profile-panel=\"comments\""));
}

/// Under the handle, the month they joined -- in Seoul, so an account
/// made on the evening of 29 February UTC joined in March. The locale is
/// handed numbers, not a formatted date, and the `<time>` carries the
/// machine-readable month.
#[test]
fn the_profile_says_when_its_owner_joined() {
    let env = test_support::env();
    let rendered = env
        .get_template("profile.jinja")
        .expect("profile loads")
        .render(context! {
            user => json!({
                "id": "u1",
                "login_name": "oeee",
                "display_name": "오이",
                // What chrono's serde writes for a `DateTime<Utc>`.
                "created_at": "2024-02-29T16:30:00.123456Z",
            }),
            banner => json!(null),
            links => Vec::<serde_json::Value>::new(),
            followings => Vec::<serde_json::Value>::new(),
            achievements => Vec::<serde_json::Value>::new(),
            public_count => 0,
            public_feed => json!({"posts": [], "headings": [], "has_more": false, "next_url": ""}),
            private_count => 0,
            private_feed => json!({"posts": [], "headings": [], "has_more": false, "next_url": ""}),
            domain => "oeee.cafe",
            is_following => false,
            ..chrome()
        })
        .expect("profile renders");
    assert!(
        rendered
            .contains(r#"<time datetime="2024-03">profile-member-since(month=3,year=2024)</time>"#),
        "{rendered}"
    );
    let at = |needle: &str| {
        rendered
            .find(needle)
            .unwrap_or_else(|| panic!("no {needle}"))
    };
    assert!(at("profile-handle") < at("profile-joined"));
}

/// Every year they have supported, earliest first, each on the platform
/// that year's pack was bought on -- including years that have passed,
/// whose mark they no longer wear.
#[test]
fn a_supporters_profile_says_so_first() {
    let env = test_support::env();
    let render = |supporter_standings: serde_json::Value| {
        env.get_template("profile.jinja")
            .expect("profile loads")
            .render(context! {
                user => json!({"id": "u1", "login_name": "oeee", "display_name": "오이", "created_at": "2024-03-05T12:00:00Z"}),
                banner => json!(null),
                links => Vec::<serde_json::Value>::new(),
                followings => Vec::<serde_json::Value>::new(),
                achievements => Vec::<serde_json::Value>::new(),
                supporter_standings,
                public_count => 0,
                public_feed => json!({"posts": [], "headings": [], "has_more": false, "next_url": ""}),
                private_count => 0,
                private_feed => json!({"posts": [], "headings": [], "has_more": false, "next_url": ""}),
                domain => "oeee.cafe",
                is_following => false,
                ..chrome()
            })
            .expect("profile renders")
    };
    let supporter = render(json!([
        {"store": "steam", "year": 2026, "since": "2026-09-22T00:00:00Z"},
        {"store": "apple", "year": 2027, "since": "2027-01-04T00:00:00Z"},
    ]));
    let chip = supporter.find("supporter-chip").expect("a supporter chip");
    assert!(
        chip < supporter.find("/@oeee/guestbook").unwrap(),
        "before the guestbook"
    );
    assert!(supporter.contains(r#"href="/about#supporters""#));
    assert_eq!(
        supporter.matches("supporter-chip").count(),
        2,
        "one per year"
    );
    let steam = supporter
        .find("supporter-badge-steam")
        .expect("the Steam chip");
    let apple = supporter
        .find("supporter-badge-apple")
        .expect("the App Store chip");
    assert!(steam < apple, "earliest year first");
    // The year is the chip, and the platform is what it is read as.
    assert!(supporter.contains("🎮</span>2026</a>"), "{supporter}");
    assert!(supporter.contains("🍎</span>2027</a>"));
    assert!(supporter.contains("supporter-year(platform=supporter-badge-steam,year=2026)"));
    assert!(!render(json!([])).contains("supporter-chip"));
}

/// Following, in its own section under the card with its count:
/// everyone the same shape, a banner where they have drawn one and a
/// frame of the same size holding their name where they have not. No
/// section for someone who follows nobody, and following is never a
/// tab, so with only drawings there is no switch.
#[test]
fn the_profile_shows_everyone_followed_the_same_way() {
    let env = test_support::env();
    let render = |followings: serde_json::Value| {
        env.get_template("profile.jinja")
            .expect("profile loads")
            .render(context! {
                user => json!({"id": "u1", "login_name": "oeee", "display_name": "오이", "created_at": "2024-03-05T12:00:00Z"}),
                banner => json!(null),
                links => Vec::<serde_json::Value>::new(),
                followings,
                achievements => Vec::<serde_json::Value>::new(),
                public_count => 0,
                public_feed => json!({"posts": [], "headings": [], "has_more": false, "next_url": ""}),
                private_count => 0,
                private_feed => json!({"posts": [], "headings": [], "has_more": false, "next_url": ""}),
                domain => "oeee.cafe",
                is_following => false,
                ..chrome()
            })
            .expect("profile renders")
    };
    let banner = json!({
        "login_name": "a", "display_name": "에이",
        "banner_image_filename": "abcdef.png", "banner_image_width": 200, "banner_image_height": 40,
    });
    let plain = json!({"login_name": "b", "display_name": "비", "banner_image_filename": null});

    let both = render(json!([banner, plain]));
    assert!(both.contains(r#"<div class="profile-section-label">profile-following<span class="profile-tab-count">2</span></div>"#));
    assert!(!both.contains("data-profile-tab"), "following is not a tab");
    assert_eq!(both.matches(r#"class="profile-follow""#).count(), 2);
    assert!(both.contains(r#"<a class="profile-follow" href="/@a""#));
    assert!(both.contains("/image/ab/abcdef.png"));
    assert!(both.contains(r#"<a class="profile-follow" href="/@b""#));
    assert!(both.contains(r#"profile-follow-blank" aria-hidden="true">비</span>"#));

    let nobody = render(json!([]));
    assert!(!nobody.contains("data-profile-tab"), "one grid, no switch");
    assert!(!nobody.contains("profile-follows"));
    assert!(nobody.contains(r#"<div class="profile-section-label">profile-tab-drawings</div>"#));
}

/// On their own profile, what the private drawings are sits beside the
/// switch rather than over the grid, and only while that tab is chosen;
/// /@name/private is the address that chooses it.
#[test]
fn the_private_note_sits_beside_the_switch() {
    let env = test_support::env();
    let render = |tab: &str| {
        env.get_template("profile.jinja")
            .expect("profile loads")
            .render(context! {
                user => json!({"id": "u1", "login_name": "oeee", "display_name": "오이", "created_at": "2024-03-05T12:00:00Z"}),
                current_user => json!({"id": "u1", "login_name": "oeee"}),
                banner => json!(null),
                links => Vec::<serde_json::Value>::new(),
                followings => Vec::<serde_json::Value>::new(),
                achievements => Vec::<serde_json::Value>::new(),
                comment_count => 0,
                tab,
                public_count => 0,
                public_feed => json!({"posts": [], "headings": [], "has_more": false, "next_url": ""}),
                private_count => 0,
                private_feed => json!({"posts": [], "headings": [], "has_more": false, "next_url": ""}),
                domain => "oeee.cafe",
                is_following => false,
                messages => Vec::<serde_json::Value>::new(),
                draft_post_count => 0,
                unread_notification_count => 0,
                ftl_lang => "en",
            })
            .expect("profile renders")
            .replace("&#x2f;", "/")
    };
    let private = render("private");
    let at = |needle: &str| {
        private
            .find(needle)
            .unwrap_or_else(|| panic!("no {needle}"))
    };
    assert!(private
        .contains(r#"<a href="/@oeee/private" data-profile-tab="private" aria-current="page">"#));
    assert!(private.contains(r#"<p class="profile-panel-note" data-profile-note="private">"#));
    assert!(at("profile-tabs-bar") < at("profile-panel-note"));
    assert!(at("profile-panel-note") < at("data-profile-panel=\"public\""));
    assert!(render("public").contains(r#"data-profile-note="private" hidden>"#));
}

/// What a visitor can do about someone: follow them and sign their
/// guestbook side by side, and report them from the menu after -- never
/// a button at Follow's weight. Someone signed out gets the guestbook
/// and nothing that needs an account.
#[test]
fn a_profile_keeps_reporting_behind_its_menu() {
    let env = test_support::env();
    let render = |current_user: serde_json::Value| {
        env.get_template("profile.jinja")
            .expect("profile loads")
            .render(context! {
                user => json!({"id": "u1", "login_name": "oeee", "display_name": "오이", "created_at": "2024-03-05T12:00:00Z"}),
                banner => json!({"image_filename": "abcdef.png", "width": 200, "height": 40}),
                links => Vec::<serde_json::Value>::new(),
                followings => Vec::<serde_json::Value>::new(),
                achievements => Vec::<serde_json::Value>::new(),
                public_count => 0,
                public_feed => json!({"posts": [], "headings": [], "has_more": false, "next_url": ""}),
                private_count => 0,
                private_feed => json!({"posts": [], "headings": [], "has_more": false, "next_url": ""}),
                domain => "oeee.cafe",
                is_following => false,
                r2_public_endpoint_url => "https://images.example",
                current_user,
                ..chrome()
            })
            .expect("profile renders")
    };
    let visitor = render(json!({"id": "u2", "login_name": "fan", "display_name": "Fan"}));
    let at = |needle: &str| {
        visitor
            .find(needle)
            .unwrap_or_else(|| panic!("no {needle}"))
    };
    assert!(at("/@oeee/follow") < at("/@oeee/guestbook"));
    assert!(at("/@oeee/guestbook") < at(r#"<details class="toolbar-menu profile-more">"#));
    assert!(at(r#"<details class="toolbar-menu profile-more">"#) < at("showProfileReportModal()"));
    // Their banner, not a link for someone who cannot redraw it.
    assert!(visitor.contains(r#"<span class="profile-banner">"#));

    let owner = render(json!({"id": "u1", "login_name": "oeee", "display_name": "오이"}));
    // The head names the menu's items among what is felt as a press
    // (theme_head.jinja), so it is the menu itself that is looked for.
    assert!(!owner.contains(r#"<details class="toolbar-menu profile-more">"#));
    assert!(owner.contains(r#"<a class="profile-banner" href="/banners/draw""#));
    assert!(owner.contains(r#"data-profile-tab="private""#));

    let signed_out = render(json!(null));
    assert!(signed_out.contains("/@oeee/guestbook"));
    assert!(!signed_out.contains("/@oeee/follow"));
    assert!(!signed_out.contains(r#"<details class="toolbar-menu profile-more">"#));
}

#[test]
fn the_banner_grid_renders_the_shape_its_handler_passes() {
    // `list_user_banners` returns a real DateTime and a real bool, and the
    // grid pipes the first through `datetimeformat` and branches on the
    // second. Parsing sees none of that, and both buttons on this page now
    // swap this template in, so a render failure would take out activating
    // and deleting rather than one page load.
    let env = test_support::env();
    let template = env
        .get_template("banner_grid.jinja")
        .unwrap_or_else(|e| panic!("banner_grid.jinja loads: {e:#}"));

    let banner = |is_active: bool| {
        json!({
            "id": "6f2b4e4c-95f6-4d8a-9c47-1f2f3f4a5b6c",
            "image_filename": "abcd1234.png",
            "created_at": "2026-08-29T04:00:00Z",
            "is_active": is_active,
        })
    };
    let rendered = template
        .render(context! {
            banners => vec![
                (banner(true), "https://img.example/image/ab/abcd1234.png"),
                (banner(false), "https://img.example/image/ab/abcd1234.png"),
            ],
            ftl_lang => "en",
        })
        .unwrap_or_else(|e| panic!("banner_grid.jinja renders: {e:#}"));

    assert!(
        rendered.contains("id=\"banner-grid\""),
        "both buttons target this id; without it their swaps go nowhere"
    );
    // One card is active and shows no buttons, the other shows both.
    assert_eq!(
        rendered.matches("hx-target=\"#banner-grid\"").count(),
        2,
        "the inactive card should offer exactly activate and delete"
    );
    assert!(
        !rendered.contains("location.reload"),
        "these buttons stopped reloading the page"
    );
}

#[test]
fn follow_and_unfollow_each_carry_the_others_words() {
    // Swapped in by the follow handlers with only these keys; the press
    // shows the other button's words before the response (optimistic.jinja).
    let env = test_support::env();
    let render = |name: &str| {
        env.get_template(name)
            .unwrap_or_else(|e| panic!("{name} loads: {e:#}"))
            .render(context! {
                current_user => json!({"id": "reader"}),
                user => json!({"id": "artist", "login_name": "artist"}),
                ftl_lang => "en",
            })
            .unwrap_or_else(|e| panic!("{name} renders: {e:#}"))
    };
    let follow = render("follow_button.jinja");
    assert!(follow.contains("data-optimistic-toggle"));
    assert!(
        follow.contains(r#"data-pressed-label="unfollow""#),
        "got: {follow}"
    );
    assert!(follow.contains(r#"hx-sync="this:drop""#));
    assert!(
        !follow.contains("hx-disable"),
        "a disabled button would dim the answer shown"
    );
    let unfollow = render("unfollow_button.jinja");
    assert!(
        unfollow.contains(r#"data-pressed-label="follow""#),
        "got: {unfollow}"
    );
}
