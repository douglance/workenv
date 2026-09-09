//! Claiming one warm pool slot.
use anyhow::Result;
use serde_json::{Value, json};
use workenv_platform::with_exclusive_lock;
use workenv_protocol::{AdapterRequest, AdapterResponse, ResponseStatus};

use super::{Provider, host::HostRunner, ledger, model, spec::Spec, spec::state_dir};

/// Claim a ready slot, or report why no worker was produced.
pub(super) fn run<R: HostRunner>(
    provider: &mut Provider<R>,
    request: &AdapterRequest,
    resolved: &Spec,
) -> Result<AdapterResponse> {
    if resolved.adopt {
        return Ok(adopt(provider, request, resolved));
    }
    let claim = match provider.claim(resolved, &request.request_id) {
        Ok(claim) => claim,
        Err(error) => {
            let data = model::uncertain(resolved, false, &Value::Null, &error);
            return Ok(AdapterResponse::new(request, ResponseStatus::Pending, data));
        }
    };
    if claim["status"] == "pool_empty" {
        return Ok(AdapterResponse::new(
            request,
            ResponseStatus::Pending,
            claim,
        ));
    }
    record(provider, request, resolved, &claim)
}

/// Verify the claimed slot answers at its statically declared address.
///
/// `target.address` comes only from the evaluated manifest, so a divergence
/// here would otherwise surface much later as an unexplained SSH timeout.
fn record<R: HostRunner>(
    provider: &mut Provider<R>,
    request: &AdapterRequest,
    resolved: &Spec,
    claim: &Value,
) -> Result<AdapterResponse> {
    let identity = claim
        .get("instance_identity")
        .cloned()
        .unwrap_or(Value::Null);
    if let Some(declared) = request.target.address.as_deref() {
        let reported = model::ssh_dest(claim).unwrap_or_default();
        if reported != declared {
            return Ok(mismatch(provider, request, resolved, &identity, reported));
        }
    }
    let dir = state_dir(request);
    let entry = ledger::entry_value(resolved, "claimed", &identity);
    with_exclusive_lock(&ledger::lock_path(&dir), || {
        ledger::write_entry(
            &ledger::entry_path(&dir, &resolved.vm_host, &resolved.slot),
            &entry,
        )
    })?;
    Ok(AdapterResponse::new(
        request,
        ResponseStatus::Changed,
        model::claimed(resolved, claim),
    ))
}

/// Release a slot whose address does not match the manifest, then fail loudly.
fn mismatch<R: HostRunner>(
    provider: &mut Provider<R>,
    request: &AdapterRequest,
    resolved: &Spec,
    identity: &Value,
    reported: &str,
) -> AdapterResponse {
    if let Some(uuid) = model::claim_uuid(identity) {
        let _released = provider.release(&resolved.slot, uuid);
    }
    let declared = request.target.address.as_deref().unwrap_or_default();
    model::response(
        request,
        ResponseStatus::Failed,
        json!({"resource_id":resolved.slot,"owned":false,
            "declared_address":declared,"reported_address":reported}),
        Some("claimed slot does not answer at the declared host address"),
    )
}

/// Observe a slot without claiming it; an adopted slot is never destroyable.
fn adopt<R: HostRunner>(
    provider: &mut Provider<R>,
    request: &AdapterRequest,
    resolved: &Spec,
) -> AdapterResponse {
    match provider.list() {
        Ok(vms) => {
            let found = vms.into_iter().find(|vm| vm["vm_name"] == resolved.slot);
            let status = if found.is_some() {
                ResponseStatus::Ready
            } else {
                ResponseStatus::Failed
            };
            let data =
                found.unwrap_or_else(|| json!({"status":"missing","resource_id":resolved.slot}));
            let mut data = data;
            data["owned"] = json!(false);
            data["resource_id"] = json!(resolved.slot);
            AdapterResponse::new(request, status, data)
        }
        Err(error) => model::response(request, ResponseStatus::Failed, json!({}), Some(&error)),
    }
}
