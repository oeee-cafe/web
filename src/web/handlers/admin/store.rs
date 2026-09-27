//! The store's catalogue of supporter products.

use crate::app_error::AppError;
use crate::models::store_product::{
    self, add as add_store_product, find as find_store_product, list_all as list_store_products,
    purchase_counts as store_purchase_counts, set_details as set_store_product_details,
    set_on_sale as set_store_product_on_sale, set_sale_window as set_store_product_sale_window,
    StoreProduct,
};
use crate::models::supporter::{current_year, Store};
use crate::web::context::CommonContext;
use crate::web::handlers::identity::from_this_site;
use crate::web::handlers::AdminUser;
use crate::web::i18n::ExtractFtlLang;
use crate::web::state::AppState;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::Form;
use minijinja::context;
use serde::Deserialize;

/// The time zone /admin/store is read and written in, as its dates are
/// printed elsewhere on the admin pages.
const STORE_TZ: chrono_tz::Tz = chrono_tz::Asia::Seoul;

/// How a `datetime-local` input writes a moment, and so how one is read back.
const LOCAL_INPUT: &str = "%Y-%m-%dT%H:%M";

/// The catalogue a store at a time, as /admin/store lists it.
#[derive(serde::Serialize)]
struct StoreGroup {
    store: Store,
    products: Vec<StoreProductRow>,
}

/// A product as its row shows it: the window again as the `datetime-local`
/// inputs that change it want it, in the page's own time zone.
#[derive(serde::Serialize)]
struct StoreProductRow {
    #[serde(flatten)]
    product: StoreProduct,
    sale_starts_local: String,
    sale_ends_local: String,
    /// How many purchases it has, which a change of year moves with it.
    purchases: i64,
}

pub(super) fn to_local_input(at: Option<chrono::DateTime<chrono::Utc>>) -> String {
    at.map(|at| at.with_timezone(&STORE_TZ).format(LOCAL_INPUT).to_string())
        .unwrap_or_default()
}

/// A sale window as two `datetime-local` values in Seoul time, either empty
/// for an end left open. A browser that was given a step adds seconds, so
/// those are read too.
pub(super) fn parse_sale_window(
    starts: &str,
    ends: &str,
) -> Result<
    (
        Option<chrono::DateTime<chrono::Utc>>,
        Option<chrono::DateTime<chrono::Utc>>,
    ),
    String,
> {
    use chrono::{NaiveDateTime, TimeZone};
    let parse =
        |value: &str, which: &str| -> Result<Option<chrono::DateTime<chrono::Utc>>, String> {
            let value = value.trim();
            if value.is_empty() {
                return Ok(None);
            }
            let naive = NaiveDateTime::parse_from_str(value, LOCAL_INPUT)
                .or_else(|_| NaiveDateTime::parse_from_str(value, "%Y-%m-%dT%H:%M:%S"))
                .map_err(|_| format!("The {which} is not a date and time."))?;
            // Seoul has no daylight saving, so every local time is exactly one
            // moment; `single` is only there to say so.
            STORE_TZ
                .from_local_datetime(&naive)
                .single()
                .map(|at| Some(at.with_timezone(&chrono::Utc)))
                .ok_or_else(|| format!("The {which} is not a time Seoul has."))
        };
    let starts = parse(starts, "start")?;
    let ends = parse(ends, "end")?;
    if let (Some(starts), Some(ends)) = (starts, ends) {
        if ends <= starts {
            return Err("A sale has to end after it starts.".to_string());
        }
    }
    Ok((starts, ends))
}

/// What the add form was sent, echoed back when it is turned away so
/// nothing has to be typed twice.
#[derive(Debug, Default, Deserialize, serde::Serialize)]
pub struct AddStoreProductForm {
    #[serde(default)]
    pub(super) store: String,
    #[serde(default)]
    pub(super) product: String,
    #[serde(default)]
    pub(super) year: String,
    #[serde(default)]
    pub(super) label: String,
    #[serde(default)]
    pub(super) sale_starts_at: String,
    #[serde(default)]
    pub(super) sale_ends_at: String,
}

/// The years a pack may be for: from the first one sold to a few ahead,
/// which is as far as anyone plans a pack. A typo like 20226 or 206 is
/// caught here rather than credited to purchases for ever.
fn sensible_years() -> std::ops::RangeInclusive<i32> {
    2020..=current_year() + 5
}

/// A year a product may count for, or why not.
fn validate_year(year: &str) -> Result<i32, String> {
    year.trim()
        .parse::<i32>()
        .ok()
        .filter(|year| sensible_years().contains(year))
        .ok_or_else(|| {
            let years = sensible_years();
            format!(
                "The year has to be between {} and {}.",
                years.start(),
                years.end()
            )
        })
}

/// A button's own words, `None` for the usual ones, or why not.
fn validate_label(label: &str) -> Result<Option<String>, String> {
    let label = label.trim();
    if label.chars().count() > 100 {
        return Err("A button label is a hundred characters at most.".to_string());
    }
    Ok((!label.is_empty()).then(|| label.to_string()))
}

/// A product ready to add, or why not.
pub(super) fn validate_store_product(
    form: &AddStoreProductForm,
    steam_app_id: Option<u32>,
) -> Result<(Store, String, i32, Option<String>), String> {
    let store = Store::parse(form.store.trim()).ok_or("Choose a store.")?;
    let product = form.product.trim();
    if product.is_empty() {
        return Err("A product id is needed.".to_string());
    }
    // Every store's ids are one unbroken word. Whitespace inside one is a
    // paste gone wrong, and would never match anything the store says.
    if product.chars().any(char::is_whitespace) || product.chars().count() > 100 {
        return Err("A product id is one word, with no spaces.".to_string());
    }
    // Play Console takes only these for a product id, so anything else was
    // never one of its products.
    if store == Store::Google
        && !(product.starts_with(|c: char| c.is_ascii_lowercase() || c.is_ascii_digit())
            && product
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '.'))
    {
        return Err(
            "A Google Play product id is lower-case letters, digits, _ and ., starting with a letter or digit."
                .to_string(),
        );
    }
    if store == Store::Steam {
        let Ok(app_id) = product.parse::<u32>() else {
            return Err("A Steam product is a DLC's app id, which is a number.".to_string());
        };
        if Some(app_id) == steam_app_id {
            return Err(
                "That is Oeee Cafe's own app id. Buying the app is not supporting it.".to_string(),
            );
        }
    }
    let year = validate_year(&form.year)?;
    let label = validate_label(&form.label)?;
    Ok((store, product.to_string(), year, label))
}

async fn render_store_page(
    state: &AppState,
    admin: &AdminUser,
    ftl_lang: String,
    error: Option<String>,
    form: AddStoreProductForm,
) -> Result<String, AppError> {
    let mut tx = state.db_pool.begin().await?;
    let mut products = list_store_products(&mut tx).await?;
    let counts = store_purchase_counts(&mut tx).await?;
    let common_ctx = CommonContext::build(&mut tx, Some(&admin.0), &ftl_lang).await?;
    tx.commit().await?;

    let groups: Vec<StoreGroup> = Store::ALL
        .into_iter()
        .map(|store| StoreGroup {
            store,
            products: {
                let (mine, rest): (Vec<_>, Vec<_>) = products
                    .drain(..)
                    .partition(|product| product.store == store.as_str());
                products = rest;
                mine.into_iter()
                    .map(|product| StoreProductRow {
                        sale_starts_local: to_local_input(product.sale_starts_at),
                        sale_ends_local: to_local_input(product.sale_ends_at),
                        purchases: counts
                            .get(&(product.store.clone(), product.product.clone()))
                            .copied()
                            .unwrap_or(0),
                        product,
                    })
                    .collect()
            },
        })
        .collect();

    Ok(state
        .render_page(
            "admin/store.jinja",
            common_ctx,
            context! {
                groups,
                stores => Store::ALL,
                this_year => current_year(),
                microsoft_configured => state.config.microsoft_store.is_some(),
                google_play_configured => state.config.google_play.is_some(),
                error,
                form,
            },
        )
        .await?)
}

/// GET /admin/store -- the Supporter Pack catalogue: every product each
/// store sells or has sold, which year it counts for, and whether
/// /supporter offers it.
pub async fn admin_store(
    admin: AdminUser,
    ExtractFtlLang(ftl_lang): ExtractFtlLang,
    State(state): State<AppState>,
) -> Result<Html<String>, AppError> {
    let rendered = render_store_page(
        &state,
        &admin,
        ftl_lang,
        None,
        AddStoreProductForm::default(),
    )
    .await?;
    Ok(Html(rendered))
}

/// POST /admin/store -- adds a product, on sale. Turned away, the page comes
/// back with why and with what was typed.
///
/// Admin POSTs otherwise lean on the session cookie being SameSite=Lax.
/// This one decides what people are charged for, so it checks the Origin as
/// the purchase routes do (`from_this_site`) as well.
pub async fn admin_add_store_product(
    admin: AdminUser,
    ExtractFtlLang(ftl_lang): ExtractFtlLang,
    State(state): State<AppState>,
    headers: HeaderMap,
    Form(form): Form<AddStoreProductForm>,
) -> Result<Response, AppError> {
    if !from_this_site(&headers, &state.config.base_url) {
        return Err(AppError::Forbidden);
    }
    let steam_app_id = state.config.steam.as_ref().map(|steam| steam.app_id);
    let refused = |error: String, form: AddStoreProductForm| async {
        let rendered =
            render_store_page(&state, &admin, ftl_lang.clone(), Some(error), form).await?;
        Ok::<_, AppError>((StatusCode::BAD_REQUEST, Html(rendered)).into_response())
    };
    let (store, product, year, label) = match validate_store_product(&form, steam_app_id) {
        Ok(valid) => valid,
        Err(error) => return refused(error, form).await,
    };
    let (starts_at, ends_at) = match parse_sale_window(&form.sale_starts_at, &form.sale_ends_at) {
        Ok(window) => window,
        Err(error) => return refused(error, form).await,
    };

    let mut tx = state.db_pool.begin().await?;
    let added = add_store_product(&mut tx, store, &product, year, label.as_deref()).await?;
    if added && (starts_at.is_some() || ends_at.is_some()) {
        set_store_product_sale_window(&mut tx, store, &product, starts_at, ends_at).await?;
    }
    tx.commit().await?;
    if !added {
        return refused(
            format!(
                "{} already has {product}. Change its year or button in the table; to stop selling it, take it off sale.",
                store.as_str()
            ),
            form,
        )
        .await;
    }
    tracing::info!(
        admin = %admin.0.login_name,
        store = store.as_str(),
        product,
        year,
        ?starts_at,
        ?ends_at,
        "added a product to the store catalogue"
    );
    store_product::refresh_any_on_sale(&state.db_pool).await?;
    Ok(Redirect::to("/admin/store").into_response())
}

#[derive(Debug, Deserialize)]
pub struct StoreProductOnSaleForm {
    /// Desired end state, not a toggle, so a double-submit is idempotent.
    pub on_sale: bool,
}

/// POST /admin/store/:store/:product/on-sale -- puts a product on sale or
/// takes it off. Off sale is off /supporter and nowhere else: whoever
/// bought it keeps it, and its refunds are still heard.
pub async fn admin_set_store_product_on_sale(
    admin: AdminUser,
    State(state): State<AppState>,
    Path((store, product)): Path<(String, String)>,
    headers: HeaderMap,
    Form(form): Form<StoreProductOnSaleForm>,
) -> Result<Response, AppError> {
    if !from_this_site(&headers, &state.config.base_url) {
        return Err(AppError::Forbidden);
    }
    let Some(store) = Store::parse(&store) else {
        return Err(AppError::NotFound("Store".to_string()));
    };
    let mut tx = state.db_pool.begin().await?;
    if find_store_product(&mut tx, store, &product)
        .await?
        .is_none()
    {
        return Err(AppError::NotFound("Product".to_string()));
    }
    set_store_product_on_sale(&mut tx, store, &product, form.on_sale).await?;
    tx.commit().await?;
    tracing::info!(
        admin = %admin.0.login_name,
        store = store.as_str(),
        product,
        on_sale = form.on_sale,
        "changed whether a product is on sale"
    );
    store_product::refresh_any_on_sale(&state.db_pool).await?;
    Ok(Redirect::to("/admin/store").into_response())
}

#[derive(Debug, Deserialize)]
pub struct StoreProductSaleWindowForm {
    #[serde(default)]
    pub sale_starts_at: String,
    #[serde(default)]
    pub sale_ends_at: String,
}

/// POST /admin/store/:store/:product/sale-window -- sets when a product is
/// sold, in Seoul time, either end left empty for open. It narrows being on
/// sale and nothing more: /supporter offers the product inside the window
/// while it is on sale, and its purchases count whenever they were made.
pub async fn admin_set_store_product_sale_window(
    admin: AdminUser,
    ExtractFtlLang(ftl_lang): ExtractFtlLang,
    State(state): State<AppState>,
    Path((store, product)): Path<(String, String)>,
    headers: HeaderMap,
    Form(form): Form<StoreProductSaleWindowForm>,
) -> Result<Response, AppError> {
    if !from_this_site(&headers, &state.config.base_url) {
        return Err(AppError::Forbidden);
    }
    let Some(store) = Store::parse(&store) else {
        return Err(AppError::NotFound("Store".to_string()));
    };
    let (starts_at, ends_at) = match parse_sale_window(&form.sale_starts_at, &form.sale_ends_at) {
        Ok(window) => window,
        Err(error) => {
            let rendered = render_store_page(
                &state,
                &admin,
                ftl_lang,
                Some(format!("{product}: {error}")),
                AddStoreProductForm::default(),
            )
            .await?;
            return Ok((StatusCode::BAD_REQUEST, Html(rendered)).into_response());
        }
    };
    let mut tx = state.db_pool.begin().await?;
    if !set_store_product_sale_window(&mut tx, store, &product, starts_at, ends_at).await? {
        return Err(AppError::NotFound("Product".to_string()));
    }
    tx.commit().await?;
    tracing::info!(
        admin = %admin.0.login_name,
        store = store.as_str(),
        product,
        ?starts_at,
        ?ends_at,
        "changed when a product is sold"
    );
    store_product::refresh_any_on_sale(&state.db_pool).await?;
    Ok(Redirect::to("/admin/store").into_response())
}

#[derive(Debug, Deserialize)]
pub struct StoreProductDetailsForm {
    #[serde(default)]
    pub year: String,
    #[serde(default)]
    pub label: String,
}

/// POST /admin/store/:store/:product/details -- corrects a product's year
/// and its button's words. A new year moves every purchase of it with it
/// (`store_product::set_details`); the table says how many before the
/// button is pressed. Turned away, the page comes back with why.
pub async fn admin_set_store_product_details(
    admin: AdminUser,
    ExtractFtlLang(ftl_lang): ExtractFtlLang,
    State(state): State<AppState>,
    Path((store, product)): Path<(String, String)>,
    headers: HeaderMap,
    Form(form): Form<StoreProductDetailsForm>,
) -> Result<Response, AppError> {
    if !from_this_site(&headers, &state.config.base_url) {
        return Err(AppError::Forbidden);
    }
    let Some(store) = Store::parse(&store) else {
        return Err(AppError::NotFound("Store".to_string()));
    };
    let valid = validate_year(&form.year)
        .and_then(|year| validate_label(&form.label).map(|label| (year, label)));
    let (year, label) = match valid {
        Ok(valid) => valid,
        Err(error) => {
            let rendered = render_store_page(
                &state,
                &admin,
                ftl_lang,
                Some(format!("{product}: {error}")),
                AddStoreProductForm::default(),
            )
            .await?;
            return Ok((StatusCode::BAD_REQUEST, Html(rendered)).into_response());
        }
    };
    let mut tx = state.db_pool.begin().await?;
    let Some(moved) =
        set_store_product_details(&mut tx, store, &product, year, label.as_deref()).await?
    else {
        return Err(AppError::NotFound("Product".to_string()));
    };
    tx.commit().await?;
    tracing::info!(
        admin = %admin.0.login_name,
        store = store.as_str(),
        product,
        year,
        ?label,
        moved,
        "changed a product's year or button"
    );
    Ok(Redirect::to("/admin/store").into_response())
}
