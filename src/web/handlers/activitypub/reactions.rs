//! Likes and emoji reactions, from other servers.

use serde::{Deserialize, Serialize};
use url::Url;

use super::{actor_from_signature_deser, string_or_vec_deser};

#[derive(Deserialize, Serialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct Like {
    pub actor: Url,
    #[serde(rename = "object")]
    pub object: Url,
    #[serde(rename = "type")]
    pub r#type: String,
    pub id: Url,
    #[serde(default)]
    pub to: Vec<String>,
    #[serde(default)]
    pub cc: Vec<String>,
}

#[derive(Deserialize, Serialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct EmojiReact {
    #[serde(
        skip_serializing_if = "Option::is_none",
        deserialize_with = "actor_from_signature_deser"
    )]
    pub actor: Option<Url>,
    #[serde(rename = "object")]
    pub object: Url,
    pub content: String,
    #[serde(rename = "type")]
    pub r#type: String,
    pub id: Url,
    #[serde(default, deserialize_with = "string_or_vec_deser")]
    pub to: Vec<String>,
    #[serde(default, deserialize_with = "string_or_vec_deser")]
    pub cc: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signature: Option<serde_json::Value>,
}
