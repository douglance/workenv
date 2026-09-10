//! Native adapter for scoped Herdr configuration and registration.

use anyhow::{Context, Result, bail};
use serde_json::{Map, Value, json};
use workenv_platform::{ApocExecutor, ExecutionSpec, Executor};

mod cleanup;
mod machines;
mod util;

use util::{
    atomic_json, optional_string, output_json, pending_response, response, response_status,
};
use workenv_protocol::{AdapterRequest, AdapterResponse, ResponseStatus};

const HERDR_VERSION: &str = "0.9.0";
const HERDR_PROTOCOL: i64 = 22;

/// Handle one Herdr adapter request using `APoC` for command execution.
///
/// # Errors
///
/// Returns an error when the current directory or adapter operation fails.
pub fn handle(request: &AdapterRequest) -> Result<AdapterResponse> {
    let runner = ApocExecutor::new(std::env::current_dir()?);
    handle_with(request, &runner)
}

/// Handle one Herdr adapter request with an injected runner.
///
/// # Errors
///
/// Returns an error when a command, file write, or request validation fails.
pub fn handle_with(request: &AdapterRequest, runner: &impl Executor) -> Result<AdapterResponse> {
    match request.operation.as_str() {
        "inspect" => inspect(request, runner),
        "register" => register(request, runner),
        "cleanup" => cleanup::run(request, runner),
        "connect" => Ok(connect(request)),
        "config" | "apply" => configure(request),
        operation => bail!("unsupported Herdr operation {operation}"),
    }
}

fn inspect(request: &AdapterRequest, runner: &impl Executor) -> Result<AdapterResponse> {
    let session = session(request);
    let output = runner.execute(ExecutionSpec {
        executable: "herdr".to_string(),
        arg: vec![
            "--session".to_string(),
            session.clone(),
            "status".to_string(),
            "server".to_string(),
            "--json".to_string(),
        ],
        cwd: Some(request.target.directory.clone()),
        stdin: None,
        idempotency_key: format!("{}:herdr-status", request.request_id),
        purpose: "Inspect scoped Herdr server status.".to_string(),
        timeout_ms: 60_000,
    })?;
    let mut data = Map::new();
    data.insert("session".to_string(), json!(session));
    data.insert("execution_id".to_string(), json!(output.execution_id));
    if output.exit_code.is_none() {
        data.insert("status".to_string(), json!("herdr_status_pending"));
        return Ok(pending_response(request, output.execution_id, data));
    }
    if output.exit_code != Some(0) {
        data.insert("status".to_string(), json!("herdr_not_ready"));
        return Ok(response(request, ResponseStatus::Failed, data));
    }
    let server = output_json(&output)?;
    let ready = herdr_ready(&server);
    data.insert("server".to_string(), server);
    data.insert(
        "status".to_string(),
        json!(if ready {
            "herdr_ready"
        } else {
            "herdr_not_ready"
        }),
    );
    Ok(response(
        request,
        if ready {
            ResponseStatus::Ready
        } else {
            ResponseStatus::Failed
        },
        data,
    ))
}

fn register(request: &AdapterRequest, runner: &impl Executor) -> Result<AdapterResponse> {
    let target = target(request)?;
    let session = session(request);
    let machines = machines::list_from_controller(request, runner)?;
    if let Some(response) = machines::response_for_state(request, &machines, "registration_lookup")
    {
        return Ok(response);
    }
    let existing = machines::find(&machines, &request.target.host, &target, &session);
    if let Some(machine) = existing {
        return Ok(machines::registered_response(
            request, machine, &target, &session,
        ));
    }
    let output = runner.execute(ExecutionSpec {
        executable: "herdr".to_string(),
        arg: vec![
            "machine".to_string(),
            "add".to_string(),
            target.clone(),
            "--label".to_string(),
            request.target.host.clone(),
            "--remote-session".to_string(),
            session.clone(),
        ],
        cwd: None,
        stdin: None,
        idempotency_key: format!("{}:herdr-register", request.request_id),
        purpose: "Register scoped Herdr machine connection.".to_string(),
        timeout_ms: 60_000,
    })?;
    let mut data = Map::new();
    data.insert("status".to_string(), json!("registered"));
    data.insert("target".to_string(), json!(target));
    data.insert("session".to_string(), json!(session));
    data.insert("execution_id".to_string(), json!(output.execution_id));
    if output.exit_code.is_none() {
        data.insert("status".to_string(), json!("registration_pending"));
        return Ok(pending_response(request, output.execution_id, data));
    }
    if output.exit_code != Some(0) {
        data.insert("status".to_string(), json!("registration_failed"));
        data.insert("stderr".to_string(), json!(output.stderr));
        return Ok(response(request, ResponseStatus::Failed, data));
    }
    let machines = machines::list_after_register(request, runner)?;
    if let Some(response) =
        machines::response_for_state(request, &machines, "registration_verification")
    {
        return Ok(response);
    }
    if let Some(machine) = machines::find_exact(&machines, &target, &session) {
        return Ok(registered_changed_response(request, machine, data));
    }
    data.insert("status".to_string(), json!("registration_unverified"));
    Ok(response(request, ResponseStatus::Failed, data))
}

fn registered_changed_response(
    request: &AdapterRequest,
    machine: Value,
    mut data: Map<String, Value>,
) -> AdapterResponse {
    let Some(profile_id) = machines::profile_id(&machine) else {
        data.insert("status".to_string(), json!("herdr_profile_id_missing"));
        data.insert("machine".to_string(), machine);
        return response(request, ResponseStatus::Failed, data);
    };
    data.insert("status".to_string(), json!("registered"));
    data.insert("profile_id".to_string(), json!(profile_id));
    data.insert("machine".to_string(), machine);
    response(request, ResponseStatus::Changed, data)
}

fn connect(request: &AdapterRequest) -> AdapterResponse {
    let session = session(request);
    let mut data = Map::new();
    data.insert("status".to_string(), json!("connection"));
    data.insert("session".to_string(), json!(session));
    if let Some(address) = &request.target.address {
        data.insert("target".to_string(), json!(address));
        data.insert(
            "attach_argv".to_string(),
            json!(["herdr", "--remote", address, "--session", session]),
        );
        data.insert("ssh_argv".to_string(), json!(["ssh", address]));
    } else {
        data.insert("target".to_string(), json!("local"));
        data.insert(
            "attach_argv".to_string(),
            json!(["herdr", "--session", session]),
        );
        data.insert("ssh_argv".to_string(), Value::Null);
    }
    response(request, ResponseStatus::Ready, data)
}

fn configure(request: &AdapterRequest) -> Result<AdapterResponse> {
    let profile = optional_string(&request.config, "profile");
    let digest = optional_string(&request.config, "profile_digest");
    let path = request
        .target
        .directory
        .join(".state/herdr/identity-profile.json");
    let payload = json!({
        "schema_version": 1,
        "session": session(request),
        "profile": profile,
        "profile_digest": digest,
    });
    let changed = atomic_json(&path, &payload)?;
    let mut data = Map::new();
    data.insert("status".to_string(), json!("herdr_configured"));
    data.insert("path".to_string(), json!(path));
    data.insert("profile".to_string(), payload["profile"].clone());
    Ok(response_status(request, changed, data))
}

fn herdr_ready(server: &Value) -> bool {
    server.get("running") == Some(&json!(true))
        && server.get("compatible") == Some(&json!(true))
        && server.get("version") == Some(&json!(HERDR_VERSION))
        && protocol(server) == Some(HERDR_PROTOCOL)
        && server.get("server_binary_stale") == Some(&json!(false))
        && server.pointer("/capabilities/detached_server_daemon") == Some(&json!(true))
}

fn protocol(server: &Value) -> Option<i64> {
    server
        .get("protocol")
        .or_else(|| server.get("protocol_version"))
        .and_then(Value::as_i64)
}

fn session(request: &AdapterRequest) -> String {
    optional_string(&request.config, "session").unwrap_or_else(|| "workenv".to_string())
}

fn target(request: &AdapterRequest) -> Result<String> {
    request
        .target
        .address
        .clone()
        .or_else(|| optional_string(&request.input, "target"))
        .or_else(|| optional_string(&request.config, "target"))
        .context("Herdr registration requires target address")
}
