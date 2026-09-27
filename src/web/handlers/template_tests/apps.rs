//! What the pages hand the iOS and Android apps.

use crate::web::handlers::test_support;
use minijinja::context;
use serde_json::json;

use super::chrome;

/// The words the apps say over the page (app_bridge.jinja) come from the
/// site's catalogues, and a missing one is not an error anywhere: the
/// real ftl_get_message answers with the message's id, which an app
/// would put in a dialog as it is. So every one is looked for in every
/// language, and the head that carries them is rendered.
#[test]
fn the_apps_are_given_their_words_in_every_language() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    // app_sign_in.jinja says the one thing the page says for the Steam app.
    let bridge = ["app_bridge.jinja", "app_sign_in.jinja"]
        .map(|name| std::fs::read_to_string(root.join("templates").join(name)).unwrap())
        .join("\n");
    let ids: Vec<&str> = bridge
        .split("ftl_get_message(\"")
        .skip(1)
        .filter_map(|rest| rest.split('"').next())
        .collect();
    assert!(ids.len() >= 12, "{ids:?}");
    for lang in ["en", "ko", "ja", "zh"] {
        let ftl = std::fs::read_to_string(root.join(format!("locales/{lang}.ftl"))).unwrap();
        for id in &ids {
            assert!(
                ftl.lines()
                    .any(|line| line.starts_with(&format!("{id} = "))),
                "{id} is missing from {lang}.ftl"
            );
        }
    }

    let head = test_support::env()
        .get_template("theme_head.jinja")
        .unwrap()
        .render(context! { ftl_lang => "en" })
        .unwrap();
    // A string the page hands a script is a JSON literal, not text
    // pasted between quotes.
    assert!(head.contains(r#"leaveTitle: "app-leave-title","#), "{head}");
    assert!(head.contains("window.oeeeApp.signIn = {"));
    assert!(head.contains(r#"OeeeCafe((?: (?:platform|store)\/\w+)+)"#));
}

/// What app_bridge.jinja tells the apps is read from marks the templates
/// make, and an app sees nothing wrong when one goes missing: it is just
/// told 0, or nothing. So the marks are pinned here.
#[test]
fn the_apps_are_told_the_unread_count_and_who_is_signed_in() {
    let env = test_support::env();
    let bell = |count: i64| {
        env.get_template("nav_notifications.jinja")
            .unwrap()
            .render(context! { unread_notification_count => count, ftl_lang => "en" })
            .unwrap()
    };
    let three = bell(3);
    assert!(three.contains(r#"data-unread="3""#), "{three}");
    assert!(bell(0).contains(r#"data-unread="0""#));

    let toolbar = |current_user: serde_json::Value| {
        env.get_template("toolbar.jinja")
            .unwrap()
            .render(context! { current_user, ..chrome() })
            .unwrap()
    };
    assert!(!toolbar(json!(null)).contains("data-signed-in"));
    let signed_in = toolbar(json!({
        "id": "9c881320-2b43-4afa-b2bb-7128c8a3e985",
        "login_name": "reader",
        "display_name": "Reader",
        "email_verified_at": "2026-01-01",
    }));
    assert!(signed_in.contains(r#"data-tauri-drag-region="deep" data-signed-in"#));
    // What the apps call on the page is on one object (app_bridge.jinja);
    // the toolbar adds its commands to it.
    assert!(signed_in.contains("window.oeeeApp.command = command;"));
}

/// Only a drawing that is not blurred is offered to an app's long-press
/// menu, whose preview would show it as it is.
#[test]
fn only_drawings_shown_as_they_are_are_offered_to_the_apps() {
    let card = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("templates/post_card.jinja"),
    )
    .expect("post_card.jinja reads");
    assert!(
        card.contains("{% if not post.is_sensitive and not admin %}data-oeee-drawing{% endif %}")
    );
}
