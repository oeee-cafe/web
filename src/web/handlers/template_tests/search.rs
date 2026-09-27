//! The search page.

use crate::web::handlers::test_support;
use minijinja::context;

use super::chrome;

/// Rendered from the handler's own row types, so the fixture cannot drift
/// from what `search_page` actually hands the template.
fn render_search(
    search_query: Option<&str>,
    posts: Vec<crate::web::handlers::search::SearchPostRow>,
) -> String {
    render_search_with_people(search_query, Vec::new(), posts)
}

fn render_search_with_people(
    search_query: Option<&str>,
    people: Vec<crate::web::handlers::search::SearchPersonRow>,
    posts: Vec<crate::web::handlers::search::SearchPostRow>,
) -> String {
    test_support::env()
        .get_template("search.jinja")
        .unwrap_or_else(|e| panic!("search.jinja loads: {e:#}"))
        .render(context! {
            search_query,
            people,
            posts,
            ..chrome()
        })
        .unwrap_or_else(|e| panic!("search.jinja renders: {e:#}"))
}

#[test]
fn search_page_renders_form_empty_state_and_results() {
    use crate::web::handlers::search::SearchPostRow;

    // Nothing asked yet: the form alone. Asserted on the page's own
    // form, `communities-filters` -- the toolbar in every page has a
    // form to /search of its own now, so `action="/search"` alone no
    // longer says this page has one.
    let blank = render_search(None, vec![]);
    assert!(blank.contains("communities-filters") && blank.contains(r#"name="q""#));
    assert!(!blank.contains("search-no-results"));

    // Asked, and nothing matched.
    let none = render_search(Some("zzz"), vec![]);
    assert!(none.contains("search-no-results"));
    assert!(none.contains(r#"value="zzz""#));

    let found = render_search(
        Some("그림"),
        vec![SearchPostRow {
            id: uuid::Uuid::nil(),
            title: Some("그림".into()),
            user_login_name: "someone".into(),
            image_filename: Some("abcdef0123.png".into()),
            image_width: Some(640),
            image_height: Some(480),
            is_sensitive: false,
            community_slug: None,
            community_name: None,
            published_at: Some(chrono::Utc::now()),
        }],
    );
    assert!(
        !found.contains("search-people"),
        "no one matched, so no heading for them"
    );
    assert!(found.contains(r#"href="/@someone/00000000-0000-0000-0000-000000000000""#));
    assert!(found.contains("/image/ab/abcdef0123.png"));
    assert!(!found.contains("search-no-results"));
}

#[test]
fn search_page_lists_people_as_the_profile_does() {
    use crate::web::handlers::search::SearchPersonRow;
    let people = render_search_with_people(
        Some("오이"),
        vec![
            SearchPersonRow {
                login_name: "oeee".into(),
                display_name: "오이 <b>".into(),
                banner_image_filename: Some("abcdef.png".into()),
            },
            SearchPersonRow {
                login_name: "plain".into(),
                display_name: "Plain".into(),
                banner_image_filename: None,
            },
        ],
        vec![],
    );
    assert!(people.contains("search-people"));
    assert!(people.contains(r#"href="/@oeee""#));
    assert!(people.contains("/image/ab/abcdef.png"));
    assert!(
        people.contains("profile-follow-blank"),
        "no banner, a frame with the name"
    );
    assert!(
        people.contains("오이 &lt;b&gt;"),
        "a name is text, not markup"
    );
    assert!(
        !people.contains("search-no-results"),
        "people found is a result, drawings or not"
    );
}
