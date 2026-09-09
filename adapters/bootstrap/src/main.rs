//! Bootstrap adapter for installing Nix, devenv, and seed tools.
mod config;
mod privilege;
mod scripts;
#[cfg(test)]
mod tests;

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::Result;
use config::BootstrapConfig;
use privilege::{can_install_without_privilege, has_privilege, privilege_required};
use scripts::{install_script, probe_script};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use workenv_platform::{ApocExecutor, ExecutionOutput, ExecutionSpec, Executor};
use workenv_protocol::{AdapterRequest, AdapterResponse, ResponseStatus, serve};

fn main() -> Result<()> {
    serve(handle)
}

fn handle(request: &AdapterRequest) -> Result<AdapterResponse> {
    match request.operation.as_str() {
        "inspect" => inspect(request),
        "bootstrap" | "install" => bootstrap(request),
        _ => Ok(response(
            request,
            ResponseStatus::Unsupported,
            json!({}),
            Some("unsupported bootstrap operation"),
        )),
    }
}

fn inspect(request: &AdapterRequest) -> Result<AdapterResponse> {
    let config = BootstrapConfig::from_request(request)?;
    let (report, pending_id) = probe(request, &config, "inspect")?;
    let status = if report["ready"] == true {
        ResponseStatus::Ready
    } else {
        ResponseStatus::Pending
    };
    Ok(with_execution_id(
        AdapterResponse::new(request, status, report),
        pending_id,
    ))
}

fn bootstrap(request: &AdapterRequest) -> Result<AdapterResponse> {
    let config = BootstrapConfig::from_request(request)?;
    let (before, pending_id) = probe(request, &config, "probe-before")?;
    if pending_id.is_some() {
        return Ok(with_execution_id(
            AdapterResponse::new(request, ResponseStatus::Pending, before),
            pending_id,
        ));
    }
    if before["ready"] == true {
        return Ok(AdapterResponse::new(request, ResponseStatus::Ready, before));
    }
    if !can_install_without_privilege(request, &config, &before)? && !has_privilege(request)? {
        return Ok(response(
            request,
            ResponseStatus::Failed,
            privilege_required(request, &config),
            Some("bootstrap requires passwordless sudo or root"),
        ));
    }
    let output = run_target(
        request,
        &install_script(&config),
        Duration::from_millis(config.timeout_ms),
        "install",
    )?;
    if output.exit_code.is_none() {
        return Ok(with_execution_id(
            AdapterResponse::new(request, ResponseStatus::Pending, output_data(&output)),
            Some(output.execution_id),
        ));
    }
    if output.exit_code != Some(0) {
        return Ok(response(
            request,
            ResponseStatus::Failed,
            output_data(&output),
            Some("bootstrap install command failed"),
        ));
    }
    let (after, pending_id) = probe(request, &config, "probe-after")?;
    if pending_id.is_some() {
        return Ok(with_execution_id(
            AdapterResponse::new(request, ResponseStatus::Pending, after),
            pending_id,
        ));
    }
    let (status, error) = if after["ready"] == true {
        (ResponseStatus::Changed, None)
    } else {
        (
            ResponseStatus::Failed,
            Some("bootstrap install completed but prerequisites are still missing"),
        )
    };
    Ok(response(request, status, after, error))
}

fn probe(
    request: &AdapterRequest,
    config: &BootstrapConfig,
    phase: &str,
) -> Result<(Value, Option<String>)> {
    let output = run_target(
        request,
        &probe_script(config),
        Duration::from_secs(30),
        phase,
    )?;
    if output.exit_code.is_none() {
        let id = output.execution_id.clone();
        return Ok((
            json!({"ready":false,"status":"probe_pending","execution_id":id}),
            Some(output.execution_id),
        ));
    }
    if output.exit_code != Some(0) {
        return Ok((
            json!({"ready":false,"status":"probe_failed","stderr":output.stderr,
                "execution_id":output.execution_id}),
            None,
        ));
    }
    Ok((parse_probe(&output.stdout, config), None))
}

fn parse_probe(text: &str, config: &BootstrapConfig) -> Value {
    let mut rows = serde_json::Map::new();
    for line in text.lines() {
        if let Some((key, value)) = line.split_once('\t') {
            rows.insert(key.into(), json!(value));
        }
    }
    let nix_ready = rows
        .get("nix")
        .and_then(Value::as_str)
        .is_some_and(|v| v.contains(&config.nix_version));
    let devenv_ready = rows
        .get("devenv")
        .and_then(Value::as_str)
        .is_some_and(|v| v.contains(&config.devenv_version));
    let tool_rows = seed_tool_report(&rows, config);
    let tools_ready = tool_rows.iter().all(|tool| tool["ok"] == true);
    json!({"ready":nix_ready && devenv_ready && tools_ready,
        "status":if nix_ready && devenv_ready && tools_ready {"ready"} else {"needs_setup"},
        "platform":{"system":rows.get("system"),"machine":rows.get("machine")},
        "nix":{"expected":config.nix_version,"version_output":rows.get("nix"),"ok":nix_ready},
        "devenv":{"expected":config.devenv_version,"version_output":rows.get("devenv"),"ok":devenv_ready},
        "seed_tools":tool_rows,
        "shared_tools":"provided_by_devenv","cargo_runtime_required":false})
}

pub(crate) fn run_target(
    request: &AdapterRequest,
    script: &str,
    timeout: Duration,
    phase: &str,
) -> Result<ExecutionOutput> {
    let (executable, arg) = if let Some(address) = request.target.address.as_deref() {
        ("ssh".to_owned(), ssh_args(address, script))
    } else {
        ("bash".to_owned(), vec!["-lc".to_owned(), script.to_owned()])
    };
    ApocExecutor::new(std::env::current_dir()?).execute(ExecutionSpec {
        executable,
        arg,
        cwd: std::env::current_dir().ok(),
        stdin: None,
        timeout_ms: u64::try_from(timeout.as_millis()).unwrap_or(900_000),
        idempotency_key: idempotency_key(request, phase, script),
        purpose: "Run Workenv bootstrap adapter command through APoC.".into(),
    })
}

fn ssh_args(address: &str, script: &str) -> Vec<String> {
    [
        "-o",
        "BatchMode=yes",
        "-o",
        "ConnectTimeout=15",
        "-o",
        "StrictHostKeyChecking=yes",
        address,
        script,
    ]
    .iter()
    .map(ToString::to_string)
    .collect()
}

fn idempotency_key(request: &AdapterRequest, phase: &str, script: &str) -> String {
    let mut key = format!(
        "workenv-bootstrap:{}:{}:{}",
        request.request_id,
        phase,
        digest(script)
    );
    if phase != "install" {
        key.push(':');
        key.push_str(&observation_nonce());
    }
    key
}

fn digest(value: &str) -> String {
    format!("{:x}", Sha256::digest(value.as_bytes()))
}

fn observation_nonce() -> String {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or_else(
        |_| "0".to_owned(),
        |duration| duration.as_nanos().to_string(),
    )
}

fn output_data(output: &ExecutionOutput) -> Value {
    json!({"stdout":output.stdout.trim(),"stderr":output.stderr.trim(),
        "exit_code":output.exit_code,"execution_id":output.execution_id})
}

fn seed_tool_report(rows: &serde_json::Map<String, Value>, config: &BootstrapConfig) -> Vec<Value> {
    config
        .seed_tools
        .iter()
        .map(|tool| {
            let row = rows
                .get(&format!("tool.{}", tool.name))
                .and_then(Value::as_str)
                .unwrap_or("\tmissing");
            let (path, hash) = row.split_once('\t').unwrap_or(("", "missing"));
            json!({"name":tool.name,"path":path,"sha256":hash,
                "expected_sha256":tool.sha256,"ok":hash == tool.sha256})
        })
        .collect()
}

fn response(
    request: &AdapterRequest,
    status: ResponseStatus,
    data: Value,
    error: Option<&str>,
) -> AdapterResponse {
    let mut response = AdapterResponse::new(request, status, data);
    response.error = error.map(str::to_owned);
    response
}

fn with_execution_id(
    mut response: AdapterResponse,
    execution_id: Option<String>,
) -> AdapterResponse {
    response.execution_id = execution_id;
    response
}
