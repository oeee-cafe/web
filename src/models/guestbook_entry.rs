use crate::models::handle::LoginName;
use anyhow::Result;
use chrono::{DateTime, Utc};

use serde::Serialize;
use sqlx::{query, Postgres, Transaction};
use uuid::Uuid;

#[derive(Clone, Debug, Serialize)]
pub struct GuestbookEntry {
    pub id: Uuid,
    pub author_id: Uuid,
    pub recipient_id: Uuid,
    pub content: String,
    pub reply: Option<String>,
    pub created_at: DateTime<Utc>,
    pub replied_at: Option<DateTime<Utc>>,
}

#[derive(Clone, Debug, Serialize)]
pub struct SerializableGuestbookEntry {
    pub id: Uuid,
    pub author_id: Uuid,
    pub author_login_name: LoginName,
    pub author_display_name: String,
    pub recipient_login_name: LoginName,
    pub recipient_display_name: String,
    pub recipient_id: Uuid,
    pub content: String,
    pub reply: Option<String>,
    pub created_at: DateTime<Utc>,
    pub replied_at: Option<DateTime<Utc>>,
}

pub struct GuestbookEntryDraft {
    pub author_id: Uuid,
    pub recipient_id: Uuid,
    pub content: String,
}

pub async fn add_guestbook_entry_reply(
    tx: &mut Transaction<'_, Postgres>,
    entry_id: Uuid,
    reply: String,
) -> Result<DateTime<Utc>> {
    let replied_at = query!(
        "
            UPDATE guestbook_entries
            SET reply = $1, replied_at = NOW()
            WHERE id = $2
            RETURNING replied_at
        ",
        reply,
        entry_id
    )
    .fetch_one(&mut **tx)
    .await?
    .replied_at
    .expect("Valid guestbook entry ID must be parseable as i32");

    Ok(replied_at)
}

pub async fn find_guestbook_entry_by_id(
    tx: &mut Transaction<'_, Postgres>,
    entry_id: Uuid,
) -> Result<Option<SerializableGuestbookEntry>> {
    let entry = query!(
        "
                SELECT
                guestbook_entries.id,
                author.login_name AS author_login_name,
                author.display_name AS author_display_name,
                recipient.login_name AS recipient_login_name,
                recipient.display_name AS recipient_display_name,
                guestbook_entries.author_id,
                guestbook_entries.recipient_id,
                guestbook_entries.content,
                guestbook_entries.reply,
                guestbook_entries.created_at,
                guestbook_entries.replied_at
            FROM guestbook_entries
            JOIN users AS author ON author.id = author_id
            JOIN users AS recipient ON recipient.id = recipient_id
            WHERE guestbook_entries.id = $1
        ",
        entry_id
    )
    .fetch_optional(&mut **tx)
    .await?;

    Ok(entry.map(|entry| SerializableGuestbookEntry {
        id: entry.id,
        author_id: entry.author_id,
        author_login_name: entry.author_login_name.into(),
        author_display_name: entry.author_display_name,
        recipient_id: entry.recipient_id,
        recipient_login_name: entry.recipient_login_name.into(),
        recipient_display_name: entry.recipient_display_name,
        content: entry.content,
        reply: entry.reply,
        created_at: entry.created_at,
        replied_at: entry.replied_at,
    }))
}

pub async fn delete_guestbook_entry(
    tx: &mut Transaction<'_, Postgres>,
    entry_id: Uuid,
) -> Result<()> {
    query!(
        "
            DELETE FROM guestbook_entries
            WHERE id = $1
        ",
        entry_id
    )
    .execute(&mut **tx)
    .await?;

    Ok(())
}

pub async fn create_guestbook_entry(
    tx: &mut Transaction<'_, Postgres>,
    draft: GuestbookEntryDraft,
) -> Result<SerializableGuestbookEntry> {
    let entry = query!(
        "
            INSERT INTO guestbook_entries (author_id, recipient_id, content)
            VALUES ($1, $2, $3)
            RETURNING id, created_at
        ",
        draft.author_id,
        draft.recipient_id,
        draft.content
    )
    .fetch_one(&mut **tx)
    .await?;

    let entry = query!(
        "
            SELECT
                guestbook_entries.id,
                author.login_name AS author_login_name,
                author.display_name AS author_display_name,
                recipient.login_name AS recipient_login_name,
                recipient.display_name AS recipient_display_name,
                guestbook_entries.author_id,
                guestbook_entries.recipient_id,
                guestbook_entries.content,
                guestbook_entries.reply,
                guestbook_entries.created_at,
                guestbook_entries.replied_at
            FROM guestbook_entries
            JOIN users AS author ON author.id = author_id
            JOIN users AS recipient ON recipient.id = recipient_id
            WHERE guestbook_entries.id = $1
        ",
        entry.id
    )
    .fetch_one(&mut **tx)
    .await?;

    Ok(SerializableGuestbookEntry {
        id: entry.id,
        author_id: entry.author_id,
        author_login_name: entry.author_login_name.into(),
        author_display_name: entry.author_display_name,
        recipient_login_name: entry.recipient_login_name.into(),
        recipient_display_name: entry.recipient_display_name,
        recipient_id: entry.recipient_id,
        content: entry.content,
        reply: entry.reply,
        created_at: entry.created_at,
        replied_at: entry.replied_at,
    })
}

pub async fn find_guestbook_entries_by_recipient_id(
    tx: &mut Transaction<'_, Postgres>,
    recipient_id: Uuid,
) -> Result<Vec<SerializableGuestbookEntry>> {
    let entries = query!(
        "
            SELECT
                guestbook_entries.id,
                guestbook_entries.author_id,
                author.login_name AS author_login_name,
                author.display_name AS author_display_name,
                recipient.login_name AS recipient_login_name,
                recipient.display_name AS recipient_display_name,
                guestbook_entries.recipient_id,
                guestbook_entries.content,
                guestbook_entries.reply,
                guestbook_entries.created_at,
                guestbook_entries.replied_at
            FROM guestbook_entries
            JOIN users AS author ON author.id = author_id
            JOIN users AS recipient ON recipient.id = recipient_id
            WHERE recipient_id = $1
            ORDER BY created_at DESC
        ",
        recipient_id
    )
    .fetch_all(&mut **tx)
    .await?;

    Ok(entries
        .into_iter()
        .map(|entry| SerializableGuestbookEntry {
            id: entry.id,
            author_id: entry.author_id,
            author_login_name: entry.author_login_name.into(),
            author_display_name: entry.author_display_name,
            recipient_login_name: entry.recipient_login_name.into(),
            recipient_display_name: entry.recipient_display_name,
            recipient_id: entry.recipient_id,
            content: entry.content,
            reply: entry.reply,
            created_at: entry.created_at,
            replied_at: entry.replied_at,
        })
        .collect())
}
