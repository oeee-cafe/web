//! Supporters: accounts whose linked Steam account owns one of the apps in
//! `steam.supporter_app_ids` -- the Supporter Pack DLC.
//!
//! Standing is whatever Steam said last. It is asked at every Steam sign-in
//! and link (`identity::touch_identity`, `identity::link_identity`) and once
//! a day besides for every linked Steam account (`steam::recheck_supporters`),
//! so a purchase or a refund shows within a day whether or not anyone signs
//! in. It lives on the identity: unlinking Steam ends it.
//!
//! A supporter wears a badge beside their name and, unless they turn it off
//! on their account page, is thanked by name in the credits on /about.

use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::types::Uuid;
use sqlx::{query, query_scalar, Postgres, Transaction};

use super::identity::VerifiedIdentity;

/// Records what a provider said at sign-in about owning the Supporter Pack.
/// Says nothing when the provider could not be asked.
pub async fn record_supporter_check_for(
    tx: &mut Transaction<'_, Postgres>,
    identity: &VerifiedIdentity,
) -> Result<()> {
    if let Some(owned) = identity.purchased {
        record(tx, identity.provider.as_str(), &identity.subject, owned).await?;
    }
    Ok(())
}

/// Records what Steam said, on a recheck, about `steam_id` owning the
/// Supporter Pack.
pub async fn record_supporter_check(
    tx: &mut Transaction<'_, Postgres>,
    steam_id: &str,
    owned: bool,
) -> Result<()> {
    record(tx, "steam", steam_id, owned).await
}

async fn record(
    tx: &mut Transaction<'_, Postgres>,
    provider: &str,
    subject: &str,
    owned: bool,
) -> Result<()> {
    // A supporter since their first purchase stays one since then for as
    // long as they keep owning it; losing it and buying again starts over.
    query!(
        r#"
        UPDATE user_identities
        SET supporter_since = CASE WHEN $3 THEN COALESCE(supporter_since, now()) END,
            supporter_checked_at = now()
        WHERE provider = $1 AND subject = $2
        "#,
        provider,
        subject,
        owned,
    )
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// Linked Steam accounts Steam has not been asked about for a day, never
/// asked first, then longest ago.
pub async fn steam_accounts_due_for_check(
    tx: &mut Transaction<'_, Postgres>,
    limit: i64,
) -> Result<Vec<String>> {
    let due = query_scalar!(
        r#"
        SELECT subject FROM user_identities
        WHERE provider = 'steam'
          AND (supporter_checked_at IS NULL OR supporter_checked_at < now() - interval '1 day')
        ORDER BY supporter_checked_at NULLS FIRST
        LIMIT $1
        "#,
        limit,
    )
    .fetch_all(&mut **tx)
    .await?;
    Ok(due)
}

pub async fn is_supporter(tx: &mut Transaction<'_, Postgres>, user_id: Uuid) -> Result<bool> {
    let supporter = query_scalar!(
        r#"
        SELECT EXISTS (
            SELECT 1 FROM user_identities
            WHERE user_id = $1 AND supporter_since IS NOT NULL
        ) AS "supporter!"
        "#,
        user_id,
    )
    .fetch_one(&mut **tx)
    .await?;
    Ok(supporter)
}

/// The login names of the supporters a post's page names: whoever drew it,
/// whoever drew it with them, and whoever commented. The page checks each
/// name it prints against this.
pub async fn supporters_on_post(
    tx: &mut Transaction<'_, Postgres>,
    post_id: Uuid,
) -> Result<Vec<String>> {
    let names = query_scalar!(
        r#"
        SELECT DISTINCT users.login_name
        FROM users
        JOIN user_identities ON user_identities.user_id = users.id
        WHERE user_identities.supporter_since IS NOT NULL
          AND users.deleted_at IS NULL
          AND (
            users.id = (SELECT author_id FROM posts WHERE id = $1)
            OR users.id IN (
                SELECT participants.user_id
                FROM collaborative_sessions sessions
                JOIN collaborative_sessions_participants participants
                  ON participants.session_id = sessions.id
                WHERE sessions.saved_post_id = $1
            )
            OR users.id IN (
                SELECT actors.user_id
                FROM comments
                JOIN actors ON actors.id = comments.actor_id
                WHERE comments.post_id = $1 AND actors.user_id IS NOT NULL
            )
          )
        "#,
        post_id,
    )
    .fetch_all(&mut **tx)
    .await?;
    Ok(names)
}

/// A line in the credits on /about.
#[derive(Clone, Debug, Serialize)]
pub struct Credit {
    pub login_name: String,
    pub display_name: String,
    pub since: DateTime<Utc>,
}

/// Every supporter who has not asked to be left off, earliest first.
pub async fn list_credits(tx: &mut Transaction<'_, Postgres>) -> Result<Vec<Credit>> {
    let rows = query!(
        r#"
        SELECT
            users.login_name,
            users.display_name,
            min(user_identities.supporter_since) AS "since!"
        FROM users
        JOIN user_identities ON user_identities.user_id = users.id
        WHERE user_identities.supporter_since IS NOT NULL
          AND users.deleted_at IS NULL
          AND users.show_in_credits
        GROUP BY users.id
        ORDER BY 3, users.login_name
        "#
    )
    .fetch_all(&mut **tx)
    .await?;
    Ok(rows
        .into_iter()
        .map(|row| Credit {
            login_name: row.login_name,
            display_name: row.display_name,
            since: row.since,
        })
        .collect())
}

pub async fn shows_in_credits(tx: &mut Transaction<'_, Postgres>, user_id: Uuid) -> Result<bool> {
    let shown = query_scalar!("SELECT show_in_credits FROM users WHERE id = $1", user_id)
        .fetch_one(&mut **tx)
        .await?;
    Ok(shown)
}

pub async fn set_show_in_credits(
    tx: &mut Transaction<'_, Postgres>,
    user_id: Uuid,
    shown: bool,
) -> Result<()> {
    query!(
        "UPDATE users SET show_in_credits = $2 WHERE id = $1",
        user_id,
        shown
    )
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// Against the database `DATABASE_URL` names, inside a transaction that is
/// never committed. Skipped when there is no database to reach.
#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::identity::{link_identity, touch_identity, unlink_identity, Provider};
    use sqlx::PgPool;

    async fn tx() -> Option<Transaction<'static, Postgres>> {
        let url = std::env::var("DATABASE_URL").ok()?;
        let pool = PgPool::connect(&url).await.ok()?;
        pool.begin().await.ok()
    }

    async fn user(tx: &mut Transaction<'_, Postgres>, login_name: &str) -> Uuid {
        query!(
            "INSERT INTO users (login_name, display_name, password_hash) VALUES ($1, $1, 'x') RETURNING id",
            login_name
        )
        .fetch_one(&mut **tx)
        .await
        .unwrap()
        .id
    }

    fn steam(subject: &str, purchased: Option<bool>) -> VerifiedIdentity {
        VerifiedIdentity {
            provider: Provider::Steam,
            subject: subject.to_string(),
            name: None,
            email: None,
            purchased,
        }
    }

    fn credited(credits: &[Credit], login_name: &str) -> bool {
        credits.iter().any(|c| c.login_name == login_name)
    }

    #[tokio::test]
    async fn owning_the_pack_makes_a_supporter_and_a_refund_unmakes_one() {
        let Some(mut tx) = tx().await else { return };
        let id = user(&mut tx, "supporter_test_a").await;
        let subject = "76561190000000101";

        link_identity(&mut tx, id, &steam(subject, Some(false)))
            .await
            .unwrap()
            .unwrap();
        assert!(!is_supporter(&mut tx, id).await.unwrap());

        touch_identity(&mut tx, &steam(subject, Some(true)))
            .await
            .unwrap();
        assert!(is_supporter(&mut tx, id).await.unwrap());
        assert!(credited(
            &list_credits(&mut tx).await.unwrap(),
            "supporter_test_a"
        ));

        // Steam could not be asked: nothing changes.
        touch_identity(&mut tx, &steam(subject, None))
            .await
            .unwrap();
        assert!(is_supporter(&mut tx, id).await.unwrap());

        // Refunded, and noticed by the daily recheck.
        record_supporter_check(&mut tx, subject, false)
            .await
            .unwrap();
        assert!(!is_supporter(&mut tx, id).await.unwrap());
        assert!(!credited(
            &list_credits(&mut tx).await.unwrap(),
            "supporter_test_a"
        ));
        tx.rollback().await.unwrap();
    }

    #[tokio::test]
    async fn the_badge_goes_with_the_steam_account() {
        let Some(mut tx) = tx().await else { return };
        let id = user(&mut tx, "supporter_test_b").await;
        link_identity(&mut tx, id, &steam("76561190000000102", Some(true)))
            .await
            .unwrap()
            .unwrap();
        assert!(is_supporter(&mut tx, id).await.unwrap());

        let user = crate::models::user::find_user_by_id(&mut tx, id)
            .await
            .unwrap()
            .unwrap();
        unlink_identity(&mut tx, &user, Provider::Steam)
            .await
            .unwrap()
            .unwrap();
        assert!(!is_supporter(&mut tx, id).await.unwrap());
        tx.rollback().await.unwrap();
    }

    #[tokio::test]
    async fn a_supporter_can_leave_the_credits_and_keep_the_badge() {
        let Some(mut tx) = tx().await else { return };
        let id = user(&mut tx, "supporter_test_c").await;
        link_identity(&mut tx, id, &steam("76561190000000103", Some(true)))
            .await
            .unwrap()
            .unwrap();
        assert!(
            shows_in_credits(&mut tx, id).await.unwrap(),
            "listed by default"
        );

        set_show_in_credits(&mut tx, id, false).await.unwrap();
        assert!(!credited(
            &list_credits(&mut tx).await.unwrap(),
            "supporter_test_c"
        ));
        assert!(is_supporter(&mut tx, id).await.unwrap());
        tx.rollback().await.unwrap();
    }

    /// The author and a commenter who support are named; a commenter who
    /// does not, and a supporter who never touched the post, are not.
    #[tokio::test]
    async fn a_post_names_the_supporters_on_its_page() {
        let Some(mut tx) = tx().await else { return };
        let author = user(&mut tx, "supporter_test_g").await;
        let commenter = user(&mut tx, "supporter_test_h").await;
        let bystander = user(&mut tx, "supporter_test_i").await;
        let plain = user(&mut tx, "supporter_test_j").await;
        for (id, subject) in [
            (author, "76561190000000107"),
            (commenter, "76561190000000108"),
            (bystander, "76561190000000109"),
        ] {
            link_identity(&mut tx, id, &steam(subject, Some(true)))
                .await
                .unwrap()
                .unwrap();
        }

        let image: Uuid = sqlx::query_scalar(
            "INSERT INTO images (width, height, paint_duration, stroke_count, image_filename, tool)
             VALUES (10, 10, '0'::interval, 0, $1, 'neo') RETURNING id",
        )
        .bind(format!("{}.png", Uuid::new_v4()))
        .fetch_one(&mut *tx)
        .await
        .unwrap();
        let post: Uuid = sqlx::query_scalar(
            "INSERT INTO posts (author_id, image_id, published_at) VALUES ($1, $2, now()) RETURNING id",
        )
        .bind(author)
        .bind(image)
        .fetch_one(&mut *tx)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO instances (host) VALUES ('supporter-test.example') ON CONFLICT DO NOTHING",
        )
        .execute(&mut *tx)
        .await
        .unwrap();
        for id in [commenter, plain] {
            let actor: Uuid = sqlx::query_scalar(
                "INSERT INTO actors (iri, url, type, username, instance_host, handle_host, handle, name,
                                     inbox_url, followers_url, public_key_pem, user_id)
                 VALUES ($1, $1, 'Person', $1, 'supporter-test.example', 'supporter-test.example',
                         $1, $1, $1, $1, '', $2)
                 RETURNING id",
            )
            .bind(id.to_string())
            .bind(id)
            .fetch_one(&mut *tx)
            .await
            .unwrap();
            sqlx::query("INSERT INTO comments (post_id, actor_id, content) VALUES ($1, $2, 'hi')")
                .bind(post)
                .bind(actor)
                .execute(&mut *tx)
                .await
                .unwrap();
        }

        let mut named = supporters_on_post(&mut tx, post).await.unwrap();
        named.sort();
        assert_eq!(named, ["supporter_test_g", "supporter_test_h"]);
        tx.rollback().await.unwrap();
    }

    #[tokio::test]
    async fn accounts_not_asked_about_today_are_asked_again() {
        let Some(mut tx) = tx().await else { return };
        let fresh = user(&mut tx, "supporter_test_d").await;
        let stale = user(&mut tx, "supporter_test_e").await;
        let never = user(&mut tx, "supporter_test_f").await;
        for (id, subject, owned) in [
            (fresh, "76561190000000104", true),
            (stale, "76561190000000105", true),
            (never, "76561190000000106", false),
        ] {
            link_identity(&mut tx, id, &steam(subject, Some(owned)))
                .await
                .unwrap()
                .unwrap();
        }
        query!(
            "UPDATE user_identities SET supporter_checked_at = now() - interval '2 days' WHERE subject = '76561190000000105'"
        )
        .execute(&mut *tx)
        .await
        .unwrap();

        query!(
            "UPDATE user_identities SET supporter_checked_at = NULL WHERE subject = '76561190000000106'"
        )
        .execute(&mut *tx)
        .await
        .unwrap();

        // Supporter or not: a purchase made while signed in is noticed too.
        let due = steam_accounts_due_for_check(&mut tx, 1000).await.unwrap();
        assert!(due.contains(&"76561190000000105".to_string()));
        assert!(
            due.contains(&"76561190000000106".to_string()),
            "never asked"
        );
        assert!(!due.contains(&"76561190000000104".to_string()));
        tx.rollback().await.unwrap();
    }
}
