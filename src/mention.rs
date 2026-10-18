//! `@login_name` in what someone wrote: where each one is, for the comment
//! it notifies (handlers/post/comment.rs) and the link it is drawn as
//! (`comment_markdown`, main.rs).
//!
//! A mention is an `@` that does not continue a word -- `mail@example.com`
//! names nobody -- followed by what a login name may be made of
//! (`CK_users_login_name_RegularExpression`: ASCII letters, digits, `_` and
//! `-`, at most 50). `@name@their.host` is somebody on another server, not
//! the `name` here, and is left alone.

use std::ops::Range;

/// The longest login name there is.
const MAX_LOGIN_NAME: usize = 50;

fn is_name_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'-'
}

/// Each mention in `text`: the byte range of the `@` and the name, which
/// the name alone is `text[range.start + 1..range.end]` of.
pub fn spans(text: &str) -> Vec<Range<usize>> {
    let bytes = text.as_bytes();
    let mut found = Vec::new();
    let mut at = 0;
    while at < bytes.len() {
        if bytes[at] != b'@' {
            at += 1;
            continue;
        }
        // An `@` inside a word (an address, `a@b`) or after another `@`.
        let continues_word = text[..at]
            .chars()
            .next_back()
            .is_some_and(|c| c.is_alphanumeric() || c == '_' || c == '-' || c == '@' || c == '.');
        let start = at;
        let mut end = at + 1;
        while end < bytes.len() && is_name_byte(bytes[end]) {
            end += 1;
        }
        let name_len = end - start - 1;
        // Somebody elsewhere: `@name@host`, which the host goes with.
        let elsewhere = bytes.get(end) == Some(&b'@')
            && bytes
                .get(end + 1)
                .is_some_and(|b| b.is_ascii_alphanumeric());
        if !continues_word && name_len > 0 && name_len <= MAX_LOGIN_NAME && !elsewhere {
            found.push(start..end);
        }
        // Past the name and whatever was joined to it, so `@a@b` is not
        // read again from its second `@`.
        at = end.max(at + 1);
        while at < bytes.len()
            && (is_name_byte(bytes[at]) || bytes[at] == b'@' || bytes[at] == b'.')
        {
            at += 1;
        }
    }
    found
}

/// The login names `text` mentions, each once, in the order they first
/// appear.
pub fn login_names(text: &str) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    for span in spans(text) {
        let name = &text[span.start + 1..span.end];
        if !names.iter().any(|seen| seen == name) {
            names.push(name.to_string());
        }
    }
    names
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_mention_is_an_at_and_a_login_name() {
        assert_eq!(
            login_names("hi @neo, and @tandem_maus-2!"),
            ["neo", "tandem_maus-2"]
        );
        assert_eq!(login_names("@neo"), ["neo"]);
        assert_eq!(login_names("(@neo)"), ["neo"]);
        assert_eq!(login_names("그림 @neo 좋아요"), ["neo"]);
    }

    #[test]
    fn each_name_once_in_the_order_written() {
        assert_eq!(login_names("@b @a @b @a"), ["b", "a"]);
    }

    #[test]
    fn an_address_or_a_handle_elsewhere_names_nobody_here() {
        assert!(login_names("write to mail@example.com").is_empty());
        assert!(login_names("그림@neo").is_empty());
        assert!(login_names("@neo@mastodon.social").is_empty());
        assert!(login_names("@@neo").is_empty());
        assert!(login_names("@ neo").is_empty());
        // A name ends where a login name could not go on; what follows is
        // only punctuation, not a host.
        assert_eq!(login_names("thanks @neo@"), ["neo"]);
        assert_eq!(login_names("@neo."), ["neo"]);
    }

    #[test]
    fn longer_than_any_login_name_is_not_one() {
        let long = format!("@{}", "a".repeat(51));
        assert!(login_names(&long).is_empty());
        let longest = format!("@{}", "a".repeat(50));
        assert_eq!(login_names(&longest).len(), 1);
    }

    #[test]
    fn spans_cover_the_at_and_the_name() {
        let text = "a @neo b";
        let spans = spans(text);
        assert_eq!(spans.len(), 1);
        assert_eq!(&text[spans[0].clone()], "@neo");
    }
}
