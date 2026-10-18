//! Finding a person by what someone has typed: the one rule for every box
//! that does it -- `/search`, the quick switcher (`/jump`) and the `@`
//! autocomplete under a comment.
//!
//! People are not listed by part of a name to just anyone: typing `@a`
//! should not page through the site's accounts. So a person is found two
//! ways, and only these:
//!
//! - by their handle, typed exactly (any case, `@` or not), whoever they
//!   are -- knowing someone's handle is knowing who they are;
//! - by the start of their handle or display name, if they are *nearby*:
//!   someone the reader follows or who follows them, and, where a post is
//!   being written under, its author and the people in its comments.
//!
//! The exact handle comes first, then the post's own people, then those
//! the reader follows, then those following the reader. The reader is never
//! among them, and neither is a deleted account.

use serde::Serialize;
use sqlx::{Postgres, Transaction};
use uuid::Uuid;

use crate::models::handle::LoginName;
use crate::models::tag::escape_like;

/// A person who matches, as the profile's follow card
/// (person_card_macro.jinja) draws them.
#[derive(Serialize, Debug)]
pub struct PersonMatch {
    pub login_name: LoginName,
    pub display_name: String,
    pub banner_image_filename: Option<String>,
}

/// What is being looked for, and by whom.
pub struct PeopleQuery<'a> {
    /// Who is typing; nobody signed in has nobody nearby.
    pub reader: Option<Uuid>,
    /// What they typed, with or without its `@`.
    pub q: &'a str,
    /// The post being commented on, whose author and commenters are nearby.
    pub post: Option<Uuid>,
    pub limit: i64,
}

/// The people `query` finds, in the order above. Nothing typed is the
/// nearby people alone, for an autocomplete that has only just opened.
pub async fn find_people(
    tx: &mut Transaction<'_, Postgres>,
    query: &PeopleQuery<'_>,
) -> anyhow::Result<Vec<PersonMatch>> {
    let typed = query.q.trim().trim_start_matches('@');
    let exact = escape_like(typed);
    let people = sqlx::query_as!(
        PersonMatch,
        r#"
        WITH nearby AS (
            SELECT author_id AS user_id, 1 AS rank
            FROM posts
            WHERE id = $2 AND deleted_at IS NULL
            UNION ALL
            SELECT a.user_id, 2
            FROM comments c
            JOIN actors a ON a.id = c.actor_id
            WHERE c.post_id = $2 AND c.deleted_at IS NULL AND a.user_id IS NOT NULL
            UNION ALL
            SELECT them.user_id, 3
            FROM actors me
            JOIN follows f ON f.follower_actor_id = me.id
            JOIN actors them ON them.id = f.following_actor_id
            WHERE me.user_id = $1 AND them.user_id IS NOT NULL
            UNION ALL
            SELECT them.user_id, 4
            FROM actors me
            JOIN follows f ON f.following_actor_id = me.id
            JOIN actors them ON them.id = f.follower_actor_id
            WHERE me.user_id = $1 AND them.user_id IS NOT NULL
        ),
        ranked AS (
            SELECT user_id, MIN(rank) AS rank FROM nearby GROUP BY user_id
        )
        SELECT
            u.login_name AS "login_name: LoginName",
            u.display_name,
            i.image_filename AS "banner_image_filename?"
        FROM users u
        LEFT JOIN ranked r ON r.user_id = u.id
        LEFT JOIN banners b ON b.id = u.banner_id
            AND b.deleted_at IS NULL
            AND b.is_explicit = false
        LEFT JOIN images i ON i.id = b.image_id
        WHERE u.deleted_at IS NULL
          AND u.id IS DISTINCT FROM $1
          AND (
            ($3 <> '' AND u.login_name ILIKE $3 ESCAPE '\')
            OR (r.user_id IS NOT NULL AND (
                u.login_name ILIKE $3 || '%' ESCAPE '\'
                OR u.display_name ILIKE $3 || '%' ESCAPE '\'
            ))
          )
        ORDER BY
            ($3 <> '' AND u.login_name ILIKE $3 ESCAPE '\') DESC,
            r.rank ASC NULLS LAST,
            u.login_name ASC
        LIMIT $4
        "#,
        query.reader,
        query.post,
        exact,
        query.limit
    )
    .fetch_all(&mut **tx)
    .await?;
    Ok(people)
}

/// Rows for tests that need people: accounts here with their actors, a
/// drawing to comment on and comments. Each is in the transaction it is
/// given, which the test leaves to roll back.
#[cfg(test)]
pub(crate) mod fixtures {
    use sqlx::{Postgres, Transaction};
    use uuid::Uuid;

    pub async fn tx() -> Option<Transaction<'static, Postgres>> {
        let url = std::env::var("DATABASE_URL").ok()?;
        let pool = sqlx::PgPool::connect(&url).await.ok()?;
        pool.begin().await.ok()
    }

    /// A tag that keeps one test's names apart from another's.
    pub fn tag() -> String {
        Uuid::new_v4().simple().to_string()[..10].to_string()
    }

    /// An account here and its actor: (user id, actor id).
    pub async fn person(
        tx: &mut Transaction<'_, Postgres>,
        login_name: &str,
        display_name: &str,
    ) -> (Uuid, Uuid) {
        let user = sqlx::query_scalar!(
            "INSERT INTO users (login_name, display_name) VALUES ($1, $2) RETURNING id",
            login_name,
            display_name
        )
        .fetch_one(&mut **tx)
        .await
        .unwrap();
        sqlx::query!("INSERT INTO instances (host) VALUES ('people.test') ON CONFLICT DO NOTHING")
            .execute(&mut **tx)
            .await
            .unwrap();
        let iri = format!("https://people.test/ap/users/{user}");
        let actor = sqlx::query_scalar!(
            r#"
            INSERT INTO actors (iri, url, type, username, instance_host, handle_host,
                                handle, name, inbox_url, followers_url, public_key_pem, user_id)
            VALUES ($1, $1, 'Person', $2, 'people.test', 'people.test', $2 || '@people.test',
                    $3, $1, $1, '', $4)
            RETURNING id
            "#,
            iri,
            login_name,
            display_name,
            user
        )
        .fetch_one(&mut **tx)
        .await
        .unwrap();
        (user, actor)
    }

    /// A published drawing by `author`.
    pub async fn post(tx: &mut Transaction<'_, Postgres>, author: Uuid) -> Uuid {
        let image = sqlx::query_scalar!(
            r#"
            INSERT INTO images (width, height, paint_duration, stroke_count, image_filename, tool)
            VALUES (10, 10, '0'::interval, 0, $1, 'neo')
            RETURNING id
            "#,
            format!("{}.png", Uuid::new_v4())
        )
        .fetch_one(&mut **tx)
        .await
        .unwrap();
        sqlx::query_scalar!(
            "INSERT INTO posts (author_id, image_id, published_at) VALUES ($1, $2, now()) RETURNING id",
            author,
            image
        )
        .fetch_one(&mut **tx)
        .await
        .unwrap()
    }

    pub async fn comment(
        tx: &mut Transaction<'_, Postgres>,
        post: Uuid,
        actor: Uuid,
        content: &str,
    ) -> Uuid {
        sqlx::query_scalar!(
            "INSERT INTO comments (post_id, actor_id, content) VALUES ($1, $2, $3) RETURNING id",
            post,
            actor,
            content
        )
        .fetch_one(&mut **tx)
        .await
        .unwrap()
    }

    pub async fn follow(tx: &mut Transaction<'_, Postgres>, follower: Uuid, following: Uuid) {
        sqlx::query!(
            "INSERT INTO follows (follower_actor_id, following_actor_id) VALUES ($1, $2)",
            follower,
            following
        )
        .execute(&mut **tx)
        .await
        .unwrap();
    }
}

#[cfg(test)]
mod tests {
    use super::fixtures::{comment, follow, person, post, tag, tx};
    use super::*;

    async fn names(
        tx: &mut Transaction<'_, Postgres>,
        reader: Uuid,
        q: &str,
        post: Option<Uuid>,
    ) -> Vec<String> {
        find_people(
            tx,
            &PeopleQuery {
                reader: Some(reader),
                q,
                post,
                limit: 20,
            },
        )
        .await
        .unwrap()
        .into_iter()
        .map(|person| person.login_name.to_string())
        .collect()
    }

    /// The start of a name finds the people near the reader -- whom they
    /// follow, who follows them, a post's author and commenters -- and
    /// nobody else; a handle typed in full finds anyone. The reader and a
    /// deleted account are never found.
    #[tokio::test]
    async fn a_name_begun_finds_nearby_people_and_a_handle_finds_anyone() {
        let Some(mut tx) = tx().await else { return };
        let t = tag();
        let (reader, reader_actor) = person(&mut tx, &format!("r{t}"), "Reader").await;
        let (_, followed) = person(&mut tx, &format!("p{t}a"), "Followed").await;
        let (_, follower) = person(&mut tx, &format!("p{t}b"), "Follower").await;
        let (author, _) = person(&mut tx, &format!("p{t}c"), "Author").await;
        let (_, commenter) = person(&mut tx, &format!("p{t}d"), "Commenter").await;
        person(&mut tx, &format!("p{t}e"), "Stranger").await;
        let (gone, _) = person(&mut tx, &format!("p{t}f"), "Gone").await;
        sqlx::query!("UPDATE users SET deleted_at = now() WHERE id = $1", gone)
            .execute(&mut *tx)
            .await
            .unwrap();
        follow(&mut tx, reader_actor, followed).await;
        follow(&mut tx, follower, reader_actor).await;
        let drawing = post(&mut tx, author).await;
        comment(&mut tx, drawing, commenter, "hi").await;
        // Its own deleted rows add nobody.
        let deleted = comment(&mut tx, drawing, reader_actor, "bye").await;
        sqlx::query!(
            "UPDATE comments SET deleted_at = now() WHERE id = $1",
            deleted
        )
        .execute(&mut *tx)
        .await
        .unwrap();

        let p = format!("p{t}");
        assert_eq!(
            names(&mut tx, reader, &p, None).await,
            [format!("p{t}a"), format!("p{t}b")],
            "away from a post, whom they follow and who follows them"
        );
        assert_eq!(
            names(&mut tx, reader, &format!("@{p}"), Some(drawing)).await,
            [
                format!("p{t}c"),
                format!("p{t}d"),
                format!("p{t}a"),
                format!("p{t}b")
            ],
            "under a post, its author and commenters first"
        );
        assert_eq!(
            names(&mut tx, reader, "", Some(drawing)).await.len(),
            4,
            "nothing typed is the nearby people"
        );
        // A display name begun, any case.
        assert_eq!(names(&mut tx, reader, "follo", None).await.len(), 2);
        // A stranger's handle in full, in any case, and first.
        assert_eq!(
            names(&mut tx, reader, &format!("P{t}E"), Some(drawing)).await,
            [format!("p{t}e")]
        );
        assert!(names(&mut tx, reader, "Strang", None).await.is_empty());
        assert!(names(&mut tx, reader, &format!("p{t}f"), None)
            .await
            .is_empty());
        assert!(names(&mut tx, reader, &format!("r{t}"), None)
            .await
            .is_empty());
        // `_` and `%` are letters, not patterns.
        assert!(names(&mut tx, reader, "%", None).await.is_empty());
        assert!(names(&mut tx, reader, &format!("p{t}_"), None)
            .await
            .is_empty());

        // Signed out, there is nobody nearby: only a handle in full.
        let anyone = |q: String| PeopleQuery {
            reader: None,
            q: Box::leak(q.into_boxed_str()),
            post: None,
            limit: 20,
        };
        assert!(find_people(&mut tx, &anyone(p.clone()))
            .await
            .unwrap()
            .is_empty());
        assert_eq!(
            find_people(&mut tx, &anyone(format!("p{t}a")))
                .await
                .unwrap()
                .len(),
            1
        );
    }
}
