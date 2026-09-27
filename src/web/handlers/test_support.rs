//! Shared minijinja environment for template render tests.
//!
//! Must mirror the setup in `main.rs` — most importantly the autoescape
//! callback, or tests would pass while production rendered unescaped.

use minijinja::{path_loader, Environment, State};
use std::path::PathBuf;

pub fn env() -> Environment<'static> {
    let mut env = Environment::new();
    env.set_auto_escape_callback(|_| minijinja::AutoEscape::Html);
    minijinja_contrib::add_to_environment(&mut env);
    env.add_filter("cachebuster", |value: String| value);
    env.add_filter("markdown", |value: String| value);
    // The real filter: it is pure, so tests render what production does.
    env.add_filter("ago", crate::relative_time::ago_filter);
    env.add_function("ftl_get_message", |_state: &State, id: String| id);
    // The real function interpolates the arguments into the locale's
    // pattern. This stub has no bundle to interpolate into, so it appends
    // them as `id(name=value)` instead of dropping them: with the arguments
    // discarded, a template that passes the wrong variable — or none —
    // renders byte for byte like one that passes the right one, and no test
    // can tell the difference.
    env.add_function(
        "ftl_format_pattern",
        |_state: &State, id: String, args: minijinja::Value| {
            let mut pairs: Vec<String> = Vec::new();
            if let Ok(keys) = args.try_iter() {
                for key in keys {
                    if let Ok(value) = args.get_item(&key) {
                        pairs.push(format!("{key}={value}"));
                    }
                }
            }
            // Argument order is not guaranteed; sort so assertions are stable.
            pairs.sort();
            if pairs.is_empty() {
                id
            } else {
                format!("{id}({})", pairs.join(","))
            }
        },
    );
    crate::web::templates::add_to_environment(&mut env);
    env.add_global("r2_public_endpoint_url", "https://example.test");
    env.add_global("base_url", "https://oeee.test");
    env.set_loader(path_loader(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("templates"),
    ));
    env
}
