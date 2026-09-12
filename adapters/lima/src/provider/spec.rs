//! Resolved settings for one Lima worker request.
use std::path::PathBuf;

use anyhow::{Result, bail};
use serde_json::Value;
use workenv_protocol::AdapterRequest;

/// Default seconds a claimed worker may live before it becomes reapable.
///
/// A default, not a guarantee. The host reaps on whatever lease it was handed,
/// so the only property this crate can assert is that the lease is non-zero; any
/// relationship to the tailnet's own ephemeral-node lifetime would have to be
/// measured against a live tailnet.
const DEFAULT_LEASE_SECONDS: u64 = 14_400;

/// One request's resolved worker settings.
#[derive(Clone)]
pub(super) struct Spec {
    /// Stable slot name; also the Lima instance and tailnet hostname.
    pub(super) slot: String,
    /// SSH destination of the Mac hosting the pool.
    pub(super) vm_host: String,
    /// Host CLI command name on that Mac.
    pub(super) command: String,
    /// Environment this claim belongs to.
    pub(super) environment: String,
    /// Seconds before the claim becomes reapable.
    pub(super) lease_seconds: u64,
    /// Permit a cold boot when the pool is empty.
    pub(super) allow_cold_start: bool,
    /// Observe only; never claim, never report ownership.
    pub(super) adopt: bool,
}

/// Resolve a spec from binding config, then operation input, then the target.
pub(super) fn spec(request: &AdapterRequest) -> Result<Spec> {
    let slot = field(request, "slot")
        .or_else(|| field(request, "instance_name"))
        .unwrap_or(&request.target.environment)
        .to_owned();
    if slot.trim().is_empty() {
        bail!("Lima slot name is required");
    }
    let vm_host = field(request, "vm_host")
        .or_else(|| field(request, "dave_address"))
        .unwrap_or("")
        .to_owned();
    if vm_host.trim().is_empty() {
        bail!("provider config vm_host is required");
    }
    Ok(Spec {
        environment: request.target.environment.clone(),
        command: field(request, "command")
            .unwrap_or("workenv-lima")
            .to_owned(),
        lease_seconds: number(request, "lease_seconds").unwrap_or(DEFAULT_LEASE_SECONDS),
        allow_cold_start: flag(request, "allow_cold_start"),
        adopt: flag(request, "adopt"),
        slot,
        vm_host,
    })
}

/// Directory holding this adapter's own ledger, independent of core receipts.
pub(super) fn state_dir(request: &AdapterRequest) -> PathBuf {
    request
        .config
        .get("state_dir")
        .and_then(Value::as_str)
        .map_or_else(
            || {
                std::env::current_dir()
                    .unwrap_or_else(|_| PathBuf::from("."))
                    .join(".state/workenv-adapters/lima")
            },
            PathBuf::from,
        )
}

fn field<'a>(request: &'a AdapterRequest, key: &str) -> Option<&'a str> {
    request
        .config
        .get(key)
        .and_then(Value::as_str)
        .or_else(|| request.input.get(key).and_then(Value::as_str))
}

fn number(request: &AdapterRequest, key: &str) -> Option<u64> {
    request
        .config
        .get(key)
        .and_then(Value::as_u64)
        .or_else(|| request.input.get(key).and_then(Value::as_u64))
        .filter(|value| *value > 0)
}

fn flag(request: &AdapterRequest, key: &str) -> bool {
    request
        .config
        .get(key)
        .and_then(Value::as_bool)
        .or_else(|| request.input.get(key).and_then(Value::as_bool))
        .unwrap_or(false)
}
