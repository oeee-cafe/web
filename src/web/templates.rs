//! The one way a page is rendered, and the one place supporters are asked
//! about.
//!
//! A handle's pill is lime for a supporter (person_macro.jinja), and who is
//! a supporter is Postgres's to say, at the moment the page is drawn. Rather
//! than every handler working out who its page will name and asking, the
//! page says: `handle()` records each login name it prints, through
//! [`supporter_slot`], and leaves a placeholder where its pill goes. Once the
//! page is rendered, the names it printed are asked about in one query and
//! each placeholder becomes its pill. However a handler built its context --
//! typed rows, `json!`, strings it formatted itself -- a handle that is
//! printed cannot be missed, and one that is not is not asked about.
//!
//! [`Templates`] is what `AppState` holds instead of the environment, and it
//! offers no way to render that skips this: a handler that renders a page
//! goes through [`Templates::render`] or it does not compile.
//!
//! A placeholder is the private-use characters U+E000 and U+E001 around a
//! nonce chosen for this render and the name's position in the list the
//! render kept. No name or text is in it, so nothing reaches the output
//! unescaped by passing through one, and text someone wrote cannot spell a
//! placeholder out without the nonce, which never leaves the server. One
//! that lands where markup means nothing or something else -- inside a tag,
//! or in a script, a style, a title or a textarea -- stops the render rather
//! than putting a pill there.

use std::collections::HashMap;
use std::sync::Mutex;

use minijinja::value::{Object, Value};
use minijinja::{context, Environment, Error, ErrorKind, State};

use crate::models::supporter::marks_for;

/// Where `render` puts the nonce in the page's context. A macro sees the
/// render's top-level context, however deeply it is imported
/// (`State::lookup`), so `supporter_slot` finds it from inside `handle()`.
const NONCE: &str = "__people_nonce";
/// The render's temp (`State::get_temp`) that the printed names are kept in.
const PRINTED: &str = "__people_printed";

/// How long a page waits for a connection to ask about its supporters.
const LOOKUP_WAIT: std::time::Duration = std::time::Duration::from_millis(500);

const OPEN: char = '\u{E000}';
const CLOSE: char = '\u{E001}';

/// The login names a render has printed a handle for, in order, each with
/// the text the pill will say.
#[derive(Debug, Default)]
struct Printed(Mutex<Vec<(String, String)>>);

impl Object for Printed {}

/// `supporter_slot(login_name, text)`, for person_macro.jinja's `handle()`:
/// records the name and returns the placeholder its pill will take.
/// Outside [`Templates`] -- a test rendering a template on its own -- there
/// is nothing to record into, and it returns none, which `handle()` answers
/// by drawing the pill without a mark.
pub fn supporter_slot(state: &State, login_name: String, text: String) -> Value {
    let Some(nonce) = state.lookup(NONCE) else {
        return Value::from(());
    };
    let printed = state.get_or_set_temp_object(PRINTED, Printed::default);
    let mut printed = printed.0.lock().unwrap();
    let index = printed.len();
    printed.push((login_name, text));
    Value::from_safe_string(format!("{OPEN}{nonce}:{index}{CLOSE}"))
}

/// Registers what `handle()` calls. Every environment the templates run in
/// needs it -- the server's and the tests'.
pub fn add_to_environment(env: &mut Environment<'_>) {
    env.add_function("supporter_slot", supporter_slot);
}

/// The server's templates. Clone is cheap: the environment's templates are
/// shared.
#[derive(Clone)]
pub struct Templates {
    env: Environment<'static>,
}

/// A page rendered and waiting for its pills.
struct Rendered {
    output: String,
    nonce: String,
    printed: Vec<(String, String)>,
}

impl Templates {
    pub fn new(env: Environment<'static>) -> Self {
        Templates { env }
    }

    /// Renders `name` with `ctx`, then asks who among the people it printed
    /// is supporting and draws their pills.
    pub async fn render(&self, db: &sqlx::PgPool, name: &str, ctx: Value) -> Result<String, Error> {
        let rendered = self.render_collecting(name, None, ctx)?;
        let marks = self.look_up(db, &rendered.printed).await;
        self.fill(rendered, &marks)
    }

    /// As [`render`](Self::render), for one block of `name`: what an htmx
    /// request that replaces that block asks for.
    pub async fn render_block(
        &self,
        db: &sqlx::PgPool,
        name: &str,
        block: &str,
        ctx: Value,
    ) -> Result<String, Error> {
        let rendered = self.render_collecting(name, Some(block), ctx)?;
        let marks = self.look_up(db, &rendered.printed).await;
        self.fill(rendered, &marks)
    }

    /// For what is rendered away from a request and its database -- a live
    /// event's markup, say -- and so has nobody to ask. Printing a handle
    /// there is an error, not a pill without its mark.
    pub fn render_without_people(&self, name: &str, ctx: Value) -> Result<String, Error> {
        let rendered = self.render_collecting(name, None, ctx)?;
        if !rendered.printed.is_empty() {
            return Err(Error::new(
                ErrorKind::InvalidOperation,
                format!("{name} printed a handle where no supporter can be asked about"),
            ));
        }
        self.fill(rendered, &HashMap::new())
    }

    /// The whole of [`render`](Self::render) but the query: for tests, which
    /// are handed the names the page printed, as the query would be, and say
    /// who among them is supporting.
    #[cfg(test)]
    pub fn render_with(
        &self,
        name: &str,
        ctx: Value,
        look_up: impl FnOnce(&[String]) -> HashMap<String, String>,
    ) -> Result<String, Error> {
        let rendered = self.render_collecting(name, None, ctx)?;
        let mut names: Vec<String> = rendered
            .printed
            .iter()
            .map(|(name, _)| name.clone())
            .collect();
        names.sort();
        names.dedup();
        let marks = look_up(&names);
        self.fill(rendered, &marks)
    }

    fn render_collecting(
        &self,
        name: &str,
        block: Option<&str>,
        ctx: Value,
    ) -> Result<Rendered, Error> {
        let nonce = uuid::Uuid::new_v4().simple().to_string();
        let ctx = context! { __people_nonce => &nonce, ..ctx };
        let template = self.env.get_template(name)?;
        let printed = |state: &State| {
            state
                .get_temp(PRINTED)
                .and_then(|value| value.downcast_object::<Printed>())
                .map(|printed| std::mem::take(&mut *printed.0.lock().unwrap()))
                .unwrap_or_default()
        };
        match block {
            None => {
                let captured = template.render_captured(ctx)?;
                let printed = printed(captured.state());
                Ok(Rendered {
                    output: captured.into_output(),
                    nonce,
                    printed,
                })
            }
            Some(block) => {
                // The whole template is evaluated first, into nothing; what
                // it printed on the way is not on the page, so it is
                // forgotten before the block is drawn.
                let mut captured = template.render_captured_to(ctx, std::io::sink())?;
                captured.with_state_mut(|state| {
                    printed(state);
                    let output = state.render_block(block)?;
                    Ok(Rendered {
                        output,
                        nonce,
                        printed: printed(state),
                    })
                })
            }
        }
    }

    /// The marks of the people printed. A lookup that fails costs the page
    /// its lime, not the page: the pills are drawn plain and the failure
    /// is reported.
    ///
    /// The connection is waited for only briefly. A handler that still holds
    /// a transaction while it renders has one connection already, and with
    /// the pool at its limit -- both colours are up during a deploy -- a
    /// request waiting indefinitely for a second could wait on itself.
    async fn look_up(
        &self,
        db: &sqlx::PgPool,
        printed: &[(String, String)],
    ) -> HashMap<String, String> {
        if printed.is_empty() {
            return HashMap::new();
        }
        let mut names: Vec<String> = printed.iter().map(|(name, _)| name.clone()).collect();
        names.sort();
        names.dedup();
        let mut connection = match tokio::time::timeout(LOOKUP_WAIT, db.acquire()).await {
            Ok(Ok(connection)) => connection,
            Ok(Err(e)) => {
                tracing::error!("no connection to look supporters up for a page: {e:#}");
                return HashMap::new();
            }
            Err(_) => {
                tracing::warn!("no connection within {LOOKUP_WAIT:?} to look supporters up");
                return HashMap::new();
            }
        };
        match marks_for(&mut *connection, &names).await {
            Ok(marks) => marks,
            Err(e) => {
                tracing::error!("supporters could not be looked up for a page: {e:#}");
                HashMap::new()
            }
        }
    }

    fn fill(&self, rendered: Rendered, marks: &HashMap<String, String>) -> Result<String, Error> {
        if rendered.printed.is_empty() {
            return Ok(rendered.output);
        }
        // The pill is person_macro.jinja's, drawn by its own macro.
        let person = self.env.get_template("person_macro.jinja")?;
        let person = person.render_captured(())?;
        let person = person.state();
        let pill = |index: usize| -> Result<String, Error> {
            let (login_name, text) = rendered.printed.get(index).ok_or_else(|| {
                Error::new(
                    ErrorKind::InvalidOperation,
                    "a handle's placeholder is out of range",
                )
            })?;
            let mark = marks.get(login_name).cloned();
            person.call_macro("pill", &[Value::from(mark), Value::from(text.as_str())])
        };
        replace_placeholders(&rendered.output, &rendered.nonce, pill)
    }
}

/// Where in a page a placeholder is, as far as putting markup there goes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Place {
    /// Between tags, where a pill is an element like any other.
    Text,
    /// Inside a tag: an attribute's value, or between its attributes.
    /// `raw` is the raw text element it opens, if it is one.
    Tag {
        quote: Option<u8>,
        raw: Option<&'static str>,
    },
    /// Inside an element whose content is not markup, until its end tag.
    Raw(&'static str),
    Comment,
}

/// The elements whose content the browser does not read as markup, or
/// reads only as text: a pill in any of them is not a pill.
const RAW: [&str; 4] = ["script", "style", "textarea", "title"];

fn starts_with_ignore_case(haystack: &[u8], needle: &str) -> bool {
    haystack.len() >= needle.len()
        && haystack[..needle.len()].eq_ignore_ascii_case(needle.as_bytes())
}

/// Replaces each placeholder carrying `nonce` with `pill(index)`, refusing
/// any that is not between tags. Everything else in the page -- a U+E000
/// someone typed included -- is passed through as it is.
///
/// Enough of HTML's tokenizer to tell text from a tag, a quoted attribute,
/// a comment and a raw text element's content. The page is ours, so this is
/// not a parser for any HTML at all: it is a guard against a template
/// putting `handle()` somewhere a pill does not belong.
fn replace_placeholders(
    output: &str,
    nonce: &str,
    mut pill: impl FnMut(usize) -> Result<String, Error>,
) -> Result<String, Error> {
    let opening = format!("{OPEN}{nonce}:");
    let bytes = output.as_bytes();
    let mut out = String::with_capacity(output.len() + output.len() / 8);
    let mut place = Place::Text;
    // Everything from `copied` up to `at` is still to be copied.
    let mut copied = 0;
    let mut at = 0;
    while at < bytes.len() {
        let rest = &bytes[at..];
        if rest.starts_with(opening.as_bytes()) {
            let after = &output[at + opening.len()..];
            let end = after.find(CLOSE).ok_or_else(|| {
                Error::new(
                    ErrorKind::InvalidOperation,
                    "a handle's placeholder is not closed",
                )
            })?;
            let index: usize = after[..end].parse().map_err(|_| {
                Error::new(
                    ErrorKind::InvalidOperation,
                    "a handle's placeholder is malformed",
                )
            })?;
            if place != Place::Text {
                return Err(Error::new(
                    ErrorKind::InvalidOperation,
                    format!(
                        "a handle was printed where a pill cannot go ({place:?}); \
                         an attribute takes handle_text()"
                    ),
                ));
            }
            out.push_str(&output[copied..at]);
            out.push_str(&pill(index)?);
            at += opening.len() + end + CLOSE.len_utf8();
            copied = at;
            continue;
        }
        let (next, step) = match place {
            Place::Text if rest.starts_with(b"<!--") => (Place::Comment, 4),
            Place::Text if rest[0] == b'<' => {
                let name = &rest[1..];
                let raw = RAW.into_iter().find(|raw| {
                    starts_with_ignore_case(name, raw)
                        && !name
                            .get(raw.len())
                            .is_some_and(|b| b.is_ascii_alphanumeric() || *b == b'-')
                });
                (Place::Tag { quote: None, raw }, 1)
            }
            Place::Comment if rest.starts_with(b"-->") => (Place::Text, 3),
            Place::Tag {
                quote: Some(q),
                raw,
            } if rest[0] == q => (Place::Tag { quote: None, raw }, 1),
            Place::Tag { quote: None, raw } if rest[0] == b'"' || rest[0] == b'\'' => (
                Place::Tag {
                    quote: Some(rest[0]),
                    raw,
                },
                1,
            ),
            Place::Tag { quote: None, raw } if rest[0] == b'>' => {
                (raw.map_or(Place::Text, Place::Raw), 1)
            }
            Place::Raw(name)
                if rest.starts_with(b"</") && starts_with_ignore_case(&rest[2..], name) =>
            {
                (
                    Place::Tag {
                        quote: None,
                        raw: None,
                    },
                    2,
                )
            }
            other => (other, 1),
        };
        place = next;
        at += step;
    }
    out.push_str(&output[copied..]);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const NONCE: &str = "n0nce";

    fn slot(index: usize) -> String {
        format!("{OPEN}{NONCE}:{index}{CLOSE}")
    }

    fn fill(page: &str) -> Result<String, Error> {
        replace_placeholders(page, NONCE, |index| Ok(format!("<pill {index}>")))
    }

    #[test]
    fn a_placeholder_between_tags_becomes_its_pill() {
        let page = format!("<p>by {} and {}</p>", slot(0), slot(1));
        assert_eq!(fill(&page).unwrap(), "<p>by <pill 0> and <pill 1></p>");
    }

    #[test]
    fn a_placeholder_in_a_tag_or_raw_text_is_refused() {
        for page in [
            format!(r#"<a title="{}">x</a>"#, slot(0)),
            format!(r#"<a title='{}'>x</a>"#, slot(0)),
            format!(r#"<a {}>x</a>"#, slot(0)),
            format!("<script>let who = '{}';</script>", slot(0)),
            format!("<SCRIPT type=module>{}</SCRIPT>", slot(0)),
            format!("<style>.a{{content:'{}'}}</style>", slot(0)),
            format!("<title>{}</title>", slot(0)),
            format!("<textarea>{}</textarea>", slot(0)),
            format!("<!-- {} -->", slot(0)),
        ] {
            assert!(fill(&page).is_err(), "{page}");
        }
    }

    /// A `>` inside a quoted attribute does not end the tag, and the end of
    /// a raw text element does, so what follows each is text again.
    #[test]
    fn text_resumes_where_html_says_it_does() {
        let page = format!(
            r#"<a title="a > b">x</a><script>if (a < b) {{}}</script><title>t</title><scripts-list></scripts-list>{}"#,
            slot(0)
        );
        assert!(fill(&page).unwrap().ends_with("<pill 0>"));
    }

    fn templates_with(page: &str) -> Templates {
        let mut env = crate::web::handlers::test_support::env();
        env.add_template_owned(
            "page.jinja",
            format!(r#"{{% from "person_macro.jinja" import handle, handle_text %}}{page}"#),
        )
        .unwrap();
        Templates::new(env)
    }

    /// The names asked about are the ones printed, and the pill a
    /// supporter's handle becomes is the lime one with their mark.
    #[test]
    fn a_page_asks_about_who_it_printed_and_draws_their_pills() {
        let templates = templates_with(
            r#"<p>{{ handle("fan") }} {{ handle("plain") }} {{ handle("fan") }}</p>"#,
        );
        let mut asked = Vec::new();
        let page = templates
            .render_with("page.jinja", context! {}, |names| {
                asked = names.to_vec();
                HashMap::from([("fan".to_string(), "steam".to_string())])
            })
            .unwrap();
        assert_eq!(asked, ["fan", "plain"]);
        assert_eq!(page.matches("ds-handle-supporter").count(), 2);
        assert!(page.contains(r#"<span class="ds-handle">@plain</span>"#));
        assert!(!page.contains(OPEN));
    }

    #[test]
    fn a_page_that_prints_nobody_asks_about_nobody() {
        let templates = templates_with("<p>nobody here</p>");
        let page = templates
            .render_with("page.jinja", context! {}, |names| {
                assert!(names.is_empty());
                HashMap::new()
            })
            .unwrap();
        assert_eq!(page, "<p>nobody here</p>");
    }

    /// A handle in an attribute stops the render; handle_text() is the one
    /// that goes there.
    #[test]
    fn a_handle_where_a_pill_cannot_go_stops_the_page() {
        let templates = templates_with(r#"<a title="{{ handle("fan") }}">x</a>"#);
        assert!(templates
            .render_with("page.jinja", context! {}, |_| HashMap::new())
            .is_err());
        let templates = templates_with(r#"<a title="{{ handle_text("fan") }}">x</a>"#);
        assert_eq!(
            templates
                .render_with("page.jinja", context! {}, |_| HashMap::new())
                .unwrap(),
            r#"<a title="@fan">x</a>"#
        );
    }

    /// A block is drawn after the whole template has been evaluated into
    /// nothing; only the handles in the block are asked about, and each of
    /// its placeholders finds its own name.
    #[test]
    fn a_block_asks_only_about_who_it_printed() {
        let templates = templates_with(
            r#"<p>{{ handle("outside") }}</p>{% block part %}<p>{{ handle("inside") }} {{ handle("fan") }}</p>{% endblock %}"#,
        );
        let rendered = templates
            .render_collecting("page.jinja", Some("part"), context! {})
            .unwrap();
        let names: Vec<&str> = rendered
            .printed
            .iter()
            .map(|(name, _)| name.as_str())
            .collect();
        assert_eq!(names, ["inside", "fan"]);
        let marks = HashMap::from([("fan".to_string(), "apple".to_string())]);
        let page = templates.fill(rendered, &marks).unwrap();
        assert!(page.starts_with(r#"<p><span class="ds-handle">@inside</span> <span class="ds-handle ds-handle-supporter"#));
        assert!(!page.contains("outside"));
    }

    /// Away from a request there is nobody to ask, and a handle printed
    /// there is an error rather than a pill quietly missing its mark.
    #[test]
    fn a_handle_printed_away_from_a_request_is_an_error() {
        let templates = templates_with(r#"<p>{{ handle("fan") }}</p>"#);
        assert!(templates
            .render_without_people("page.jinja", context! {})
            .is_err());
        let templates = templates_with("<p>a bell</p>");
        assert_eq!(
            templates
                .render_without_people("page.jinja", context! {})
                .unwrap(),
            "<p>a bell</p>"
        );
    }

    /// Text someone wrote cannot stand in for a placeholder without the
    /// nonce, which never leaves the server.
    #[test]
    fn a_placeholder_without_the_nonce_is_only_text() {
        let forged = format!("{OPEN}guess:0{CLOSE}");
        let page = format!("<p>{forged}</p>");
        assert_eq!(fill(&page).unwrap(), page);
    }
}
