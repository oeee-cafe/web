//! Tags: their pages, their comments and the suggestions while typing one.

use crate::web::handlers::test_support;
use minijinja::context;
use serde_json::json;

use super::chrome;
use minijinja::value::Serde;

#[test]
fn the_tag_results_render_for_both_the_page_and_the_search_box() {
    // Rendered inline by the page and standalone by /api/tags/cards.
    // The standalone call passes no `sort_by`, no `current_user` and no
    // chrome, so anything the fragment reaches for beyond its own three
    // keys would 500 the search box while the page stayed fine.
    let env = test_support::env();
    let template = env
        .get_template("tag_results.jinja")
        .unwrap_or_else(|e| panic!("tag_results.jinja loads: {e:#}"));

    let tags = vec![json!({
        "name": "oekaki",
        "display_name": "oekaki",
        "post_count": 12,
    })];

    let searched = template
        .render(context! {
            tags => Serde(tags.clone()),
            search_query => Some("oek"),
            ftl_lang => "en",
        })
        .unwrap_or_else(|e| panic!("renders a search: {e:#}"));
    assert!(searched.contains("tag-search-info"));
    assert!(searched.contains("/tags/oekaki"));

    let browsing = template
        .render(context! {
            tags => Serde(tags),
            search_query => None::<String>,
            ftl_lang => "en",
        })
        .unwrap_or_else(|e| panic!("renders while browsing: {e:#}"));
    assert!(
        !browsing.contains("tag-search-info"),
        "browsing is not a search and should not claim to be one"
    );

    let empty = template
        .render(context! {
            tags => Serde(Vec::<serde_json::Value>::new()),
            search_query => Some("zzzz"),
            ftl_lang => "en",
        })
        .unwrap_or_else(|e| panic!("renders no matches: {e:#}"));
    assert!(empty.contains("no-tags-found"));
}

#[test]
fn the_tag_page_draws_the_shared_post_cards() {
    // This page used to write its own <img> tags. That is how sensitive
    // drawings came to be blurred everywhere except here, and how it came
    // to load every thumbnail eagerly with no way to reach the next batch.
    let env = test_support::env();
    let rendered = env
        .get_template("tag_view.jinja")
        .unwrap_or_else(|e| panic!("tag_view.jinja loads: {e:#}"))
        .render(context! {
            tag => Serde(json!({
                "name": "oekaki",
                "display_name": "Oekaki",
                "post_count": 2,
            })),
            post_count => 2,
            feed => Serde(json!({
                "posts": [{
                    "id": "9c881320-2b43-4afa-b2bb-7128c8a3e985",
                    "title": "Tandemaus",
                    "user_login_name": "someone",
                    "image_filename": "abcdef.png",
                    "image_width": 300,
                    "image_height": 300,
                    "is_sensitive": true,
                    "community_slug": null,
                    "community_name": null,
                    "published_at": "2026-08-01T00:00:00Z",
                }],
                "has_more": true,
                "next_url": "/tags/oekaki/posts?offset=60&limit=60",
            })),
            ..chrome()
        })
        .unwrap_or_else(|e| panic!("tag_view.jinja renders: {e:#}"));

    assert!(
        rendered.contains(r#"class="sensitive""#),
        "a sensitive drawing has to be blurred here too"
    );
    assert!(rendered.contains(r#"loading="lazy""#));
    // The autoescaper writes `/` as `&#x2f;` in attributes.
    let links_in = rendered.replace("&#x2f;", "/").replace("&amp;", "&");
    assert!(
        links_in.contains("/tags/oekaki/posts?offset=60"),
        "the page needs the sentinel that loads the next batch"
    );
    // The count is passed separately from the tag now, because it is
    // counted over what this viewer can actually see.
    assert!(rendered.contains("tag-post-count(count=2)"));
}

#[test]
fn the_tag_page_names_the_tag_in_its_link_preview() {
    let env = test_support::env();
    let rendered = env
        .get_template("tag_view.jinja")
        .unwrap_or_else(|e| panic!("tag_view.jinja loads: {e:#}"))
        .render(context! {
            // A tag in a non-Latin script has to survive being put in a URL.
            tag => Serde(json!({ "name": "그림", "display_name": "그림", "post_count": 0 })),
            post_count => 0,
            feed => Serde(json!({ "posts": [], "has_more": false, "next_url": "" })),
            ..chrome()
        })
        .unwrap_or_else(|e| panic!("tag_view.jinja renders empty: {e:#}"));

    assert!(
        rendered.contains(r#"content="https://oeee.test/tags/%EA%B7%B8%EB%A6%BC""#),
        "og:url should be the escaped canonical name, got: {}",
        &rendered[..rendered.find("</head>").unwrap_or(400)]
    );
    assert!(rendered.contains("tag-no-posts"));
}

/// What is said on a tag's drawings goes beside them, as on a
/// community's page, and is the other half of a pill: /tags/name and
/// /tags/name/comments, each loading on from the tag's own endpoint.
/// A tag in a non-Latin script keeps its escaping in all of them.
#[test]
fn a_tags_comments_go_beside_its_drawings_and_on_a_page_of_their_own() {
    let env = test_support::env();
    let comments = json!({
        "rows": [{
            "post_id": "9c881320-2b43-4afa-b2bb-7128c8a3e985",
            "post_author_login_name": "someone",
            "post_title": "Tandemaus",
            "actor_name": "Commenter",
            "handle": {"login_name": "commenter", "name": null, "host": null},
            "content": "Lovely colours",
            "created_at": "2026-08-02T00:00:00Z",
        }],
        "next_url": "/api/tags/%EA%B7%B8%EB%A6%BC/comments?after=00000000-0000-0000-0000-000000000009",
    });
    let render = |template: &str| {
        env.get_template(template)
            .unwrap_or_else(|e| panic!("{template} loads: {e:#}"))
            .render(context! {
                tag => Serde(json!({ "name": "그림", "display_name": "그림", "post_count": 1 })),
                post_count => 1,
                feed => Serde(json!({
                    "posts": [{
                        "id": "9c881320-2b43-4afa-b2bb-7128c8a3e985",
                        "title": "Tandemaus",
                        "user_login_name": "someone",
                        "image_filename": "abcdef.png",
                        "image_width": 300,
                        "image_height": 300,
                        "is_sensitive": false,
                        "community_slug": null,
                        "community_name": null,
                        "published_at": "2026-08-01T00:00:00Z",
                    }],
                    "has_more": false,
                    "next_url": "",
                })),
                comments => Serde(comments.clone()),
                ..chrome()
            })
            .unwrap_or_else(|e| panic!("{template} renders: {e:#}"))
    };

    let drawings = render("tag_view.jinja");
    let links_in = drawings.replace("&#x2f;", "/");
    assert!(drawings.contains(r#"<aside class="feed-comments" aria-labelledby"#));
    assert!(drawings.contains(r#"id="post-feed-grid""#));
    assert!(links_in.contains(
        r#"<a href="/tags/%EA%B7%B8%EB%A6%BC" aria-current="page">feed-view-drawings</a>"#
    ));
    assert!(
        links_in.contains(r#"<a href="/tags/%EA%B7%B8%EB%A6%BC/comments">feed-view-comments</a>"#)
    );
    assert!(links_in.contains(r#"hx-get="/api/tags/%EA%B7%B8%EB%A6%BC/comments?after="#));
    assert!(links_in
        .contains(r#"<a class="feed-comments-more" href="/tags/%EA%B7%B8%EB%A6%BC/comments">"#));

    let said = render("tag_comments.jinja");
    let links_in = said.replace("&#x2f;", "/");
    assert!(
        said.contains("tag-post-count(count=1)"),
        "under the same card"
    );
    assert!(said.contains(r#"<div class="comment-grid">"#));
    assert!(said.contains("Lovely colours"));
    assert!(
        !said.contains(r#"id="post-feed-grid""#),
        "no drawings under it"
    );
    assert!(links_in.contains(
        r#"<a href="/tags/%EA%B7%B8%EB%A6%BC/comments" aria-current="page">feed-view-comments</a>"#
    ));
    assert!(links_in.contains(r#"hx-get="/api/tags/%EA%B7%B8%EB%A6%BC/comments?after="#));
}

#[test]
fn the_tag_suggestions_are_options_a_keyboard_can_reach() {
    // The menu used to be plain <li>s that only answered a click, inside a
    // container announcing itself as a listbox.
    let env = test_support::env();
    let rendered = env
        .get_template("tag_autocomplete.jinja")
        .unwrap_or_else(|e| panic!("tag_autocomplete.jinja loads: {e:#}"))
        .render(context! {
            tags => Serde(vec![json!({
                "name": "oekaki",
                "display_name": "Oekaki",
                "post_count": 12,
            })]),
            ftl_lang => "en",
        })
        .unwrap_or_else(|e| panic!("tag_autocomplete.jinja renders: {e:#}"));

    assert!(rendered.contains(r#"role="option""#));
    assert!(rendered.contains(r#"id="tag-option-0""#));
    assert!(rendered.contains(r#"aria-selected="false""#));
    assert!(rendered.contains("tag-post-count(count=12)"));
}
