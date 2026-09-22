//! Supporters: accounts that have bought a Supporter Pack -- on Steam as the
//! DLC for that year in `steam.supporter_apps`, or in the iOS app as the
//! year's non-consumable in `app_store.supporter_products`.
//!
//! **A pack is a year's.** Buying 2026's supports the site through 2026, and
//! supporting again means buying 2027's when it comes. So the mark beside
//! someone's name is this year's pack and nothing else, while a profile
//! names every year they have supported -- what they gave, kept, rather than
//! a mark that quietly means something different in December than it did in
//! January.
//!
//! **A pack belongs to the account, not to an identity.** Buying it does not
//! mean signing in with the platform it was bought on: someone signed in
//! with a password inside the Steam app buys the DLC and the Steam app hands
//! over a ticket, and someone signed in with a password inside the iOS app
//! buys the pack and that app hands over the transaction. Neither has to
//! link anything for the mark to appear.
//!
//! **A purchase is one row, whoever holds it.** `supporter_purchases` is
//! keyed by the purchase as the platform knows it -- the provider, the
//! account or transaction that owns it, and the product -- so restoring it
//! in the iOS app, or signing into a different Oeee Cafe account inside the
//! Steam app, moves that row rather than making another. Restoring can give
//! a pack back; it cannot make two.
//!
//! Standing is whatever the platform said last. Steam is asked when someone
//! signs in or the app hands over a ticket, and once a day besides for every
//! Steam account the site knows (`steam::recheck_supporters`); the App Store
//! is asked when the app hands over a transaction (`app_store::look_up`) and
//! once a day for every purchase it has told us about
//! (`app_store::recheck_supporters`). Either way a purchase or a refund
//! shows within a day whether or not anyone signs in.

use std::collections::HashMap;

use anyhow::Result;
use chrono::{DateTime, Datelike, Utc};
use serde::{Deserialize, Serialize};
use sqlx::types::Uuid;
use sqlx::{query, query_scalar, Postgres, Transaction};

use super::identity::Provider;

/// One Supporter Pack, as the platform names it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OwnedProduct {
    /// A Steam app id written out, or an App Store product id.
    pub product: String,
    /// The year it supports, from the configuration that names it.
    pub year: i32,
}

/// The year a pack has to be for to put a mark beside someone's name now.
///
/// Seoul's, which is the clock the site shows its dates on: a year ends for
/// everyone when it ends where Oeee Cafe is.
pub fn current_year() -> i32 {
    Utc::now().with_timezone(&chrono_tz::Asia::Seoul).year()
}

/// Records everything `owner` owns on `provider` now, and nothing else, for
/// `user_id`: a pack that was owned and is not in `owned` has been refunded,
/// and is revoked.
///
/// For a platform that answers about every pack at once, which Steam does --
/// the site knows the app ids and asks about each. The App Store answers
/// about one transaction at a time and goes through [`record_purchase`].
pub async fn record_owned_products(
    tx: &mut Transaction<'_, Postgres>,
    user_id: Uuid,
    provider: Provider,
    owner: &str,
    owned: &[OwnedProduct],
) -> Result<()> {
    for pack in owned {
        record_purchase(tx, user_id, provider, owner, pack, true).await?;
    }
    let kept: Vec<String> = owned.iter().map(|pack| pack.product.clone()).collect();
    query!(
        r#"
        UPDATE supporter_purchases
        SET revoked_at = COALESCE(revoked_at, now()), checked_at = now()
        WHERE provider = $1 AND owner = $2 AND product <> ALL($3)
        "#,
        provider.as_str(),
        owner,
        &kept,
    )
    .execute(&mut **tx)
    .await?;
    touch_identity_check(tx, provider, owner).await
}

/// One pack, owned or not, held by `user_id`.
///
/// Whoever hands the purchase over holds it: the same transaction restored
/// on another account moves the row there, because the row is keyed by the
/// purchase and not by the account. That is what keeps a restore from
/// becoming a second supporter.
pub async fn record_purchase(
    tx: &mut Transaction<'_, Postgres>,
    user_id: Uuid,
    provider: Provider,
    owner: &str,
    pack: &OwnedProduct,
    owned: bool,
) -> Result<()> {
    query!(
        r#"
        INSERT INTO supporter_purchases (user_id, provider, owner, product, year, revoked_at)
        VALUES ($1, $2, $3, $4, $5, CASE WHEN $6 THEN NULL ELSE now() END)
        ON CONFLICT (provider, owner, product) DO UPDATE
        SET user_id = EXCLUDED.user_id,
            year = EXCLUDED.year,
            revoked_at = CASE
                WHEN $6 THEN NULL
                ELSE COALESCE(supporter_purchases.revoked_at, now())
            END,
            -- Owning it again after a refund starts that pack over, which is
            -- what the site has always said a repurchase does.
            purchased_at = CASE
                WHEN $6 AND supporter_purchases.revoked_at IS NOT NULL THEN now()
                ELSE supporter_purchases.purchased_at
            END,
            checked_at = now()
        "#,
        user_id,
        provider.as_str(),
        owner,
        pack.product,
        pack.year,
        owned,
    )
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// What a recheck records: whether the purchase still stands, and nothing
/// about who holds it. Asking Apple again is not a claim on anyone's behalf.
pub async fn record_recheck(
    tx: &mut Transaction<'_, Postgres>,
    provider: Provider,
    owner: &str,
    product: &str,
    owned: bool,
) -> Result<()> {
    query!(
        r#"
        UPDATE supporter_purchases
        SET revoked_at = CASE
                WHEN $4 THEN NULL
                ELSE COALESCE(revoked_at, now())
            END,
            purchased_at = CASE
                WHEN $4 AND revoked_at IS NOT NULL THEN now()
                ELSE purchased_at
            END,
            checked_at = now()
        WHERE provider = $1 AND owner = $2 AND product = $3
        "#,
        provider.as_str(),
        owner,
        product,
        owned,
    )
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// The clock the Steam recheck works through, kept on the identity where
/// there is one so a linked account that owns nothing is not asked about
/// every ten minutes for ever.
async fn touch_identity_check(
    tx: &mut Transaction<'_, Postgres>,
    provider: Provider,
    subject: &str,
) -> Result<()> {
    query!(
        "UPDATE user_identities SET supporter_checked_at = now()
         WHERE provider = $1 AND subject = $2",
        provider.as_str(),
        subject,
    )
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// A Steam account to ask Steam about again, and the account holding what it
/// owns.
pub struct DueSteamAccount {
    pub steam_id: String,
    pub user_id: Uuid,
}

/// Steam accounts Steam has not been asked about for a day, never asked
/// first, then longest ago: the ones linked to an account, which may have
/// bought a pack since, and the ones that own packs here without being
/// linked at all.
pub async fn steam_accounts_due_for_check(
    tx: &mut Transaction<'_, Postgres>,
    limit: i64,
) -> Result<Vec<DueSteamAccount>> {
    let rows = query!(
        r#"
        SELECT steam_id AS "steam_id!", user_id AS "user_id!"
        FROM (
            SELECT subject AS steam_id, user_id, supporter_checked_at AS checked_at
            FROM user_identities
            WHERE provider = 'steam'
            UNION ALL
            SELECT owner, user_id, checked_at
            FROM supporter_purchases
            WHERE provider = 'steam'
        ) due
        GROUP BY steam_id, user_id
        HAVING min(checked_at) IS NULL OR min(checked_at) < now() - interval '1 day'
        ORDER BY min(checked_at) NULLS FIRST
        LIMIT $1
        "#,
        limit,
    )
    .fetch_all(&mut **tx)
    .await?;
    Ok(rows
        .into_iter()
        .map(|row| DueSteamAccount {
            steam_id: row.steam_id,
            user_id: row.user_id,
        })
        .collect())
}

/// An App Store purchase to ask Apple about again: the transaction, and
/// which pack it was.
pub struct DuePurchase {
    pub transaction: String,
    pub product: String,
}

/// App Store purchases Apple has not been asked about for a day, longest ago
/// first.
pub async fn apple_purchases_due_for_check(
    tx: &mut Transaction<'_, Postgres>,
    limit: i64,
) -> Result<Vec<DuePurchase>> {
    let rows = query!(
        r#"
        SELECT owner, product
        FROM supporter_purchases
        WHERE provider = 'apple' AND checked_at < now() - interval '1 day'
        ORDER BY checked_at
        LIMIT $1
        "#,
        limit,
    )
    .fetch_all(&mut **tx)
    .await?;
    Ok(rows
        .into_iter()
        .map(|row| DuePurchase {
            transaction: row.owner,
            product: row.product,
        })
        .collect())
}

/// A year someone supported, and where they bought that year's pack. A
/// profile lists every one of them, whichever mark they wear now.
#[derive(Clone, Debug, Serialize)]
pub struct Standing {
    pub provider: String,
    pub year: i32,
    pub since: DateTime<Utc>,
}

/// Every pack `user_id` still owns, earliest year first. Empty for anyone
/// who has never supported -- and a year that has passed is still in here,
/// which is the point: the mark goes, the year stays.
pub async fn standings(tx: &mut Transaction<'_, Postgres>, user_id: Uuid) -> Result<Vec<Standing>> {
    let rows = query!(
        r#"
        SELECT provider, year, purchased_at
        FROM supporter_purchases
        WHERE user_id = $1 AND revoked_at IS NULL
        ORDER BY year, provider
        "#,
        user_id,
    )
    .fetch_all(&mut **tx)
    .await?;
    Ok(rows
        .into_iter()
        .map(|row| Standing {
            provider: row.provider,
            year: row.year,
            since: row.purchased_at,
        })
        .collect())
}

/// The mark `user_id` wears now: the platform they bought this year's pack
/// on, or `None` for anyone who has not bought it.
///
/// The choice they made on their account page is honoured only while that
/// platform is one they bought this year's pack on: the ordering below
/// prefers the platform they named and falls back to whichever they bought
/// first, so a refund moves the mark rather than leaving them wearing a
/// store they no longer own.
pub async fn mark_for(tx: &mut Transaction<'_, Postgres>, user_id: Uuid) -> Result<Option<String>> {
    let mark = query_scalar!(
        r#"
        SELECT purchases.provider
        FROM users
        JOIN supporter_purchases purchases ON purchases.user_id = users.id
        WHERE users.id = $1
          AND users.deleted_at IS NULL
          AND purchases.revoked_at IS NULL
          AND purchases.year = $2
        ORDER BY COALESCE(purchases.provider = users.supporter_mark, false) DESC,
                 purchases.purchased_at
        LIMIT 1
        "#,
        user_id,
        current_year(),
    )
    .fetch_optional(&mut **tx)
    .await?;
    Ok(mark)
}

/// Which platform's mark to wear. `None` goes back to the default: the
/// platform they bought this year's pack on first. Taking a [`Provider`]
/// rather than a name is what keeps an unknown one from reaching
/// `users_supporter_mark_check`.
pub async fn set_mark(
    tx: &mut Transaction<'_, Postgres>,
    user_id: Uuid,
    mark: Option<Provider>,
) -> Result<()> {
    query!(
        "UPDATE users SET supporter_mark = $2 WHERE id = $1",
        user_id,
        mark.map(|provider| provider.as_str()),
    )
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// The supporters a post's page names -- whoever drew it, whoever drew it
/// with them, and whoever commented -- by login name, each with the mark
/// they wear. The page asks this for every name it prints; one it does not
/// find gets no mark.
pub async fn supporter_marks_on_post(
    tx: &mut Transaction<'_, Postgres>,
    post_id: Uuid,
) -> Result<HashMap<String, String>> {
    let rows = query!(
        r#"
        SELECT DISTINCT ON (users.id) users.login_name, purchases.provider
        FROM users
        JOIN supporter_purchases purchases ON purchases.user_id = users.id
        WHERE purchases.revoked_at IS NULL
          AND purchases.year = $2
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
        ORDER BY users.id,
                 COALESCE(purchases.provider = users.supporter_mark, false) DESC,
                 purchases.purchased_at
        "#,
        post_id,
        current_year(),
    )
    .fetch_all(&mut **tx)
    .await?;
    Ok(rows
        .into_iter()
        .map(|row| (row.login_name, row.provider))
        .collect())
}

/// A line in the credits on /about: the mark they wear, and since when they
/// have supported at all -- their first year, which is not always the year
/// or the platform whose mark they wear now.
#[derive(Clone, Debug, Serialize)]
pub struct Credit {
    pub login_name: String,
    pub display_name: String,
    pub mark: String,
    pub since: DateTime<Utc>,
}

/// Everyone supporting this year who has not asked to be left off, whoever
/// has been at it longest first. The thanks are in the present tense: it is
/// the people keeping the site running now.
pub async fn list_credits(tx: &mut Transaction<'_, Postgres>) -> Result<Vec<Credit>> {
    // One row per supporter comes out of the inner query, which resolves the
    // mark the way mark_for does; DISTINCT ON has to order by the user, so
    // the credits' own order is the outer one.
    let rows = query!(
        r#"
        SELECT
            login_name AS "login_name!",
            display_name AS "display_name!",
            mark AS "mark!",
            since AS "since!"
        FROM (
            SELECT DISTINCT ON (users.id)
                users.login_name,
                users.display_name,
                purchases.provider AS mark,
                (
                    SELECT min(first.purchased_at)
                    FROM supporter_purchases first
                    WHERE first.user_id = users.id AND first.revoked_at IS NULL
                ) AS since
            FROM users
            JOIN supporter_purchases purchases ON purchases.user_id = users.id
            WHERE purchases.revoked_at IS NULL
              AND purchases.year = $1
              AND users.deleted_at IS NULL
              AND users.show_in_credits
            ORDER BY users.id,
                     COALESCE(purchases.provider = users.supporter_mark, false) DESC,
                     purchases.purchased_at
        ) resolved
        ORDER BY since, login_name
        "#,
        current_year(),
    )
    .fetch_all(&mut **tx)
    .await?;
    Ok(rows
        .into_iter()
        .map(|row| Credit {
            login_name: row.login_name,
            display_name: row.display_name,
            mark: row.mark,
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

    /// A year's pack, as the platform selling it would name it.
    fn pack(year: i32) -> OwnedProduct {
        OwnedProduct {
            product: format!("supporter-{year}"),
            year,
        }
    }

    fn this_year() -> i32 {
        current_year()
    }

    fn credited(credits: &[Credit], login_name: &str) -> bool {
        credits.iter().any(|c| c.login_name == login_name)
    }

    fn credited_mark(credits: &[Credit], login_name: &str) -> Option<String> {
        credits
            .iter()
            .find(|c| c.login_name == login_name)
            .map(|c| c.mark.clone())
    }

    async fn marks(tx: &mut Transaction<'_, Postgres>, user_id: Uuid) -> Option<String> {
        mark_for(tx, user_id).await.unwrap()
    }

    /// Nothing is linked anywhere in this test: buying a pack inside an app
    /// is not signing in with the store that sold it.
    #[tokio::test]
    async fn this_years_pack_makes_a_supporter_and_a_refund_unmakes_one() {
        let Some(mut tx) = tx().await else { return };
        let id = user(&mut tx, "supporter_test_a").await;
        let steam_id = "76561190000000101";
        assert_eq!(marks(&mut tx, id).await, None);

        record_owned_products(&mut tx, id, Provider::Steam, steam_id, &[pack(this_year())])
            .await
            .unwrap();
        assert_eq!(marks(&mut tx, id).await.as_deref(), Some("steam"));

        let credits = list_credits(&mut tx).await.unwrap();
        assert!(credited(&credits, "supporter_test_a"));
        assert_eq!(
            credited_mark(&credits, "supporter_test_a").as_deref(),
            Some("steam"),
            "the credits wear the mark too"
        );

        // Refunded: Steam says it owns nothing now.
        record_owned_products(&mut tx, id, Provider::Steam, steam_id, &[])
            .await
            .unwrap();
        assert_eq!(marks(&mut tx, id).await, None);
        assert!(!credited(
            &list_credits(&mut tx).await.unwrap(),
            "supporter_test_a"
        ));
        tx.rollback().await.unwrap();
    }

    /// A pack is a year's: last year's is still theirs, and says so on their
    /// profile, but it is not what this year's mark is for.
    #[tokio::test]
    async fn last_years_pack_keeps_its_year_and_loses_the_mark() {
        let Some(mut tx) = tx().await else { return };
        let id = user(&mut tx, "supporter_test_b").await;
        record_owned_products(
            &mut tx,
            id,
            Provider::Steam,
            "76561190000000102",
            &[pack(this_year() - 1)],
        )
        .await
        .unwrap();

        assert_eq!(marks(&mut tx, id).await, None, "last year is not this year");
        assert!(!credited(
            &list_credits(&mut tx).await.unwrap(),
            "supporter_test_b"
        ));
        let listed = standings(&mut tx, id).await.unwrap();
        assert_eq!(
            listed
                .iter()
                .map(|standing| standing.year)
                .collect::<Vec<_>>(),
            [this_year() - 1],
            "the year stays on the profile"
        );

        // This year's as well: both years stand, and the mark is back.
        record_owned_products(
            &mut tx,
            id,
            Provider::Steam,
            "76561190000000102",
            &[pack(this_year() - 1), pack(this_year())],
        )
        .await
        .unwrap();
        assert_eq!(marks(&mut tx, id).await.as_deref(), Some("steam"));
        let listed = standings(&mut tx, id).await.unwrap();
        assert_eq!(
            listed
                .iter()
                .map(|standing| standing.year)
                .collect::<Vec<_>>(),
            [this_year() - 1, this_year()],
            "earliest year first"
        );
        tx.rollback().await.unwrap();
    }

    /// Restoring hands the same purchase over again. It can give a pack back
    /// and it can hand it to whoever restored it, but there is only ever one
    /// of it.
    #[tokio::test]
    async fn restoring_a_purchase_moves_it_rather_than_making_another() {
        let Some(mut tx) = tx().await else { return };
        let first = user(&mut tx, "supporter_test_c").await;
        let second = user(&mut tx, "supporter_test_d").await;
        let transaction = "2000000000000001";
        let bought = pack(this_year());

        record_purchase(&mut tx, first, Provider::Apple, transaction, &bought, true)
            .await
            .unwrap();
        assert_eq!(marks(&mut tx, first).await.as_deref(), Some("apple"));

        // Restored on the same account: nothing changes, and nothing is
        // added.
        record_purchase(&mut tx, first, Provider::Apple, transaction, &bought, true)
            .await
            .unwrap();
        assert_eq!(standings(&mut tx, first).await.unwrap().len(), 1);

        // Restored on another account: the pack moves, and the first
        // account is left with none. Two supporters out of one purchase is
        // what the key on the table exists to prevent.
        record_purchase(&mut tx, second, Provider::Apple, transaction, &bought, true)
            .await
            .unwrap();
        assert_eq!(marks(&mut tx, second).await.as_deref(), Some("apple"));
        assert_eq!(marks(&mut tx, first).await, None);
        assert_eq!(standings(&mut tx, first).await.unwrap().len(), 0);
        tx.rollback().await.unwrap();
    }

    /// A recheck says whether the purchase still stands and nothing about
    /// whose it is: Apple being asked again is not a claim by anyone.
    #[tokio::test]
    async fn a_recheck_answers_for_the_purchase_and_not_for_an_account() {
        let Some(mut tx) = tx().await else { return };
        let id = user(&mut tx, "supporter_test_e").await;
        let transaction = "2000000000000002";
        let bought = pack(this_year());
        record_purchase(&mut tx, id, Provider::Apple, transaction, &bought, true)
            .await
            .unwrap();

        let due = apple_purchases_due_for_check(&mut tx, 100).await.unwrap();
        assert!(
            !due.iter().any(|due| due.transaction == transaction),
            "asked about today already"
        );
        query!(
            "UPDATE supporter_purchases SET checked_at = now() - interval '2 days'
             WHERE provider = 'apple' AND owner = $1",
            transaction,
        )
        .execute(&mut *tx)
        .await
        .unwrap();
        let due = apple_purchases_due_for_check(&mut tx, 100).await.unwrap();
        let mine = due
            .iter()
            .find(|due| due.transaction == transaction)
            .expect("due for a recheck");
        assert_eq!(mine.product, bought.product);

        // Refunded.
        record_recheck(
            &mut tx,
            Provider::Apple,
            transaction,
            &bought.product,
            false,
        )
        .await
        .unwrap();
        assert_eq!(marks(&mut tx, id).await, None);

        // And bought again.
        record_recheck(&mut tx, Provider::Apple, transaction, &bought.product, true)
            .await
            .unwrap();
        assert_eq!(marks(&mut tx, id).await.as_deref(), Some("apple"));
        tx.rollback().await.unwrap();
    }

    /// Two platforms, one mark: theirs to choose, and only among the ones
    /// they actually bought this year's pack on.
    #[tokio::test]
    async fn a_supporter_wears_the_platform_they_bought_on_until_they_choose() {
        let Some(mut tx) = tx().await else { return };
        let id = user(&mut tx, "supporter_test_f").await;
        let steam_id = "76561190000000103";
        record_owned_products(&mut tx, id, Provider::Steam, steam_id, &[pack(this_year())])
            .await
            .unwrap();
        assert_eq!(marks(&mut tx, id).await.as_deref(), Some("steam"));

        // Supporting in the App Store as well changes nothing by itself:
        // the mark is the platform they bought on first.
        query!(
            "UPDATE supporter_purchases SET purchased_at = now() - interval '1 day'
             WHERE user_id = $1",
            id,
        )
        .execute(&mut *tx)
        .await
        .unwrap();
        record_purchase(
            &mut tx,
            id,
            Provider::Apple,
            "2000000000000003",
            &pack(this_year()),
            true,
        )
        .await
        .unwrap();
        assert_eq!(marks(&mut tx, id).await.as_deref(), Some("steam"));

        set_mark(&mut tx, id, Some(Provider::Apple)).await.unwrap();
        assert_eq!(marks(&mut tx, id).await.as_deref(), Some("apple"));

        // Refunded on the platform they chose: the mark moves rather than
        // leaving them wearing a store they no longer own.
        record_recheck(
            &mut tx,
            Provider::Apple,
            "2000000000000003",
            &pack(this_year()).product,
            false,
        )
        .await
        .unwrap();
        assert_eq!(marks(&mut tx, id).await.as_deref(), Some("steam"));

        set_mark(&mut tx, id, None).await.unwrap();
        assert_eq!(marks(&mut tx, id).await.as_deref(), Some("steam"));
        tx.rollback().await.unwrap();
    }

    /// Unlinking Steam leaves the packs where they are: they were bought by
    /// the account, not by the identity, and the account is still there.
    #[tokio::test]
    async fn unlinking_the_platform_does_not_take_the_packs() {
        use crate::models::identity::{link_identity, unlink_identity, VerifiedIdentity};
        let Some(mut tx) = tx().await else { return };
        let id = user(&mut tx, "supporter_test_g").await;
        let steam_id = "76561190000000104";
        link_identity(
            &mut tx,
            id,
            &VerifiedIdentity {
                provider: Provider::Steam,
                subject: steam_id.to_string(),
                name: None,
                email: None,
                purchased: Some(vec![pack(this_year())]),
            },
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(marks(&mut tx, id).await.as_deref(), Some("steam"));

        let account = crate::models::user::find_user_by_id(&mut tx, id)
            .await
            .unwrap()
            .unwrap();
        unlink_identity(&mut tx, &account, Provider::Steam)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            marks(&mut tx, id).await.as_deref(),
            Some("steam"),
            "the pack was bought by the account"
        );
        tx.rollback().await.unwrap();
    }

    #[tokio::test]
    async fn a_supporter_can_leave_the_credits_and_keep_the_mark() {
        let Some(mut tx) = tx().await else { return };
        let id = user(&mut tx, "supporter_test_h").await;
        record_owned_products(
            &mut tx,
            id,
            Provider::Steam,
            "76561190000000105",
            &[pack(this_year())],
        )
        .await
        .unwrap();
        assert!(
            shows_in_credits(&mut tx, id).await.unwrap(),
            "listed by default"
        );

        set_show_in_credits(&mut tx, id, false).await.unwrap();
        assert!(!credited(
            &list_credits(&mut tx).await.unwrap(),
            "supporter_test_h"
        ));
        assert_eq!(marks(&mut tx, id).await.as_deref(), Some("steam"));
        tx.rollback().await.unwrap();
    }

    /// The author and a commenter who support are named, each with their own
    /// platform's mark; a commenter who does not, and a supporter who never
    /// touched the post, are not.
    #[tokio::test]
    async fn a_post_names_the_supporters_on_its_page() {
        let Some(mut tx) = tx().await else { return };
        let author = user(&mut tx, "supporter_test_i").await;
        let commenter = user(&mut tx, "supporter_test_j").await;
        let bystander = user(&mut tx, "supporter_test_k").await;
        let plain = user(&mut tx, "supporter_test_l").await;
        for (id, steam_id) in [
            (author, "76561190000000106"),
            (bystander, "76561190000000107"),
        ] {
            record_owned_products(&mut tx, id, Provider::Steam, steam_id, &[pack(this_year())])
                .await
                .unwrap();
        }
        record_purchase(
            &mut tx,
            commenter,
            Provider::Apple,
            "2000000000000004",
            &pack(this_year()),
            true,
        )
        .await
        .unwrap();

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

        let marks = supporter_marks_on_post(&mut tx, post).await.unwrap();
        let mut named = marks.keys().cloned().collect::<Vec<_>>();
        named.sort();
        assert_eq!(named, ["supporter_test_i", "supporter_test_j"]);
        assert_eq!(marks["supporter_test_i"], "steam");
        assert_eq!(marks["supporter_test_j"], "apple");
        tx.rollback().await.unwrap();
    }

    /// Steam is asked about the accounts it has linked -- which may have
    /// bought since -- and about the accounts that own packs here without
    /// being linked at all.
    #[tokio::test]
    async fn steam_accounts_not_asked_about_today_are_asked_again() {
        use crate::models::identity::{link_identity, VerifiedIdentity};
        let Some(mut tx) = tx().await else { return };
        let linked = user(&mut tx, "supporter_test_m").await;
        let unlinked = user(&mut tx, "supporter_test_n").await;
        link_identity(
            &mut tx,
            linked,
            &VerifiedIdentity {
                provider: Provider::Steam,
                subject: "76561190000000108".to_string(),
                name: None,
                email: None,
                purchased: None,
            },
        )
        .await
        .unwrap()
        .unwrap();
        record_owned_products(
            &mut tx,
            unlinked,
            Provider::Steam,
            "76561190000000109",
            &[pack(this_year())],
        )
        .await
        .unwrap();

        let due = steam_accounts_due_for_check(&mut tx, 1000).await.unwrap();
        assert!(
            !due.iter()
                .any(|account| account.steam_id == "76561190000000109"),
            "asked about today already"
        );

        query!("UPDATE user_identities SET supporter_checked_at = now() - interval '2 days'")
            .execute(&mut *tx)
            .await
            .unwrap();
        query!("UPDATE supporter_purchases SET checked_at = now() - interval '2 days'")
            .execute(&mut *tx)
            .await
            .unwrap();
        let due = steam_accounts_due_for_check(&mut tx, 1000).await.unwrap();
        let named = |steam_id: &str| {
            due.iter()
                .find(|account| account.steam_id == steam_id)
                .map(|account| account.user_id)
        };
        assert_eq!(named("76561190000000108"), Some(linked), "linked");
        assert_eq!(
            named("76561190000000109"),
            Some(unlinked),
            "owns a pack without being linked"
        );
        tx.rollback().await.unwrap();
    }

    /// The marks someone may choose between and the providers a purchase may
    /// come from are the same list: the platforms that sell a pack. Each has
    /// to be a provider an identity can come from as well, though that list
    /// is longer -- Google signs people in and sells nothing, which is why
    /// this names the sellers rather than every [`Provider`].
    #[tokio::test]
    async fn the_marks_are_what_the_database_allows() {
        let Some(mut tx) = tx().await else { return };
        async fn named(tx: &mut Transaction<'_, Postgres>, constraint: &str) -> Vec<String> {
            let definition: String = sqlx::query_scalar(
                "SELECT pg_get_constraintdef(oid) FROM pg_constraint WHERE conname = $1",
            )
            .bind(constraint)
            .fetch_one(&mut **tx)
            .await
            .unwrap();
            // CHECK ((supporter_mark = ANY (ARRAY['steam'::text, ...])))
            let mut names: Vec<String> = definition
                .split('\'')
                .skip(1)
                .step_by(2)
                .map(|name| name.to_string())
                .collect();
            names.sort();
            names
        }

        let marks = named(&mut tx, "users_supporter_mark_check").await;
        assert_eq!(
            marks,
            named(&mut tx, "supporter_purchases_provider_check").await
        );

        let mut sells = [Provider::Steam, Provider::Apple]
            .map(|provider| provider.as_str().to_string())
            .to_vec();
        sells.sort();
        assert_eq!(
            marks, sells,
            "a seller the code knows and the database does not, or the other way about"
        );
        assert!(
            !marks.contains(&Provider::Google.as_str().to_string()),
            "Google sells no pack yet; giving it one is a migration, not a mark"
        );

        let signs_in = named(&mut tx, "user_identities_provider_check").await;
        for mark in &marks {
            assert!(
                signs_in.contains(mark),
                "{mark} sells packs and cannot sign anyone in"
            );
        }
        tx.rollback().await.unwrap();
    }

    #[test]
    fn every_platform_mark_is_worded_in_every_locale() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("locales");
        for locale in ["en", "ko", "ja", "zh"] {
            let text = std::fs::read_to_string(dir.join(format!("{locale}.ftl"))).unwrap();
            let has = |id: &str| {
                text.lines()
                    .any(|line| line.starts_with(&format!("{id} = ")))
            };
            for provider in [Provider::Steam, Provider::Apple] {
                let id = format!("supporter-badge-{}", provider.as_str());
                assert!(has(&id), "{locale}.ftl has no {id}");
            }
            for id in ["supporter-year", "account-supporter-mark"] {
                assert!(has(id), "{locale}.ftl has no {id}");
            }
        }
    }

    /// Artwork and a name for each, in the one file that draws them.
    #[test]
    fn every_platform_has_its_own_mark() {
        let macro_file = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/templates/supporter_badge_macro.jinja"
        ))
        .unwrap();
        for provider in [Provider::Steam, Provider::Apple] {
            let branch = format!("mark == \"{}\"", provider.as_str());
            assert_eq!(
                macro_file.matches(&branch).count(),
                2,
                "{} wants artwork and a name",
                provider.as_str()
            );
        }
    }
}
