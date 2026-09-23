use serde::Serialize;

/// Response for unread notification count
#[derive(Serialize, Debug)]
pub struct UnreadCountResponse {
    pub count: i64,
}
