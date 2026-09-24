//! What the public bucket holds, and what type the image store answers
//! each file with.
//!
//! R2 serves an object with the `Content-Type` it was written with, and one
//! written without is `application/octet-stream`. A page's `<img>` shows it
//! all the same, since browsers sniff images, but nothing else does: opening
//! a drawing's address downloads it instead of showing it, and the apps'
//! long-press save and copy take it for a file of no particular kind. Every
//! drawing written before `upload_object` said otherwise was stored that
//! way; [`set_png_content_type`] puts them right.

use anyhow::{Context, Result};
use aws_sdk_s3::types::MetadataDirective;
use aws_sdk_s3::Client;
use futures_util::{stream, StreamExt};

/// A drawing, as `image/<ab>/<sha256>.png`.
pub const PNG: &str = "image/png";
/// A replay, as `replay/<ab>/<sha256>.pch`: NEO's own format, with no
/// registered type of its own.
pub const PCH: &str = "application/octet-stream";

/// Where the drawings are.
const IMAGES: &str = "image/";

/// How many objects are looked at, and rewritten, at once.
const AT_ONCE: usize = 16;

/// What a pass over the drawings found.
#[derive(Debug, Default)]
pub struct Tally {
    /// Drawings looked at.
    pub seen: u64,
    /// Already `image/png`, and left alone.
    pub already: u64,
    /// Rewritten as `image/png` (or that would have been, in a dry run).
    pub fixed: u64,
    /// Could not be read or rewritten; named on stderr as they happen.
    pub failed: u64,
}

enum Outcome {
    Already,
    Fixed,
    Failed,
}

/// Sets `Content-Type: image/png` on every `.png` under `image/` in
/// `bucket` that does not already have it, by copying each object onto
/// itself with its metadata replaced. The bytes are not read or changed,
/// and neither is the key, so every URL stays as it is.
///
/// Safe to stop and run again: a drawing already `image/png` is only looked
/// at. With `dry_run`, nothing is written and `fixed` counts what would be.
pub async fn set_png_content_type(client: &Client, bucket: &str, dry_run: bool) -> Result<Tally> {
    let mut tally = Tally::default();
    let mut pages = client
        .list_objects_v2()
        .bucket(bucket)
        .prefix(IMAGES)
        .into_paginator()
        .send();
    while let Some(page) = pages.next().await {
        let page = page.context("listing the bucket")?;
        let keys: Vec<String> = page
            .contents()
            .iter()
            .filter_map(|object| object.key())
            .filter(|key| key.ends_with(".png"))
            .map(str::to_owned)
            .collect();
        let outcomes: Vec<Outcome> = stream::iter(keys)
            .map(|key| async move { set_one(client, bucket, &key, dry_run).await })
            .buffer_unordered(AT_ONCE)
            .collect()
            .await;
        for outcome in outcomes {
            tally.seen += 1;
            match outcome {
                Outcome::Already => tally.already += 1,
                Outcome::Fixed => tally.fixed += 1,
                Outcome::Failed => tally.failed += 1,
            }
        }
        eprintln!(
            "{} seen, {} already image/png, {} {}, {} failed",
            tally.seen,
            tally.already,
            tally.fixed,
            if dry_run { "to fix" } else { "fixed" },
            tally.failed
        );
    }
    Ok(tally)
}

async fn set_one(client: &Client, bucket: &str, key: &str, dry_run: bool) -> Outcome {
    let head = match client.head_object().bucket(bucket).key(key).send().await {
        Ok(head) => head,
        Err(error) => {
            eprintln!("{key}: could not be read: {error:?}");
            return Outcome::Failed;
        }
    };
    if head.content_type() == Some(PNG) {
        return Outcome::Already;
    }
    if dry_run {
        return Outcome::Fixed;
    }
    // Keys are hex digits and slashes, so the source needs no escaping.
    let copied = client
        .copy_object()
        .bucket(bucket)
        .key(key)
        .copy_source(format!("{bucket}/{key}"))
        .metadata_directive(MetadataDirective::Replace)
        .content_type(PNG)
        .send()
        .await;
    match copied {
        Ok(_) => Outcome::Fixed,
        Err(error) => {
            eprintln!("{key}: could not be rewritten: {error:?}");
            Outcome::Failed
        }
    }
}
