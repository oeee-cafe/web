//! The catalogue: what each store sells as a Supporter Pack, and which year
//! each product is for.
//!
//! Everything that asks "is this one of ours, and which year is it?" asks
//! here -- the App Store lookup, Steam's ownership check, the Microsoft
//! Store's collections query, the daily rechecks -- and /supporter asks here
//! what to offer. Staff change it at /admin/store.
//!
//! **A product is never taken out.** A pack counts for whoever bought it
//! whether or not it is still sold: a delisted Steam DLC stays owned, and a
//! refund of last year's pack still has to be heard so it can be taken back.
//! So there is no delete. Taking a product off sale is `on_sale = false`,
//! which only stops /supporter offering it; every check still knows it.
//!
//! **A product's year does not change.** It is what a purchase is credited
//! with, and changing it would quietly move every purchase already made
//! from one year to another. A different year is a different product.

use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::{query, query_as, query_scalar, Postgres, Transaction};

use super::supporter::{OwnedProduct, Store};
use crate::config::AppConfig;

/// One product in the catalogue.
#[derive(Clone, Debug, Serialize)]
pub struct StoreProduct {
    /// "apple", "microsoft" or "steam".
    pub store: String,
    /// The store's own id for it: an App Store product id, a Microsoft Store
    /// ID, or a Steam app id written out.
    pub product: String,
    /// The Supporter Pack year a purchase of it counts for.
    pub year: i32,
    /// What its button says instead of the usual "Buy the 2026 Supporter
    /// Pack", where staff have given it words of its own.
    pub label: Option<String>,
    /// Whether /supporter offers it. A product off sale still counts for
    /// whoever bought it.
    pub on_sale: bool,
    pub created_at: DateTime<Utc>,
}

/// Every product, on sale or not, a store at a time and the newest years
/// first: what /admin/store lists.
pub async fn list_all(tx: &mut Transaction<'_, Postgres>) -> Result<Vec<StoreProduct>> {
    let products = query_as!(
        StoreProduct,
        r#"
        SELECT store, product, year, label, on_sale, created_at
        FROM store_products
        ORDER BY store, year DESC, created_at, product
        "#,
    )
    .fetch_all(&mut **tx)
    .await?;
    Ok(products)
}

/// What /supporter offers in `store` for `year`: the products on sale, in
/// the order they were added.
pub async fn list_on_sale(
    tx: &mut Transaction<'_, Postgres>,
    store: Store,
    year: i32,
) -> Result<Vec<StoreProduct>> {
    let products = query_as!(
        StoreProduct,
        r#"
        SELECT store, product, year, label, on_sale, created_at
        FROM store_products
        WHERE store = $1 AND year = $2 AND on_sale
        ORDER BY created_at, product
        "#,
        store.as_str(),
        year,
    )
    .fetch_all(&mut **tx)
    .await?;
    Ok(products)
}

/// Every product `store` has ever sold, on sale or not, as a purchase of it
/// is recorded: what a purchase from that store is checked against.
pub async fn packs(tx: &mut Transaction<'_, Postgres>, store: Store) -> Result<Vec<OwnedProduct>> {
    let packs = query!(
        "SELECT product, year FROM store_products WHERE store = $1 ORDER BY year, product",
        store.as_str(),
    )
    .fetch_all(&mut **tx)
    .await?
    .into_iter()
    .map(|row| OwnedProduct {
        product: row.product,
        year: row.year,
    })
    .collect();
    Ok(packs)
}

/// The same, for a background job that holds a pool rather than a
/// transaction.
pub async fn packs_in(db: &sqlx::PgPool, store: Store) -> Result<Vec<OwnedProduct>> {
    let mut tx = db.begin().await?;
    let packs = packs(&mut tx, store).await?;
    tx.commit().await?;
    Ok(packs)
}

pub async fn find(
    tx: &mut Transaction<'_, Postgres>,
    store: Store,
    product: &str,
) -> Result<Option<StoreProduct>> {
    let found = query_as!(
        StoreProduct,
        r#"
        SELECT store, product, year, label, on_sale, created_at
        FROM store_products
        WHERE store = $1 AND product = $2
        "#,
        store.as_str(),
        product,
    )
    .fetch_optional(&mut **tx)
    .await?;
    Ok(found)
}

/// Adds a product, on sale. `false` when the store already has one by that
/// id, which is left exactly as it was: its year is what its purchases were
/// credited with, and is not something a second form can change.
pub async fn add(
    tx: &mut Transaction<'_, Postgres>,
    store: Store,
    product: &str,
    year: i32,
    label: Option<&str>,
) -> Result<bool> {
    let added = query!(
        r#"
        INSERT INTO store_products (store, product, year, label)
        VALUES ($1, $2, $3, $4)
        ON CONFLICT (store, product) DO NOTHING
        "#,
        store.as_str(),
        product,
        year,
        label,
    )
    .execute(&mut **tx)
    .await?
    .rows_affected();
    Ok(added == 1)
}

/// Puts a product on sale or takes it off. `false` when there is no such
/// product.
pub async fn set_on_sale(
    tx: &mut Transaction<'_, Postgres>,
    store: Store,
    product: &str,
    on_sale: bool,
) -> Result<bool> {
    let changed = query!(
        "UPDATE store_products SET on_sale = $3 WHERE store = $1 AND product = $2",
        store.as_str(),
        product,
        on_sale,
    )
    .execute(&mut **tx)
    .await?
    .rows_affected();
    Ok(changed == 1)
}

/// The products the config still lists, from before the catalogue was a
/// table: `[[app_store.supporter_products]]` and `[[steam.supporter_apps]]`,
/// leaving out the Steam app itself as those lists always did -- buying Oeee
/// Cafe is not supporting it.
pub fn configured(config: &AppConfig) -> Vec<(Store, OwnedProduct)> {
    let mut products = Vec::new();
    if let Some(app_store) = config.app_store.as_ref() {
        for pack in &app_store.supporter_products {
            products.push((
                Store::Apple,
                OwnedProduct {
                    product: pack.product_id.clone(),
                    year: pack.year,
                },
            ));
        }
    }
    if let Some(steam) = config.steam.as_ref() {
        for pack in &steam.supporter_apps {
            if pack.app_id != steam.app_id {
                products.push((
                    Store::Steam,
                    OwnedProduct {
                        product: pack.app_id.to_string(),
                        year: pack.year,
                    },
                ));
            }
        }
    }
    products
}

/// Brings what the config lists into the catalogue, on boot.
///
/// Only ever adds. A product already here is left as it is -- taken off sale
/// at /admin/store, it stays off however long the config goes on listing it
/// -- so running this on every boot, from either colour, changes nothing
/// after the first. The config tables are kept parseable so a server whose
/// config still has them boots, and so the first boot of this release
/// starts with the packs it was selling rather than with none.
pub async fn import_configured(db: &sqlx::PgPool, config: &AppConfig) -> Result<u64> {
    let mut tx = db.begin().await?;
    let mut added = 0;
    for (store, pack) in configured(config) {
        if add(&mut tx, store, &pack.product, pack.year, None).await? {
            added += 1;
        }
    }
    tx.commit().await?;
    Ok(added)
}

static ANY_ON_SALE: AtomicBool = AtomicBool::new(false);

/// Whether any store has anything on sale: what decides if the toolbar has
/// a heart in it at all. Kept in memory, because the toolbar is on every
/// page and the answer changes only when the catalogue does.
pub fn any_on_sale() -> bool {
    ANY_ON_SALE.load(Ordering::Relaxed)
}

/// Reads [`any_on_sale`] again from the table: on boot, and after every
/// change at /admin/store.
///
/// Per process, so per colour. Only one serves at a time, and the one that
/// starts next reads it on boot.
pub async fn refresh_any_on_sale(db: &sqlx::PgPool) -> Result<bool> {
    let any =
        query_scalar!(r#"SELECT EXISTS (SELECT 1 FROM store_products WHERE on_sale) AS "any!""#)
            .fetch_one(db)
            .await?;
    ANY_ON_SALE.store(any, Ordering::Relaxed);
    Ok(any)
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

    /// Off sale is off the page and nowhere else: every check still knows
    /// the product, because somebody bought it while it was on sale.
    #[tokio::test]
    async fn a_product_off_sale_still_counts() {
        let Some(mut tx) = tx().await else { return };
        assert!(add(&mut tx, Store::Microsoft, "9TESTPACK001", 2026, None)
            .await
            .unwrap());
        assert!(add(
            &mut tx,
            Store::Microsoft,
            "9TESTPACK002",
            2026,
            Some("Buy it")
        )
        .await
        .unwrap());
        let offered = |products: Vec<StoreProduct>| {
            products
                .into_iter()
                .map(|product| product.product)
                .filter(|product| product.starts_with("9TESTPACK"))
                .collect::<Vec<_>>()
        };
        assert_eq!(
            offered(list_on_sale(&mut tx, Store::Microsoft, 2026).await.unwrap()),
            ["9TESTPACK001", "9TESTPACK002"]
        );
        assert!(offered(list_on_sale(&mut tx, Store::Microsoft, 2027).await.unwrap()).is_empty());
        assert!(offered(list_on_sale(&mut tx, Store::Steam, 2026).await.unwrap()).is_empty());

        assert!(
            set_on_sale(&mut tx, Store::Microsoft, "9TESTPACK001", false)
                .await
                .unwrap()
        );
        assert_eq!(
            offered(list_on_sale(&mut tx, Store::Microsoft, 2026).await.unwrap()),
            ["9TESTPACK002"]
        );
        let known = packs(&mut tx, Store::Microsoft).await.unwrap();
        assert!(known
            .iter()
            .any(|pack| pack.product == "9TESTPACK001" && pack.year == 2026));

        let found = find(&mut tx, Store::Microsoft, "9TESTPACK002")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(found.label.as_deref(), Some("Buy it"));
        assert!(found.on_sale);
        assert!(
            !set_on_sale(&mut tx, Store::Microsoft, "9NOSUCHPACK", false)
                .await
                .unwrap()
        );
        tx.rollback().await.unwrap();
    }

    /// Adding a product that is already there changes nothing: its year is
    /// what its purchases were credited with.
    #[tokio::test]
    async fn a_product_is_added_once_and_keeps_its_year() {
        let Some(mut tx) = tx().await else { return };
        assert!(
            add(&mut tx, Store::Apple, "cafe.oeee.test.2026", 2026, None)
                .await
                .unwrap()
        );
        set_on_sale(&mut tx, Store::Apple, "cafe.oeee.test.2026", false)
            .await
            .unwrap();
        assert!(
            !add(&mut tx, Store::Apple, "cafe.oeee.test.2026", 2030, None)
                .await
                .unwrap()
        );
        let found = find(&mut tx, Store::Apple, "cafe.oeee.test.2026")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(found.year, 2026);
        assert!(
            !found.on_sale,
            "still off sale: the import does not put it back"
        );
        tx.rollback().await.unwrap();
    }
}
