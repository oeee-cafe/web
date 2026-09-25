//! The session store: `tower-sessions-sqlx-store`'s `PostgresStore`, brought
//! in-tree.
//!
//! That crate turns on sqlx's `time` feature, and once `time` is on, sqlx
//! 0.8's `query!` macros map every `timestamptz` to `OffsetDateTime` instead
//! of the `chrono` types the models are written against. Nothing else here
//! needs `time` from sqlx, so the store binds its one timestamp as `chrono`.
//!
//! The row format is unchanged -- the same `sessions` table, created by the
//! init migration, holding the `Record` as MessagePack -- so a deploy does not
//! sign anybody out, and the colour being replaced reads what this one writes.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sqlx::{PgConnection, PgPool};
use tower_sessions::session::{Id, Record};
use tower_sessions::session_store::{self, ExpiredDeletion, SessionStore};

#[derive(Clone, Debug)]
pub struct PostgresStore {
    pool: PgPool,
}

impl PostgresStore {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    async fn id_exists(&self, conn: &mut PgConnection, id: &Id) -> session_store::Result<bool> {
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM sessions WHERE id = $1)")
            .bind(id.to_string())
            .fetch_one(conn)
            .await
            .map_err(backend)
    }

    async fn save_with_conn(
        &self,
        conn: &mut PgConnection,
        record: &Record,
    ) -> session_store::Result<()> {
        let data =
            rmp_serde::to_vec(record).map_err(|e| session_store::Error::Encode(e.to_string()))?;
        let expiry_date =
            DateTime::<Utc>::from_timestamp_nanos(record.expiry_date.unix_timestamp_nanos() as i64);
        sqlx::query(
            r#"
            INSERT INTO sessions (id, data, expiry_date)
            VALUES ($1, $2, $3)
            ON CONFLICT (id) DO UPDATE
            SET data = excluded.data, expiry_date = excluded.expiry_date
            "#,
        )
        .bind(record.id.to_string())
        .bind(data)
        .bind(expiry_date)
        .execute(conn)
        .await
        .map_err(backend)?;
        Ok(())
    }
}

fn backend(e: sqlx::Error) -> session_store::Error {
    session_store::Error::Backend(e.to_string())
}

#[async_trait]
impl ExpiredDeletion for PostgresStore {
    async fn delete_expired(&self) -> session_store::Result<()> {
        sqlx::query("DELETE FROM sessions WHERE expiry_date < now()")
            .execute(&self.pool)
            .await
            .map_err(backend)?;
        Ok(())
    }
}

#[async_trait]
impl SessionStore for PostgresStore {
    async fn create(&self, record: &mut Record) -> session_store::Result<()> {
        let mut tx = self.pool.begin().await.map_err(backend)?;
        while self.id_exists(&mut tx, &record.id).await? {
            record.id = Id::default();
        }
        self.save_with_conn(&mut tx, record).await?;
        tx.commit().await.map_err(backend)?;
        Ok(())
    }

    async fn save(&self, record: &Record) -> session_store::Result<()> {
        let mut conn = self.pool.acquire().await.map_err(backend)?;
        self.save_with_conn(&mut conn, record).await
    }

    async fn load(&self, session_id: &Id) -> session_store::Result<Option<Record>> {
        let data: Option<Vec<u8>> =
            sqlx::query_scalar("SELECT data FROM sessions WHERE id = $1 AND expiry_date > now()")
                .bind(session_id.to_string())
                .fetch_optional(&self.pool)
                .await
                .map_err(backend)?;
        data.map(|data| {
            rmp_serde::from_slice(&data).map_err(|e| session_store::Error::Decode(e.to_string()))
        })
        .transpose()
    }

    async fn delete(&self, session_id: &Id) -> session_store::Result<()> {
        sqlx::query("DELETE FROM sessions WHERE id = $1")
            .bind(session_id.to_string())
            .execute(&self.pool)
            .await
            .map_err(backend)?;
        Ok(())
    }
}
