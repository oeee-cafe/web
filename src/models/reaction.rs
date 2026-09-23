use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::{Postgres, Transaction};
use uuid::Uuid;

// Available emoji reactions
pub const AVAILABLE_EMOJIS: &[&str] = &["❤️", "🎉", "😂", "😲", "🤔", "😢", "👀"];

#[derive(Clone, Debug, Serialize)]
pub struct Reaction {
    pub iri: String,
    pub post_id: Uuid,
    pub actor_id: Uuid,
    pub emoji: String,
    pub created_at: DateTime<Utc>,
}

pub struct ReactionDraft {
    pub post_id: Uuid,
    pub actor_id: Uuid,
    pub emoji: String,
}

#[derive(Serialize)]
pub struct SerializableReaction {
    pub iri: String,
    pub post_id: Uuid,
    pub actor_id: Uuid,
    pub emoji: String,
    pub created_at: DateTime<Utc>,
    pub actor_name: String,
    pub actor_handle: String,
    pub actor_url: String,
    pub actor_login_name: Option<String>,
}

#[derive(Serialize)]
pub struct ReactionCount {
    pub emoji: String,
    pub count: i64,
    pub reacted_by_user: bool,
}

pub async fn create_reaction(
    tx: &mut Transaction<'_, Postgres>,
    draft: ReactionDraft,
    domain: &str,
) -> Result<Reaction> {
    // Generate IRI for local reactions
    let iri = format!(
        "https://{}/ap/emojireacts/{}/{}",
        domain,
        draft.post_id,
        uuid::Uuid::new_v4()
    );

    let reaction = sqlx::query_as!(
        Reaction,
        r#"
        INSERT INTO reactions (iri, post_id, actor_id, emoji)
        VALUES ($1, $2, $3, $4)
        RETURNING iri, post_id, actor_id, emoji, created_at
        "#,
        iri,
        draft.post_id,
        draft.actor_id,
        draft.emoji
    )
    .fetch_one(&mut **tx)
    .await?;

    Ok(reaction)
}

pub async fn create_reaction_from_activitypub(
    tx: &mut Transaction<'_, Postgres>,
    iri: String,
    post_id: Uuid,
    actor_id: Uuid,
    emoji: String,
) -> Result<Reaction> {
    let reaction = sqlx::query_as!(
        Reaction,
        r#"
        INSERT INTO reactions (iri, post_id, actor_id, emoji)
        VALUES ($1, $2, $3, $4)
        ON CONFLICT (iri) DO NOTHING
        RETURNING iri, post_id, actor_id, emoji, created_at
        "#,
        iri,
        post_id,
        actor_id,
        emoji
    )
    .fetch_one(&mut **tx)
    .await?;

    Ok(reaction)
}

pub async fn find_reactions_by_post_id(
    tx: &mut Transaction<'_, Postgres>,
    post_id: Uuid,
) -> Result<Vec<SerializableReaction>> {
    let reactions = sqlx::query!(
        r#"
        SELECT
            reactions.iri,
            reactions.post_id,
            reactions.actor_id,
            reactions.emoji,
            reactions.created_at,
            actors.name AS actor_name,
            actors.handle AS actor_handle,
            actors.url AS actor_url,
            users.login_name AS "actor_login_name?"
        FROM reactions
        LEFT JOIN actors ON reactions.actor_id = actors.id
        LEFT JOIN users ON actors.user_id = users.id
        WHERE post_id = $1
        ORDER BY created_at DESC
        "#,
        post_id
    )
    .fetch_all(&mut **tx)
    .await?;

    Ok(reactions
        .into_iter()
        .map(|reaction| SerializableReaction {
            iri: reaction.iri,
            post_id: reaction.post_id,
            actor_id: reaction.actor_id,
            emoji: reaction.emoji,
            created_at: reaction.created_at,
            actor_name: reaction.actor_name,
            actor_handle: reaction.actor_handle,
            actor_url: reaction.actor_url,
            actor_login_name: reaction.actor_login_name,
        })
        .collect())
}

pub async fn get_reaction_counts(
    tx: &mut Transaction<'_, Postgres>,
    post_id: Uuid,
    user_actor_id: Option<Uuid>,
) -> Result<Vec<ReactionCount>> {
    let reactions = sqlx::query!(
        r#"
        SELECT
            emoji,
            COUNT(*) as count,
            COALESCE(BOOL_OR(actor_id = $2), false) as "reacted_by_user!"
        FROM reactions
        WHERE post_id = $1
        GROUP BY emoji
        ORDER BY count DESC, emoji
        "#,
        post_id,
        user_actor_id
    )
    .fetch_all(&mut **tx)
    .await?;

    // Convert to a map for easy lookup
    let mut reaction_map: std::collections::HashMap<String, (i64, bool)> = reactions
        .into_iter()
        .map(|row| {
            (
                row.emoji.clone(),
                (row.count.unwrap_or(0), row.reacted_by_user),
            )
        })
        .collect();

    // Always return all available emojis in order, with count 0 for unused ones
    let mut result: Vec<ReactionCount> = AVAILABLE_EMOJIS
        .iter()
        .map(|emoji| {
            let (count, reacted_by_user) = reaction_map.remove(*emoji).unwrap_or((0, false));
            ReactionCount {
                emoji: emoji.to_string(),
                count,
                reacted_by_user,
            }
        })
        .collect();

    // Add any custom emojis that aren't in the standard list
    for (emoji, (count, reacted_by_user)) in reaction_map {
        result.push(ReactionCount {
            emoji,
            count,
            reacted_by_user,
        });
    }

    Ok(result)
}

pub async fn delete_reaction(
    tx: &mut Transaction<'_, Postgres>,
    post_id: Uuid,
    actor_id: Uuid,
    emoji: &str,
) -> Result<bool> {
    let result = sqlx::query!(
        r#"
        DELETE FROM reactions
        WHERE post_id = $1 AND actor_id = $2 AND emoji = $3
        RETURNING iri
        "#,
        post_id,
        actor_id,
        emoji
    )
    .fetch_optional(&mut **tx)
    .await?;

    Ok(result.is_some())
}

pub async fn find_reaction_by_iri(
    tx: &mut Transaction<'_, Postgres>,
    iri: &str,
) -> Result<Option<Reaction>> {
    let reaction = sqlx::query_as!(
        Reaction,
        r#"
        SELECT iri, post_id, actor_id, emoji, created_at
        FROM reactions
        WHERE iri = $1
        "#,
        iri
    )
    .fetch_optional(&mut **tx)
    .await?;

    Ok(reaction)
}

pub async fn delete_reaction_by_iri(tx: &mut Transaction<'_, Postgres>, iri: &str) -> Result<bool> {
    let result = sqlx::query!(
        r#"
        DELETE FROM reactions
        WHERE iri = $1
        "#,
        iri
    )
    .execute(&mut **tx)
    .await?;

    Ok(result.rows_affected() > 0)
}

pub async fn find_user_reaction(
    tx: &mut Transaction<'_, Postgres>,
    post_id: Uuid,
    actor_id: Uuid,
    emoji: &str,
) -> Result<Option<Reaction>> {
    let reaction = sqlx::query_as!(
        Reaction,
        r#"
        SELECT iri, post_id, actor_id, emoji, created_at
        FROM reactions
        WHERE post_id = $1 AND actor_id = $2 AND emoji = $3
        "#,
        post_id,
        actor_id,
        emoji
    )
    .fetch_optional(&mut **tx)
    .await?;

    Ok(reaction)
}
