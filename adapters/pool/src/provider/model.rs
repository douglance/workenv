//! Response shapes and previous-resource reading.
use serde_json::{Value, json};
use workenv_protocol::{AdapterRequest, AdapterResponse, ResponseStatus};

use super::rank::Candidate;
use super::spec::Spec;

/// Build a response, optionally carrying a failure explanation.
pub(super) fn response(
    request: &AdapterRequest,
    status: ResponseStatus,
    data: Value,
    error: Option<&str>,
) -> AdapterResponse {
    let mut response = AdapterResponse::new(request, status, data);
    response.error = error.map(str::to_owned);
    response
}

/// Successful placement.
///
/// `owned` must be a JSON boolean and `resource_id` a JSON string: the
/// controller checks both types exactly before it will permit a teardown. The
/// resource id is the environment name, not the slot, so it does not vary with
/// where the environment landed.
pub(super) fn placed(
    resolved: &Spec,
    site: &str,
    placement: &Value,
    device_id: Option<&str>,
    considered: &[Value],
) -> Value {
    json!({
        "status": "present",
        "resource_id": resolved.name,
        "owned": true,
        "address": resolved.address(),
        "instance_identity": identity(resolved, site, placement),
        "device_id": device_id,
        "tailnet": {"dns_name": resolved.dns_name(), "device_id": device_id},
        "placement": {"site": site, "considered": considered}
    })
}

/// The identity teardown re-verifies. `site` is what routes a release home.
pub(super) fn identity(resolved: &Spec, site: &str, placement: &Value) -> Value {
    json!({
        "site": site,
        "native_id": placement.get("vm_name").cloned().unwrap_or(Value::Null),
        "native_address": placement.get("ssh_dest").cloned().unwrap_or(Value::Null),
        "claim_uuid": placement
            .get("instance_identity")
            .and_then(|value| value.get("claim_uuid"))
            .cloned()
            .unwrap_or(Value::Null),
        "boot_id": placement
            .get("instance_identity")
            .and_then(|value| value.get("boot_id"))
            .cloned()
            .unwrap_or(Value::Null),
        "environment": resolved.name,
        "dns_name": resolved.dns_name()
    })
}

/// Nothing fit anywhere. Report every candidate's numbers, not just a verdict.
pub(super) fn no_capacity(resolved: &Spec, considered: &[Value]) -> Value {
    json!({
        "status": "no_capacity",
        "resource_id": resolved.name,
        "owned": false,
        "required": {
            "system": resolved.requirement.system,
            "cpus": resolved.requirement.cpus,
            "memory_gb": resolved.requirement.memory_gb,
            "disk_gb": resolved.requirement.disk_gb
        },
        "candidates": considered
    })
}

/// An outcome we could not observe. `owned` stays truthful so reclamation works.
pub(super) fn uncertain(resolved: &Spec, owned: bool, identity: &Value, message: &str) -> Value {
    json!({
        "status": "unknown",
        "resource_id": resolved.name,
        "owned": owned,
        "instance_identity": identity,
        "error": message
    })
}

/// The environment is gone.
pub(super) fn released(name: &str, device_id: Option<&str>) -> Value {
    json!({"status":"missing","resource_id":name,"owned":false,
           "destroyed":true,"device_id":device_id})
}

/// Candidate reports in declaration order, for the audit trail.
pub(super) fn considered(candidates: &[Candidate]) -> Vec<Value> {
    candidates
        .iter()
        .map(|candidate| candidate.report.clone())
        .collect()
}

/// Whether a prior response proves this adapter owns the named environment.
pub(super) fn previous_owned(request: &AdapterRequest, name: &str) -> bool {
    previous_resources(request)
        .into_iter()
        .any(|value| value["owned"] == true && value["resource_id"] == name)
}

/// The identity recorded at placement.
pub(super) fn previous_identity(request: &AdapterRequest) -> Option<Value> {
    previous_resources(request)
        .into_iter()
        .find_map(|value| value.get("instance_identity").cloned())
        .filter(|identity| !identity.is_null())
}

/// A recorded string field of the placement identity.
pub(super) fn identity_field<'a>(identity: &'a Value, key: &str) -> Option<&'a str> {
    identity.get(key).and_then(Value::as_str)
}

/// Controller create receipts arrive by three different routes.
fn previous_resources(request: &AdapterRequest) -> Vec<&Value> {
    [
        request.previous.as_ref(),
        request
            .previous
            .as_ref()
            .and_then(|value| value.get("data")),
        request.input.get("create"),
    ]
    .into_iter()
    .flatten()
    .collect()
}
