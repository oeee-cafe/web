//! Reading a recording back: the whole of it, its tail, its manifest and chat.

use futures_util::{StreamExt, TryStreamExt};
use tracing::warn;
use uuid::Uuid;

use crate::web::state::AppState;

use super::{
    bucket, buffered_chat, chat_key, decompress_off_thread, encode_runs, flush_room, manifest_key,
    s3_client, ArchiveBuffer, ARCHIVE_R2_PREFIX, CHUNK_SUFFIX, CHUNK_SUFFIX_PLAIN,
    FETCH_CONCURRENCY,
};

/// The transcript, for the viewer and for staff reading one back.
///
/// From the Redis buffer while it lasts, since that holds every line -- it is
/// never trimmed as it is written out -- including the ones said since the
/// last flush. Read there rather than flushed first: the inspector asks every
/// few seconds while it follows a live room, and a flush per ask would write a
/// sliver of a chunk per ask. Storage is the answer once the buffer expires.
pub async fn read_chat(
    state: &AppState,
    room_uuid: Uuid,
) -> Result<Vec<serde_json::Value>, Box<dyn std::error::Error + Send + Sync>> {
    let Some(bucket) = bucket(&state.config) else {
        return Ok(Vec::new());
    };
    match buffered_chat(state, room_uuid).await {
        Ok(held) if !held.is_empty() => return Ok(held),
        Ok(_) => {}
        Err(e) => warn!(
            "Failed to read the buffered transcript for room {}: {}",
            room_uuid, e
        ),
    }
    let object = s3_client(&state.config)
        .get_object()
        .bucket(bucket)
        .key(chat_key(room_uuid))
        .send()
        .await;
    match object {
        Ok(object) => {
            let bytes = object.body.collect().await?.into_bytes();
            Ok(serde_json::from_slice(&bytes)?)
        }
        // A room where nobody said anything has no transcript, which is an
        // answer rather than a failure.
        Err(e) if e.raw_response().map(|r| r.status().as_u16()) == Some(404) => Ok(Vec::new()),
        Err(e) => Err(Box::new(e)),
    }
}

/// Every key under a prefix, across every page the bucket answers with.
///
/// One page is a thousand keys, in name order: taken alone, a session with
/// more chunks than that came back without its newest ones, and read as
/// complete.
pub(super) async fn list_keys(
    client: &aws_sdk_s3::Client,
    bucket: &str,
    prefix: String,
) -> Result<Vec<String>, Box<dyn std::error::Error + Send + Sync>> {
    let mut pages = client
        .list_objects_v2()
        .bucket(bucket)
        .prefix(prefix)
        .into_paginator()
        .send();
    let mut keys = Vec::new();
    while let Some(page) = pages.next().await {
        let page = page?;
        keys.extend(
            page.contents()
                .iter()
                .filter_map(|object| object.key())
                .map(str::to_string),
        );
    }
    Ok(keys)
}

/// Everything stored for a session, its objects end to end, and the manifest
/// beside them.
///
/// Whole rather than paged: the point of holding one of these is to run it
/// through the client and watch where the canvas goes wrong, and a session is
/// a few hundred kilobytes. Anything buffered but not yet flushed is written
/// out first, so what comes back is current rather than up to five hundred
/// messages behind.
pub async fn download_session(
    state: &AppState,
    room_uuid: Uuid,
) -> Result<Vec<u8>, Box<dyn std::error::Error + Send + Sync>> {
    let Some(bucket) = bucket(&state.config) else {
        return Ok(Vec::new());
    };
    flush_room(state, room_uuid).await;

    let client = s3_client(&state.config);
    let mut keys: Vec<String> =
        list_keys(&client, bucket, format!("{ARCHIVE_R2_PREFIX}/{room_uuid}/"))
            .await?
            .into_iter()
            .filter(|key| key.ends_with(CHUNK_SUFFIX) || key.ends_with(CHUNK_SUFFIX_PLAIN))
            .collect();
    // Names are the zero-padded first sequence, so this is sequence order.
    keys.sort();

    assemble(keys, |key| {
        let client = client.clone();
        let bucket = bucket.to_string();
        async move {
            let object = client.get_object().bucket(bucket).key(&key).send().await?;
            let bytes = object.body.collect().await?.into_bytes();
            Ok(decompress_off_thread(bytes.to_vec()).await?)
        }
    })
    .await
}

/// The recording after `after`, as it stands, without writing anything out.
///
/// For following a live session: the inspector asks every few seconds, and
/// `download_session` would flush on each ask and leave a chunk of a few
/// messages behind every time. This reads the stored chunks that can hold
/// anything later than `after` -- the one it falls inside and those after it,
/// found by name -- and then whatever is still buffered in Redis.
///
/// May repeat messages at or before `after`, and may repeat one between a
/// stored chunk and the buffer while a flush is under way; a reader keeps what
/// is past the last sequence it has. Empty when there is nothing new.
pub async fn read_tail(
    state: &AppState,
    room_uuid: Uuid,
    after: u64,
) -> Result<Vec<u8>, Box<dyn std::error::Error + Send + Sync>> {
    let Some(bucket) = bucket(&state.config) else {
        return Ok(Vec::new());
    };
    let client = s3_client(&state.config);
    let mut chunks: Vec<(u64, String)> =
        list_keys(&client, bucket, format!("{ARCHIVE_R2_PREFIX}/{room_uuid}/"))
            .await?
            .into_iter()
            .filter_map(|key| chunk_first_seq(&key).map(|first| (first, key)))
            .collect();
    chunks.sort();
    let keys = chunks_after(&chunks, after);

    let mut out = assemble(keys, |key| {
        let client = client.clone();
        let bucket = bucket.to_string();
        async move {
            let object = client.get_object().bucket(bucket).key(&key).send().await?;
            let bytes = object.body.collect().await?.into_bytes();
            Ok(decompress_off_thread(bytes.to_vec()).await?)
        }
    })
    .await?;

    let buffered = ArchiveBuffer::new(state.redis_pool.clone())
        .peek_all(room_uuid)
        .await?;
    out.extend(encode_runs(
        buffered
            .into_iter()
            .filter(|message| message.seq > after)
            .collect(),
    ));
    Ok(out)
}

/// The first sequence a chunk holds, from its name; None for anything under
/// the prefix that is not a chunk.
pub(super) fn chunk_first_seq(key: &str) -> Option<u64> {
    let name = key.rsplit('/').next()?;
    let stem = name
        .strip_suffix(CHUNK_SUFFIX)
        .or_else(|| name.strip_suffix(CHUNK_SUFFIX_PLAIN))?;
    stem.parse().ok()
}

/// The chunks that can hold a sequence past `after`: the last one starting at
/// or before it, which it may fall inside, and every one starting later.
/// `chunks` is sorted by first sequence.
pub(super) fn chunks_after(chunks: &[(u64, String)], after: u64) -> Vec<String> {
    let from = chunks
        .iter()
        .rposition(|(first, _)| *first <= after)
        .unwrap_or(0);
    chunks[from..].iter().map(|(_, key)| key.clone()).collect()
}

/// Fetches a session's chunks and joins them into one log.
///
/// Separate from where the bytes come from because the property that matters
/// here is the joining: several requests are in flight at once and they finish
/// in whatever order they like, so a bug would show up as a recording that is
/// silently out of order rather than as a failure. `buffered` yields results
/// in the order the keys were given however the requests complete, which is
/// what makes that safe -- and what the test below pins.
///
/// A chunk that fails takes the whole download with it. A recording with a
/// hole in it that reads as whole is worse than one that failed to arrive.
pub(super) async fn assemble<F, Fut>(
    keys: Vec<String>,
    fetch: F,
) -> Result<Vec<u8>, Box<dyn std::error::Error + Send + Sync>>
where
    F: Fn(String) -> Fut,
    Fut: std::future::Future<Output = Result<Vec<u8>, Box<dyn std::error::Error + Send + Sync>>>,
{
    let chunks: Vec<Vec<u8>> = futures_util::stream::iter(keys)
        .map(fetch)
        .buffered(FETCH_CONCURRENCY)
        .try_collect()
        .await?;

    let mut out = Vec::with_capacity(chunks.iter().map(|chunk| chunk.len()).sum());
    for chunk in chunks {
        out.extend_from_slice(&chunk);
    }
    Ok(out)
}

/// What a recording says about itself: canvas, participants, and the span it
/// covers. None when the session has no recording.
pub async fn read_manifest(
    state: &AppState,
    room_uuid: Uuid,
) -> Result<Option<serde_json::Value>, Box<dyn std::error::Error + Send + Sync>> {
    let Some(bucket) = bucket(&state.config) else {
        return Ok(None);
    };
    let object = s3_client(&state.config)
        .get_object()
        .bucket(bucket)
        .key(manifest_key(room_uuid))
        .send()
        .await;
    match object {
        Ok(object) => {
            let bytes = object.body.collect().await?.into_bytes();
            Ok(Some(serde_json::from_slice(&bytes)?))
        }
        // A session that was never recorded has no manifest, which is an
        // answer rather than a failure.
        Err(e) if e.raw_response().map(|r| r.status().as_u16()) == Some(404) => Ok(None),
        Err(e) => Err(Box::new(e)),
    }
}
