use serde::Serialize;
use sqlx::{Postgres, Transaction};

use crate::app_error::AppError;
use crate::models::notification::get_badge_count;
use crate::models::post::get_draft_post_count;
use crate::models::user::User;

/// What the site's chrome around every page reads from its context: who is
/// signed in, the toolbar's two counts, and the page's language.
///
/// Built inside the handler's transaction, after anything the handler wrote,
/// so a page drawn after publishing a draft counts one draft fewer.
/// `AppState::render_page` merges it under the page's own context.
#[derive(Serialize)]
pub struct CommonContext {
    pub current_user: Option<User>,
    pub draft_post_count: i64,
    pub unread_notification_count: i64,
    pub ftl_lang: String,
}

impl CommonContext {
    /// The counts are zero for somebody signed out, and nothing is asked.
    ///
    /// A failed count is an error rather than a zero: inside a transaction
    /// the failure aborts it, so pretending otherwise only moves the error to
    /// the next statement, where it no longer says what went wrong.
    pub async fn build(
        tx: &mut Transaction<'_, Postgres>,
        user: Option<&User>,
        ftl_lang: &str,
    ) -> Result<Self, AppError> {
        let (draft_post_count, unread_notification_count) = match user {
            Some(user) => (
                get_draft_post_count(tx, user.id).await?,
                get_badge_count(tx, user.id).await?,
            ),
            None => (0, 0),
        };

        Ok(CommonContext {
            current_user: user.cloned(),
            draft_post_count,
            unread_notification_count,
            ftl_lang: ftl_lang.to_string(),
        })
    }

    /// `ctx` with this filled in under it: a key in both is `ctx`'s.
    pub fn merge(self, ctx: minijinja::Value) -> minijinja::Value {
        // The last map to have a key is the one it is read from.
        minijinja::value::merge_maps([minijinja::Value::from_serialize(&self), ctx])
    }

    /// For a page with nobody to count for, such as a signed-out reader's
    /// error page: no transaction to open.
    pub fn anonymous(ftl_lang: &str) -> Self {
        CommonContext {
            current_user: None,
            draft_post_count: 0,
            unread_notification_count: 0,
            ftl_lang: ftl_lang.to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::CommonContext;
    use crate::models::user::User;
    use crate::web::handlers::test_support;
    use minijinja::context;
    use serde_json::json;

    fn user() -> User {
        serde_json::from_value(json!({
            "id": "00000000-0000-0000-0000-000000000001",
            "login_name": "oeee",
            "display_name": "오이",
            "email": null,
            "email_verified_at": null,
            "updated_at": "2026-01-01T00:00:00Z",
            "created_at": "2026-01-01T00:00:00Z",
            "banner_id": null,
            "preferred_language": null,
            "deleted_at": null,
            "show_sensitive_content": false,
            "role": "user",
        }))
        .expect("a user")
    }

    /// What `render_page` hands a template is a merge of two values, and a
    /// merge that lost one side would render the page with nobody signed in
    /// and in no language, without an error to say so.
    #[test]
    fn a_merged_page_has_the_chrome_and_its_own_context() {
        let common = CommonContext {
            current_user: Some(user()),
            draft_post_count: 3,
            unread_notification_count: 0,
            ftl_lang: "ja".to_string(),
        };
        let rendered = test_support::env()
            .get_template("404.jinja")
            .expect("404 loads")
            .render(common.merge(context! { messages => Vec::<()>::new() }))
            .expect("404 renders");

        assert!(rendered.contains(r#"<html lang="ja">"#));
        assert!(rendered.contains(r#"data-server-count="3""#));
    }

    #[test]
    fn the_page_wins_a_key_both_set() {
        let merged = CommonContext::anonymous("en").merge(context! { ftl_lang => "ko" });
        assert_eq!(merged.get_attr("ftl_lang").unwrap().as_str(), Some("ko"));
        assert_eq!(
            merged.get_attr("draft_post_count").unwrap().as_i64(),
            Some(0)
        );
    }
}
