//! The diagnostics a client files about a session that went wrong.

use aws_sdk_s3::primitives::ByteStream;
use serde::Serialize;
use tracing::warn;
use uuid::Uuid;

use crate::web::state::AppState;

use super::{bucket, list_keys, s3_client, ARCHIVE_R2_PREFIX};

/// The most reports read back for one session. Keys sort by the moment
/// filed, so these are the newest.
const MAX_DIAGNOSTICS_READ: usize = 100;

/// The largest report accepted. The trace is bounded at 512 events by the
/// painter, so this is headroom rather than a limit anyone should meet.
pub const MAX_DIAGNOSTIC_BYTES: usize = 512 * 1024;

/// Files one client's account of what it believed, beside the session's log.
///
/// Under the archive's own prefix on purpose: a report is only ever read
/// together with the stream it disagrees with, and keeping them in two places
/// is how one of them gets cleaned up without the other.
pub async fn store_diagnostic(
    state: &AppState,
    room_uuid: Uuid,
    user_login_name: &str,
    body: Vec<u8>,
) -> Result<Option<String>, Box<dyn std::error::Error + Send + Sync>> {
    let Some(bucket) = bucket(&state.config) else {
        return Ok(None);
    };
    // A few random characters after the moment, so two reports filed in the
    // same millisecond -- a client files two back to back when a checkpoint
    // fails and the socket then closes on the gap -- do not overwrite.
    let key = format!(
        "{ARCHIVE_R2_PREFIX}/{room_uuid}/diagnostics/{}-{}-{}.json",
        chrono::Utc::now().format("%Y%m%dT%H%M%S%.3fZ"),
        user_login_name,
        &Uuid::new_v4().simple().to_string()[..6],
    );
    s3_client(&state.config)
        .put_object()
        .bucket(bucket)
        .key(&key)
        .content_type("application/json")
        .body(ByteStream::from(body))
        .send()
        .await?;
    Ok(Some(key))
}

/// Every synchronisation report filed for a session, newest last.
///
/// Returned as one JSON array so a session's reports can be read together:
/// what matters is usually the difference between what two clients believed at
/// the same moment, which is a comparison and not a file.
pub async fn download_diagnostics(
    state: &AppState,
    room_uuid: Uuid,
) -> Result<Vec<FiledDiagnostic>, Box<dyn std::error::Error + Send + Sync>> {
    let Some(bucket) = bucket(&state.config) else {
        return Ok(Vec::new());
    };
    let client = s3_client(&state.config);
    // The key begins with the moment it was filed, so this is chronological.
    let mut keys = list_keys(
        &client,
        bucket,
        format!("{ARCHIVE_R2_PREFIX}/{room_uuid}/diagnostics/"),
    )
    .await?;
    keys.sort();
    // The newest, bounded: each is a fetch and up to half a megabyte held.
    if keys.len() > MAX_DIAGNOSTICS_READ {
        keys.drain(..keys.len() - MAX_DIAGNOSTICS_READ);
    }

    let mut reports = Vec::new();
    for key in keys {
        let object = client.get_object().bucket(bucket).key(&key).send().await?;
        let bytes = object.body.collect().await?.into_bytes();
        match serde_json::from_slice(&bytes) {
            Ok(report) => {
                let (filed_at, filed_by) = filed_as(&key);
                reports.push(FiledDiagnostic {
                    filed_at,
                    filed_by,
                    key,
                    report,
                })
            }
            Err(e) => warn!("Skipping unreadable diagnostic {}: {}", key, e),
        }
    }
    Ok(reports)
}

/// One report, with what only its storage key knows.
///
/// The body is whatever the client posted, and the client does not say who
/// it is -- the server does, by writing the authenticated login name into the
/// key. A reader comparing two clients needs that name beside the report.
#[derive(Debug, Serialize)]
pub struct FiledDiagnostic {
    /// When the server received it, as written into the key.
    pub filed_at: Option<String>,
    pub filed_by: Option<String>,
    pub key: String,
    pub report: serde_json::Value,
}

/// Reads `.../diagnostics/<timestamp>-<login>.json` back into its parts.
///
/// Split at the first hyphen: the timestamp has none, and a login name may.
pub(super) fn filed_as(key: &str) -> (Option<String>, Option<String>) {
    let Some(name) = key
        .rsplit('/')
        .next()
        .and_then(|name| name.strip_suffix(".json"))
    else {
        return (None, None);
    };
    // Less the random tail `store_diagnostic` adds; a name from before it
    // had none.
    let name = match name.rsplit_once('-') {
        Some((rest, tail)) if tail.len() == 6 && tail.bytes().all(|b| b.is_ascii_hexdigit()) => {
            rest
        }
        _ => name,
    };
    match name.split_once('-') {
        Some((at, by)) => {
            let at = chrono::NaiveDateTime::parse_from_str(at, "%Y%m%dT%H%M%S%.3fZ")
                .ok()
                .map(|at| {
                    at.and_utc()
                        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
                });
            (at, Some(by.to_string()))
        }
        None => (None, None),
    }
}
