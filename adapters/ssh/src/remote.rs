use std::{
    path::Path,
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

mod argv;

use anyhow::{Context, Result};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use workenv_platform::{ApocExecutor, ExecutionSpec, Executor};
use workenv_protocol::AdapterRequest;

pub(crate) use argv::ssh_argv;
#[cfg(test)]
use argv::stdin_wrapper_args;
use argv::{logs_argv, ssh_args, start_argv, wait_argv};

const OUTER_SSH_TIMEOUT_MS: u64 = 30_000;
const REMOTE_WAIT_TIMEOUT_MS: u64 = 25_000;
static OBSERVATION_SEQUENCE: AtomicU64 = AtomicU64::new(1);

pub(crate) struct ExecuteResult {
    pub(crate) data: Value,
    pub(crate) execution_id: Option<String>,
}

#[derive(Clone, Copy)]
pub(crate) struct RemoteExecute<'a> {
    pub(crate) address: &'a str,
    pub(crate) request: &'a AdapterRequest,
    pub(crate) argv: &'a [String],
    pub(crate) cwd: &'a str,
    pub(crate) purpose: &'a str,
    pub(crate) timeout: u64,
}

pub(crate) fn execute_remote(input: RemoteExecute<'_>) -> Result<ExecuteResult> {
    let cwd = std::env::current_dir()?;
    let executor = ApocExecutor::new(cwd.clone());
    execute_remote_with(input, &executor, &cwd)
}

fn execute_remote_with(
    input: RemoteExecute<'_>,
    executor: &dyn Executor,
    local_cwd: &Path,
) -> Result<ExecuteResult> {
    let id = start_remote(input, executor, local_cwd)?;
    let Some(waited) = wait_remote(input, executor, local_cwd, &id) else {
        return Ok(pending(id));
    };
    if outcome(&waited).is_none() {
        return Ok(remote_error(&id, &waited));
    }
    completed_result(input, executor, local_cwd, &id, &waited)
}

fn start_remote(
    input: RemoteExecute<'_>,
    executor: &dyn Executor,
    local_cwd: &Path,
) -> Result<String> {
    let start_argv = start_argv(input.request, input.argv, input.cwd, input.purpose)?;
    let start = remote_json(
        RemoteCommand {
            address: input.address,
            remote: &start_argv,
            timeout_ms: input.timeout.min(OUTER_SSH_TIMEOUT_MS),
            request_id: &input.request.request_id,
            phase: "start",
            purpose: input.purpose,
            key: RemoteKey::Stable,
        },
        executor,
        local_cwd,
    )?;
    execution_id(&start).context("remote APoC start returned no execution ID")
}

fn wait_remote(
    input: RemoteExecute<'_>,
    executor: &dyn Executor,
    local_cwd: &Path,
    id: &str,
) -> Option<Value> {
    let wait_argv = wait_argv(id, input.purpose, remote_wait_timeout(input.timeout));
    let Ok(waited) = remote_json(
        RemoteCommand {
            address: input.address,
            remote: &wait_argv,
            timeout_ms: OUTER_SSH_TIMEOUT_MS,
            request_id: &input.request.request_id,
            phase: "wait",
            purpose: input.purpose,
            key: RemoteKey::Fresh,
        },
        executor,
        local_cwd,
    ) else {
        return None;
    };
    if observation_waits(&waited) {
        return None;
    }
    Some(waited)
}

fn completed_result(
    input: RemoteExecute<'_>,
    executor: &dyn Executor,
    local_cwd: &Path,
    id: &str,
    waited: &Value,
) -> Result<ExecuteResult> {
    let logs_argv = logs_argv(id, input.purpose);
    let logs = remote_json(
        RemoteCommand {
            address: input.address,
            remote: &logs_argv,
            timeout_ms: OUTER_SSH_TIMEOUT_MS,
            request_id: &input.request.request_id,
            phase: "logs",
            purpose: input.purpose,
            key: RemoteKey::Fresh,
        },
        executor,
        local_cwd,
    )?;
    let code = exit_code(waited).unwrap_or_else(|| i64::from(outcome(waited) != Some("passed")));
    Ok(ExecuteResult {
        data: json!({"stdout":logs["stdout"].as_str().unwrap_or_default(),
            "stderr":logs["stderr"].as_str().unwrap_or_default(),
            "exit_code":code,"execution_id":id}),
        execution_id: None,
    })
}

fn pending(id: String) -> ExecuteResult {
    ExecuteResult {
        data: json!({"execution_id":id}),
        execution_id: Some(id),
    }
}

fn remote_error(id: &str, error: &Value) -> ExecuteResult {
    ExecuteResult {
        data: json!({"stdout":"","stderr":error.to_string(),"exit_code":1,"execution_id":id}),
        execution_id: None,
    }
}

fn observation_waits(value: &Value) -> bool {
    value["outcome"] == "pending" || value["status"] == "pending" || value["code"] == "TIMEOUT"
}

fn outcome(value: &Value) -> Option<&str> {
    value.get("outcome").and_then(Value::as_str)
}

fn remote_wait_timeout(timeout: u64) -> u64 {
    timeout.min(REMOTE_WAIT_TIMEOUT_MS)
}

#[derive(Clone, Copy)]
struct RemoteCommand<'a> {
    address: &'a str,
    remote: &'a [String],
    timeout_ms: u64,
    request_id: &'a str,
    phase: &'a str,
    purpose: &'a str,
    key: RemoteKey,
}

#[derive(Clone, Copy)]
enum RemoteKey {
    Stable,
    Fresh,
}

fn remote_json(
    command: RemoteCommand<'_>,
    executor: &dyn Executor,
    local_cwd: &Path,
) -> Result<Value> {
    let output = executor.execute(ExecutionSpec {
        executable: "ssh".into(),
        arg: ssh_args(command.address, command.remote),
        cwd: Some(local_cwd.to_owned()),
        stdin: None,
        timeout_ms: command.timeout_ms,
        idempotency_key: idempotency_key(command),
        purpose: command.purpose.into(),
    })?;
    serde_json::from_str(&output.stdout)
        .with_context(|| format!("remote command returned invalid JSON: {}", output.stderr))
}

fn idempotency_key(command: RemoteCommand<'_>) -> String {
    match command.key {
        RemoteKey::Stable => format!(
            "{}:ssh:{}:{}",
            command.request_id,
            command.phase,
            digest(command.remote)
        ),
        RemoteKey::Fresh => fresh_observation_key(command),
    }
}

fn fresh_observation_key(command: RemoteCommand<'_>) -> String {
    let sequence = OBSERVATION_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos());
    format!(
        "{}:ssh:{}:{}:{now}:{sequence}",
        command.request_id,
        command.phase,
        digest(command.remote)
    )
}

fn digest(argv: &[String]) -> String {
    let body = serde_json::to_vec(argv).unwrap_or_else(|_| Vec::new());
    format!("{:x}", Sha256::digest(body))
}

fn execution_id(value: &Value) -> Option<String> {
    value
        .pointer("/execution/id")
        .or_else(|| value.get("id"))
        .and_then(Value::as_str)
        .map(str::to_owned)
}

fn exit_code(value: &Value) -> Option<i64> {
    value
        .pointer("/result/exit_code")
        .or_else(|| value.pointer("/execution/result/exit_code"))
        .or_else(|| value.get("exit_code"))
        .and_then(Value::as_i64)
}

#[cfg(test)]
#[path = "remote_tests.rs"]
mod tests;
