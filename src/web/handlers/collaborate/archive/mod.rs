//! An append-only recording of a room's canonical stream.
//!
//! The live history in `redis_messages` is a working set, not a record. A
//! checkpoint squashes everything at or below its base into snapshots and the
//! room's TTL takes the rest, which is exactly what keeps a busy room cheap --
//! and it is why a session that lost its drawing could only be examined
//! through whatever happened to survive in Redis an hour later.
//!
//! So this keeps the other copy: every message that was ever given a sequence
//! number, in the order it was given one, with the connection that sent it and
//! the moment it arrived. Nothing removes from it.
//!
//! Two things will read it. A replay of a finished collaboration is this log
//! applied from the beginning; a post-mortem of one that went wrong is the
//! same log read rather than rendered. Neither is built here -- this is the
//! recording, and it is the part that has to exist before either is possible,
//! because a log that was not kept cannot be added afterwards.
//!
//! ## What is and is not in it
//!
//! Everything that passes through `sequence_and_publish`, which is every
//! message that enters canonical history and the RESET_POINT that marks a
//! checkpoint. Not the checkpoint snapshots themselves: those are written
//! straight into history by the reset script, they run to megabytes, and a log
//! that starts at sequence 1 can render the same pixels from the operations.
//! `first_seq` in the manifest is what says whether a given archive does start
//! at 1 -- a session already under way when archiving was deployed does not,
//! and a reader has to be able to tell.

use uuid::Uuid;

use crate::AppConfig;

mod format;
pub use format::*;
mod buffer;
pub use buffer::*;
mod write;
pub use write::*;
mod read;
pub use read::*;
mod diagnostics;
pub use diagnostics::*;

const ARCHIVE_PREFIX: &str = "oeee:archive:v1:";

const ARCHIVE_CLAIM_PREFIX: &str = "oeee:archive_claim:v1:";

const ARCHIVE_CHAT_PREFIX: &str = "oeee:archive_chat:v1:";

const ARCHIVE_BOUNDS_PREFIX: &str = "oeee:archive_bounds:v1:";

/// How long un-flushed entries wait in Redis before they are given up on.
///
/// Generous on purpose: this is the window in which a flush has to succeed,
/// and the cost of it being long is memory for a room nobody is in. A day
/// covers a Redis that was unreachable for an afternoon.
pub const ARCHIVE_BUFFER_TTL: u64 = 24 * 60 * 60;

/// Entries moved into one object. A chunk is written whole or not at all, so
/// this also bounds what one failed flush has to do again.
const FLUSH_BATCH: isize = 2048;

/// A sequence divisible by this asks the room to flush.
///
/// Only the connection that sent that message sees it, so the trigger fires
/// once per this many messages rather than once per participant. It is a
/// nudge, not a guarantee: the guarantees are the seal when a session ends and
/// the sweeper that finds rooms nobody ended.
pub const FLUSH_EVERY: u64 = 512;

const FLUSH_CLAIM_TTL: u64 = 120;

/// How long a seal waits for a flush already under way to finish, in
/// attempts of a quarter second. A flush is a few object writes; one that
/// takes longer than this is stuck, and the sweep will seal on its next pass.
const SEAL_CLAIM_ATTEMPTS: u32 = 40;

/// The most transcript lines kept for one session. The transcript is stored
/// whole on every flush, so an unbounded one would be re-uploaded in full
/// each time; a session's chat is far below this.
const MAX_CHAT_LINES: isize = 5000;

/// How many of a session's chunks are fetched at once.
///
/// A round trip to object storage is about forty milliseconds from the app
/// host, and a session is one chunk per five hundred messages -- fetched one
/// after another, a long afternoon's drawing would spend over a second in
/// nothing but waiting. Bounded rather than unbounded because the whole
/// recording is held in memory while it is assembled, and forty requests at
/// once buys nothing over eight.
const FETCH_CONCURRENCY: usize = 8;

/// Where a session's objects live, under the bucket the images already use.
const ARCHIVE_R2_PREFIX: &str = "collaborate-archive";

/// Where recordings go, or None when this deployment has not been given
/// anywhere private to put them.
///
/// Deliberately not defaulted to the image bucket: that one is served straight
/// to browsers from `r2_public_endpoint_url`, so a recording written there
/// would be readable by anyone holding the session's id. Recording nothing is
/// the right failure.
pub fn bucket(config: &AppConfig) -> Option<&str> {
    selected_bucket(config.archive_s3_bucket.as_deref())
}

/// A blank setting means the same as an absent one: a config written out with
/// the key empty is a deployment that has not chosen a bucket, not one that
/// has chosen the empty bucket.
fn selected_bucket(configured: Option<&str>) -> Option<&str> {
    configured.filter(|name| !name.is_empty())
}

/// Named by the sequencer, which appends to it in the same step it assigns a
/// position.
pub fn buffer_key(room_uuid: Uuid) -> String {
    format!("{}{}", ARCHIVE_PREFIX, room_uuid)
}

fn bounds_key(room_uuid: Uuid) -> String {
    format!("{}{}", ARCHIVE_BOUNDS_PREFIX, room_uuid)
}

fn chat_buffer_key(room_uuid: Uuid) -> String {
    format!("{}{}", ARCHIVE_CHAT_PREFIX, room_uuid)
}

fn archive_claim_key(room_uuid: Uuid) -> String {
    format!("{}{}", ARCHIVE_CLAIM_PREFIX, room_uuid)
}

type BufferResult<T> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

/// An error from storage, with the reason in it.
///
/// The SDK's own Display for a failed request is the two words "service
/// error"; what R2 actually said -- AccessDenied, NoSuchBucket -- is further
/// down the source chain, and printing the error with `{}` drops it. Every
/// place that logs or returns a storage failure goes through this.
pub fn describe(e: &(dyn std::error::Error + 'static)) -> String {
    aws_sdk_s3::error::DisplayErrorContext(e).to_string()
}

/// The bucket client, built the same way the image upload builds it.
pub fn s3_client(config: &AppConfig) -> aws_sdk_s3::Client {
    let credentials = aws_sdk_s3::config::Credentials::new(
        config.aws_access_key_id.clone(),
        config.aws_secret_access_key.clone(),
        None,
        None,
        "",
    );
    let s3_config = aws_sdk_s3::Config::builder()
        .endpoint_url(config.r2_endpoint_url.clone())
        .region(aws_sdk_s3::config::Region::new(config.aws_region.clone()))
        .credentials_provider(aws_sdk_s3::config::SharedCredentialsProvider::new(
            credentials,
        ))
        .behavior_version_latest()
        .build();
    aws_sdk_s3::Client::from_conf(s3_config)
}

#[cfg(test)]
mod tests;
