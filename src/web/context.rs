use sqlx::{Postgres, Transaction};
use uuid::Uuid;

use crate::app_error::AppError;
use crate::models::notification::get_badge_count;
use crate::models::post::get_draft_post_count;

/// Common context data needed by most template renders
pub struct CommonContext {
    pub draft_post_count: i64,
    pub unread_notification_count: i64,
}

impl CommonContext {
    /// Build common context for a user, fetching draft post count and unread notifications
    /// Returns zero values for anonymous users (when user_id is None)
    pub async fn build(
        tx: &mut Transaction<'_, Postgres>,
        user_id: Option<Uuid>,
    ) -> Result<Self, AppError> {
        match user_id {
            Some(user_id) => {
                let draft_post_count = get_draft_post_count(tx, user_id).await.unwrap_or_default();
                let unread_notification_count = get_badge_count(tx, user_id).await.unwrap_or(0);

                Ok(CommonContext {
                    draft_post_count,
                    unread_notification_count,
                })
            }
            None => Ok(CommonContext {
                draft_post_count: 0,
                unread_notification_count: 0,
            }),
        }
    }
}
