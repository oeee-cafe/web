//! What someone is doing on a page, for their Steam friends to see.
//!
//! The Oeee Cafe app on Steam reads this from `<meta name="oeee-presence">`
//! on each page it loads and hands it to Steam as rich presence ("Drawing in
//! 오이카페 모에화"). The site only says what the page is; the words are
//! Steam's, from the localisation file uploaded with the app
//! (`steam/rich_presence.vdf` in oeee-cafe-desktop). A page without the tag
//! is browsing.
//!
//! Only a public community is named. Rich presence is shown to every one of
//! a player's Steam friends, who are not the community's members.

use serde::Serialize;
use uuid::Uuid;

use crate::models::community::{Community, CommunityVisibility};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Activity {
    Drawing,
    /// Drawing onto someone else's drawing: a relay.
    Relaying,
    DrawingBanner,
    Collaborating,
    WatchingReplay,
}

#[derive(Clone, Debug, Serialize)]
pub struct Presence {
    pub activity: Activity,
    /// The community's name, when the community is public.
    pub community: Option<String>,
    /// The same for everyone in one collaborative room, so Steam shows
    /// friends drawing together as a group. Derived from the room's id rather
    /// than the id itself: a room's id is what joins it.
    pub group: Option<String>,
}

impl Presence {
    pub fn new(activity: Activity) -> Self {
        Self {
            activity,
            community: None,
            group: None,
        }
    }

    pub fn in_community(mut self, community: Option<&Community>) -> Self {
        self.community = community.and_then(public_name);
        self
    }

    pub fn in_room(mut self, session_id: Uuid) -> Self {
        let digest = sha256::digest(format!("oeee-presence:{session_id}"));
        self.group = Some(digest[..16].to_string());
        self
    }
}

/// A community's name, if it is one anybody may see.
pub fn public_name(community: &Community) -> Option<String> {
    (community.visibility == CommunityVisibility::Public).then(|| community.name.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_room_is_grouped_without_giving_its_id_away() {
        let id = Uuid::parse_str("9c881320-2b43-4afa-b2bb-7128c8a3e985").unwrap();
        let a = Presence::new(Activity::Collaborating)
            .in_room(id)
            .group
            .unwrap();
        let b = Presence::new(Activity::Collaborating)
            .in_room(id)
            .group
            .unwrap();
        assert_eq!(a, b);
        assert_eq!(a.len(), 16);
        assert!(!a.contains("9c881320"));
        let other = Presence::new(Activity::Collaborating)
            .in_room(Uuid::new_v4())
            .group
            .unwrap();
        assert_ne!(a, other);
    }

    #[test]
    fn activities_are_named_as_the_app_reads_them() {
        assert_eq!(
            serde_json::to_value(Activity::DrawingBanner).unwrap(),
            "drawing-banner"
        );
        assert_eq!(
            serde_json::to_value(Activity::WatchingReplay).unwrap(),
            "watching-replay"
        );
    }
}
