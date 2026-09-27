//! Session resets: asking a client for a checkpoint, taking its upload, and
//! squashing history under it.

use axum::extract::ws::Message;
use tracing::{debug, error, info, warn};

use crate::web::handlers::collaborate::{messages, redis_messages, utils};

use super::{SessionContext, RESET_THRESHOLD_MESSAGES};

// An in-progress session reset upload from this connection: `count` snapshot
// messages follow a RESET_BEGIN and replace all history up to `base_seq`.
pub(super) struct PendingReset {
    base_seq: u64,
    pub(super) remaining: u16,
    payloads: Vec<Vec<u8>>,
    /// What `payloads` weighs, against `MAX_CHECKPOINT_BYTES`.
    bytes: usize,
    /// False when this connection was not the one asked for the checkpoint.
    /// The snapshots are still counted off the wire -- see `parse_reset_begin`
    /// -- and then dropped.
    pub(super) accepted: bool,
}

impl PendingReset {
    /// Takes one snapshot off the wire. Returns true the moment the upload
    /// has grown past what a checkpoint may weigh: from then on it is
    /// counted off and dropped like an unselected uploader's, so one
    /// connection cannot make the process hold 510 snapshots of 4 MiB.
    pub(super) fn take_snapshot(&mut self, data: Vec<u8>) -> bool {
        if !self.accepted {
            return false;
        }
        self.bytes += data.len();
        if self.bytes as u64 > redis_messages::MAX_CHECKPOINT_BYTES {
            self.accepted = false;
            self.payloads = Vec::new();
            return true;
        }
        self.payloads.push(data);
        false
    }
}

/// The position and snapshot count a RESET_BEGIN announces, if it announces a
/// checkpoint that could exist.
///
/// A checkpoint is one background and one foreground per participant who has
/// drawn, so the count is even, non-zero, and bounded by the session user id
/// space. The pairing itself -- which participant each snapshot belongs to --
/// is checked against the payloads once they arrive, in `valid_reset_payloads`.
///
/// None means the count cannot be trusted, and a count that cannot be trusted
/// is worse than useless: everything after a RESET_BEGIN is read as part of the
/// checkpoint until the count runs out, so a wrong one either swallows ordinary
/// drawing or lets snapshots loose into history.
pub(in crate::web::handlers::collaborate) fn reset_snapshot_count(
    data: &[u8],
) -> Option<(u64, u16)> {
    if data.len() < 11 {
        return None;
    }
    let base_seq = utils::read_u64_le(data, 1);
    let count = u16::from_le_bytes([data[9], data[10]]);
    if count == 0 || count % 2 != 0 || count > 2 * u16::from(u8::MAX) {
        return None;
    }
    Some((base_seq, count))
}

pub(super) async fn parse_reset_begin(
    data: &[u8],
    ctx: &SessionContext<'_>,
) -> Option<PendingReset> {
    let Some((base_seq, count)) = reset_snapshot_count(data) else {
        warn!(
            "Rejecting an unusable RESET_BEGIN from connection {} in room {}",
            ctx.connection_id, ctx.room_uuid
        );
        return None;
    };
    let accepted = match ctx
        .state
        .redis_state
        .is_reset_uploader(ctx.room_uuid, ctx.connection_id)
        .await
    {
        Ok(accepted) => accepted,
        Err(e) => {
            error!(
                "Could not authorize reset uploader {}: {}",
                ctx.connection_id, e
            );
            false
        }
    };

    if accepted {
        info!(
            "Session reset upload started for room {} at seq {} ({} snapshots)",
            ctx.room_uuid, base_seq, count
        );
    } else {
        // The snapshots are coming whether we asked for them or not: a client
        // that predates the query phase reads the query as an instruction and
        // starts uploading, and a client that lost the race to answer may
        // already be under way. They are counted off the wire and dropped
        // here, because a snapshot the server does not recognise as part of a
        // checkpoint is still a valid message -- it would be sequenced into
        // history as an ordinary one and stamp the uploader's canvas over
        // everybody's.
        warn!(
            "Discarding a {}-snapshot upload from unselected connection {} in room {}",
            count, ctx.connection_id, ctx.room_uuid
        );
    }

    Some(PendingReset {
        base_seq,
        remaining: count,
        payloads: Vec::with_capacity(if accepted { count as usize } else { 0 }),
        bytes: 0,
        accepted,
    })
}

pub(super) async fn finish_reset(ctx: &SessionContext<'_>, reset: PendingReset) {
    if !valid_reset_payloads(&reset.payloads) {
        warn!(
            "Rejecting malformed reset snapshots from connection {} in room {}",
            ctx.connection_id, ctx.room_uuid
        );
        let _ = ctx
            .state
            .redis_state
            .clear_reset_pending(ctx.room_uuid)
            .await;
        return;
    }
    let redis_store = redis_messages::RedisMessageStore::new(ctx.state.redis_pool.clone());
    match redis_store
        .apply_reset(ctx.room_uuid, reset.base_seq, &reset.payloads)
        .await
    {
        Ok(()) => {
            info!(
                "Session reset applied for room {} at seq {} ({} snapshots)",
                ctx.room_uuid,
                reset.base_seq,
                reset.payloads.len()
            );

            // Tell all clients (and future late joiners, via history) that
            // everything at or below base_seq is squashed into the reset
            // snapshots, so they can freeze undo state and reclaim memory.
            let reset_point =
                messages::reset_point_frame(reset.base_seq, reset.payloads.len() as u16);
            if let Err(e) = messages::sequence_and_broadcast(
                &Message::Binary(reset_point.into()),
                ctx.room_uuid,
                "system",
                ctx.state,
            )
            .await
            {
                error!(
                    "Failed to broadcast reset point for room {}: {}",
                    ctx.room_uuid, e
                );
            }
        }
        Err(e) => {
            error!(
                "Failed to apply session reset for room {}: {}",
                ctx.room_uuid, e
            );
        }
    }

    if let Err(e) = ctx
        .state
        .redis_state
        .clear_reset_pending(ctx.room_uuid)
        .await
    {
        error!(
            "Failed to clear reset-pending flag for room {}: {}",
            ctx.room_uuid, e
        );
    }
}

/// A reset checkpoint describes every participant's layer pair, so it carries
/// one background and one foreground per participant who has drawn -- not the
/// two of a shared canvas. Each snapshot names its owner in the user byte.
pub(in crate::web::handlers::collaborate) fn valid_reset_payloads(payloads: &[Vec<u8>]) -> bool {
    if payloads.is_empty() || payloads.len() % 2 != 0 {
        return false;
    }
    let mut seen: std::collections::HashMap<u8, u8> = std::collections::HashMap::new();
    for payload in payloads {
        if payload.len() < 4 || payload[0] != messages::MessageType::Snapshot as u8 {
            return false;
        }
        // [type][author][target owner][layer]...: one client uploads the whole
        // canvas, so the author is the same throughout and the owner byte is
        // what says whose pair each snapshot is.
        let (owner, layer) = (payload[2], payload[3]);
        if layer > 1 {
            return false;
        }
        let bit = 1u8 << layer;
        let held = seen.entry(owner).or_insert(0);
        // The same layer twice for one participant would leave the checkpoint
        // ambiguous about which copy is current.
        if *held & bit != 0 {
            return false;
        }
        *held |= bit;
    }
    seen.values().all(|held| *held == 0b11)
}

/// Asks the room for a checkpoint once it has drawn enough on top of the last
/// one to be worth replacing.
///
/// `size` is measured by the sequencer, in the same script that stored the
/// message it describes -- this is on the path of every drawing message, and a
/// separate read to answer a question that is almost always "no" cost the room
/// a round trip per mark.
pub(super) async fn maybe_request_reset(
    ctx: &SessionContext<'_>,
    size: redis_messages::HistorySize,
) {
    // Two meters, both counted from the last checkpoint rather than from
    // nothing, because a checkpoint is most of what a busy room's history
    // weighs and an absolute threshold would be over the moment one landed.
    //
    // Bytes for what a late joiner has to be sent, messages for what it then
    // has to apply: our operations run from a two-byte undo point to a
    // half-megabyte region of pixels, so neither meter stands in for the other.
    let over_bytes = size.bytes > redis_messages::effective_auto_reset_bytes(size.base_bytes);
    let over_messages = size.messages_since_reset() >= RESET_THRESHOLD_MESSAGES;
    if !over_bytes && !over_messages {
        return;
    }

    match ctx
        .state
        .redis_state
        .try_open_reset_query(ctx.room_uuid)
        .await
    {
        Ok(true) => {}
        Ok(false) => return, // a query or an upload is already in flight
        Err(e) => {
            error!(
                "Failed to open a checkpoint query for room {}: {}",
                ctx.room_uuid, e
            );
            return;
        }
    }

    info!(
        "Room {} has added {} messages and {} bytes since its last checkpoint - asking for a new one",
        ctx.room_uuid,
        size.messages_since_reset(),
        size.bytes_since_reset()
    );
    messages::send_reset_request(ctx.room_uuid, None, messages::ResetPhase::Query, ctx.state).await;
}

/// Asks again regardless of what is already outstanding, for the room that has
/// run out of history and cannot draw until somebody checkpoints it.
pub(super) async fn force_reset_request(ctx: &SessionContext<'_>) {
    match ctx
        .state
        .redis_state
        .reopen_reset_query(ctx.room_uuid)
        .await
    {
        // A query is already out and unanswered; asking again would only add
        // to what the room cannot deliver.
        Ok(false) => return,
        Ok(true) => {}
        Err(e) => {
            error!(
                "Failed to reopen the checkpoint query for room {}: {}",
                ctx.room_uuid, e
            );
            return;
        }
    }
    messages::send_reset_request(ctx.room_uuid, None, messages::ResetPhase::Query, ctx.state).await;
}

/// A client says it is caught up and able to upload the checkpoint. The first
/// one to say so gets it; the rest are told nothing and do nothing.
pub(super) async fn handle_reset_offer(ctx: &SessionContext<'_>) {
    match ctx
        .state
        .redis_state
        .claim_reset_upload(ctx.room_uuid, ctx.connection_id)
        .await
    {
        Ok(true) => {
            info!(
                "Connection {} (user {}) volunteered to checkpoint room {}",
                ctx.connection_id, ctx.user_login_name, ctx.room_uuid
            );
            messages::send_reset_request(
                ctx.room_uuid,
                Some(ctx.connection_id),
                messages::ResetPhase::Upload,
                ctx.state,
            )
            .await;
        }
        Ok(false) => debug!(
            "Connection {} offered to checkpoint room {}, but the job was taken",
            ctx.connection_id, ctx.room_uuid
        ),
        Err(e) => error!(
            "Failed to claim the checkpoint for room {}: {}",
            ctx.room_uuid, e
        ),
    }
}

#[cfg(test)]
mod reset_upload_tests {
    use super::PendingReset;
    use crate::web::handlers::collaborate::protocol::MAX_SNAPSHOT_BYTES;
    use crate::web::handlers::collaborate::redis_messages::MAX_CHECKPOINT_BYTES;

    fn upload(accepted: bool) -> PendingReset {
        PendingReset {
            base_seq: 7,
            remaining: 510,
            payloads: Vec::new(),
            bytes: 0,
            accepted,
        }
    }

    /// An accepted upload holding the largest checkpoint there can be, every
    /// snapshot of which was taken without complaint.
    fn filled() -> PendingReset {
        let mut reset = upload(true);
        let snapshot = vec![0u8; MAX_SNAPSHOT_BYTES];
        let fits = (MAX_CHECKPOINT_BYTES as usize) / MAX_SNAPSHOT_BYTES;
        for _ in 0..fits {
            assert!(!reset.take_snapshot(snapshot.clone()));
        }
        assert_eq!(reset.payloads.len(), fits);
        reset
    }

    #[test]
    fn keeps_a_checkpoint_of_the_largest_legal_size() {
        assert!(filled().accepted);
    }

    #[test]
    fn drops_the_upload_the_moment_it_outweighs_a_checkpoint() {
        let mut reset = filled();
        let snapshot = vec![0u8; MAX_SNAPSHOT_BYTES];
        assert!(reset.take_snapshot(vec![0u8; 1]));
        assert!(!reset.accepted);
        // Nothing is held for an upload that will not be applied, and the
        // snapshots that follow are counted off without being kept.
        assert!(reset.payloads.is_empty());
        assert!(!reset.take_snapshot(snapshot));
        assert!(reset.payloads.is_empty());
    }

    #[test]
    fn an_unselected_upload_is_never_kept() {
        let mut reset = upload(false);
        assert!(!reset.take_snapshot(vec![0u8; 64]));
        assert!(reset.payloads.is_empty());
        assert_eq!(reset.bytes, 0);
    }
}
