//! How a person is addressed on a page.
//!
//! Someone from this site is `@login_name`; someone from another server is
//! `@name@their.host`, as their server gave it. Which one a row is by is a
//! fact about the row -- whether its actor belongs to one of our users --
//! and [`Handle`] is where that is decided, once, rather than in each
//! template from an `is_local` flag and a login name that may or may not be
//! there.

use serde::{Deserialize, Serialize};

/// A local account's login name: the `users.login_name` a profile lives at.
/// Not a remote handle, a community slug or a display name, which are all
/// strings too.
///
/// Serialized as the bare string, so a template or a JSON API sees what it
/// always has.
#[derive(
    Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, sqlx::Type,
)]
#[serde(transparent)]
#[sqlx(transparent)]
pub struct LoginName(String);

impl LoginName {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for LoginName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<String> for LoginName {
    fn from(login_name: String) -> Self {
        LoginName(login_name)
    }
}

impl From<&str> for LoginName {
    fn from(login_name: &str) -> Self {
        LoginName(login_name.to_string())
    }
}

impl PartialEq<str> for LoginName {
    fn eq(&self, other: &str) -> bool {
        self.0 == other
    }
}

impl PartialEq<&str> for LoginName {
    fn eq(&self, other: &&str) -> bool {
        self.0 == *other
    }
}

impl PartialEq<String> for LoginName {
    fn eq(&self, other: &String) -> bool {
        &self.0 == other
    }
}

/// A login name can be read as the string it is -- in a URL, a format, a
/// query's parameter -- but a string cannot be taken for a login name
/// without saying so (`LoginName::from`), which is the direction the type
/// is for.
impl std::ops::Deref for LoginName {
    type Target = str;

    fn deref(&self) -> &str {
        &self.0
    }
}

impl AsRef<str> for LoginName {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

/// Who a row is by, as a page prints them. A local person with no login
/// name, or a remote one with one, cannot be built.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Handle {
    Local(LoginName),
    /// `@name@their.host`, as stored in `actors.handle`. Its name is held to
    /// `actors_username_is_plain`, so it has no `@` of its own.
    Remote(String),
}

impl Handle {
    /// From an `actors` row and the `users.login_name` it joins to: an actor
    /// that belongs to one of our users is ours.
    pub fn of_actor(login_name: Option<LoginName>, actor_handle: String) -> Self {
        match login_name {
            Some(login_name) => Handle::Local(login_name),
            None => Handle::Remote(actor_handle),
        }
    }

    pub fn is_local(&self) -> bool {
        matches!(self, Handle::Local(_))
    }

    pub fn login_name(&self) -> Option<&LoginName> {
        match self {
            Handle::Local(login_name) => Some(login_name),
            Handle::Remote(_) => None,
        }
    }
}

/// To a template, `{login_name, name, host}`. `login_name` is there only for
/// a local person; `name` and `host` only for a remote one, split at the
/// last `@` so person_macro.jinja can keep the host in view.
impl Serialize for Handle {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let mut out = serializer.serialize_struct("Handle", 3)?;
        match self {
            Handle::Local(login_name) => {
                out.serialize_field("login_name", login_name)?;
                out.serialize_field("name", &None::<&str>)?;
                out.serialize_field("host", &None::<&str>)?;
            }
            Handle::Remote(handle) => {
                let (name, host) = handle
                    .trim_start_matches('@')
                    .rsplit_once('@')
                    .unwrap_or((handle.trim_start_matches('@'), ""));
                out.serialize_field("login_name", &None::<&str>)?;
                out.serialize_field("name", name)?;
                out.serialize_field("host", host)?;
            }
        }
        out.end()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn an_actor_with_a_user_is_ours_and_one_without_is_not() {
        assert_eq!(
            Handle::of_actor(
                Some("tandemaus".to_string().into()),
                "@tandemaus@oeee.cafe".into()
            ),
            Handle::Local("tandemaus".to_string().into())
        );
        assert_eq!(
            Handle::of_actor(None, "@far@example.social".into()),
            Handle::Remote("@far@example.social".into())
        );
    }

    #[test]
    fn a_template_is_told_which_it_is_and_where_a_remote_host_starts() {
        assert_eq!(
            serde_json::to_value(Handle::Local("tandemaus".to_string().into())).unwrap(),
            json!({"login_name": "tandemaus", "name": null, "host": null})
        );
        assert_eq!(
            serde_json::to_value(Handle::Remote("@far@example.social".into())).unwrap(),
            json!({"login_name": null, "name": "far", "host": "example.social"})
        );
    }

    #[test]
    fn a_login_name_is_its_string_to_a_template() {
        assert_eq!(
            serde_json::to_value(LoginName::from("oeee".to_string())).unwrap(),
            json!("oeee")
        );
    }
}
