//! Reporting a post.

use crate::web::handlers::test_support;
use minijinja::context;
use std::path::PathBuf;

#[test]
fn the_report_result_looks_up_the_key_it_was_handed() {
    // The handler picks one of four Fluent keys and passes it in as a
    // value. That indirection is easy to get wrong in a way parsing
    // cannot see -- `ftl_get_message("message_key")` renders happily and
    // shows every reporter the same wrong string. The stub echoes ids
    // back, so the id that comes out is the id the template looked up.
    let env = test_support::env();
    let template = env
        .get_template("report_result.jinja")
        .unwrap_or_else(|e| panic!("report_result.jinja loads: {e:#}"));

    for key in [
        "post-report-success",
        "post-report-error",
        "profile-report-success",
        "profile-report-error",
    ] {
        let rendered = template
            .render(context! { message_key => key, ftl_lang => "en" })
            .unwrap_or_else(|e| panic!("report_result.jinja renders {key}: {e:#}"));

        assert!(
            rendered.contains(key),
            "{key} was not the id looked up; the variable is not being dereferenced"
        );
        assert!(
            !rendered.contains("message_key"),
            "the template looked up the literal name of the variable"
        );
        assert!(
            rendered.contains("report-result"),
            "the modal needs the wrapper to swap over the form"
        );
    }
}

#[test]
fn every_locale_defines_the_keys_the_report_result_can_ask_for() {
    // The companion to the test above: that one proves the id reaches
    // Fluent, this one proves Fluent has something to say for it. Neither
    // catches the other's failure, because the render tests stub the
    // bundle out entirely.
    let locales = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("locales");
    for locale in ["en", "ko", "ja", "zh"] {
        let path = locales.join(format!("{locale}.ftl"));
        let text = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("{} reads: {e:#}", path.display()));
        for key in [
            "post-report-success",
            "post-report-error",
            "profile-report-success",
            "profile-report-error",
            "close",
        ] {
            assert!(
                text.lines()
                    .any(|line| line.starts_with(&format!("{key} = "))),
                "{locale}.ftl has no {key}, so that modal would show its own key name"
            );
        }
    }
}
