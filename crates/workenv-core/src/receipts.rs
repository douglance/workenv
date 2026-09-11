//! Idempotent mutation receipts.
use std::{
    path::{Path, PathBuf},
    time::SystemTime,
};

use anyhow::{Context as _, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};
use workenv_platform::{read_json, with_exclusive_lock, write_json_atomic};
use workenv_protocol::{AdapterResponse, ResponseStatus};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct ReceiptIdentity {
    pub(crate) environment: String,
    pub(crate) extension: String,
    pub(crate) operation: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct ReceiptRecord {
    request_id: String,
    fingerprint: String,
    identity: ReceiptIdentity,
    phase: String,
    response: Option<AdapterResponse>,
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
        let lock = self.dir.join("receipts.lock");
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
        write_record(path, &record(id, request_id, fingerprint, None))?;
        let response = invoke(previous_value)?;
        write_record(path, &record(id, request_id, fingerprint, Some(&response)))?;
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
}

fn record(
    id: &ReceiptIdentity,
    request_id: &str,
    fingerprint: &str,
    response: Option<&AdapterResponse>,
) -> ReceiptRecord {
    ReceiptRecord {
        request_id: request_id.to_owned(),
        fingerprint: fingerprint.to_owned(),
        identity: id.clone(),
        phase: if response.is_some() {
            "response".to_owned()
        } else {
            "started".to_owned()
        },
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

fn matching_record(path: &Path, id: &ReceiptIdentity) -> Result<Option<ReceiptRecord>> {
    let record: ReceiptRecord =
        read_json(path).with_context(|| format!("read receipt {}", path.display()))?;
    if record.identity == *id {
        Ok(Some(record))
    } else {
        Ok(None)
    }
}

fn latest_matching_record(
    dir: &Path,
    id: &ReceiptIdentity,
) -> Result<Option<(ReceiptRecord, SystemTime)>> {
    for (modified, path) in receipt_paths_newest_first(dir)? {
        if let Some(record) = matching_record(&path, id)? {
            return Ok(Some((record, modified)));
        }
    }
    Ok(None)
}

fn ensure_fingerprint(record: &ReceiptRecord, expected_fingerprint: &str) -> Result<()> {
    if record.fingerprint == expected_fingerprint {
        Ok(())
    } else {
        bail!("latest receipt fingerprint does not match current operation");
    }
}

fn recorded_response(record: &ReceiptRecord, modified: SystemTime) -> Result<RecordedResponse> {
    Ok(RecordedResponse {
        response: response_from_record(record)?,
        modified,
    })
}

fn response_from_record(record: &ReceiptRecord) -> Result<AdapterResponse> {
    record
        .response
        .clone()
        .context("latest matching receipt has no response; inspect before retrying")
}

fn recorded_between(
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

fn first_recorded_between(
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

fn receipt_paths_newest_first(dir: &Path) -> Result<Vec<(SystemTime, PathBuf)>> {
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

#[cfg(test)]
#[path = "receipts_tests.rs"]
mod tests;
