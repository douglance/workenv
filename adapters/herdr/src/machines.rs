use anyhow::Result;
use serde_json::{Map, Value, json};
use workenv_platform::{ExecutionSpec, Executor};
use workenv_protocol::{AdapterRequest, AdapterResponse, ResponseStatus};

use crate::util::{field_bool, output_json, pending_response, response};

pub(crate) fn list_from_controller(
    request: &AdapterRequest,
    runner: &impl Executor,
) -> Result<Value> {
    run_list(
        runner,
        &format!("{}:herdr-machines", request.request_id),
        "List local Herdr machine registrations.",
        None,
    )
}

pub(crate) fn list_after_register(
    request: &AdapterRequest,
    runner: &impl Executor,
) -> Result<Value> {
    run_list(
        runner,
        &format!("{}:herdr-machines-after-register", request.request_id),
        "Verify Herdr machine registration after add.",
        None,
    )
}

pub(crate) fn list_after_cleanup(
    request: &AdapterRequest,
    runner: &impl Executor,
) -> Result<Value> {
    run_list(
        runner,
        &format!("{}:herdr-machines-after-cleanup", request.request_id),
        "Verify Herdr machine cleanup after remove.",
        None,
    )
}

pub(crate) fn registered_response(
    request: &AdapterRequest,
    machine: Value,
    target: &str,
    session: &str,
) -> AdapterResponse {
    let mut data = Map::new();
    let enabled = field_bool(&machine, "enabled", false);
    let exact = machine.get("target") == Some(&json!(target))
        && machine.get("session") == Some(&json!(session))
        && enabled;
    let profile_id = profile_id(&machine).map(ToOwned::to_owned);
    data.insert("machine".to_string(), machine);
    data.insert("target".to_string(), json!(target));
    data.insert("session".to_string(), json!(session));
    if let Some(id) = &profile_id {
        data.insert("profile_id".to_string(), json!(id));
    }
    data.insert(
        "status".to_string(),
        json!(if exact && profile_id.is_some() {
            "already_registered"
        } else if exact {
            "herdr_profile_id_missing"
        } else {
            "herdr_profile_mismatch"
        }),
    );
    response(
        request,
        if exact && profile_id.is_some() {
            ResponseStatus::Ready
        } else {
            ResponseStatus::Failed
        },
        data,
    )
}

pub(crate) fn profile_id(machine: &Value) -> Option<&str> {
    machine.get("id").and_then(Value::as_str)
}

pub(crate) fn find(machines: &Value, label: &str, target: &str, session: &str) -> Option<Value> {
    rows(machines)?
        .iter()
        .find(|machine| {
            machine.get("label") == Some(&json!(label))
                || (machine.get("target") == Some(&json!(target))
                    && machine.get("session") == Some(&json!(session)))
        })
        .cloned()
}

pub(crate) fn find_exact(machines: &Value, target: &str, session: &str) -> Option<Value> {
    rows(machines)?
        .iter()
        .find(|machine| {
            machine.get("target") == Some(&json!(target))
                && machine.get("session") == Some(&json!(session))
                && field_bool(machine, "enabled", false)
        })
        .cloned()
}

pub(crate) fn response_for_state(
    request: &AdapterRequest,
    machines: &Value,
    status_prefix: &str,
) -> Option<AdapterResponse> {
    let execution_id = machines.get("execution_id").and_then(Value::as_str)?;
    let mut data = Map::new();
    data.insert("execution_id".to_string(), json!(execution_id));
    if flag(machines, "machines_pending") {
        data.insert(
            "status".to_string(),
            json!(format!("{status_prefix}_pending")),
        );
        return Some(pending_response(request, execution_id.to_string(), data));
    }
    if flag(machines, "machines_failed") {
        data.insert(
            "status".to_string(),
            json!(format!("{status_prefix}_failed")),
        );
        data.insert("stderr".to_string(), machines["stderr"].clone());
        return Some(response(request, ResponseStatus::Failed, data));
    }
    None
}

fn run_list(
    runner: &impl Executor,
    idempotency_key: &str,
    purpose: &str,
    cwd: Option<std::path::PathBuf>,
) -> Result<Value> {
    let output = runner.execute(ExecutionSpec {
        executable: "herdr".to_string(),
        arg: vec![
            "machine".to_string(),
            "list".to_string(),
            "--json".to_string(),
        ],
        cwd,
        stdin: None,
        idempotency_key: format!("{idempotency_key}:{}", uuid::Uuid::new_v4()),
        purpose: purpose.to_string(),
        timeout_ms: 60_000,
    })?;
    if output.exit_code.is_none() {
        return Ok(json!({"machines_pending":true,"execution_id":output.execution_id}));
    }
    if output.exit_code != Some(0) {
        return Ok(
            json!({"machines_failed":true,"execution_id":output.execution_id,"stderr":output.stderr}),
        );
    }
    output_json(&output)
}

fn rows(machines: &Value) -> Option<&Vec<Value>> {
    machines
        .as_array()
        .or_else(|| machines.get("machines").and_then(Value::as_array))
}

fn flag(value: &Value, key: &str) -> bool {
    value.get(key).and_then(Value::as_bool).unwrap_or(false)
}
