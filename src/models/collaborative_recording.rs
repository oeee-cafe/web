//! The summary of a collaborative session's recording that the admin list
//! reads; see the `collaborative_session_recordings` migration.
//!
//! Every write is an upsert: a row appears the first time anything about the
//! recording is noted, from whichever of the writers gets there first.

use anyhow::Result;
use serde::Deserialize;
use sqlx::{query, PgPool};
use uuid::Uuid;

/// The span the stored log covers, as a manifest states it.
pub async fn note_span(
    pool: &PgPool,
    session_id: Uuid,
    first_seq: Option<u64>,
    last_seq: Option<u64>,
    messages: u64,
    sealed: bool,
) -> Result<()> {
    query!(
        r#"
        INSERT INTO collaborative_session_recordings (session_id, first_seq, last_seq, messages, sealed)
        VALUES ($1, $2, $3, $4, $5)
        ON CONFLICT (session_id) DO UPDATE SET
            first_seq = EXCLUDED.first_seq,
            last_seq = EXCLUDED.last_seq,
            messages = EXCLUDED.messages,
            sealed = EXCLUDED.sealed,
            updated_at = now()
        "#,
        session_id,
        first_seq.map(|seq| seq as i64),
        last_seq.map(|seq| seq as i64),
        messages as i64,
        sealed,
    )
    .execute(pool)
    .await?;
    Ok(())
}

/// How long the stored transcript is. The transcript is stored whole each
/// time, so this is its length and not an increment.
pub async fn note_chat_lines(pool: &PgPool, session_id: Uuid, lines: usize) -> Result<()> {
    query!(
        r#"
        INSERT INTO collaborative_session_recordings (session_id, chat_lines)
        VALUES ($1, $2)
        ON CONFLICT (session_id) DO UPDATE SET
            chat_lines = GREATEST(collaborative_session_recordings.chat_lines, EXCLUDED.chat_lines),
            updated_at = now()
        "#,
        session_id,
        lines as i32,
    )
    .execute(pool)
    .await?;
    Ok(())
}

/// One more synchronisation report filed.
pub async fn note_report(pool: &PgPool, session_id: Uuid) -> Result<()> {
    query!(
        r#"
        INSERT INTO collaborative_session_recordings (session_id, reports)
        VALUES ($1, 1)
        ON CONFLICT (session_id) DO UPDATE SET
            reports = collaborative_session_recordings.reports + 1,
            updated_at = now()
        "#,
        session_id,
    )
    .execute(pool)
    .await?;
    Ok(())
}

/// What the inspector found when it played a recording back.
#[derive(Debug, Deserialize)]
pub struct ReplayCheck {
    /// `match`, `differs`, `incomplete` or `unavailable`; the table's CHECK
    /// constraint holds the list.
    pub outcome: String,
    pub differing_pixels: Option<i64>,
    pub total_pixels: Option<i64>,
    pub seq: Option<i64>,
    pub note: Option<String>,
    /// What the inspector read on its way, for sessions recorded before any
    /// of this was noted as it happened. Counts only ever grow here: a
    /// transcript or a report the inspector could not read is not evidence
    /// that there is none.
    pub chat_lines: i32,
    pub reports: i32,
    pub span: Option<ReplaySpan>,
}

#[derive(Debug, Deserialize)]
pub struct ReplaySpan {
    pub first_seq: Option<i64>,
    pub last_seq: Option<i64>,
    pub messages: i64,
    pub sealed: bool,
}

pub async fn note_check(
    pool: &PgPool,
    session_id: Uuid,
    checked_by: Uuid,
    check: &ReplayCheck,
) -> Result<()> {
    let span = check.span.as_ref();
    query!(
        r#"
        INSERT INTO collaborative_session_recordings (
            session_id, first_seq, last_seq, messages, sealed, chat_lines, reports,
            check_outcome, check_differing_pixels, check_total_pixels, check_seq, check_note,
            checked_at, checked_by
        )
        VALUES ($1, $2, $3, COALESCE($4::bigint, 0), COALESCE($5::boolean, FALSE), $6, $7, $8, $9, $10, $11, $12, now(), $13)
        ON CONFLICT (session_id) DO UPDATE SET
            first_seq = COALESCE(EXCLUDED.first_seq, collaborative_session_recordings.first_seq),
            last_seq = COALESCE(EXCLUDED.last_seq, collaborative_session_recordings.last_seq),
            messages = GREATEST(collaborative_session_recordings.messages, EXCLUDED.messages),
            sealed = collaborative_session_recordings.sealed OR EXCLUDED.sealed,
            chat_lines = GREATEST(collaborative_session_recordings.chat_lines, EXCLUDED.chat_lines),
            reports = GREATEST(collaborative_session_recordings.reports, EXCLUDED.reports),
            check_outcome = EXCLUDED.check_outcome,
            check_differing_pixels = EXCLUDED.check_differing_pixels,
            check_total_pixels = EXCLUDED.check_total_pixels,
            check_seq = EXCLUDED.check_seq,
            check_note = EXCLUDED.check_note,
            checked_at = EXCLUDED.checked_at,
            checked_by = EXCLUDED.checked_by,
            updated_at = now()
        "#,
        session_id,
        span.and_then(|span| span.first_seq),
        span.and_then(|span| span.last_seq),
        span.map(|span| span.messages),
        span.map(|span| span.sealed),
        check.chat_lines,
        check.reports,
        check.outcome,
        check.differing_pixels,
        check.total_pixels,
        check.seq,
        check.note,
        checked_by,
    )
    .execute(pool)
    .await?;
    Ok(())
}
