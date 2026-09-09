//! Releasing one claimed slot.
use anyhow::Result;
use serde_json::{Value, json};
use workenv_platform::with_exclusive_lock;
use workenv_protocol::{AdapterRequest, AdapterResponse, ResponseStatus};

use super::{Provider, host::HostRunner, ledger, model, spec::Spec, spec::state_dir};

/// Release the slot named by a previous claim, refusing on identity drift.
pub(super) fn run<R: HostRunner>(
    provider: &mut Provider<R>,
    request: &AdapterRequest,
    resolved: &Spec,
) -> Result<AdapterResponse> {
    if !model::previous_owned(request, &resolved.slot) {
        return Ok(model::response(
            request,
            ResponseStatus::Failed,
            json!({}),
            Some("destroy requires a matching owned previous resource"),
        ));
    }
    let identity = model::previous_identity(request).unwrap_or(Value::Null);
    let Some(claim_uuid) = model::claim_uuid(&identity).map(str::to_owned) else {
        return Ok(model::response(
            request,
            ResponseStatus::Failed,
            json!({"resource_id":resolved.slot}),
            Some("destroy requires a recorded claim identity"),
        ));
    };
    let released = match provider.release(&resolved.slot, &claim_uuid) {
        Ok(released) => released,
        Err(error) => {
            let data = model::uncertain(resolved, true, &identity, &error);
            return Ok(AdapterResponse::new(request, ResponseStatus::Pending, data));
        }
    };
    finish(request, resolved, &identity, &released)
}

/// Interpret the VM host's release verdict.
fn finish(
    request: &AdapterRequest,
    resolved: &Spec,
    identity: &Value,
    released: &Value,
) -> Result<AdapterResponse> {
    match released["status"].as_str() {
        Some("destroyed") => Ok(settle(
            request,
            resolved,
            identity,
            ResponseStatus::Changed,
        )?),
        Some("already_destroyed") => {
            Ok(settle(request, resolved, identity, ResponseStatus::Ready)?)
        }
        Some("identity_mismatch") => Ok(model::response(
            request,
            ResponseStatus::Failed,
            released.clone(),
            Some("destroy requires matching provider instance identity"),
        )),
        _ => Ok(AdapterResponse::new(
            request,
            ResponseStatus::Pending,
            model::uncertain(
                resolved,
                true,
                identity,
                "release outcome is uncertain; inspect before retrying",
            ),
        )),
    }
}

/// Tombstone the ledger entry and report the slot gone.
fn settle(
    request: &AdapterRequest,
    resolved: &Spec,
    identity: &Value,
    status: ResponseStatus,
) -> Result<AdapterResponse> {
    let dir = state_dir(request);
    let entry = ledger::entry_value(resolved, "released", identity);
    with_exclusive_lock(&ledger::lock_path(&dir), || {
        ledger::write_entry(
            &ledger::entry_path(&dir, &resolved.vm_host, &resolved.slot),
            &entry,
        )
    })?;
    Ok(AdapterResponse::new(
        request,
        status,
        model::destroyed(&resolved.slot),
    ))
}
