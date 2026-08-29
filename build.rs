//! Rebuild when a migration is added or changed.
//!
//! `sqlx::migrate!()` in `web::app::App::new` embeds the contents of
//! `migrations/` into the binary at compile time, and it does that with
//! `include_str!`, so rustc only records the files that existed when it last
//! compiled. Adding a *new* migration therefore changes nothing rustc is
//! watching: cargo reuses the cached build, the binary ships the old set of
//! migrations, and the new one silently never runs.
//!
//! That is not theoretical. `20260829055733_drop_hashtag_post_count` was
//! deployed on 2026-08-29 in a release that touched nothing but `migrations/`.
//! The build was served from cache, the container came up healthy, the deploy
//! reported success, and the column was still there — the binary did not
//! contain the migration at all. A release that changes any Rust source hides
//! this, because then the crate rebuilds for other reasons.
//!
//! Watching the directory catches a new file, which is exactly the case
//! `include_str!` misses.
fn main() {
    println!("cargo:rerun-if-changed=migrations");
}
