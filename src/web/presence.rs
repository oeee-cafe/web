//! What someone is doing on a page, for their Steam friends to see.
//!
//! Each page says it in `<meta name="oeee-presence">`, which the page passes
//! to the app in its `page` message (`presence`, `community` and `group`,
//! app_bridge.jinja), and the Oeee Cafe app on Steam hands it to Steam as
//! rich presence ("Drawing in 오이카페 모에화"). The site only says what the page is; the words are
//! Steam's, from the localisation file uploaded with the app
//! (`steam/rich_presence.vdf` in oeee-cafe-desktop). A page without the tag
//! is browsing.
//!
//! Only a public community is named. Rich presence is shown to every one of
//! a player's Steam friends, who are not the community's members.
//!
//! A collaborative room also says how to join it, for Discord: the Oeee Cafe
//! app on Windows hands it over as the join secret of the player's activity,
//! which Discord gives only to a friend the player invited or let in, and
//! never shows. So unlike `group` it may be the room's own address.

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
    /// Where a friend asked in goes: the room's path. None for a room a
    /// friend could not enter (`joinable`).
    pub join: Option<String>,
    /// How many may be in the room at once, beside `join`.
    pub seats: Option<i32>,
}

impl Presence {
    pub fn new(activity: Activity) -> Self {
        Self {
            activity,
            community: None,
            group: None,
            join: None,
            seats: None,
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

    /// Lets friends be asked into the room, when anyone signed in may enter
    /// it: not one in a private community, whose members alone may
    /// (`viewer_may_enter`), and who a player's friends mostly are not.
    pub fn joinable(mut self, session_id: Uuid, seats: i32, community: Option<&Community>) -> Self {
        if community.is_some_and(|c| c.visibility == CommunityVisibility::Private) {
            return self;
        }
        self.join = Some(join_path(session_id));
        self.seats = Some(seats);
        self
    }
}

/// What `join` says for a room.
fn join_path(session_id: Uuid) -> String {
    format!("/collaborate/{session_id}")
}

/// The room a `join` names, for a friend Discord sent here with it: it comes
/// back from whoever passed it on, so nothing but a room's path is a room.
pub fn joined_room(join: &str) -> Option<Uuid> {
    let id = Uuid::parse_str(join.strip_prefix("/collaborate/")?).ok()?;
    (join_path(id) == join).then_some(id)
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

    fn community(visibility: CommunityVisibility) -> Community {
        Community {
            id: Uuid::new_v4(),
            owner_id: Uuid::new_v4(),
            name: "오이카페".into(),
            slug: "oeee".into(),
            description: String::new(),
            visibility,
            updated_at: chrono::Utc::now(),
            created_at: chrono::Utc::now(),
            background_color: None,
            foreground_color: None,
        }
    }

    #[test]
    fn a_room_anyone_may_enter_says_how_to_join_it() {
        let id = Uuid::parse_str("9c881320-2b43-4afa-b2bb-7128c8a3e985").unwrap();
        for c in [
            None,
            Some(community(CommunityVisibility::Public)),
            Some(community(CommunityVisibility::Unlisted)),
        ] {
            let room = Presence::new(Activity::Collaborating).joinable(id, 4, c.as_ref());
            assert_eq!(
                room.join.as_deref(),
                Some("/collaborate/9c881320-2b43-4afa-b2bb-7128c8a3e985")
            );
            assert_eq!(room.seats, Some(4));
        }
        let private = community(CommunityVisibility::Private);
        let room = Presence::new(Activity::Collaborating).joinable(id, 4, Some(&private));
        assert_eq!((room.join, room.seats), (None, None));
    }

    #[test]
    fn only_a_rooms_own_path_is_a_room() {
        let id = Uuid::parse_str("9c881320-2b43-4afa-b2bb-7128c8a3e985").unwrap();
        let joinable = Presence::new(Activity::Collaborating).joinable(id, 4, None);
        assert_eq!(joined_room(&joinable.join.unwrap()), Some(id));
        for join in [
            "",
            "/collaborate/",
            "9c881320-2b43-4afa-b2bb-7128c8a3e985",
            "/collaborate/9C881320-2B43-4AFA-B2BB-7128C8A3E985",
            "/collaborate/9c8813202b434afab2bb7128c8a3e985",
            "/collaborate/{9c881320-2b43-4afa-b2bb-7128c8a3e985}",
            "/collaborate/9c881320-2b43-4afa-b2bb-7128c8a3e985/ws",
            "https://example.com/collaborate/9c881320-2b43-4afa-b2bb-7128c8a3e985",
            "//example.com",
        ] {
            assert_eq!(joined_room(join), None, "{join}");
        }
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
