//! Accounts from other services that sign into an Oeee Cafe account: Steam,
//! Apple and Google now, Microsoft after them.
//!
//! A provider is only ever asked one thing -- who is this? -- and answers
//! with a [`VerifiedIdentity`]. Everything after that (signing in, linking to
//! the account already signed in, linking by email, making a new account) is
//! the same for every provider and lives in `web::handlers::identity`.

use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::types::Uuid;
use sqlx::{query, query_as, Postgres, Transaction};

use super::user::User;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Provider {
    Steam,
    Apple,
    Google,
}

impl Provider {
    /// The name stored in `user_identities.provider` and used in URLs.
    pub fn as_str(self) -> &'static str {
        match self {
            Provider::Steam => "steam",
            Provider::Apple => "apple",
            Provider::Google => "google",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "steam" => Some(Provider::Steam),
            "apple" => Some(Provider::Apple),
            "google" => Some(Provider::Google),
            _ => None,
        }
    }

    /// The provider's own name for itself, which is not translated.
    pub fn display_name(self) -> &'static str {
        match self {
            Provider::Steam => "Steam",
            Provider::Apple => "Apple",
            Provider::Google => "Google",
        }
    }
}

/// Who a provider says someone is, once what the client sent has been
/// checked with that provider.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct VerifiedIdentity {
    pub provider: Provider,
    /// The provider's stable id for the person: a SteamID64, Apple's or
    /// Google's `sub`.
    pub subject: String,
    /// What the provider calls them, such as a Steam persona name. Offered as
    /// the display name of a new account.
    pub name: Option<String>,
    /// An address the provider vouches for. Trusted: an Oeee Cafe account
    /// with this address, verified, is signed into and linked.
    pub email: Option<String>,
    /// Whether the provider says this account owns the Supporter Pack now --
    /// the DLC, on Steam. `None` when that could not be asked, which leaves
    /// the account's standing as it was. Whichever account it signs into is a
    /// supporter while it is `Some(true)`, and keeps the achievement after.
    #[serde(default)]
    pub purchased: Option<bool>,
}

impl VerifiedIdentity {
    /// The achievement for having bought the Supporter Pack from this
    /// provider.
    fn supporter_achievement(&self) -> Option<&'static str> {
        match self.provider {
            Provider::Steam => Some("STEAM_SUPPORTER"),
            Provider::Apple | Provider::Google => None,
        }
        .filter(|_| self.purchased == Some(true))
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct Identity {
    pub id: Uuid,
    pub user_id: Uuid,
    pub provider: String,
    pub subject: String,
    pub display_hint: Option<String>,
    pub email: Option<String>,
    pub created_at: DateTime<Utc>,
    pub last_used_at: DateTime<Utc>,
}

/// The account a provider's identity signs into, if it is linked to one that
/// has not been deleted.
pub async fn find_user_by_identity(
    tx: &mut Transaction<'_, Postgres>,
    provider: Provider,
    subject: &str,
) -> Result<Option<User>> {
    let user = query_as!(
        User,
        r#"
        SELECT
            users.id,
            users.login_name,
            users.password_hash,
            users.display_name,
            users.email,
            users.email_verified_at,
            users.created_at,
            users.updated_at,
            users.banner_id,
            users.preferred_language AS "preferred_language: _",
            users.deleted_at,
            users.show_sensitive_content,
            users.role AS "role: _"
        FROM user_identities
        JOIN users ON users.id = user_identities.user_id
        WHERE user_identities.provider = $1
          AND user_identities.subject = $2
          AND users.deleted_at IS NULL
        "#,
        provider.as_str(),
        subject,
    )
    .fetch_optional(&mut **tx)
    .await?;
    Ok(user)
}

/// The account whose verified address is `email`, compared without regard to
/// case. Only a verified address counts: anyone can type any address into
/// their account page, and an unverified one proves nothing about who owns
/// the account.
pub async fn find_user_by_verified_email(
    tx: &mut Transaction<'_, Postgres>,
    email: &str,
) -> Result<Option<User>> {
    let user = query_as!(
        User,
        r#"
        SELECT
            id,
            login_name,
            password_hash,
            display_name,
            email,
            email_verified_at,
            created_at,
            updated_at,
            banner_id,
            preferred_language AS "preferred_language: _",
            deleted_at,
            show_sensitive_content,
            role AS "role: _"
        FROM users
        WHERE lower(email) = lower($1)
          AND email_verified_at IS NOT NULL
          AND deleted_at IS NULL
        ORDER BY email_verified_at
        LIMIT 1
        "#,
        email,
    )
    .fetch_optional(&mut **tx)
    .await?;
    Ok(user)
}

pub async fn list_identities_for_user(
    tx: &mut Transaction<'_, Postgres>,
    user_id: Uuid,
) -> Result<Vec<Identity>> {
    let identities = query_as!(
        Identity,
        r#"
        SELECT id, user_id, provider, subject, display_hint, email, created_at, last_used_at
        FROM user_identities
        WHERE user_id = $1
        ORDER BY created_at
        "#,
        user_id,
    )
    .fetch_all(&mut **tx)
    .await?;
    Ok(identities)
}

/// Why an identity could not be linked.
#[derive(Debug, PartialEq, Eq)]
pub enum LinkError {
    /// This provider account already signs into another Oeee Cafe account.
    LinkedElsewhere,
    /// The account already has an account from this provider linked.
    ProviderAlreadyLinked,
}

pub async fn link_identity(
    tx: &mut Transaction<'_, Postgres>,
    user_id: Uuid,
    identity: &VerifiedIdentity,
) -> Result<std::result::Result<(), LinkError>> {
    // Inserted first and asked about after, so two links racing for the same
    // provider account each get an answer rather than one getting a unique
    // violation.
    let inserted = query!(
        r#"
        INSERT INTO user_identities (user_id, provider, subject, display_hint, email)
        VALUES ($1, $2, $3, $4, $5)
        ON CONFLICT DO NOTHING
        "#,
        user_id,
        identity.provider.as_str(),
        identity.subject,
        identity.name,
        identity.email,
    )
    .execute(&mut **tx)
    .await?;
    if inserted.rows_affected() == 1 {
        super::supporter::record_supporter_check_for(tx, identity).await?;
        if let Some(achievement) = identity.supporter_achievement() {
            super::achievement::grant_achievement(tx, user_id, achievement).await?;
        }
        if identity.provider == Provider::Steam {
            // A Steam account newly linked is given everything already
            // earned, drawn before Steam or not, and told about all of it.
            super::achievement::award_achievements(tx, user_id).await?;
            super::achievement::resend_achievements_to_steam(tx, user_id).await?;
        }
        return Ok(Ok(()));
    }

    let owner = query!(
        "SELECT user_id FROM user_identities WHERE provider = $1 AND subject = $2",
        identity.provider.as_str(),
        identity.subject,
    )
    .fetch_optional(&mut **tx)
    .await?;
    match owner {
        Some(row) if row.user_id == user_id => {
            touch_identity(tx, identity).await?;
            Ok(Ok(()))
        }
        Some(_) => Ok(Err(LinkError::LinkedElsewhere)),
        None => Ok(Err(LinkError::ProviderAlreadyLinked)),
    }
}

/// Records a sign-in, keeping the name and address shown on the account page
/// current with what the provider says now.
pub async fn touch_identity(
    tx: &mut Transaction<'_, Postgres>,
    identity: &VerifiedIdentity,
) -> Result<()> {
    query!(
        r#"
        UPDATE user_identities
        SET last_used_at = now(),
            display_hint = COALESCE($3, display_hint),
            email = COALESCE($4, email)
        WHERE provider = $1 AND subject = $2
        "#,
        identity.provider.as_str(),
        identity.subject,
        identity.name,
        identity.email,
    )
    .execute(&mut **tx)
    .await?;
    refresh_standing(tx, identity).await
}

/// Records what the provider says now about the Supporter Pack for the
/// account `identity` is linked to, and gives it the achievement if it owns
/// one -- bought since it was linked, or before the achievement existed.
/// Neither signs anyone in nor links anything: an identity linked to no
/// account changes nothing.
pub async fn refresh_standing(
    tx: &mut Transaction<'_, Postgres>,
    identity: &VerifiedIdentity,
) -> Result<()> {
    super::supporter::record_supporter_check_for(tx, identity).await?;
    if let Some(achievement) = identity.supporter_achievement() {
        query!(
            r#"
            INSERT INTO user_achievements (user_id, achievement)
            SELECT user_id, $3 FROM user_identities WHERE provider = $1 AND subject = $2
            ON CONFLICT DO NOTHING
            "#,
            identity.provider.as_str(),
            identity.subject,
            achievement,
        )
        .execute(&mut **tx)
        .await?;
    }
    Ok(())
}

/// Why an identity could not be unlinked.
#[derive(Debug, PartialEq, Eq)]
pub enum UnlinkError {
    NotLinked,
    /// It is the only way into an account with no password: unlinking it
    /// would leave nobody able to sign in.
    LastSignIn,
}

pub async fn unlink_identity(
    tx: &mut Transaction<'_, Postgres>,
    user: &User,
    provider: Provider,
) -> Result<std::result::Result<(), UnlinkError>> {
    // Locks the account's identities so two unlinks at once cannot each see
    // the other still there and together leave none.
    let identities = query!(
        "SELECT provider FROM user_identities WHERE user_id = $1 FOR UPDATE",
        user.id,
    )
    .fetch_all(&mut **tx)
    .await?;

    if !identities
        .iter()
        .any(|row| row.provider == provider.as_str())
    {
        return Ok(Err(UnlinkError::NotLinked));
    }
    if !user.has_password() && identities.len() <= 1 {
        return Ok(Err(UnlinkError::LastSignIn));
    }

    query!(
        "DELETE FROM user_identities WHERE user_id = $1 AND provider = $2",
        user.id,
        provider.as_str(),
    )
    .execute(&mut **tx)
    .await?;
    Ok(Ok(()))
}

/// Against the database `DATABASE_URL` names, inside a transaction that is
/// never committed. Skipped when there is no database to reach.
#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::PgPool;

    async fn tx() -> Option<Transaction<'static, Postgres>> {
        let url = std::env::var("DATABASE_URL").ok()?;
        let pool = PgPool::connect(&url).await.ok()?;
        pool.begin().await.ok()
    }

    async fn user(
        tx: &mut Transaction<'_, Postgres>,
        login_name: &str,
        password_hash: Option<&str>,
        verified_email: Option<&str>,
    ) -> User {
        let id: Uuid = query!(
            r#"
            INSERT INTO users (login_name, display_name, password_hash, email, email_verified_at)
            VALUES ($1, $1, $2, $3::varchar, CASE WHEN $3::varchar IS NULL THEN NULL ELSE now() END)
            RETURNING id
            "#,
            login_name,
            password_hash,
            verified_email,
        )
        .fetch_one(&mut **tx)
        .await
        .unwrap()
        .id;
        crate::models::user::find_user_by_id(tx, id)
            .await
            .unwrap()
            .unwrap()
    }

    fn steam(subject: &str) -> VerifiedIdentity {
        VerifiedIdentity {
            provider: Provider::Steam,
            subject: subject.to_string(),
            name: Some("오이".to_string()),
            email: None,
            purchased: Some(false),
        }
    }

    #[tokio::test]
    async fn an_identity_signs_into_the_one_account_it_is_linked_to() {
        let Some(mut tx) = tx().await else { return };
        let a = user(&mut tx, "identity_test_a", None, None).await;
        let b = user(&mut tx, "identity_test_b", None, None).await;
        let id = steam("76561190000000001");

        assert!(find_user_by_identity(&mut tx, Provider::Steam, &id.subject)
            .await
            .unwrap()
            .is_none());
        assert_eq!(link_identity(&mut tx, a.id, &id).await.unwrap(), Ok(()));
        // Linking again to the same account is not an error.
        assert_eq!(link_identity(&mut tx, a.id, &id).await.unwrap(), Ok(()));
        assert_eq!(
            find_user_by_identity(&mut tx, Provider::Steam, &id.subject)
                .await
                .unwrap()
                .map(|u| u.id),
            Some(a.id)
        );

        assert_eq!(
            link_identity(&mut tx, b.id, &id).await.unwrap(),
            Err(LinkError::LinkedElsewhere)
        );
        assert_eq!(
            link_identity(&mut tx, a.id, &steam("76561190000000002"))
                .await
                .unwrap(),
            Err(LinkError::ProviderAlreadyLinked)
        );
        tx.rollback().await.unwrap();
    }

    #[tokio::test]
    async fn the_last_way_into_an_account_without_a_password_stays() {
        let Some(mut tx) = tx().await else { return };
        let no_password = user(&mut tx, "identity_test_c", None, None).await;
        let id = steam("76561190000000003");
        link_identity(&mut tx, no_password.id, &id)
            .await
            .unwrap()
            .unwrap();
        assert!(!no_password.has_password());
        assert!(no_password.verify_password("").is_err());

        assert_eq!(
            unlink_identity(&mut tx, &no_password, Provider::Steam)
                .await
                .unwrap(),
            Err(UnlinkError::LastSignIn)
        );

        // With a password it can go.
        let hash = UserDraftHash::of("correct horse");
        let with_password = user(&mut tx, "identity_test_d", Some(&hash), None).await;
        let other = steam("76561190000000004");
        link_identity(&mut tx, with_password.id, &other)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            unlink_identity(&mut tx, &with_password, Provider::Steam)
                .await
                .unwrap(),
            Ok(())
        );
        assert_eq!(
            unlink_identity(&mut tx, &with_password, Provider::Steam)
                .await
                .unwrap(),
            Err(UnlinkError::NotLinked)
        );
        tx.rollback().await.unwrap();
    }

    #[tokio::test]
    async fn linking_steam_hands_over_what_was_already_earned() {
        let Some(mut tx) = tx().await else { return };
        let artist = user(&mut tx, "identity_test_g", None, None).await;
        let image = query!(
            r#"
            INSERT INTO images (width, height, paint_duration, stroke_count, image_filename, tool)
            VALUES (10, 10, '0'::interval, 0, $1, 'neo') RETURNING id
            "#,
            format!("{}.png", Uuid::new_v4()),
        )
        .fetch_one(&mut *tx)
        .await
        .unwrap()
        .id;
        query!(
            "INSERT INTO posts (author_id, image_id, published_at) VALUES ($1, $2, now())",
            artist.id,
            image
        )
        .execute(&mut *tx)
        .await
        .unwrap();

        link_identity(&mut tx, artist.id, &steam("76561190000000010"))
            .await
            .unwrap()
            .unwrap();
        let waiting = crate::models::achievement::unsynced_achievements(&mut tx, 1000)
            .await
            .unwrap()
            .into_iter()
            .find(|u| u.user_id == artist.id)
            .expect("waiting for Steam");
        assert_eq!(waiting.steam_id, "76561190000000010");
        assert_eq!(waiting.achievements, ["FIRST_DRAWING"]);
        tx.rollback().await.unwrap();
    }

    async fn has_supporter_achievement(tx: &mut Transaction<'_, Postgres>, id: Uuid) -> bool {
        crate::models::achievement::list_achievements(tx, id)
            .await
            .unwrap()
            .iter()
            .any(|e| e.achievement == "STEAM_SUPPORTER" && e.key == "steam-supporter")
    }

    #[tokio::test]
    async fn buying_on_steam_is_an_achievement_whenever_steam_says_so() {
        let Some(mut tx) = tx().await else { return };
        // Bought, then linked.
        let buyer = user(&mut tx, "identity_test_h", None, None).await;
        let bought = VerifiedIdentity {
            purchased: Some(true),
            ..steam("76561190000000011")
        };
        link_identity(&mut tx, buyer.id, &bought)
            .await
            .unwrap()
            .unwrap();
        assert!(has_supporter_achievement(&mut tx, buyer.id).await);

        // Linked from a borrowed copy, then signed in having bought it.
        let borrower = user(&mut tx, "identity_test_i", None, None).await;
        let borrowed = steam("76561190000000012");
        link_identity(&mut tx, borrower.id, &borrowed)
            .await
            .unwrap()
            .unwrap();
        assert!(!has_supporter_achievement(&mut tx, borrower.id).await);
        touch_identity(
            &mut tx,
            &VerifiedIdentity {
                purchased: Some(true),
                ..borrowed
            },
        )
        .await
        .unwrap();
        assert!(has_supporter_achievement(&mut tx, borrower.id).await);
        tx.rollback().await.unwrap();
    }

    #[tokio::test]
    async fn only_a_verified_address_finds_an_account() {
        let Some(mut tx) = tx().await else { return };
        let verified = user(&mut tx, "identity_test_e", None, Some("Oeee@Example.test")).await;
        query!(
            "INSERT INTO users (login_name, display_name, email) VALUES ('identity_test_f', 'f', 'unverified@example.test')"
        )
        .execute(&mut *tx)
        .await
        .unwrap();

        assert_eq!(
            find_user_by_verified_email(&mut tx, "oeee@example.TEST")
                .await
                .unwrap()
                .map(|u| u.id),
            Some(verified.id)
        );
        assert!(
            find_user_by_verified_email(&mut tx, "unverified@example.test")
                .await
                .unwrap()
                .is_none()
        );
        tx.rollback().await.unwrap();
    }

    /// A real argon2 hash, as `UserDraft::new` makes one.
    struct UserDraftHash;
    impl UserDraftHash {
        fn of(password: &str) -> String {
            crate::models::user::UserDraft::new("x".into(), password.into(), "x".into())
                .unwrap()
                .password_hash
                .unwrap()
        }
    }
}
