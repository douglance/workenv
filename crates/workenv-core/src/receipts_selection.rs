//! Choosing which receipt on disk answers a question.
//!
//! Split out of receipts.rs to keep both files inside this repository's 300-line
//! limit. The seam is deliberate: everything here reads the receipt directory and
//! decides which record applies, while receipts.rs owns writing them and the
//! locking around an adapter call.
use std::{
    path::{Path, PathBuf},
    time::SystemTime,
};

use anyhow::{Context, Result, bail};
use workenv_protocol::AdapterResponse;

use super::receipts::{ReceiptIdentity, ReceiptRecord, RecordedResponse};
use workenv_platform::read_json;

pub(crate) fn matching_record(path: &Path, id: &ReceiptIdentity) -> Result<Option<ReceiptRecord>> {
    let record: ReceiptRecord =
        read_json(path).with_context(|| format!("read receipt {}", path.display()))?;
    if record.identity == *id {
        Ok(Some(record))
    } else {
        Ok(None)
    }
}

pub(crate) fn latest_matching_record(
    dir: &Path,
    id: &ReceiptIdentity,
) -> Result<Option<(ReceiptRecord, SystemTime)>> {
    for (modified, path) in receipt_paths_newest_first(dir)? {
        // Skip records carrying no response at all. They match on identity, so the
        // newest-first walk used to stop on one and report "latest matching receipt
        // has no response" -- which meant a single failed `create` under a *new*
        // idempotency key permanently blocked `destroy` of the resource an earlier
        // `create` had really made. A record with no response is an attempt with no
        // known outcome, not an answer about the resource, so it is not the latest
        // answer; `recorded_response_for`, which asks about one specific request,
        // still bails rather than skipping.
        let Some(record) = matching_record(&path, id)? else {
            continue;
        };
        if record.response.is_none() {
            continue;
        }
        return Ok(Some((record, modified)));
    }
    Ok(None)
}

pub(crate) fn ensure_fingerprint(record: &ReceiptRecord, expected_fingerprint: &str) -> Result<()> {
    if record.fingerprint == expected_fingerprint {
        Ok(())
    } else {
        bail!("latest receipt fingerprint does not match current operation");
    }
}

pub(crate) fn recorded_response(
    record: &ReceiptRecord,
    modified: SystemTime,
) -> Result<RecordedResponse> {
    Ok(RecordedResponse {
        response: response_from_record(record)?,
        modified,
    })
}

pub(crate) fn response_from_record(record: &ReceiptRecord) -> Result<AdapterResponse> {
    record
        .response
        .clone()
        .context("latest matching receipt has no response; inspect before retrying")
}

pub(crate) fn recorded_between(
    entry: (SystemTime, PathBuf),
    id: &ReceiptIdentity,
    start: SystemTime,
    end: SystemTime,
) -> Result<Option<RecordedResponse>> {
    let (modified, path) = entry;
    if modified < start || modified > end {
        return Ok(None);
    }
    let Some(record) = matching_record(&path, id)? else {
        return Ok(None);
    };
    recorded_response(&record, modified).map(Some)
}

pub(crate) fn first_recorded_between(
    entries: Vec<(SystemTime, PathBuf)>,
    id: &ReceiptIdentity,
    start: SystemTime,
    end: SystemTime,
) -> Result<Option<RecordedResponse>> {
    for entry in entries {
        let recorded = recorded_between(entry, id, start, end)?;
        if recorded.is_some() {
            return Ok(recorded);
        }
    }
    Ok(None)
}

pub(crate) fn receipt_paths_newest_first(dir: &Path) -> Result<Vec<(SystemTime, PathBuf)>> {
    let mut paths = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        if path.extension().is_some_and(|ext| ext == "json") {
            let modified = std::fs::metadata(&path)?.modified()?;
            paths.push((modified, path));
        }
    }
    paths.sort_by_key(|entry| std::cmp::Reverse(entry.0));
    Ok(paths)
}
