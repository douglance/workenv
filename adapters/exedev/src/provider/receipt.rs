//! Provider receipts: the record of what this adapter did, and to what.
//!
//! Kept apart from the resource model because they answer a different question.
//! `model` describes the VM a request asks for; this describes what a previous
//! run of that request already did, which is what makes a retry safe rather than
//! a second create. The fingerprint is the guard: a request ID replayed against
//! different parameters is refused instead of silently applied.
use std::{fs, path::PathBuf};

use anyhow::Result;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use workenv_protocol::AdapterRequest;

use super::Spec;

pub(super) fn receipt_path(dir: &std::path::Path, key: &str) -> PathBuf {
    dir.join(format!("{:x}.json", Sha256::digest(key.as_bytes())))
}

pub(super) fn read_receipt(path: &std::path::Path) -> Result<Option<Value>> {
    if !path.exists() {
        return Ok(None);
    }
    Ok(Some(serde_json::from_slice(&fs::read(path)?)?))
}

pub(super) fn write_receipt(path: &std::path::Path, value: &Value) -> Result<()> {
    fs::write(path, serde_json::to_vec_pretty(value)?)?;
    Ok(())
}

pub(super) fn receipt_value(
    request: &AdapterRequest,
    spec: &Spec,
    phase: &str,
    fingerprint: &str,
    result: &Value,
) -> Value {
    json!({"request_id":request.request_id,"fingerprint":fingerprint,"phase":phase,
        "resource_id":spec.name,"provider_id":spec.name,"owned":phase=="created","result":result})
}

pub(super) fn fingerprint(operation: &str, spec: &Spec) -> String {
    let body = json!({"operation":operation,"name":spec.name,"cpus":spec.cpus,
        "memory_gb":spec.memory_gb,"disk_gb":spec.disk_gb,"region":spec.region,"adopt":spec.adopt});
    let bytes = serde_json::to_vec(&body).unwrap_or_else(|_| Vec::new());
    format!("{:x}", Sha256::digest(bytes))
}

pub(super) fn receipt_waits(receipt: Option<&Value>) -> bool {
    receipt.is_some_and(|r| {
        matches!(
            r["phase"].as_str(),
            Some("creating" | "destroying" | "unknown")
        )
    })
}
