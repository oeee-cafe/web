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
//! **When it is sold is a window, and the window only narrows `on_sale`.**
//! `sale_starts_at` and `sale_ends_at` are when /supporter starts and stops
//! offering it; either may be open. A product is offered while it is on sale
//! *and* inside its window, so taking it off sale still stops it at once,
//! and a purchase made outside the window counts all the same. Every query
//! below that asks what is offered spells the window out the same way.
//!
//! **A product's year does not change.** It is what a purchase is credited
//! with, and changing it would quietly move every purchase already made
//! from one year to another. A different year is a different product.

use std::sync::RwLock;

use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::{query, query_as, Postgres, Transaction};

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
    /// When /supporter starts offering it; open when there is none.
    pub sale_starts_at: Option<DateTime<Utc>>,
    /// When /supporter stops offering it; open when there is none.
    pub sale_ends_at: Option<DateTime<Utc>>,
    /// On sale and inside its window by the database's clock: whether
    /// /supporter offers it now.
    pub selling_now: bool,
    pub created_at: DateTime<Utc>,
}

/// Every product, on sale or not, a store at a time and the newest years
/// first: what /admin/store lists.
pub async fn list_all(tx: &mut Transaction<'_, Postgres>) -> Result<Vec<StoreProduct>> {
    let products = query_as!(
        StoreProduct,
        r#"
        SELECT store, product, year, label, on_sale, sale_starts_at, sale_ends_at,
               (on_sale
                AND (sale_starts_at IS NULL OR sale_starts_at <= now())
                AND (sale_ends_at IS NULL OR now() < sale_ends_at)) AS "selling_now!",
               created_at
        FROM store_products
        ORDER BY store, year DESC, created_at, product
        "#,
    )
    .fetch_all(&mut **tx)
    .await?;
    Ok(products)
}

/// What /supporter offers in `store` for `year`: the products on sale and
/// inside their windows, in the order they were added.
pub async fn list_on_sale(
    tx: &mut Transaction<'_, Postgres>,
    store: Store,
    year: i32,
) -> Result<Vec<StoreProduct>> {
    let products = query_as!(
        StoreProduct,
        r#"
        SELECT store, product, year, label, on_sale, sale_starts_at, sale_ends_at,
               (on_sale
                AND (sale_starts_at IS NULL OR sale_starts_at <= now())
                AND (sale_ends_at IS NULL OR now() < sale_ends_at)) AS "selling_now!",
               created_at
        FROM store_products
        WHERE store = $1 AND year = $2 AND on_sale
          AND (sale_starts_at IS NULL OR sale_starts_at <= now())
          AND (sale_ends_at IS NULL OR now() < sale_ends_at)
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
        SELECT store, product, year, label, on_sale, sale_starts_at, sale_ends_at,
               (on_sale
                AND (sale_starts_at IS NULL OR sale_starts_at <= now())
                AND (sale_ends_at IS NULL OR now() < sale_ends_at)) AS "selling_now!",
               created_at
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

/// Sets when a product is sold, `None` leaving that end open. `false` when
/// there is no such product. The caller has checked that a start comes
/// before an end; the table checks it again.
pub async fn set_sale_window(
    tx: &mut Transaction<'_, Postgres>,
    store: Store,
    product: &str,
    starts_at: Option<DateTime<Utc>>,
    ends_at: Option<DateTime<Utc>>,
) -> Result<bool> {
    let changed = query!(
        r#"
        UPDATE store_products SET sale_starts_at = $3, sale_ends_at = $4
        WHERE store = $1 AND product = $2
        "#,
        store.as_str(),
        product,
        starts_at,
        ends_at,
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

/// A product on sale, as the window it is sold in. What [`any_on_sale`]
/// looks at: the flag alone would go stale the moment a window opened or
/// closed, which happens with nobody at /admin/store to refresh it.
#[derive(Clone, Copy, Debug)]
struct SaleWindow {
    starts_at: Option<DateTime<Utc>>,
    ends_at: Option<DateTime<Utc>>,
}

impl SaleWindow {
    fn contains(&self, now: DateTime<Utc>) -> bool {
        self.starts_at.is_none_or(|starts| starts <= now)
            && self.ends_at.is_none_or(|ends| now < ends)
    }
}

static ON_SALE: RwLock<Vec<SaleWindow>> = RwLock::new(Vec::new());

/// Whether any store has anything on sale right now: what decides if the
/// toolbar has a heart in it at all. Kept in memory, because the toolbar is
/// on every page and the catalogue changes only at /admin/store -- the
/// windows are kept rather than the answer, so the answer follows the clock.
pub fn any_on_sale() -> bool {
    let now = Utc::now();
    ON_SALE
        .read()
        .map(|windows| windows.iter().any(|window| window.contains(now)))
        .unwrap_or(false)
}

/// Reads the windows [`any_on_sale`] looks at again from the table: on boot,
/// and after every change at /admin/store. Returns whether anything is on
/// sale now.
///
/// Per process, so per colour. Only one serves at a time, and the one that
/// starts next reads it on boot.
pub async fn refresh_any_on_sale(db: &sqlx::PgPool) -> Result<bool> {
    let windows: Vec<SaleWindow> = query!(
        "SELECT sale_starts_at, sale_ends_at FROM store_products WHERE on_sale"
    )
    .fetch_all(db)
    .await?
    .into_iter()
    .map(|row| SaleWindow {
        starts_at: row.sale_starts_at,
        ends_at: row.sale_ends_at,
    })
    .collect();
    if let Ok(mut on_sale) = ON_SALE.write() {
        *on_sale = windows;
    }
    Ok(any_on_sale())
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

    /// Outside its window a product on sale is not offered, and says so;
    /// taking the window away offers it again. Its purchases count
    /// throughout.
    #[tokio::test]
    async fn a_product_is_offered_only_inside_its_window() {
        let Some(mut tx) = tx().await else { return };
        let offered = |products: Vec<StoreProduct>| {
            products
                .into_iter()
                .any(|product| product.product == "9TESTWINDOW1")
        };
        assert!(add(&mut tx, Store::Microsoft, "9TESTWINDOW1", 2026, None)
            .await
            .unwrap());
        let hour = chrono::Duration::hours(1);
        let now = Utc::now();
        for (starts, ends, inside) in [
            (Some(now + hour), None, false),
            (None, Some(now - hour), false),
            (Some(now - hour), Some(now + hour), true),
            (None, None, true),
        ] {
            assert!(
                set_sale_window(&mut tx, Store::Microsoft, "9TESTWINDOW1", starts, ends)
                    .await
                    .unwrap()
            );
            assert_eq!(
                offered(list_on_sale(&mut tx, Store::Microsoft, 2026).await.unwrap()),
                inside,
                "{starts:?}..{ends:?}"
            );
            let found = find(&mut tx, Store::Microsoft, "9TESTWINDOW1")
                .await
                .unwrap()
                .unwrap();
            assert_eq!(found.selling_now, inside);
            assert!(packs(&mut tx, Store::Microsoft)
                .await
                .unwrap()
                .iter()
                .any(|pack| pack.product == "9TESTWINDOW1"));
        }
        // The table refuses a window that ends before it starts.
        assert!(set_sale_window(
            &mut tx,
            Store::Microsoft,
            "9TESTWINDOW1",
            Some(now + hour),
            Some(now)
        )
        .await
        .is_err());
        tx.rollback().await.unwrap();
    }

    #[test]
    fn a_window_is_open_at_either_end_it_leaves_out() {
        let now = Utc::now();
        let hour = chrono::Duration::hours(1);
        let window = |starts_at, ends_at| SaleWindow { starts_at, ends_at };
        assert!(window(None, None).contains(now));
        assert!(window(Some(now), None).contains(now), "a start is inclusive");
        assert!(!window(None, Some(now)).contains(now), "an end is not");
        assert!(!window(Some(now + hour), None).contains(now));
        assert!(window(Some(now - hour), Some(now + hour)).contains(now));
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
