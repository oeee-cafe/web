//! Federation: what this site says to other ActivityPub servers and what it
//! hears from them. The deserialisers here are shared by the activities, which
//! accept either shape other implementations send.

use activitypub_federation::fetch::object_id::ObjectId;

use serde::Deserialize;
use url::Url;

use crate::models::actor::Actor;

mod actor;
pub use actor::*;
mod routes;
pub use routes::*;
mod following;
pub use following::*;
mod note;
pub use note::*;
mod outbox;
pub use outbox::*;
mod reactions;
pub use reactions::*;

/// An http or https address with a host: the only kind of link a remote
/// actor's profile is allowed to be.
fn is_web_address(url: &Url) -> bool {
    matches!(url.scheme(), "http" | "https") && url.host_str().is_some_and(|host| !host.is_empty())
}

// Custom deserializers for flexible ActivityPub field formats
fn string_or_vec_deser<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde_json::Value;
    let v = Value::deserialize(deserializer)?;
    match v {
        Value::String(s) => Ok(vec![s]),
        Value::Array(arr) => {
            let mut result = Vec::new();
            for item in arr {
                if let Value::String(s) = item {
                    result.push(s);
                }
            }
            Ok(result)
        }
        _ => Ok(Vec::new()),
    }
}

fn actor_from_signature_deser<'de, D>(deserializer: D) -> Result<Option<ObjectId<Actor>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    // First try to deserialize as a direct actor field
    match ObjectId::<Actor>::deserialize(deserializer) {
        Ok(actor_id) => Ok(Some(actor_id)),
        Err(_) => {
            // If that fails, return None and we'll try to extract from signature elsewhere
            Ok(None)
        }
    }
}

fn content_or_contents_deser<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde_json::Value;
    let v = Value::deserialize(deserializer)?;
    match v {
        Value::String(s) => Ok(Some(s)),
        Value::Array(arr) => {
            // Take the first string from the array if available
            for item in arr {
                if let Value::String(s) = item {
                    return Ok(Some(s));
                }
            }
            Ok(None)
        }
        _ => Ok(None),
    }
}

fn tag_or_vec_deser<'de, D>(deserializer: D) -> Result<Vec<Tag>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde_json::Value;
    let v = Value::deserialize(deserializer)?;
    match v {
        Value::Object(_) => {
            // Single tag object
            let tag: Tag = serde_json::from_value(v).map_err(serde::de::Error::custom)?;
            Ok(vec![tag])
        }
        Value::Array(arr) => {
            // Array of tag objects
            let mut result = Vec::new();
            for item in arr {
                if let Ok(tag) = serde_json::from_value::<Tag>(item) {
                    result.push(tag);
                }
            }
            Ok(result)
        }
        _ => Ok(Vec::new()),
    }
}

#[cfg(test)]
mod web_address_tests {
    use super::is_web_address;
    use url::Url;

    /// A remote actor's profile link is an href on our pages: only a web
    /// address may be one.
    #[test]
    fn only_a_web_address_is_a_profile_link() {
        for good in [
            "https://example.social/@far",
            "http://example.social/users/far",
        ] {
            assert!(is_web_address(&Url::parse(good).unwrap()), "{good}");
        }
        for bad in [
            "javascript:alert(document.cookie)",
            "data:text/html,<script>alert(1)</script>",
            "vbscript:msgbox(1)",
            "file:///etc/passwd",
            "mailto:far@example.social",
        ] {
            assert!(!is_web_address(&Url::parse(bad).unwrap()), "{bad}");
        }
    }
}
