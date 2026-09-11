//! Response shapes and previous-resource reading.
use serde_json::{Value, json};
use workenv_protocol::{AdapterRequest, AdapterResponse, ResponseStatus};

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

/// Successful claim payload built from the VM host's own record.
///
/// `owned` must be a JSON boolean and `resource_id` a JSON string: the
/// controller checks both types exactly before it will permit a teardown.
pub(super) fn claimed(resolved: &Spec, vm: &Value) -> Value {
    json!({
        "status": "present",
        "resource_id": resolved.slot,
        "owned": true,
        "address": vm.get("ssh_dest").cloned().unwrap_or(Value::Null),
        "instance_identity": vm.get("instance_identity").cloned().unwrap_or(Value::Null),
        "vm_host": resolved.vm_host,
        "environment": resolved.environment,
        "vm": vm
    })
}

/// Payload for a guest that is gone, or has just been released.
pub(super) fn destroyed(slot: &str) -> Value {
    json!({"status":"missing","resource_id":slot,"owned":false,"destroyed":true})
}

/// Payload for an uncertain outcome; `owned` stays truthful so reclamation works.
pub(super) fn uncertain(resolved: &Spec, owned: bool, identity: &Value, message: &str) -> Value {
    json!({
        "status": "unknown",
        "resource_id": resolved.slot,
        "owned": owned,
        "vm_host": resolved.vm_host,
        "instance_identity": identity,
        "error": message
    })
}

/// The reachable address the VM host reports for a slot.
pub(super) fn ssh_dest(vm: &Value) -> Option<&str> {
    vm.get("ssh_dest").and_then(Value::as_str)
}

/// The per-claim identity minted by the VM host.
pub(super) fn claim_uuid(identity: &Value) -> Option<&str> {
    identity.get("claim_uuid").and_then(Value::as_str)
}

/// Whether a prior response proves this adapter owns the named slot.
pub(super) fn previous_owned(request: &AdapterRequest, slot: &str) -> bool {
    previous_resources(request)
        .into_iter()
        .any(|value| value["owned"] == true && value["resource_id"] == slot)
}

/// The identity recorded when the slot was claimed.
pub(super) fn previous_identity(request: &AdapterRequest) -> Option<Value> {
    previous_resources(request)
        .into_iter()
        .find_map(|value| value.get("instance_identity").cloned())
        .filter(|identity| !identity.is_null())
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
