//! What every page carries: the toolbar, its menus and the toasts.

use crate::web::handlers::test_support;
use minijinja::context;
use serde_json::json;

use super::chrome;
use minijinja::value::Serde;

/// The heart in the toolbar is there when this deployment sells a pack
/// at all *and* the reader is signed in -- the pack is bought against an
/// account. Which store they are in front of hides or shows it before
/// paint, and is not the template's business.
#[test]
fn the_toolbar_has_a_heart_only_for_a_signed_in_reader_where_a_pack_is_sold() {
    let env = test_support::env();
    let render = |on_sale: serde_json::Value, current_user: serde_json::Value| {
        env.get_template("toolbar.jinja")
            .expect("toolbar loads")
            .render(context! {
                supporter_packs_on_sale => Serde(on_sale),
                current_user => Serde(current_user),
                ..chrome()
            })
            .expect("toolbar renders")
    };
    let reader = json!({"login_name": "oeee", "display_name": "오이"});

    let selling = render(json!(true), reader.clone());
    assert!(selling
        .contains(r#"class="toolbar-square toolbar-button toolbar-supporter" href="/supporter""#));

    // Nothing to sell: no heart for anyone.
    assert!(!render(json!(false), reader.clone()).contains("toolbar-supporter"));
    assert!(!render(json!(null), reader).contains("toolbar-supporter"));

    // Selling, but nobody to sell to yet: /supporter is still there to
    // read, it is just not in the bar.
    assert!(!render(json!(true), json!(null)).contains("toolbar-supporter"));
}

/// Search in the bar is a button for the quick switcher (jump.jinja),
/// the one Ctrl+K opens, and no longer a field of its own. It is a link
/// to /search underneath, which is where it goes with no script.
#[test]
fn the_bars_search_button_opens_the_quick_switcher() {
    let env = test_support::env();
    let bar = env
        .get_template("toolbar.jinja")
        .expect("toolbar loads")
        .render(chrome())
        .expect("toolbar renders");

    assert!(bar.contains(r#"class="toolbar-square toolbar-button toolbar-search""#));
    assert!(
        bar.contains(r#"href="/search""#),
        "somewhere to go with no script"
    );
    assert!(bar.contains("window.oeeeJump.open()"));
    assert!(
        !bar.contains("toolbar-search-field") && !bar.contains(r#"type="search""#),
        "the bar holds no search field of its own"
    );
}

/// What the site is stays somewhere a reader would look for it. Signed
/// in that is the foot of the account menu, under their own name;
/// signed out there is no such menu, and the only square at the bar's
/// end is the theme's — so it is a button in the bar, not a line under
/// a sun. `/about` is reachable from the toolbar and nowhere else, so
/// the menu it is hidden in is the whole way in.
#[test]
fn about_is_in_the_bar_signed_out_and_in_the_account_menu_signed_in() {
    let env = test_support::env();
    let render = |current_user: serde_json::Value| {
        env.get_template("toolbar.jinja")
            .expect("toolbar loads")
            .render(context! {
                current_user => Serde(current_user),
                messages => Serde(Vec::<serde_json::Value>::new()),
                draft_post_count => 0,
                unread_notification_count => 0,
                ftl_lang => "en",
            })
            .expect("toolbar renders")
    };

    let out = render(json!(null));
    assert!(
        out.contains(r#"<a class="toolbar-square toolbar-button toolbar-about" href="/about" aria-label="nav-about-menu" title="nav-about-menu"><svg"#),
        "signed out, About is its own button at the bar's end: a mark, with its words for its label"
    );
    assert!(
        out.contains(r#"<a class="toolbar-square toolbar-button toolbar-sign-in" href="/login" aria-label="sign-in" title="sign-in"><svg"#),
        "and signing in is a mark with its words for its label"
    );
    assert!(
        out.contains(r#"id="nav-draw-button" class="toolbar-square toolbar-draw""#),
        "Draw is the filled button signed out too: a guest can draw"
    );
    assert!(
        out.contains(r#"<a class="toolbar-square toolbar-button toolbar-drafts" href="/posts/drafts" hx-boost="false" data-server-count="0" hidden aria-label="drafts" title="drafts"><svg"#),
        "and this browser's drafts are a square of their own, hidden until the script finds some"
    );
    // The theme square opens onto the three-way switch and nothing
    // else: one link to `/about` in the whole bar, and it is not that
    // menu's. (The desktop app's Help menu also reaches it, by script.)
    assert_eq!(out.matches(r#"href="/about""#).count(), 1);
    assert!(!out.contains("toolbar-menu-about"));
    assert!(!out.contains("toolbar-menu-drafts"));

    let signed_in = render(json!({"login_name": "oeee", "display_name": "오이"}));
    assert!(
        signed_in.contains(r#"<a class="toolbar-menu-about" href="/about">"#),
        "signed in, About is the last item of the account menu"
    );
    assert_eq!(signed_in.matches(r#"href="/about""#).count(), 1);
    // Signed in with none, the drafts square is there but hidden, for
    // the script to show if this browser holds some.
    assert!(signed_in.contains(r#"toolbar-drafts" href="/posts/drafts" hx-boost="false" data-server-count="0" hidden aria-label="drafts""#));
}

/// Drafts on the server show the drafts square from the first paint,
/// counted in its label, beside the account menu's line for them.
#[test]
fn an_account_with_drafts_has_the_drafts_square_in_the_bar() {
    let env = test_support::env();
    let out = env
        .get_template("toolbar.jinja")
        .expect("toolbar loads")
        .render(context! {
            current_user => Serde(json!({"id": "00000000-0000-0000-0000-000000000001", "login_name": "oeee", "display_name": "오이"})),
            messages => Serde(Vec::<serde_json::Value>::new()),
            draft_post_count => 3,
            unread_notification_count => 0,
            ftl_lang => "en",
        })
        .expect("toolbar renders");
    assert!(out.contains(r#"<a class="toolbar-square toolbar-button toolbar-drafts" href="/posts/drafts" hx-boost="false" data-server-count="3" aria-label="drafts (3)" title="drafts (3)"><svg"#));
    // Drafts are the square's alone: no line in the account menu, and
    // the person does not pulse.
    assert!(!out.contains("toolbar-menu-drafts"));
    assert!(!out.contains("toolbar-avatar-pulse"));
}

/// The design system's reference page draws every component, so it has
/// to render -- a broken one is the first place a change to ds.css shows.
#[test]
fn the_design_reference_renders_every_component() {
    let rendered = test_support::env()
        .get_template("design.jinja")
        .expect("design.jinja loads")
        .render(chrome())
        .expect("design.jinja renders");
    for class in [
        "ds-button-primary",
        "ds-select",
        "ds-input",
        "ds-segmented",
        "ds-window",
        "ds-notice-error",
    ] {
        assert!(
            rendered.contains(class),
            "{class} missing from the reference"
        );
    }
    assert!(rendered.contains("<body class=\"ds-page\">"));
}

/// The page's stack of toasts (`toasts.jinja`), up to its script.
fn toasts(rendered: &str) -> &str {
    let start = rendered
        .find(r#"<div id="toasts""#)
        .expect("page has a stack of toasts");
    let end = start
        + rendered[start..]
            .find("<script>")
            .expect("the stack's script follows it");
    &rendered[start..end]
}

/// Flash messages are notices, coloured by their level. The context gets
/// axum-messages' own type, whose level serialises as `"Error"`, so this
/// renders that type and not a stand-in shaped the way the template wishes.
#[test]
fn flash_messages_render_as_notices_by_level() {
    let message = |level, text: &str| axum_messages::Message {
        level,
        message: text.to_string(),
        metadata: None,
    };
    let rendered = test_support::env()
        .get_template("design.jinja")
        .expect("design.jinja loads")
        .render(context! {
            current_user => Serde(json!(null)),
            messages => Serde(vec![
                message(axum_messages::Level::Success, "Welcome, Tandemaus"),
                message(axum_messages::Level::Error, "<b>not bold</b>"),
            ]),
            draft_post_count => 0,
            unread_notification_count => 0,
            ftl_lang => "en",
        })
        .expect("design.jinja renders");
    let toasts = toasts(&rendered);
    assert!(
        toasts.contains("ds-notice ds-toast ds-notice-success"),
        "got: {toasts}"
    );
    assert!(
        toasts.contains("ds-notice ds-toast ds-notice-error"),
        "got: {toasts}"
    );
    assert!(toasts.contains("Welcome, Tandemaus"));
    assert!(
        toasts.contains("&lt;b&gt;not bold"),
        "a message is text, not markup"
    );
    assert!(toasts.contains("ds-notice-close"));
    assert!(
        !rendered.contains("<ul class=\"ds-notices\">"),
        "a flash message is a toast, not a row of the header"
    );
}

/// With nothing to say the stack holds nothing at all, so `:empty` keeps
/// it out of the way -- and it is still there for htmx to add to.
#[test]
fn no_flash_messages_render_an_empty_stack() {
    let rendered = test_support::env()
        .get_template("design.jinja")
        .expect("design.jinja loads")
        .render(chrome())
        .expect("design.jinja renders");
    assert!(
        rendered.contains(r#"<div id="toasts" class="ds-toasts" aria-live="polite"></div>"#),
        "got: {}",
        toasts(&rendered)
    );
}

#[test]
fn the_toolbar_marks_the_language_in_use() {
    // The macro reads the page's context from inside the toolbar, and
    // `preferred_language` arrives as a string or as nothing, so this is
    // rendered with the shapes the handlers really pass.
    let render = |current_user: serde_json::Value, ftl_lang: &str| {
        test_support::env()
            .get_template("home.jinja")
            .unwrap_or_else(|e| panic!("home.jinja loads: {e:#}"))
            .render(context! {
                feed => context! {
                    posts => Serde(Vec::<serde_json::Value>::new()),
                    has_more => false,
                    next_url => "",
                },
                current_user => Serde(current_user),
                messages => Serde(Vec::<serde_json::Value>::new()),
                draft_post_count => 0,
                unread_notification_count => 0,
                ftl_lang => ftl_lang,
            })
            .unwrap_or_else(|e| panic!("home.jinja renders: {e:#}"))
    };

    let chose = render(
        json!({ "login_name": "artist", "id": "u1", "preferred_language": "ja" }),
        "ja",
    );
    assert!(chose.contains(r#"<option value="ja" lang="ja" selected>"#));
    assert!(!chose.contains(r#"<option value="auto" selected>"#));
    assert!(!chose.contains("data-guest"));

    let auto = render(
        json!({ "login_name": "artist", "id": "u1", "preferred_language": null }),
        "ko",
    );
    assert!(auto.contains(r#"<option value="auto" selected>"#));

    // Signed out, the language in use; the page's script moves it to
    // Auto when no cookie chose it.
    let guest = render(serde_json::Value::Null, "zh");
    assert!(guest.contains(r#"<option value="zh" lang="zh" selected>"#));
    assert!(guest.contains("data-guest"));
    assert!(guest.contains(r#"action="/language" method="post" hx-boost="false""#));
}

#[test]
fn nothing_in_the_boosted_nav_reaches_a_module_bundle() {
    // The nav carries hx-boost. A boosted navigation swaps the body and
    // re-runs its scripts by cloning the tags, which does *not* re-evaluate
    // a `<script type="module">` the browser has already loaded — the
    // module map is keyed on the URL and `cachebuster` holds it fixed for a
    // deploy. So a nav entry pointing at a page that mounts a painter would
    // work once per session and then quietly stop, with a blank canvas and
    // no error.
    //
    // Everything that mounts one is reached from a page body instead, where
    // boost does not apply. This test is what keeps that true: adding a
    // link to the nav fails here until its destination is listed, which is
    // the moment to check the destination does not load a bundle.
    let env = test_support::env();
    let rendered = env
        .get_template("home.jinja")
        .unwrap_or_else(|e| panic!("home.jinja loads: {e:#}"))
        .render(context! {
            feed => context! {
                posts => Serde(Vec::<serde_json::Value>::new()),
                has_more => false,
                next_url => "",
            },
            current_user => Serde(json!({ "login_name": "artist", "id": "u1" })),
            messages => Serde(Vec::<serde_json::Value>::new()),
            draft_post_count => 0,
            unread_notification_count => 0,
            ftl_lang => "en",
        })
        .unwrap_or_else(|e| panic!("home.jinja renders: {e:#}"));

    let nav = rendered
        .split_once("<nav")
        .and_then(|(_, rest)| rest.split_once("</nav>"))
        .map(|(nav, _)| nav.to_string())
        .expect("base.jinja renders a nav");

    assert!(
        nav.contains("hx-boost:inherited=\"true\""),
        "the nav should still be the boosted scope"
    );
    assert!(
        !rendered.contains("<body hx-boost") && !rendered.contains("<main hx-boost"),
        "boost must stay scoped to the nav; widening it re-admits the drawing routes"
    );

    // Every destination reachable from the nav, and why it is safe.
    // Add here only after checking the page does not mount a bundle.
    let allowed = [
        // The toolbar's logo: home.jinja, which mounts nothing.
        "/",
        "/about",
        "/collaborate",
        "/communities",
        "/tags",
        // search.jinja: the shared post cards and the per-row control's
        // inline script, no bundle.
        "/search",
        "/following",
        "/joined",
        "/notifications",
        "/account",
        "/login",
        "/signup",
        "/posts/drafts",
        // Signing out answers with a redirect to "/", and its form opts
        // out of boost besides, so the signed out document is a fresh
        // one.
        "/logout",
        // The language choice: a redirect back to the page it was made
        // on, from a form that opts out of boost for the same reason.
        "/language",
    ];

    for (attr, _) in [("href=\"", 0), ("action=\"", 0)] {
        for piece in nav.split(attr).skip(1) {
            let target = piece.split('"').next().unwrap_or_default();
            // Profile links carry the viewer's own name.
            if target.starts_with("/@") {
                continue;
            }
            // The draw form is the one nav entry that does reach a bundle,
            // and it says so by opting out of boost.
            if target == "/draw" {
                assert!(
                    nav.contains("action=\"/draw\" method=\"post\" hx-boost=\"false\"")
                        || nav.contains("hx-boost=\"false\""),
                    "the draw form must opt out of boost"
                );
                continue;
            }
            assert!(
                allowed.contains(&target),
                "{target} is new in the boosted nav — confirm it does not load a \
                 <script type=\"module\"> before adding it to the list in this test"
            );
        }
    }
}

#[test]
fn the_notification_chrome_renders_standalone() {
    // Swapped in by handlers as well as included by the page, so it has
    // to stand up with only the keys those handlers pass.
    let env = test_support::env();

    let nav = env
        .get_template("nav_notifications.jinja")
        .unwrap_or_else(|e| panic!("nav_notifications.jinja loads: {e:#}"));
    let with_count = nav
        .render(context! { unread_notification_count => 3, ftl_lang => "en" })
        .expect("nav renders with a count");
    let without = nav
        .render(context! { unread_notification_count => 0, ftl_lang => "en" })
        .expect("nav renders at zero");
    assert!(
        with_count.contains("toolbar-bell-unread") && with_count.contains("(3)"),
        "unread fills the bell and puts the count in its label"
    );
    assert!(
        !without.contains("toolbar-bell-unread") && !without.contains("(0)"),
        "zero unread is a plain bell"
    );
    assert!(
        with_count.contains("id=\"nav-notifications\""),
        "the partial targets this id, so it has to survive its own swap"
    );
}
