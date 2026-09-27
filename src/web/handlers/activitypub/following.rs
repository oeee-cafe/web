//! Following, and taking it back.

use activitystreams_kinds::activity::{AcceptType, FollowType, UndoType};
use serde::{Deserialize, Serialize};
use url::Url;

use super::{EmojiReact, Like};

#[derive(Deserialize, Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Follow {
    pub(crate) actor: Url,
    pub(crate) object: Url,
    #[serde(rename = "type")]
    r#type: FollowType,
    id: Url,
}

impl Follow {
    pub fn new(actor: Url, object: Url, id: Url) -> Follow {
        Follow {
            actor,
            object,
            r#type: Default::default(),
            id,
        }
    }
}

#[derive(Deserialize, Serialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Accept {
    actor: Url,
    object: Follow,
    r#type: AcceptType,
    id: Url,
}

impl Accept {
    pub fn new(actor: Url, object: Follow, id: Url) -> Accept {
        Accept {
            actor,
            object,
            r#type: Default::default(),
            id,
        }
    }
}

#[derive(Deserialize, Serialize, Debug, Clone)]
#[serde(untagged)]
pub enum UndoObject {
    Follow(Box<Follow>),
    Like(Box<Like>),
    EmojiReact(Box<EmojiReact>),
}

#[derive(Deserialize, Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Undo {
    pub actor: Url,
    pub object: UndoObject,
    pub r#type: UndoType,
    pub id: Url,
}

impl Undo {
    pub fn new(actor: Url, object: Follow, id: Url) -> Undo {
        Undo {
            actor,
            object: UndoObject::Follow(Box::new(object)),
            r#type: Default::default(),
            id,
        }
    }
}
