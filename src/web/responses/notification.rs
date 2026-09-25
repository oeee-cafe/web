use serde::Serialize;

/// Response for the number on the bell: unread notifications and pending
/// invitations together.
#[derive(Serialize, Debug)]
pub struct UnreadCountResponse {
    pub count: i64,
}
