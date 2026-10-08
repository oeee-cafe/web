//! Templates are loaded and evaluated at runtime, so `cargo check` says
//! nothing about them and a mistake only surfaces when someone requests the
//! page. These close that gap in two tiers: every template has to parse,
//! and the ones with fixtures have to actually render.
//!
//! Parsing alone would not have caught the outage these were written for --
//! `{{ post.image_width + 24 }}`, where the context hands templates strings
//! and minijinja refuses to add a number to one. Only rendering catches
//! that, which is why the fixtures mirror the real context's types rather
//! than using conveniently-typed stand-ins.

use super::test_support;
use minijinja::context;
use minijinja::value::Serde;
use serde_json::json;
use std::collections::BTreeSet;
use std::path::PathBuf;

mod about;
mod account;
mod apps;
mod painter;
mod post;
mod profile;
mod report;
mod search;
mod sign_in;
mod tags;
mod toolbar;

fn template_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("templates")
}

fn template_names() -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    for entry in std::fs::read_dir(template_dir()).expect("templates directory") {
        let path = entry.expect("directory entry").path();
        if path.extension().and_then(|e| e.to_str()) == Some("jinja") {
            names.insert(
                path.file_name()
                    .and_then(|n| n.to_str())
                    .expect("template file name")
                    .to_string(),
            );
        }
    }
    names
}

#[test]
fn every_template_parses() {
    let env = test_support::env();
    let names = template_names();
    assert!(
        names.len() > 20,
        "expected to find the template set, found {}",
        names.len()
    );

    for name in &names {
        if let Err(error) = env.get_template(name) {
            panic!("{name} does not parse: {error:#}");
        }
    }
}

fn chrome() -> minijinja::Value {
    context! {
        current_user => Serde(json!(null)),
        messages => Serde(Vec::<serde_json::Value>::new()),
        draft_post_count => 0,
        unread_notification_count => 0,
        ftl_lang => "en",
    }
}

/// Shaped like what the replay handler passes: a map whose values are all
/// strings, including the dimensions. Anything that does arithmetic on
/// those has to coerce first.
fn replay_post() -> serde_json::Value {
    json!({
        "id": "9c881320-2b43-4afa-b2bb-7128c8a3e985",
        "title": "Tandemaus",
        "content": "a description",
        "image_width": "640",
        "image_height": "480",
        "image_filename": "abcdef0123.png",
        "replay_filename": "30ca3f590dda85e21dbc94250199a692b4fa5c7d626ea3445acef3bcf3c1338a.pch",
        "published_at": "2025-03-26 21:15:04",
        "paint_duration": "00:14:58",
        "community_slug": "tegaki",
        "community_name": "Tegaki",
        "login_name": "someone",
    })
}

/// The post page's own view of a post, string-valued like the real
/// context, with the replay switch and author left to the caller.
fn post_page(allow_replay: &str, author_id: &str) -> serde_json::Value {
    json!({
        "id": "9c881320-2b43-4afa-b2bb-7128c8a3e985",
        "author_id": author_id,
        "title": "Tandemaus",
        "content": "a description",
        "image_width": "640",
        "image_height": "480",
        "image_filename": "abcdef0123.png",
        "image_tool": "neo-cucumber",
        "replay_filename": "30ca3f590dda85e21dbc94250199a692b4fa5c7d626ea3445acef3bcf3c1338a.pch",
        "published_at": "2025-03-26 21:15:04",
        "paint_duration": "00:14:58",
        "viewer_count": "3",
        "allow_relay": "true",
        "allow_replay": allow_replay,
        "login_name": "someone",
        "display_name": "Someone",
    })
}

fn render_post_page(allow_replay: &str, author_id: &str, viewer: serde_json::Value) -> String {
    let env = test_support::env();
    env.get_template("post_view.jinja")
        .unwrap_or_else(|e| panic!("post_view.jinja loads: {e:#}"))
        .render(context! {
            post => Serde(post_page(allow_replay, author_id)),
            post_id => "9c881320-2b43-4afa-b2bb-7128c8a3e985",
            current_user => Serde(viewer),
            r2_public_endpoint_url => "https://images.example",
            base_url => "https://oeee.example",
            domain => "oeee.example",
            comments => Serde(Vec::<serde_json::Value>::new()),
            collaborative_participants => Serde(Vec::<serde_json::Value>::new()),
            reaction_counts => Serde(Vec::<serde_json::Value>::new()),
            tags => Serde(Vec::<serde_json::Value>::new()),
            child_posts => Serde(Vec::<serde_json::Value>::new()),
            post_community => Serde(json!(null)),
            parent_post_data => Serde(json!(null)),
            ..chrome()
        })
        .unwrap_or_else(|e| panic!("post_view.jinja renders: {e:#}"))
}
