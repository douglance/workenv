use anyhow::Result;
use serde_json::{Map, Value, json};
use workenv_platform::{ExecutionSpec, Executor};
use workenv_protocol::{AdapterRequest, AdapterResponse, ResponseStatus};

use crate::{
    machines,
    util::{pending_response, response},
};

pub(crate) fn run(request: &AdapterRequest, runner: &impl Executor) -> Result<AdapterResponse> {
    let Some(profile) = previous_profile(request) else {
        let mut data = Map::new();
        data.insert("status".to_string(), json!("herdr_cleanup_blocked"));
        return Ok(response(request, ResponseStatus::Failed, data));
    };
    let machines = machines::list_from_controller(request, runner)?;
    if let Some(response) = machines::response_for_state(request, &machines, "cleanup_lookup") {
        return Ok(response);
    }
    if !has_profile_id(&machines, &profile.id) {
        return Ok(cleaned(request, &profile.id, ResponseStatus::Ready));
    }
    if !has_profile(&machines, &profile) {
        return Ok(response(
            request,
            ResponseStatus::Failed,
            data(&profile.id, "herdr_cleanup_identity_mismatch"),
        ));
    }
    let output = runner.execute(ExecutionSpec {
        executable: "herdr".to_string(),
        arg: vec![
            "machine".to_string(),
            "remove".to_string(),
            profile.id.clone(),
        ],
        cwd: None,
        stdin: None,
        idempotency_key: format!("{}:herdr-remove", request.request_id),
        purpose: "Remove exact Herdr machine profile registration.".to_string(),
        timeout_ms: 60_000,
    })?;
    if output.exit_code.is_none() {
        let mut data = data(&profile.id, "herdr_cleanup_pending");
        data.insert(
            "execution_id".to_string(),
            json!(output.execution_id.clone()),
        );
        return Ok(pending_response(request, output.execution_id, data));
    }
    if output.exit_code != Some(0) {
        let mut data = data(&profile.id, "herdr_cleanup_failed");
        data.insert("stderr".to_string(), json!(output.stderr));
        data.insert("execution_id".to_string(), json!(output.execution_id));
        return Ok(response(request, ResponseStatus::Failed, data));
    }
    verify_removed(request, runner, &profile.id)
}

fn verify_removed(
    request: &AdapterRequest,
    runner: &impl Executor,
    profile_id: &str,
) -> Result<AdapterResponse> {
    let machines = machines::list_after_cleanup(request, runner)?;
    if let Some(response) = machines::response_for_state(request, &machines, "cleanup_verify") {
        return Ok(response);
    }
    if has_profile_id(&machines, profile_id) {
        return Ok(response(
            request,
            ResponseStatus::Failed,
            data(profile_id, "herdr_cleanup_unverified"),
        ));
    }
    Ok(cleaned(request, profile_id, ResponseStatus::Changed))
}

fn cleaned(request: &AdapterRequest, profile_id: &str, status: ResponseStatus) -> AdapterResponse {
    response(request, status, data(profile_id, "herdr_cleaned"))
}

fn data(profile_id: &str, status: &str) -> Map<String, Value> {
    let mut data = Map::new();
    data.insert("status".to_string(), json!(status));
    data.insert("profile_id".to_string(), json!(profile_id));
    data
}

fn has_profile(machines: &Value, profile: &PreviousProfile) -> bool {
    rows(machines).into_iter().flatten().any(|machine| {
        machines::profile_id(machine) == Some(profile.id.as_str())
            && machine.get("target") == Some(&json!(&profile.target))
            && machine.get("session") == Some(&json!(&profile.session))
    })
}

fn has_profile_id(machines: &Value, profile_id: &str) -> bool {
    rows(machines)
        .into_iter()
        .flatten()
        .any(|machine| machines::profile_id(machine) == Some(profile_id))
}

fn previous_profile(request: &AdapterRequest) -> Option<PreviousProfile> {
    previous_values(request).into_iter().find_map(|value| {
        let id = value
            .get("profile_id")
            .or_else(|| value.get("machine_id"))
            .and_then(Value::as_str)
            .or_else(|| value.pointer("/machine/id").and_then(Value::as_str))?;
        let target = value
            .get("target")
            .and_then(Value::as_str)
            .or_else(|| value.pointer("/machine/target").and_then(Value::as_str))?;
        let session = value
            .get("session")
            .and_then(Value::as_str)
            .or_else(|| value.pointer("/machine/session").and_then(Value::as_str))?;
        Some(PreviousProfile {
            id: id.to_string(),
            target: target.to_string(),
            session: session.to_string(),
        })
    })
}

fn previous_values(request: &AdapterRequest) -> Vec<&Value> {
    let mut values: Vec<&Value> = [
        request.previous.as_ref(),
        request
            .previous
            .as_ref()
            .and_then(|value| value.get("data")),
        request.input.get("apply"),
        request.input.get("receipt"),
    ]
    .into_iter()
    .flatten()
    .collect();
    if let Some(receipts) = request
        .input
        .get("integration_receipts")
        .and_then(Value::as_array)
    {
        values.extend(
            receipts
                .iter()
                .filter_map(|receipt| receipt.pointer("/response/data")),
        );
    }
    values
}

struct PreviousProfile {
    id: String,
    target: String,
    session: String,
}

fn rows(machines: &Value) -> Option<&Vec<Value>> {
    machines
        .as_array()
        .or_else(|| machines.get("machines").and_then(Value::as_array))
}
