//! Achievements: a first drawing, a first relay, a first collaboration.
//!
//! Earned from what is already in the database rather than from events, so
//! [`award_achievements`] can be called whenever anything might have earned
//! one -- publishing, saving a collaboration, linking Steam -- and says the
//! same thing however often it runs. Awarding at link time is what gives an
//! account that has been drawing for years its achievements the day it
//! links Steam.
//!
//! Achievements are the site's; Steam hears about them afterwards, from
//! `steam::sync_achievements`, which works through the rows Steam has not yet
//! accepted.

use anyhow::Result;
use sqlx::types::Uuid;
use sqlx::{query, Postgres, Transaction};

/// Every achievement, by its Steamworks API name. The same set as the
/// `user_achievements_achievement_check` constraint; the profile names each
/// from `achievement-<key>` and `achievement-<key>-description` in every
/// locale, and a test holds all three together.
pub const ALL: [&str; 4] = [
    "FIRST_DRAWING",
    "FIRST_RELAY",
    "FIRST_COLLABORATION",
    "STEAM_SUPPORTER",
];

/// `FIRST_DRAWING` as the locale files spell it: `first-drawing`.
pub fn locale_key(achievement: &str) -> String {
    achievement.to_lowercase().replace('_', "-")
}

/// Records whatever `user_id` has earned and not yet been given.
pub async fn award_achievements(tx: &mut Transaction<'_, Postgres>, user_id: Uuid) -> Result<()> {
    query!(
        r#"
        INSERT INTO user_achievements (user_id, achievement)
        SELECT $1, earned.achievement
        FROM (
            SELECT 'FIRST_DRAWING' AS achievement
            WHERE EXISTS (
                SELECT 1 FROM posts
                WHERE author_id = $1 AND published_at IS NOT NULL
            )
            UNION ALL
            SELECT 'FIRST_RELAY'
            WHERE EXISTS (
                SELECT 1 FROM posts
                WHERE author_id = $1 AND published_at IS NOT NULL
                  AND parent_post_id IS NOT NULL
            )
            UNION ALL
            -- Everyone who drew in the room, not only its owner, whose name
            -- the saved post goes out under -- and the owner, who may not
            -- have a participant row.
            SELECT 'FIRST_COLLABORATION'
            WHERE EXISTS (
                SELECT 1 FROM collaborative_sessions sessions
                WHERE sessions.saved_post_id IS NOT NULL
                  AND (
                    sessions.owner_id = $1
                    OR EXISTS (
                        SELECT 1 FROM collaborative_sessions_participants participants
                        WHERE participants.session_id = sessions.id
                          AND participants.user_id = $1
                    )
                  )
            )
        ) earned
        ON CONFLICT DO NOTHING
        "#,
        user_id,
    )
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// Gives `user_id` an achievement that is not derived from what the site
/// stores: having bought Oeee Cafe on Steam, which only Steam knows.
pub async fn grant_achievement(
    tx: &mut Transaction<'_, Postgres>,
    user_id: Uuid,
    achievement: &str,
) -> Result<()> {
    query!(
        "INSERT INTO user_achievements (user_id, achievement) VALUES ($1, $2) ON CONFLICT DO NOTHING",
        user_id,
        achievement,
    )
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// An achievement someone has, for their profile.
#[derive(Clone, Debug, serde::Serialize)]
pub struct Earned {
    /// The Steamworks API name, `FIRST_DRAWING`.
    pub achievement: String,
    /// The same as the locale files spell it: `first-drawing`, for
    /// `achievement-first-drawing` and `achievement-first-drawing-description`.
    pub key: String,
    pub earned_at: chrono::DateTime<chrono::Utc>,
}

pub async fn list_achievements(
    tx: &mut Transaction<'_, Postgres>,
    user_id: Uuid,
) -> Result<Vec<Earned>> {
    let rows = query!(
        "SELECT achievement, earned_at FROM user_achievements WHERE user_id = $1 ORDER BY earned_at, achievement",
        user_id
    )
    .fetch_all(&mut **tx)
    .await?;
    Ok(rows
        .into_iter()
        .map(|row| Earned {
            key: locale_key(&row.achievement),
            achievement: row.achievement,
            earned_at: row.earned_at,
        })
        .collect())
}

/// [`award_achievements`] for everyone who drew in a collaborative session.
pub async fn award_achievements_for_session(
    tx: &mut Transaction<'_, Postgres>,
    session_id: Uuid,
) -> Result<()> {
    let drew = query!(
        r#"
        SELECT DISTINCT user_id AS "user_id!" FROM collaborative_sessions_participants
        WHERE session_id = $1
        UNION
        SELECT owner_id FROM collaborative_sessions WHERE id = $1
        "#,
        session_id,
    )
    .fetch_all(&mut **tx)
    .await?;
    for row in drew {
        award_achievements(tx, row.user_id).await?;
    }
    Ok(())
}

/// Has Steam told again about everything `user_id` has earned: a Steam
/// account newly linked has been told about none of it.
pub async fn resend_achievements_to_steam(
    tx: &mut Transaction<'_, Postgres>,
    user_id: Uuid,
) -> Result<()> {
    query!(
        "UPDATE user_achievements SET steam_synced_at = NULL WHERE user_id = $1",
        user_id
    )
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// Achievements a linked Steam account has not yet accepted, a batch at a
/// time, oldest first.
pub struct Unsynced {
    pub user_id: Uuid,
    pub steam_id: String,
    pub achievements: Vec<String>,
}

pub async fn unsynced_achievements(
    tx: &mut Transaction<'_, Postgres>,
    limit: i64,
) -> Result<Vec<Unsynced>> {
    let rows = query!(
        r#"
        SELECT
            a.user_id,
            i.subject AS steam_id,
            array_agg(a.achievement ORDER BY a.achievement) AS "achievements!"
        FROM user_achievements a
        JOIN user_identities i ON i.user_id = a.user_id AND i.provider = 'steam'
        WHERE a.steam_synced_at IS NULL
        GROUP BY a.user_id, i.subject
        ORDER BY min(a.earned_at)
        LIMIT $1
        "#,
        limit,
    )
    .fetch_all(&mut **tx)
    .await?;
    Ok(rows
        .into_iter()
        .map(|row| Unsynced {
            user_id: row.user_id,
            steam_id: row.steam_id,
            achievements: row.achievements,
        })
        .collect())
}

pub async fn mark_synced(
    tx: &mut Transaction<'_, Postgres>,
    user_id: Uuid,
    achievements: &[String],
) -> Result<()> {
    query!(
        r#"
        UPDATE user_achievements SET steam_synced_at = now()
        WHERE user_id = $1 AND achievement = ANY($2) AND steam_synced_at IS NULL
        "#,
        user_id,
        achievements,
    )
    .execute(&mut **tx)
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::PgPool;

    async fn tx() -> Option<Transaction<'static, Postgres>> {
        let url = std::env::var("DATABASE_URL").ok()?;
        let pool = PgPool::connect(&url).await.ok()?;
        pool.begin().await.ok()
    }

    async fn user(tx: &mut Transaction<'_, Postgres>, login_name: &str) -> Uuid {
        query!(
            "INSERT INTO users (login_name, display_name) VALUES ($1, $1) RETURNING id",
            login_name
        )
        .fetch_one(&mut **tx)
        .await
        .unwrap()
        .id
    }

    async fn post(
        tx: &mut Transaction<'_, Postgres>,
        author: Uuid,
        published: bool,
        parent: Option<Uuid>,
    ) -> Uuid {
        let image = query!(
            r#"
            INSERT INTO images (width, height, paint_duration, stroke_count, image_filename, tool)
            VALUES (10, 10, '0'::interval, 0, $1, 'neo')
            RETURNING id
            "#,
            format!("{}.png", Uuid::new_v4()),
        )
        .fetch_one(&mut **tx)
        .await
        .unwrap()
        .id;
        query!(
            r#"
            INSERT INTO posts (author_id, image_id, parent_post_id, published_at)
            VALUES ($1, $2, $3, CASE WHEN $4 THEN now() END)
            RETURNING id
            "#,
            author,
            image,
            parent,
            published,
        )
        .fetch_one(&mut **tx)
        .await
        .unwrap()
        .id
    }

    async fn earned(tx: &mut Transaction<'_, Postgres>, user_id: Uuid) -> Vec<String> {
        query!(
            "SELECT achievement FROM user_achievements WHERE user_id = $1 ORDER BY achievement",
            user_id
        )
        .fetch_all(&mut **tx)
        .await
        .unwrap()
        .into_iter()
        .map(|row| row.achievement)
        .collect()
    }

    #[test]
    fn every_achievement_is_worded_in_every_locale() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("locales");
        for locale in ["en", "ko", "ja", "zh"] {
            let text = std::fs::read_to_string(dir.join(format!("{locale}.ftl"))).unwrap();
            for achievement in ALL {
                let key = locale_key(achievement);
                for id in [
                    format!("achievement-{key}"),
                    format!("achievement-{key}-description"),
                ] {
                    assert!(
                        text.lines()
                            .any(|line| line.starts_with(&format!("{id} = "))),
                        "{locale}.ftl has no {id}"
                    );
                }
            }
        }
    }

    #[tokio::test]
    async fn the_list_is_what_the_database_allows() {
        let Some(mut tx) = tx().await else { return };
        let definition: String = sqlx::query_scalar!(
            r#"SELECT pg_get_constraintdef(oid) AS "d!" FROM pg_constraint WHERE conname = 'user_achievements_achievement_check'"#
        )
        .fetch_one(&mut *tx)
        .await
        .unwrap();
        let allowed = definition.matches("::text").count();
        assert_eq!(allowed, ALL.len(), "{definition}");
        for achievement in ALL {
            assert!(
                definition.contains(&format!("'{achievement}'")),
                "{definition}"
            );
        }
    }

    #[tokio::test]
    async fn drawings_and_relays_are_earned_once_published() {
        let Some(mut tx) = tx().await else { return };
        let artist = user(&mut tx, "achievement_test_a").await;
        let first = post(&mut tx, artist, false, None).await;
        award_achievements(&mut tx, artist).await.unwrap();
        assert!(
            earned(&mut tx, artist).await.is_empty(),
            "a draft earns nothing"
        );

        query!("UPDATE posts SET published_at = now() WHERE id = $1", first)
            .execute(&mut *tx)
            .await
            .unwrap();
        award_achievements(&mut tx, artist).await.unwrap();
        assert_eq!(earned(&mut tx, artist).await, ["FIRST_DRAWING"]);

        post(&mut tx, artist, true, Some(first)).await;
        award_achievements(&mut tx, artist).await.unwrap();
        award_achievements(&mut tx, artist).await.unwrap();
        assert_eq!(
            earned(&mut tx, artist).await,
            ["FIRST_DRAWING", "FIRST_RELAY"]
        );
        tx.rollback().await.unwrap();
    }

    #[tokio::test]
    async fn everyone_in_a_saved_room_collaborated() {
        let Some(mut tx) = tx().await else { return };
        let owner = user(&mut tx, "achievement_test_b").await;
        let guest = user(&mut tx, "achievement_test_c").await;
        let session = query!(
            r#"
            INSERT INTO collaborative_sessions (owner_id, title, width, height, is_public, max_participants)
            VALUES ($1, 't', 10, 10, true, 4) RETURNING id
            "#,
            owner
        )
        .fetch_one(&mut *tx)
        .await
        .unwrap()
        .id;
        query!(
            "INSERT INTO collaborative_sessions_participants (session_id, user_id) VALUES ($1, $2)",
            session,
            guest
        )
        .execute(&mut *tx)
        .await
        .unwrap();

        award_achievements_for_session(&mut tx, session)
            .await
            .unwrap();
        assert!(earned(&mut tx, guest).await.is_empty(), "not saved yet");

        let saved = post(&mut tx, owner, true, None).await;
        query!(
            "UPDATE collaborative_sessions SET saved_post_id = $1 WHERE id = $2",
            saved,
            session
        )
        .execute(&mut *tx)
        .await
        .unwrap();
        award_achievements_for_session(&mut tx, session)
            .await
            .unwrap();
        assert_eq!(earned(&mut tx, guest).await, ["FIRST_COLLABORATION"]);
        assert_eq!(
            earned(&mut tx, owner).await,
            ["FIRST_COLLABORATION", "FIRST_DRAWING"]
        );
        tx.rollback().await.unwrap();
    }

    #[tokio::test]
    async fn only_accounts_with_steam_wait_for_steam() {
        let Some(mut tx) = tx().await else { return };
        let with_steam = user(&mut tx, "achievement_test_d").await;
        let without = user(&mut tx, "achievement_test_e").await;
        for id in [with_steam, without] {
            post(&mut tx, id, true, None).await;
            award_achievements(&mut tx, id).await.unwrap();
        }
        query!(
            "INSERT INTO user_identities (user_id, provider, subject) VALUES ($1, 'steam', '76561190000000009')",
            with_steam
        )
        .execute(&mut *tx)
        .await
        .unwrap();

        let waiting: Vec<_> = unsynced_achievements(&mut tx, 1000)
            .await
            .unwrap()
            .into_iter()
            .filter(|u| u.user_id == with_steam || u.user_id == without)
            .collect();
        assert_eq!(waiting.len(), 1);
        assert_eq!(waiting[0].steam_id, "76561190000000009");
        assert_eq!(waiting[0].achievements, ["FIRST_DRAWING"]);

        mark_synced(&mut tx, with_steam, &waiting[0].achievements)
            .await
            .unwrap();
        assert!(!unsynced_achievements(&mut tx, 1000)
            .await
            .unwrap()
            .iter()
            .any(|u| u.user_id == with_steam));

        resend_achievements_to_steam(&mut tx, with_steam)
            .await
            .unwrap();
        assert!(unsynced_achievements(&mut tx, 1000)
            .await
            .unwrap()
            .iter()
            .any(|u| u.user_id == with_steam));
        tx.rollback().await.unwrap();
    }
}
