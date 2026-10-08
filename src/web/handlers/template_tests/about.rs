//! The about page.

use crate::web::handlers::test_support;
use minijinja::context;
use serde_json::json;

use super::chrome;
use minijinja::value::Serde;

/// The credits: a section on /about that every badge leads to, left out
/// while there is nobody in it.
#[test]
fn the_about_page_thanks_its_supporters() {
    let env = test_support::env();
    let render = |supporters: serde_json::Value| {
        env.get_template("about.jinja")
            .expect("about loads")
            .render(context! {
                supporters => Serde(supporters),
                users_with_public_posts_and_banner => Serde(Vec::<serde_json::Value>::new()),
                ..chrome()
            })
            .expect("about renders")
    };
    let about = render(json!([
        {"login_name": "a", "display_name": "에이", "mark": "steam", "since": "2026-09-22T00:00:00Z"},
        {"login_name": "b", "display_name": "비", "mark": "apple", "since": "2026-09-23T00:00:00Z"},
    ]));
    assert!(about.contains(r#"id="supporters""#));
    // Each chip wears its own platform's mark.
    assert_eq!(about.matches("supporter-mark").count(), 2);
    assert!(about.contains("about-supporters-thanks"));
    let a = about.find(r#"href="/@a""#).expect("a is thanked");
    let b = about.find(r#"href="/@b""#).expect("b is thanked");
    assert!(a < b, "earliest first");
    assert!(!render(json!([])).contains(r#"id="supporters""#));
}

/// The commit that is serving, linked to on GitHub, under the name at
/// the top of the page rather than below every list that grows, and
/// nothing at all outside a deployed image.
#[test]
fn the_about_page_names_the_commit_it_runs() {
    let env = test_support::env();
    let render = |git_commit: Option<&str>| {
        env.get_template("about.jinja")
            .expect("about loads")
            .render(context! {
                supporters => Serde(Vec::<serde_json::Value>::new()),
                users_with_public_posts_and_banner => Serde(Vec::<serde_json::Value>::new()),
                git_commit,
                ..chrome()
            })
            .expect("about renders")
    };
    let sha = "e6851d5a0b1c2d3e4f5a6b7c8d9e0f1a2b3c4d5e";
    let about = render(Some(sha));
    assert!(about.contains(&format!(
        r#"href="https://github.com/oeee-cafe/web/commit/{sha}""#
    )));
    assert!(about.contains(">e6851d5a0b1c<span"), "{about}");
    let version = about.find("about-version").expect("the version is shown");
    let lede = about.find("about-lede").expect("the lede is shown");
    assert!(version < lede, "the version sits under the name");
    assert!(!render(None).contains("about-version"));
}
