//! Staff-only views. Every handler here takes `AdminUser`, which is what makes
//! the unfiltered queries in `models::admin` safe to expose.
//!
//! Copy is intentionally untranslated: this surface is staff-only, so it is not
//! worth carrying through the four locale bundles.

mod posts;
pub use posts::*;
mod banners;
pub use banners::*;
mod communities;
pub use communities::*;
mod lists;
pub use lists::*;
mod overview;
pub use overview::*;
mod range;
mod sessions;
pub use sessions::*;
mod store;
pub use store::*;
mod users;
pub use users::*;

#[cfg(test)]
mod tests;
