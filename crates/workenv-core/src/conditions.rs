//! Named conditions on an environment's status.
//!
//! `status` answered with one worst-of word. "pending" said something was not
//! ready and nothing about what: the reason sat three levels down, inside the
//! first step's response data, and only the devenv step ever set one. A caller
//! wanting to know whether it could start work had to know that layout.
//!
//! Conditions state each part separately, each true, false or unknown with a
//! reason, plus one `Ready` to wait on. They are derived only from what `status`
//! already measured and from the environment's creation receipt, so they add no
//! new probe and cannot disagree with the steps beside them. The top-level `ok`
//! and `status` are unchanged, which keeps exit codes where they were.
use serde_json::{Value, json};

use crate::Controller;

/// Conditions for one status report, in a fixed order.
pub(crate) fn derive(controller: &Controller, name: &str, results: &[Value]) -> Vec<Value> {
    let applied = applied(results.first());
    let integrations = integrations_ready(results.get(1..).unwrap_or_default());
    let ready = ready(&[&applied, &integrations]);
    let mut conditions = vec![applied, integrations, ready];
    // Informational, and deliberately outside `Ready`: an unfenced cloud runner
    // is a working runner. What it must not be is an unfenced runner that looks
    // like a fenced one, which is what reporting it separately prevents.
    if let Some(fence) = fenced(controller, name) {
        conditions.push(fence);
    }
    conditions
}

fn condition(kind: &str, status: &str, reason: &str) -> Value {
    json!({ "type": kind, "status": status, "reason": reason })
}

fn status_of(step: &Value) -> &str {
    step.pointer("/response/status")
        .and_then(Value::as_str)
        .unwrap_or("unknown")
}

fn complete(step: &Value) -> bool {
    matches!(status_of(step), "ready" | "changed")
}

/// Whether devenv's applied state is current.
fn applied(step: Option<&Value>) -> Value {
    let Some(step) = step else {
        return condition("Applied", "unknown", "no_devenv_step");
    };
    if complete(step) {
        return condition("Applied", "true", "applied_and_current");
    }
    // `needs_apply` is the only place a reason is written, and it is written
    // only once devenv answered -- so a reason means "known not applied".
    if let Some(reason) = step
        .pointer("/response/data/reason")
        .and_then(Value::as_str)
    {
        return condition("Applied", "false", reason);
    }
    let reason = step
        .pointer("/response/error")
        .and_then(Value::as_str)
        .unwrap_or_else(|| status_of(step));
    condition("Applied", "unknown", reason)
}

/// Whether every integration that can report on itself says it is ready.
fn integrations_ready(steps: &[Value]) -> Value {
    match steps.iter().find(|step| !complete(step)) {
        None if steps.is_empty() => condition("IntegrationsReady", "true", "none_declared"),
        None => condition("IntegrationsReady", "true", "all_ready"),
        Some(step) => {
            let extension = step["extension"].as_str().unwrap_or("an integration");
            let reason = format!("{extension} is {}", status_of(step));
            condition("IntegrationsReady", "false", &reason)
        }
    }
}

/// True only when every part is true; otherwise names the first part that is not.
fn ready(parts: &[&Value]) -> Value {
    match parts.iter().find(|part| part["status"] != "true") {
        None => condition("Ready", "true", "all_conditions_true"),
        Some(part) => {
            let kind = part["type"].as_str().unwrap_or("a condition");
            let reason = format!("{kind} is {}", part["status"].as_str().unwrap_or("unknown"));
            condition("Ready", "false", &reason)
        }
    }
}

/// The fence the environment's provider reported when it created the resource.
///
/// `None` for an environment with no provider: there is no resource to fence,
/// and an `unknown` there would be noise. An unfenced cloud runner, by contrast,
/// gets an explicit `unknown`, because silence would read as fine.
fn fenced(controller: &Controller, name: &str) -> Option<Value> {
    let environment = controller.environment_ref(name).ok()?;
    let binding = controller.host_for(environment).ok()?.provider.as_ref()?;
    let Ok(created) = controller.created_provider_resource(name, binding) else {
        return Some(condition("Fenced", "unknown", "no_recorded_creation"));
    };
    Some(fence_from(&created.response.data))
}

/// Read the fence out of a creation receipt's data.
fn fence_from(data: &Value) -> Value {
    match data.pointer("/network/isolated").and_then(Value::as_bool) {
        Some(true) => condition("Fenced", "true", "host_enforced"),
        Some(false) => condition("Fenced", "false", "not_isolated"),
        None => condition("Fenced", "unknown", "provider_reports_no_fence"),
    }
}

#[cfg(test)]
// Test-only, and only these: see the sibling test modules in lib.rs.
#[allow(clippy::expect_used, clippy::unwrap_used)]
#[path = "conditions_tests.rs"]
mod tests;
