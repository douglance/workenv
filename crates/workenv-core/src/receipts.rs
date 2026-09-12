//! Idempotent mutation receipts.
use std::{
    path::{Path, PathBuf},
    time::SystemTime,
};

use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};
use workenv_platform::{read_json, with_exclusive_lock, write_json_atomic};
use workenv_protocol::{AdapterResponse, ResponseStatus};

use crate::receipts_selection::{
    ensure_fingerprint, first_recorded_between, latest_matching_record, receipt_paths_newest_first,
    recorded_response,
};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct ReceiptIdentity {
    pub(crate) environment: String,
    pub(crate) extension: String,
    pub(crate) operation: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct ReceiptRecord {
    pub(crate) request_id: String,
    pub(crate) fingerprint: String,
    pub(crate) identity: ReceiptIdentity,
    pub(crate) phase: String,
    pub(crate) response: Option<AdapterResponse>,
}

#[derive(Clone, Debug)]
pub(crate) struct ReceiptStore {
    dir: PathBuf,
}

#[derive(Clone, Debug)]
pub(crate) struct RecordedResponse {
    pub(crate) response: AdapterResponse,
    pub(crate) modified: SystemTime,
}

impl ReceiptStore {
    pub(crate) fn new(root: &Path) -> Self {
        Self {
            dir: root.join(".state/workenv-core/receipts"),
        }
    }

    pub(crate) fn apply(
        &self,
        id: &ReceiptIdentity,
        request_id: &str,
        fingerprint: &str,
        invoke: impl FnOnce(Option<Value>) -> Result<AdapterResponse>,
    ) -> Result<AdapterResponse> {
        let path = self.receipt_path(request_id);
        // One lock per receipt, not one for the store. The store-wide lock was
        // held across `invoke`, so `up envA` serialised behind `up envB` for the
        // whole of the other environment's provider call -- measured at 10s -> 16s
        // for two concurrent ups. Receipts are keyed by request id and no two
        // requests write the same file, so the narrower lock protects the same
        // thing.
        let lock = self.lock_path(request_id);
        with_exclusive_lock(&lock, || {
            Self::apply_locked(&path, id, request_id, fingerprint, invoke)
        })
    }

    pub(crate) fn latest_recorded_response_for(
        &self,
        id: &ReceiptIdentity,
        expected_fingerprint: &str,
    ) -> Result<Option<RecordedResponse>> {
        if !self.dir.is_dir() {
            return Ok(None);
        }
        let Some((record, modified)) = latest_matching_record(&self.dir, id)? else {
            return Ok(None);
        };
        ensure_fingerprint(&record, expected_fingerprint)?;
        recorded_response(&record, modified).map(Some)
    }

    pub(crate) fn recorded_response_for(
        &self,
        id: &ReceiptIdentity,
        request_id: &str,
        expected_fingerprint: &str,
    ) -> Result<RecordedResponse> {
        let path = self.receipt_path(request_id);
        let modified = std::fs::metadata(&path)?.modified()?;
        let record: ReceiptRecord = read_json(&path)?;
        if record.identity != *id {
            bail!("receipt identity does not match current operation");
        }
        ensure_fingerprint(&record, expected_fingerprint)?;
        recorded_response(&record, modified)
    }

    pub(crate) fn latest_recorded_response_between(
        &self,
        id: &ReceiptIdentity,
        start: SystemTime,
        end: SystemTime,
    ) -> Result<Option<RecordedResponse>> {
        if !self.dir.is_dir() {
            return Ok(None);
        }
        first_recorded_between(receipt_paths_newest_first(&self.dir)?, id, start, end)
    }

    fn apply_locked(
        path: &Path,
        id: &ReceiptIdentity,
        request_id: &str,
        fingerprint: &str,
        invoke: impl FnOnce(Option<Value>) -> Result<AdapterResponse>,
    ) -> Result<AdapterResponse> {
        let previous = Self::previous(path, fingerprint)?;
        if let Some(response) = replayable(previous.as_ref()) {
            return Ok(response);
        }
        let previous_value = pending_previous(previous.as_ref())?;
        // The marker carries the previous response forward instead of erasing it.
        //
        // Writing a response-less record here is deliberate -- it is what makes a
        // later retry refuse to relaunch blindly after a crash between this write
        // and the final one. But it used to drop the recorded response with it, so
        // a pending `create` whose retry merely *failed* lost the `execution_id`
        // and `resource_id` it had already recorded. `pending_previous` then bailed
        // "operation outcome is uncertain" for that key forever, and `destroy`
        // bailed "latest matching receipt has no response" -- the resource was
        // live, recorded, and unreachable.
        //
        // Keeping the response means the next attempt can observe the execution it
        // already started rather than guessing. The crash signal survives where it
        // actually matters: a first attempt has no previous response to carry, so
        // its marker is still response-less and still refuses a blind retry.
        let carried = previous
            .as_ref()
            .and_then(|record| record.response.as_ref());
        write_record(
            path,
            &record(id, request_id, fingerprint, Phase::Started, carried),
        )?;
        let response = invoke(previous_value)?;
        write_record(
            path,
            &record(
                id,
                request_id,
                fingerprint,
                Phase::Complete,
                Some(&response),
            ),
        )?;
        Ok(response)
    }

    fn previous(path: &Path, fingerprint: &str) -> Result<Option<ReceiptRecord>> {
        if !path.exists() {
            return Ok(None);
        }
        let record: ReceiptRecord = read_json(path)?;
        if record.fingerprint == fingerprint {
            Ok(Some(record))
        } else {
            bail!("idempotency key is bound to a different operation");
        }
    }

    fn receipt_path(&self, request_id: &str) -> PathBuf {
        let digest = Sha256::digest(request_id.as_bytes());
        self.dir.join(format!("{digest:x}.json"))
    }

    /// The lock guarding one receipt. Named from the request id so two different
    /// requests never contend.
    fn lock_path(&self, request_id: &str) -> PathBuf {
        let digest = Sha256::digest(request_id.as_bytes());
        self.dir.join(format!("{digest:x}.lock"))
    }
}

/// Whether the attempt this record describes is still in flight.
///
/// Stated by the caller rather than inferred from `response.is_some()`. Once an
/// in-flight marker carries the previous attempt's response, the two cases are
/// no longer distinguishable from the response alone, and a `phase` derived that
/// way would read "response" for an operation still running -- which is the one
/// thing someone reading a receipt by hand needs it not to say.
#[derive(Clone, Copy)]
enum Phase {
    Started,
    Complete,
}

impl Phase {
    fn as_str(self) -> &'static str {
        match self {
            Self::Started => "started",
            Self::Complete => "response",
        }
    }
}

fn record(
    id: &ReceiptIdentity,
    request_id: &str,
    fingerprint: &str,
    phase: Phase,
    response: Option<&AdapterResponse>,
) -> ReceiptRecord {
    ReceiptRecord {
        request_id: request_id.to_owned(),
        fingerprint: fingerprint.to_owned(),
        identity: id.clone(),
        phase: phase.as_str().to_owned(),
        response: response.cloned(),
    }
}

fn write_record(path: &Path, record: &ReceiptRecord) -> Result<()> {
    write_json_atomic(path, record)
}

fn replayable(record: Option<&ReceiptRecord>) -> Option<AdapterResponse> {
    let response = record?.response.as_ref()?;
    if response.complete() {
        Some(response.clone())
    } else {
        None
    }
}

fn pending_previous(record: Option<&ReceiptRecord>) -> Result<Option<Value>> {
    let Some(record) = record else {
        return Ok(None);
    };
    let Some(response) = &record.response else {
        bail!("operation outcome is uncertain; inspect the receipt before retrying");
    };
    if response.status == ResponseStatus::Pending && response.execution_id.is_some() {
        return Ok(Some(json!(response)));
    }
    if response.status == ResponseStatus::Pending {
        bail!("pending operation has no execution ID; inspect the receipt before retrying");
    }
    Ok(Some(json!(response)))
}

#[cfg(test)]
#[path = "receipts_tests.rs"]
mod tests;
