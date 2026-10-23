use anyhow::Result;
use argon2::password_hash::{phc::PasswordHash, PasswordHasher, PasswordVerifier};
use argon2::Argon2;
use axum_login::{AuthUser, AuthnBackend, UserId};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::types::Uuid;
use sqlx::{query, query_as, PgPool, Postgres, Transaction, Type};

use crate::models::actor::create_actor_for_user;
use crate::models::handle::LoginName;
use crate::AppConfig;

pub struct UserDraft {
    pub login_name: String,
    /// None for an account made by signing in with an identity provider,
    /// until its owner sets a password.
    pub password_hash: Option<String>,
    pub display_name: String,
}

impl UserDraft {
    pub fn new(login_name: String, password: String, display_name: String) -> Result<Self> {
        if password.len() < 8 {
            return Err(anyhow::anyhow!("비밀번호는 8자 이상이어야 합니다"));
        }

        let password_hash = hash_password(&password)?;

        Ok(Self {
            login_name,
            password_hash: Some(password_hash),
            display_name,
        })
    }

    /// An account signed into with an identity provider rather than a
    /// password.
    pub fn without_password(login_name: String, display_name: String) -> Self {
        Self {
            login_name,
            password_hash: None,
            display_name,
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, Type)]
#[sqlx(type_name = "preferred_language", rename_all = "lowercase")]
#[serde(rename_all = "lowercase")]
pub enum Language {
    Ko,
    Ja,
    En,
    Zh,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Type)]
#[sqlx(type_name = "user_role", rename_all = "lowercase")]
#[serde(rename_all = "lowercase")]
pub enum UserRole {
    User,
    Moderator,
    Admin,
}

#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct User {
    pub id: Uuid,
    pub login_name: LoginName,
    #[serde(skip_serializing)]
    pub password_hash: Option<String>,
    pub display_name: String,
    pub email: Option<String>,
    pub email_verified_at: Option<DateTime<Utc>>,
    pub updated_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
    pub banner_id: Option<Uuid>,
    pub preferred_language: Option<Language>,
    pub deleted_at: Option<DateTime<Utc>>,
    pub show_sensitive_content: bool,
    pub role: UserRole,
}

impl User {
    pub fn verify_password(&self, password: &str) -> Result<(), argon2::password_hash::Error> {
        // No password, or the '' a deleted account used to be given: nothing
        // to match, so nothing does.
        let hash = match self.password_hash.as_deref() {
            Some(hash) if !hash.is_empty() => hash,
            _ => return Err(argon2::password_hash::Error::PasswordInvalid),
        };
        check_password(hash, password)
    }

    /// Whether the account can be signed into with a password. One made by
    /// signing in with Steam, say, cannot until its owner sets one.
    pub fn has_password(&self) -> bool {
        self.password_hash
            .as_deref()
            .is_some_and(|hash| !hash.is_empty())
    }

    /// Site-wide staff. Gates everything under `/admin`.
    pub fn is_admin(&self) -> bool {
        matches!(self.role, UserRole::Admin)
    }
}

pub async fn update_user_preferred_language(
    tx: &mut Transaction<'_, Postgres>,
    id: Uuid,
    preferred_language: Option<Language>,
) -> Result<User> {
    let q = query_as!(
        User,
        r#"
            UPDATE users
            SET preferred_language = $1, updated_at = now()
            WHERE id = $2
            RETURNING
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
        "#,
        preferred_language as _,
        id,
    );
    let result = q.fetch_one(&mut **tx).await?;

    Ok(User {
        id: result.id,
        login_name: result.login_name,
        password_hash: result.password_hash,
        display_name: result.display_name,
        email: result.email,
        email_verified_at: result.email_verified_at,
        created_at: result.created_at,
        updated_at: result.updated_at,
        banner_id: result.banner_id,
        preferred_language: result.preferred_language,
        deleted_at: result.deleted_at,
        show_sensitive_content: result.show_sensitive_content,
        role: result.role,
    })
}

/// Grants or revokes site-wide staff access. Deliberately has no HTTP handler —
/// roles are changed from the CLI only, so a compromised admin session cannot
/// mint more admins.
pub async fn update_user_role(
    tx: &mut Transaction<'_, Postgres>,
    id: Uuid,
    role: UserRole,
) -> Result<User> {
    let q = query_as!(
        User,
        r#"
            UPDATE users
            SET role = $1, updated_at = now()
            WHERE id = $2
            RETURNING
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
        "#,
        role as UserRole,
        id,
    );
    Ok(q.fetch_one(&mut **tx).await?)
}

pub async fn update_user_show_sensitive_content(
    tx: &mut Transaction<'_, Postgres>,
    id: Uuid,
    show_sensitive_content: bool,
) -> Result<User> {
    let q = query_as!(
        User,
        r#"
            UPDATE users
            SET show_sensitive_content = $1, updated_at = now()
            WHERE id = $2
            RETURNING
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
        "#,
        show_sensitive_content,
        id,
    );
    let result = q.fetch_one(&mut **tx).await?;

    Ok(User {
        id: result.id,
        login_name: result.login_name,
        password_hash: result.password_hash,
        display_name: result.display_name,
        email: result.email,
        email_verified_at: result.email_verified_at,
        created_at: result.created_at,
        updated_at: result.updated_at,
        banner_id: result.banner_id,
        preferred_language: result.preferred_language,
        deleted_at: result.deleted_at,
        show_sensitive_content: result.show_sensitive_content,
        role: result.role,
    })
}

pub async fn update_user_email_verified_at(
    tx: &mut Transaction<'_, Postgres>,
    id: Uuid,
    email: String,
    email_verified_at: DateTime<Utc>,
) -> Result<User> {
    let q = query_as!(
        User,
        r#"
            UPDATE users
            SET email = $1, email_verified_at = $2, updated_at = now()
            WHERE id = $3
            RETURNING
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
        "#,
        email,
        email_verified_at,
        id,
    );
    let result = q.fetch_one(&mut **tx).await?;

    Ok(User {
        id: result.id,
        login_name: result.login_name,
        password_hash: result.password_hash,
        display_name: result.display_name,
        email: result.email,
        email_verified_at: result.email_verified_at,
        created_at: result.created_at,
        updated_at: result.updated_at,
        banner_id: result.banner_id,
        preferred_language: result.preferred_language,
        deleted_at: result.deleted_at,
        show_sensitive_content: result.show_sensitive_content,
        role: result.role,
    })
}

/// A password as it is stored: Argon2id with the crate's defaults and a
/// fresh random salt, in the PHC string format.
fn hash_password(password: &str) -> Result<String> {
    let hash: PasswordHash = Argon2::default().hash_password(password.as_bytes())?;
    Ok(hash.to_string())
}

/// Whether `password` is the one `hash` was made from. The hash says which
/// parameters it was made with, so one made with older defaults still
/// checks out.
fn check_password(hash: &str, password: &str) -> Result<(), argon2::password_hash::Error> {
    Argon2::default().verify_password(password.as_bytes(), hash)
}

pub async fn update_password(
    tx: &mut Transaction<'_, Postgres>,
    id: Uuid,
    new_password: String,
) -> Result<User> {
    let password_hash = hash_password(&new_password)?;

    let q = query_as!(
        User,
        r#"
            UPDATE users
            SET password_hash = $1, updated_at = now()
            WHERE id = $2
            RETURNING
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
        "#,
        password_hash,
        id,
    );
    let result = q.fetch_one(&mut **tx).await?;

    Ok(User {
        id: result.id,
        login_name: result.login_name,
        password_hash: result.password_hash,
        display_name: result.display_name,
        email: result.email,
        email_verified_at: result.email_verified_at,
        created_at: result.created_at,
        updated_at: result.updated_at,
        banner_id: result.banner_id,
        preferred_language: result.preferred_language,
        deleted_at: result.deleted_at,
        show_sensitive_content: result.show_sensitive_content,
        role: result.role,
    })
}

pub async fn update_user(
    tx: &mut Transaction<'_, Postgres>,
    id: Uuid,
    login_name: String,
    display_name: String,
) -> Result<User> {
    let q = query_as!(
        User,
        r#"
            UPDATE users
            SET login_name = $1, display_name = $2, updated_at = now()
            WHERE id = $3
            RETURNING
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
        "#,
        login_name,
        display_name,
        id,
    );
    let result = q.fetch_one(&mut **tx).await?;

    Ok(User {
        id: result.id,
        login_name: result.login_name,
        password_hash: result.password_hash,
        display_name: result.display_name,
        email: result.email,
        email_verified_at: result.email_verified_at,
        created_at: result.created_at,
        updated_at: result.updated_at,
        banner_id: result.banner_id,
        preferred_language: result.preferred_language,
        deleted_at: result.deleted_at,
        show_sensitive_content: result.show_sensitive_content,
        role: result.role,
    })
}

pub async fn update_user_with_activity(
    tx: &mut Transaction<'_, Postgres>,
    id: Uuid,
    login_name: String,
    display_name: String,
    config: &AppConfig,
    state: Option<&crate::web::state::AppState>,
) -> Result<User> {
    // First update the user
    let updated_user = update_user(tx, id, login_name.clone(), display_name.clone()).await?;

    // Update the corresponding actor
    let _ = super::actor::update_actor_for_user(tx, id, login_name, display_name, config).await;

    // If state is provided, send ActivityPub Update activity
    if let Some(state) = state {
        // Get the updated actor
        if let Some(updated_actor) = super::actor::Actor::find_by_user_id(tx, id).await? {
            // Send Update activity - don't fail if this fails
            if let Err(e) =
                crate::web::handlers::activitypub::send_update_activity(&updated_actor, state).await
            {
                tracing::warn!("Failed to send Update activity for user {}: {:?}", id, e);
            }
        }
    }

    Ok(updated_user)
}

/// The longest a profile description may be, in characters. The column's
/// own check holds it to the same.
pub const BIO_MAX_CHARS: usize = 1000;

/// What a person says about themselves on their profile, as they typed it.
pub async fn find_user_bio(tx: &mut Transaction<'_, Postgres>, id: Uuid) -> Result<String> {
    let result = query!("SELECT bio FROM users WHERE id = $1", id)
        .fetch_one(&mut **tx)
        .await?;
    Ok(result.bio)
}

/// A profile description as the HTML its actor's `summary` carries: the
/// text escaped, a blank line between paragraphs, and a line break where
/// they pressed return.
pub fn bio_html(bio: &str) -> String {
    let bio = bio.replace("\r\n", "\n");
    bio.split("\n\n")
        .map(str::trim)
        .filter(|paragraph| !paragraph.is_empty())
        .map(|paragraph| {
            let lines: Vec<String> = paragraph
                .lines()
                .map(crate::markdown_utils::escape_html)
                .collect();
            format!("<p>{}</p>", lines.join("<br>"))
        })
        .collect()
}

/// Sets the name a person goes by and what they say about themselves, on
/// their account and on their actor, and tells their followers' servers.
/// The login name stays as it is; that is changed from the account page.
pub async fn update_profile_with_activity(
    tx: &mut Transaction<'_, Postgres>,
    id: Uuid,
    display_name: &str,
    bio: &str,
    state: &crate::web::state::AppState,
) -> Result<()> {
    query!(
        "UPDATE users SET display_name = $1, bio = $2, updated_at = now() WHERE id = $3",
        display_name,
        bio,
        id,
    )
    .execute(&mut **tx)
    .await?;
    query!(
        "UPDATE actors SET name = $1, bio_html = $2, updated_at = now() WHERE user_id = $3",
        display_name,
        bio_html(bio),
        id,
    )
    .execute(&mut **tx)
    .await?;

    if let Some(actor) = super::actor::Actor::find_by_user_id(tx, id).await? {
        if let Err(e) = crate::web::handlers::activitypub::send_update_activity(&actor, state).await
        {
            tracing::warn!("Failed to send Update activity for user {}: {:?}", id, e);
        }
    }

    Ok(())
}

/// Check if a login_name conflicts with any existing community slug
pub async fn login_name_conflicts_with_community(
    tx: &mut Transaction<'_, Postgres>,
    login_name: &str,
) -> Result<bool> {
    let result = query!(
        "SELECT EXISTS(SELECT 1 FROM communities WHERE slug = $1 AND deleted_at IS NULL) as \"exists!\"",
        login_name
    )
    .fetch_one(&mut **tx)
    .await?;
    Ok(result.exists)
}

pub async fn create_user(
    tx: &mut Transaction<'_, Postgres>,
    user_draft: UserDraft,
    config: &AppConfig,
    uris: &ojak::federation::Uris,
) -> Result<User> {
    let q = query!(
        "
            INSERT INTO users (
                login_name,
                password_hash,
                display_name
            )
            VALUES ($1, $2, $3)
            RETURNING id, created_at, updated_at
        ",
        user_draft.login_name,
        user_draft.password_hash.as_deref(),
        user_draft.display_name,
    );
    let result = q.fetch_one(&mut **tx).await?;

    let user = User {
        id: result.id,
        login_name: user_draft.login_name.into(),
        password_hash: user_draft.password_hash,
        display_name: user_draft.display_name,
        email: None,
        email_verified_at: None,
        created_at: result.created_at,
        updated_at: result.updated_at,
        banner_id: None,
        preferred_language: None,
        deleted_at: None,
        show_sensitive_content: false,
        role: UserRole::User,
    };

    // Create actor for the user
    create_actor_for_user(tx, &user, config, uris).await?;

    Ok(user)
}

pub async fn find_user_by_id(tx: &mut Transaction<'_, Postgres>, id: Uuid) -> Result<Option<User>> {
    let q = query_as!(
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
        WHERE id = $1"#,
        id
    );
    Ok(q.fetch_optional(&mut **tx).await?)
}

pub async fn find_user_by_login_name(
    tx: &mut Transaction<'_, Postgres>,
    login_name: &str,
) -> Result<Option<User>> {
    let q = query_as!(
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
        WHERE login_name = $1"#,
        login_name
    );
    Ok(q.fetch_optional(&mut **tx).await?)
}

pub async fn find_user_by_email(
    tx: &mut Transaction<'_, Postgres>,
    email: &str,
) -> Result<Option<User>> {
    let q = query_as!(
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
        WHERE email = $1"#,
        email
    );
    Ok(q.fetch_optional(&mut **tx).await?)
}

#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct UserWithPublicPostAndBanner {
    pub login_name: LoginName,
    pub display_name: String,
    pub banner_image_filename: String,
}

pub async fn find_users_with_public_posts_and_banner(
    tx: &mut Transaction<'_, Postgres>,
) -> Result<Vec<UserWithPublicPostAndBanner>> {
    let q = query_as!(
        UserWithPublicPostAndBanner,
        r#"
        SELECT
            u.login_name,
            u.display_name,
            i.image_filename AS banner_image_filename
        FROM users u
        JOIN posts p ON u.id = p.author_id
        JOIN communities c ON p.community_id = c.id
        JOIN banners b ON u.banner_id = b.id
        JOIN images i ON b.image_id = i.id
        WHERE p.published_at IS NOT NULL
        AND c.visibility = 'public'
        -- Staff-flagged banners are withheld from the public /about page.
        AND b.is_explicit = false
        AND b.deleted_at IS NULL
        GROUP BY u.id, banner_image_filename
        ORDER BY count(p.id) DESC
        "#,
    );
    Ok(q.fetch_all(&mut **tx).await?)
}

/// What a person gives to show they mean to delete their account.
pub enum DeleteConfirmation<'a> {
    /// Their password, for an account that has one.
    Password(&'a str),
    /// Their handle, typed out, for an account signed into only with an
    /// identity provider: there is no password to ask for, and a provider's
    /// fresh sign-in is not something every client can produce.
    LoginName(&'a str),
}

impl<'a> DeleteConfirmation<'a> {
    /// Takes whichever the account calls for from what the client sent.
    pub fn for_user(user: &User, password: Option<&'a str>, login_name: Option<&'a str>) -> Self {
        if user.has_password() {
            Self::Password(password.unwrap_or_default())
        } else {
            Self::LoginName(login_name.unwrap_or_default())
        }
    }
}

pub async fn delete_user(
    tx: &mut Transaction<'_, Postgres>,
    id: Uuid,
    confirmation: DeleteConfirmation<'_>,
    config: &AppConfig,
) -> Result<()> {
    // First, find the user
    let user = find_user_by_id(tx, id).await?;
    let user = user.ok_or_else(|| anyhow::anyhow!("User not found"))?;

    // Check if user is already deleted
    if user.deleted_at.is_some() {
        return Err(anyhow::anyhow!("User is already deleted"));
    }

    match confirmation {
        DeleteConfirmation::Password(password) => {
            user.verify_password(password)
                .map_err(|_| anyhow::anyhow!("Invalid password"))?;
        }
        DeleteConfirmation::LoginName(login_name) => {
            // Checked against the account, not just for being non-empty: an
            // account with a password never gets here, but one that answers
            // with its handle has to answer with its own.
            if user.has_password() || user.login_name != login_name.trim() {
                return Err(anyhow::anyhow!("The username does not match"));
            }
        }
    }

    // Check if user owns any communities
    let community_count = query!(
        r#"
        SELECT COUNT(*) as "count!"
        FROM communities
        WHERE owner_id = $1
        "#,
        id
    )
    .fetch_one(&mut **tx)
    .await?;

    if community_count.count > 0 {
        return Err(anyhow::anyhow!(
            "Cannot delete account while owning communities. Please transfer or delete your communities first."
        ));
    }

    // Note: Sessions are not deleted directly here because tower-sessions stores
    // session data in binary format without a direct user_id reference.
    // The user won't be able to log in again anyway because authentication
    // checks filter out deleted users.

    // Unlink every identity provider, so the Steam account (or any other)
    // that signed into this one can make a new account rather than find
    // itself linked to a deleted one.
    query!("DELETE FROM user_identities WHERE user_id = $1", id)
        .execute(&mut **tx)
        .await?;

    // Delete devices (will cascade automatically)
    query!(
        r#"
        DELETE FROM devices
        WHERE user_id = $1
        "#,
        id
    )
    .execute(&mut **tx)
    .await?;

    // Delete notifications (will cascade automatically)
    query!(
        r#"
        DELETE FROM notifications
        WHERE recipient_id = $1
        "#,
        id
    )
    .execute(&mut **tx)
    .await?;

    // Delete follow relationships
    if let Some(actor) = super::actor::Actor::find_by_user_id(tx, id).await? {
        query!(
            r#"
            DELETE FROM follows
            WHERE follower_actor_id = $1 OR following_actor_id = $1
            "#,
            actor.id
        )
        .execute(&mut **tx)
        .await?;
    }

    // Soft delete and anonymize user. The login name is given up, so that
    // somebody (the same person included) can sign up with it again. What
    // replaces it is random rather than derived from the id: the id is public
    // in the actor's IRI, and a name anyone could predict is one anyone could
    // register first and so make this account impossible to delete.
    let released_login_name = format!("deleted_{}", Uuid::new_v4().simple());
    query!(
        r#"
        UPDATE users
        SET
            deleted_at = NOW(),
            login_name = $2,
            email = NULL,
            display_name = '[deleted]',
            password_hash = NULL,
            updated_at = NOW()
        WHERE id = $1
        "#,
        id,
        released_login_name
    )
    .execute(&mut **tx)
    .await?;

    // The actor's handle is unique too, and WebFinger answers by it.
    super::actor::update_actor_for_user(
        tx,
        id,
        released_login_name,
        "[deleted]".to_string(),
        config,
    )
    .await?;

    Ok(())
}

pub async fn delete_user_with_activity(
    tx: &mut Transaction<'_, Postgres>,
    id: Uuid,
    confirmation: DeleteConfirmation<'_>,
    config: &AppConfig,
    state: Option<&crate::web::state::AppState>,
) -> Result<()> {
    // Get the user's actor before deletion
    let actor = super::actor::Actor::find_by_user_id(tx, id).await?;

    // Delete the user
    delete_user(tx, id, confirmation, config).await?;

    // If state is provided and actor exists, send ActivityPub Delete activity
    if let (Some(state), Some(actor)) = (state, actor) {
        // Use the actor's IRI as the object URL
        if let Ok(actor_url) = actor.iri.parse() {
            // Send Delete activity - don't fail if this fails
            if let Err(e) =
                crate::web::handlers::activitypub::send_delete_activity(&actor, actor_url, state)
                    .await
            {
                tracing::warn!("Failed to send Delete activity for user {}: {:?}", id, e);
            }
        }
    }

    Ok(())
}

impl AuthUser for User {
    type Id = Uuid;

    fn id(&self) -> Self::Id {
        self.id
    }

    fn session_auth_hash(&self) -> &[u8] {
        self.id.as_bytes()
    }
}

#[derive(Clone, Deserialize)]
pub struct Credentials {
    pub login_name: String,
    pub password: String,
    pub next: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Backend {
    pub db: PgPool,
}

impl AuthnBackend for Backend {
    type User = User;
    type Credentials = Credentials;
    type Error = sqlx::Error;

    async fn authenticate(
        &self,
        creds: Self::Credentials,
    ) -> Result<Option<Self::User>, Self::Error> {
        let q = query_as!(
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
            WHERE login_name = $1"#,
            creds.login_name
        );
        let user = q.fetch_optional(&self.db).await?;

        Ok(user.filter(|user| {
            user.deleted_at.is_none() && user.verify_password(&creds.password).is_ok()
        }))
    }

    async fn get_user(&self, user_id: &UserId<Self>) -> Result<Option<Self::User>, Self::Error> {
        let q = query_as!(
            User,
            r#"SELECT
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
            WHERE id = $1"#,
            user_id
        );
        let user = q.fetch_optional(&self.db).await?;
        Ok(user.filter(|user| user.deleted_at.is_none()))
    }
}

pub type AuthSession = axum_login::AuthSession<Backend>;

#[cfg(test)]
mod bio_tests {
    use super::bio_html;

    #[test]
    fn a_bio_is_paragraphs_and_line_breaks_of_escaped_text() {
        assert_eq!(
            bio_html("I draw <cats>\r\nmostly\n\n\n& dogs\n"),
            "<p>I draw &lt;cats&gt;<br>mostly</p><p>&amp; dogs</p>"
        );
        assert_eq!(bio_html("  \n\n "), "");
    }
}

#[cfg(test)]
mod password_tests {
    use super::{check_password, hash_password};

    /// Made by argon2 0.5, which hashed every password stored before 0.6:
    /// upgrading the crate must not sign everybody out of their account.
    const FROM_ARGON2_0_5: &str = "$argon2id$v=19$m=19456,t=2,p=1$becV094+RpC0sFmPNIC21A$IHRgn3M8/x116RHArS4+H/FR+3alTAgaTzqRVIHMiTE";

    #[test]
    fn a_password_stored_before_the_upgrade_still_signs_in() {
        assert!(check_password(FROM_ARGON2_0_5, "an old password").is_ok());
        assert!(check_password(FROM_ARGON2_0_5, "another password").is_err());
    }

    #[test]
    fn a_new_hash_checks_out_and_is_salted() {
        let first = hash_password("a new password").expect("hash");
        assert!(first.starts_with("$argon2id$v=19$m=19456,t=2,p=1$"));
        assert!(check_password(&first, "a new password").is_ok());
        assert!(check_password(&first, "a wrong password").is_err());
        assert_ne!(first, hash_password("a new password").expect("hash"));
    }
}
