//! Where a post's page is.

use uuid::Uuid;

/// The path of a post's canonical page.
///
/// A post in a community lives under the community's slug, and any other post
/// under its author's name. The page is where people meet a drawing, and in a
/// community that is the community's feed, so the address says so.
///
/// None of this is the post's identity on the fediverse. The Note's `id` is
/// `/ap/posts/{id}`, which names neither, and the inbox handlers find a local
/// post from either shape of URL by its uuid alone. The address only travels
/// as the Note's `url`, the "open original" link. A post moved by
/// `do_post_edit_community` changes address, so that handler sends an `Update`
/// carrying the new one; a server that misses it still holds a working link,
/// because `post_view_by_login_name` redirects every other handle here.
///
/// This is the only place that rule is written down: `post_view_by_login_name`
/// and the pages under a post redirect anything else to it, and
/// `create_note_from_post` publishes it as the Note's `url`, so the address
/// people hold and the address we federate cannot drift apart. Templates spell
/// it through `post_url_macro.jinja`.
pub fn post_page_path(
    author_login_name: &str,
    community_slug: Option<&str>,
    post_id: Uuid,
) -> String {
    format!(
        "/@{}/{}",
        community_slug.unwrap_or(author_login_name),
        post_id
    )
}

/// [`post_page_path`] as the absolute URL ActivityPub publishes.
pub fn post_page_url(
    domain: &str,
    author_login_name: &str,
    community_slug: Option<&str>,
    post_id: Uuid,
) -> String {
    format!(
        "https://{}{}",
        domain,
        post_page_path(author_login_name, community_slug, post_id)
    )
}

#[cfg(test)]
mod post_page_tests {
    use super::{post_page_path, post_page_url};
    use uuid::Uuid;

    /// A post outside any community lives under its author's name.
    #[test]
    fn a_post_without_a_community_uses_its_author() {
        let id = Uuid::nil();
        assert_eq!(post_page_path("miro", None, id), format!("/@miro/{id}"));
        assert_eq!(
            post_page_url("oeee.cafe", "miro", None, id),
            format!("https://oeee.cafe/@miro/{id}")
        );
    }

    /// A post in a community lives under the community's slug.
    #[test]
    fn a_post_in_a_community_uses_the_community() {
        let id = Uuid::nil();
        assert_eq!(
            post_page_path("miro", Some("pokemon"), id),
            format!("/@pokemon/{id}")
        );
        assert_eq!(
            post_page_url("oeee.cafe", "miro", Some("pokemon"), id),
            format!("https://oeee.cafe/@pokemon/{id}")
        );
    }
}
