//! HTML that has been cleaned, and can therefore be printed as it is.
//!
//! A comment that arrives from another server brings its body as HTML, and
//! templates print it with `|safe` (comments_macro.jinja,
//! comment_card_macro.jinja, notification_item.jinja). That is only safe
//! because it went through `ammonia::clean` on the way in. [`SanitizedHtml`]
//! is how that step is kept from being skipped: it can only be made by
//! cleaning, and the functions that store HTML take nothing else, so HTML
//! that has not been cleaned does not compile its way into the table.
//!
//! What is read back out is a `String` again. The table only ever receives
//! a `SanitizedHtml`, so what it holds has been cleaned; a row written some
//! other way -- by hand, or before this type -- is outside what the type
//! can promise.

/// HTML that has been through `ammonia::clean`, with its defaults: no
/// scripts, no event handlers, no `javascript:` links, and a short list of
/// tags and attributes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SanitizedHtml(String);

impl SanitizedHtml {
    /// The only way to make one.
    pub fn clean(html: &str) -> Self {
        SanitizedHtml(ammonia::clean(html))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn into_string(self) -> String {
        self.0
    }
}

#[cfg(test)]
mod tests {
    use super::SanitizedHtml;

    #[test]
    fn what_is_cleaned_keeps_its_words_and_loses_its_scripts() {
        let cleaned = SanitizedHtml::clean(
            r#"<p onclick="steal()">Lovely <a href="javascript:steal()">colours</a></p><script>steal()</script>"#,
        );
        assert!(cleaned.as_str().contains("Lovely"));
        assert!(cleaned.as_str().contains("colours"));
        for gone in ["onclick", "javascript:", "<script"] {
            assert!(!cleaned.as_str().contains(gone), "{gone} in {cleaned:?}");
        }
    }
}
