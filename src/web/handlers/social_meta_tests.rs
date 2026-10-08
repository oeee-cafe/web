//! The link-preview card lives in `base.jinja` and is overridden per page.
//! These render the real templates because the failure mode is silent —
//! a typo'd variable renders as an empty `content=""`, not an error.

use super::test_support;
use minijinja::context;
use minijinja::value::Serde;
use serde_json::json;

fn chrome() -> minijinja::Value {
    context! {
        current_user => Serde(json!(null)),
        messages => Serde(Vec::<serde_json::Value>::new()),
        draft_post_count => 0,
        unread_notification_count => 0,
        ftl_lang => "en",
    }
}

/// The `<head>` with runs of whitespace collapsed. Templates are formatted
/// by djlint, which wraps long tags across lines, so asserting on raw
/// output would break on reformatting rather than on behaviour. Scoping to
/// the head also keeps body text from satisfying a meta-tag assertion.
fn head(rendered: &str) -> String {
    let start = rendered.find("<head>").expect("page has a head");
    let end = rendered.find("</head>").expect("head is closed");
    rendered[start..end]
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

#[test]
fn html_lang_names_the_language_the_page_was_rendered_in() {
    // This used to read `ftl_get_message('lang')`, and no locale defines a
    // `lang` message, so every page shipped `<html lang="lang">` — the
    // fallback for a missing key is the key itself, which is silent.
    for lang in ["ko", "ja", "en", "zh"] {
        let env = test_support::env();
        let rendered = env
            .get_template("404.jinja")
            .expect("404 template loads")
            .render(context! { ftl_lang => lang, ..chrome() })
            .expect("404 renders");

        assert!(
            rendered.contains(&format!(r#"<html lang="{}">"#, lang)),
            "expected the document to declare {lang}, got: {}",
            &rendered[..rendered.find("<head>").unwrap_or(80)]
        );
    }
}

#[test]
fn pages_without_an_override_get_the_site_card() {
    let env = test_support::env();
    let rendered = env
        .get_template("404.jinja")
        .expect("404 template loads")
        .render(chrome())
        .expect("404 renders");

    let head = head(&rendered);
    assert!(head.contains(r#"<meta property="og:title" content="brand" />"#));
    assert!(head.contains(r#"<meta property="og:description" content="about" />"#));
    assert!(
        head.contains(r#"<meta property="og:url" content="https://oeee.test/" />"#),
        "site card should point at the site root"
    );
    assert!(head.contains(r#"<meta name="twitter:card" content="summary" />"#));
}

#[test]
fn public_community_gets_a_card_and_stays_indexable() {
    let env = test_support::env();
    let rendered = env
        .get_template("community.jinja")
        .expect("community template loads")
        .render(context! {
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
            feed => context! { posts => Serde(Vec::<serde_json::Value>::new()), has_more => false },
            ..chrome()
        })
        .expect("community renders");

    let head = head(&rendered);
    assert!(head.contains(r#"<meta property="og:title" content="Open Studio" />"#));
    assert!(head.contains(r#"<meta property="og:url" content="https://oeee.test/@open" />"#));
    assert!(
        !head.contains("noindex"),
        "a public community should be indexable"
    );
}

#[test]
fn private_community_is_noindexed_and_leaks_no_preview() {
    let env = test_support::env();
    let rendered = env
        .get_template("community.jinja")
        .expect("community template loads")
        .render(context! {
            community => Serde(json!({
                "id": "00000000-0000-0000-0000-000000000001",
                "name": "Secret Studio",
                "description": "Members only",
                "slug": "secret",
                "visibility": "private",
                "owner_id": "00000000-0000-0000-0000-000000000002",
            })),
            community_id => "00000000-0000-0000-0000-000000000001",
            domain => "oeee.test",
            feed => context! { posts => Serde(Vec::<serde_json::Value>::new()), has_more => false },
            ..chrome()
        })
        .expect("community renders");

    let head = head(&rendered);
    assert!(head.contains(r#"<meta name="robots" content="noindex, nofollow" />"#));
    assert!(
        !head.contains("og:title"),
        "a private community must not emit a preview card"
    );
    assert!(
        !head.contains("Members only"),
        "the description must not leak into meta tags"
    );
}

#[test]
fn profile_card_uses_the_banner_when_there_is_one() {
    let env = test_support::env();
    let user = json!({
        "id": "00000000-0000-0000-0000-000000000001",
        "login_name": "artist",
        "display_name": "An Artist",
        "created_at": "2024-03-05T12:00:00Z",
    });
    let ctx = context! {
        user => Serde(user),
        domain => "oeee.test",
        banner => Serde(json!({
            "image_filename": "abcdef.png",
            "width": 200,
            "height": 40,
        })),
        followings => Serde(Vec::<serde_json::Value>::new()),
        links => Serde(Vec::<serde_json::Value>::new()),
        public_count => 0,
        public_feed => Serde(json!({"posts": [], "headings": [], "has_more": false, "next_url": ""})),
        private_count => 0,
        private_feed => Serde(json!({"posts": [], "headings": [], "has_more": false, "next_url": ""})),
        is_following => false,
        ..chrome()
    };

    let rendered = env
        .get_template("profile.jinja")
        .expect("profile template loads")
        .render(ctx)
        .expect("profile renders");

    let head = head(&rendered);
    assert!(head.contains(r#"<meta property="og:title" content="An Artist (@artist)" />"#));
    assert!(head.contains(r#"<meta property="og:url" content="https://oeee.test/@artist" />"#));
    assert!(
        head.contains(
            r#"<meta property="og:image" content="https://example.test/image/ab/abcdef.png" />"#
        ),
        "the banner is the profile's own image and should be the preview"
    );
}
