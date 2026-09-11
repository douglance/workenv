//! Adapter-owned record of claimed Lima slots.
//!
//! This ledger is deliberately independent of controller receipts. Reclamation
//! must keep working after the adapter is rebuilt, which changes the executable
//! store path and therefore invalidates every prior controller create receipt.
use std::{
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::Result;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use workenv_platform::write_json_atomic;

use super::spec::Spec;

/// Ledger format version, recorded so a later reader can reject unknown shapes.
const SCHEMA: u64 = 1;

/// Path of the exclusive lock guarding ledger updates.
pub(super) fn lock_path(dir: &Path) -> PathBuf {
    dir.join("lima.lock")
}

/// Path of one slot entry, keyed by VM host and slot name.
pub(super) fn entry_path(dir: &Path, vm_host: &str, slot: &str) -> PathBuf {
    let mut hasher = Sha256::new();
    hasher.update(vm_host.as_bytes());
    hasher.update([0]);
    hasher.update(slot.as_bytes());
    dir.join("slots")
        .join(format!("{:x}.json", hasher.finalize()))
}

/// Replace one entry atomically.
pub(super) fn write_entry(path: &Path, value: &Value) -> Result<()> {
    write_json_atomic(path, value)
}

/// Build an entry recording one claim state.
pub(super) fn entry_value(resolved: &Spec, state: &str, identity: &Value) -> Value {
    let now = now_epoch();
    json!({
        "schema": SCHEMA,
        "vm_host": resolved.vm_host,
        "slot": resolved.slot,
        "environment": resolved.environment,
        "state": state,
        "claim_uuid": identity.get("claim_uuid").cloned().unwrap_or(Value::Null),
        "instance_identity": identity,
        "updated_at_epoch": now,
        "lease_expires_at_epoch": now.saturating_add(resolved.lease_seconds)
    })
}

/// Seconds since the Unix epoch, saturating at zero on a clock error.
pub(super) fn now_epoch() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |value| value.as_secs())
}
