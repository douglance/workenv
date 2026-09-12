//! Native adapter for explicit Tailscale inspection and enrollment.

use anyhow::{Result, bail};
use serde_json::{Map, Value, json};
use workenv_platform::{ApocExecutor, ExecutionSpec, Executor};

mod cleanup;
mod enroll;
mod status;
mod util;

use status::Report;
use util::{optional_string, output_json, pending_response, redacted, response};
use workenv_protocol::{AdapterRequest, AdapterResponse, ResponseStatus};

/// Handle one Tailscale request using `APoC` for command execution.
///
/// # Errors
/// Returns an error when the working directory, request, execution, or API call fails.
pub fn handle(request: &AdapterRequest) -> Result<AdapterResponse> {
    let runner = ApocExecutor::new(std::env::current_dir()?);
    handle_with(request, &runner)
}

/// Handle one Tailscale request with an injected runner.
///
/// # Errors
/// Returns an error when the request, execution, or API call fails.
pub fn handle_with(request: &AdapterRequest, runner: &impl Executor) -> Result<AdapterResponse> {
    match request.operation.as_str() {
        "inspect" => inspect(request, runner),
        "enroll" | "connect" | "apply" => enroll(request, runner),
        "cleanup" => cleanup::run(request, runner),
        operation => bail!("unsupported Tailscale operation {operation}"),
    }
}

fn inspect(request: &AdapterRequest, runner: &impl Executor) -> Result<AdapterResponse> {
    let status = match command_json(
        request,
        runner,
        ["tailscale", "status", "--json"],
        "tailscale_status_pending",
        "tailscale_status_failed",
    )? {
        CommandJson::Ready(value) => value,
        CommandJson::Pending {
            execution_id,
            status,
        } => return Ok(command_pending(request, execution_id, status)),
        CommandJson::Failed {
            execution_id,
            status,
            stderr,
        } => return Ok(command_failed(request, &execution_id, status, &stderr)),
    };
    let prefs = match command_json(
        request,
        runner,
        ["tailscale", "debug", "prefs"],
        "tailscale_prefs_pending",
        "tailscale_prefs_failed",
    )? {
        CommandJson::Ready(value) => Some(value),
        CommandJson::Pending {
            execution_id,
            status,
        } => return Ok(command_pending(request, execution_id, status)),
        CommandJson::Failed { .. } => None,
    };
    let report = evaluate(request, &status, prefs.as_ref());
    Ok(response(request, report.status, report.data))
}

fn enroll(request: &AdapterRequest, runner: &impl Executor) -> Result<AdapterResponse> {
    let before = inspect(request, runner)?;
    if before.complete() {
        return Ok(before);
    }
    if before.status == ResponseStatus::Pending {
        return Ok(before);
    }
    let key = enroll::read_auth_key(request)?;
    let command = enroll::command(request);
    let output = runner.execute(ExecutionSpec {
        executable: command.executable,
        arg: command.args,
        cwd: Some(request.target.directory.clone()),
        stdin: Some(key.into_bytes()),
        idempotency_key: format!("{}:tailscale-enroll", request.request_id),
        purpose: "Enroll Tailscale using a caller-supplied auth key reference.".to_string(),
        timeout_ms: 120_000,
    })?;
    if output.exit_code.is_none() {
        let mut data = Map::new();
        data.insert("status".to_string(), json!("tailscale_enroll_pending"));
        data.insert(
            "execution_id".to_string(),
            json!(output.execution_id.clone()),
        );
        return Ok(pending_response(request, output.execution_id, data));
    }
    if output.exit_code != Some(0) {
        let mut data = Map::new();
        data.insert("status".to_string(), json!("tailscale_enroll_failed"));
        data.insert("stderr".to_string(), json!(redacted(&output.stderr)));
        data.insert("execution_id".to_string(), json!(output.execution_id));
        return Ok(response(request, ResponseStatus::Failed, data));
    }
    inspect(request, runner)
}

fn command_json<const N: usize>(
    request: &AdapterRequest,
    runner: &impl Executor,
    argv: [&str; N],
    pending_status: &'static str,
    failed_status: &'static str,
) -> Result<CommandJson> {
    let mut command_args = argv.into_iter().map(ToOwned::to_owned).collect::<Vec<_>>();
    let executable = command_args.remove(0);
    let output = runner.execute(ExecutionSpec {
        executable,
        arg: command_args,
        cwd: Some(request.target.directory.clone()),
        stdin: None,
        idempotency_key: format!("{}:{}", request.request_id, argv[1]),
        purpose: "Inspect Tailscale state.".to_string(),
        timeout_ms: 60_000,
    })?;
    if output.exit_code.is_none() {
        return Ok(CommandJson::Pending {
            execution_id: output.execution_id,
            status: pending_status,
        });
    }
    if output.exit_code != Some(0) {
        return Ok(CommandJson::Failed {
            execution_id: output.execution_id,
            status: failed_status,
            stderr: redacted(&output.stderr),
        });
    }
    Ok(CommandJson::Ready(output_json(&output)?))
}

/// One refusal, carrying whatever the branch already recorded.
///
/// Extracted because five branches repeated the same four lines, which put
/// `evaluate` over this repository's 60-line block limit and buried the actual
/// decision in each arm under boilerplate.
fn refused(mut data: Map<String, Value>, status: &str) -> Report {
    data.insert("status".to_string(), json!(status));
    Report {
        status: ResponseStatus::Failed,
        data,
    }
}

fn evaluate(request: &AdapterRequest, status: &Value, prefs: Option<&Value>) -> Report {
    let mut data = Map::new();
    data.insert("tailscale".to_string(), status.clone());
    if let Some(prefs) = prefs {
        data.insert("prefs".to_string(), prefs.clone());
    }
    let dns = dns_name(status);
    let tailnet = tailnet(status);
    let device_id = device_id(status);
    let required_tag =
        optional_string(&request.config, "tag").unwrap_or_else(|| "tag:workenv".to_string());
    if status.get("BackendState") != Some(&json!("Running")) {
        return refused(data, "tailscale_not_ready");
    }
    // Without a configured suffix there is nothing to compare the node against,
    // and calling that a mismatch sent the operator to mint an auth key for a
    // node that was already enrolled and healthy.
    let Some(expected_tailnet) = expected_tailnet(request) else {
        data.insert("dns_name".to_string(), json!(dns));
        data.insert("tailnet".to_string(), json!(tailnet));
        return refused(data, "tailscale_tailnet_suffix_not_configured");
    };
    let expected_dns = format!("{}.{}", request.target.host, expected_tailnet);
    if dns != expected_dns || tailnet != expected_tailnet {
        data.insert("dns_name".to_string(), json!(dns));
        data.insert("expected_dns".to_string(), json!(expected_dns));
        data.insert("tailnet".to_string(), json!(tailnet));
        return refused(data, "tailscale_mismatch");
    }
    if !tags(status).iter().any(|tag| tag == &required_tag) {
        data.insert("required_tag".to_string(), json!(required_tag));
        return refused(data, "tailscale_configuration_blocked");
    }
    if !prefs_ready(prefs) {
        return refused(data, "tailscale_configuration_blocked");
    }
    status::ready(data, device_id, &dns, &tailnet)
}

fn prefs_ready(prefs: Option<&Value>) -> bool {
    prefs.is_some_and(|prefs| {
        prefs.get("WantRunning") == Some(&json!(true)) && prefs.get("RunSSH") == Some(&json!(true))
    })
}

fn expected_tailnet(request: &AdapterRequest) -> Option<String> {
    optional_string(&request.config, "tailnet_suffix")
}

fn dns_name(status: &Value) -> String {
    status
        .pointer("/Self/DNSName")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim_end_matches('.')
        .to_string()
}

fn device_id(status: &Value) -> Option<String> {
    status
        .pointer("/Self/ID")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
}

fn tailnet(status: &Value) -> String {
    status
        .pointer("/CurrentTailnet/MagicDNSSuffix")
        .or_else(|| status.pointer("/CurrentTailnet/Name"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

fn tags(status: &Value) -> Vec<String> {
    status
        .pointer("/Self/Tags")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(ToOwned::to_owned)
        .collect()
}

enum CommandJson {
    Ready(Value),
    Pending {
        execution_id: String,
        status: &'static str,
    },
    Failed {
        execution_id: String,
        status: &'static str,
        stderr: String,
    },
}

fn command_pending(
    request: &AdapterRequest,
    execution_id: String,
    status: &str,
) -> AdapterResponse {
    let mut data = Map::new();
    data.insert("status".to_string(), json!(status));
    data.insert("execution_id".to_string(), json!(execution_id.clone()));
    pending_response(request, execution_id, data)
}

fn command_failed(
    request: &AdapterRequest,
    execution_id: &str,
    status: &str,
    stderr: &str,
) -> AdapterResponse {
    let mut data = Map::new();
    data.insert("status".to_string(), json!(status));
    data.insert("execution_id".to_string(), json!(execution_id));
    data.insert("stderr".to_string(), json!(stderr));
    response(request, ResponseStatus::Failed, data)
}
