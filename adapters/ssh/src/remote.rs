use anyhow::{Context, Result};
use base64::{Engine, engine::general_purpose::STANDARD as BASE64};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use workenv_platform::{ApocExecutor, ExecutionSpec, Executor};
use workenv_protocol::AdapterRequest;

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
    let start_argv = start_argv(input.request, input.argv, input.cwd, input.purpose)?;
    let start = remote_json(RemoteCommand {
        address: input.address,
        remote: &start_argv,
        timeout_ms: input.timeout.min(30_000),
        request_id: &input.request.request_id,
        phase: "start",
        purpose: input.purpose,
    })?;
    let id = execution_id(&start).context("remote APoC start returned no execution ID")?;
    let wait_argv = wait_argv(&id, input.purpose, input.timeout);
    let Ok(waited) = remote_json(RemoteCommand {
        address: input.address,
        remote: &wait_argv,
        timeout_ms: input.timeout.min(30_000),
        request_id: &input.request.request_id,
        phase: "wait",
        purpose: input.purpose,
    }) else {
        return Ok(pending(id));
    };
    if waited["outcome"] == "pending" || waited["status"] == "pending" {
        return Ok(pending(id));
    }
    let logs_argv = logs_argv(&id, input.purpose);
    let logs = remote_json(RemoteCommand {
        address: input.address,
        remote: &logs_argv,
        timeout_ms: 30_000,
        request_id: &input.request.request_id,
        phase: "logs",
        purpose: input.purpose,
    })?;
    let code = exit_code(&waited).unwrap_or_else(|| i64::from(waited["outcome"] != "passed"));
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

fn start_argv(
    request: &AdapterRequest,
    argv: &[String],
    cwd: &str,
    purpose: &str,
) -> Result<Vec<String>> {
    let executable = argv.first().context("execute argv must not be empty")?;
    let mut remote = vec!["apoc".into(), "execution".into(), "start".into()];
    let child_args = if let Some(stdin) = request.input["stdin"].as_str() {
        remote.push("/bin/bash".into());
        stdin_wrapper_args(stdin, executable, &argv[1..], cwd)
    } else {
        remote.push(executable.clone());
        argv.iter().skip(1).cloned().collect()
    };
    remote.extend(apoc_start_options(cwd, purpose, &request.request_id));
    remote.extend(child_args);
    Ok(remote)
}

fn apoc_start_options(cwd: &str, purpose: &str, key: &str) -> Vec<String> {
    [
        "--cwd",
        cwd,
        "--purpose",
        purpose,
        "--idempotency-key",
        key,
        "--format",
        "json",
        "--verbosity",
        "trace",
        "--expect-exit-code",
        "0",
        "--",
    ]
    .iter()
    .map(ToString::to_string)
    .collect()
}

fn stdin_wrapper_args(stdin: &str, executable: &str, args: &[String], cwd: &str) -> Vec<String> {
    let encoded = BASE64.encode(stdin.as_bytes());
    let script = format!(
        "p=$(mktemp) || exit; trap 'rm -f \"$p\"' EXIT; printf %s {} | base64 -d > \"$p\" || exit; cd {} || exit; exec \"$@\" < \"$p\"",
        shell_quote(&encoded),
        shell_quote(cwd)
    );
    let mut out = vec!["-lc".into(), script];
    out.extend(["workenv-stdin".into(), executable.into()]);
    out.extend(args.iter().cloned());
    out
}

fn wait_argv(id: &str, purpose: &str, timeout: u64) -> Vec<String> {
    vec![
        "apoc".into(),
        "execution".into(),
        "wait".into(),
        id.into(),
        "--timeout-ms".into(),
        timeout.to_string(),
        "--purpose".into(),
        purpose.into(),
        "--format".into(),
        "json".into(),
    ]
}

fn logs_argv(id: &str, purpose: &str) -> Vec<String> {
    vec![
        "apoc".into(),
        "execution".into(),
        "logs".into(),
        id.into(),
        "--tail-bytes".into(),
        "16777216".into(),
        "--purpose".into(),
        purpose.into(),
        "--format".into(),
        "json".into(),
    ]
}

#[derive(Clone, Copy)]
struct RemoteCommand<'a> {
    address: &'a str,
    remote: &'a [String],
    timeout_ms: u64,
    request_id: &'a str,
    phase: &'a str,
    purpose: &'a str,
}

fn remote_json(command: RemoteCommand<'_>) -> Result<Value> {
    let output = ApocExecutor::new(std::env::current_dir()?).execute(ExecutionSpec {
        executable: "ssh".into(),
        arg: ssh_args(command.address, command.remote),
        cwd: std::env::current_dir().ok(),
        stdin: None,
        timeout_ms: command.timeout_ms,
        idempotency_key: format!(
            "{}:ssh:{}:{}",
            command.request_id,
            command.phase,
            digest(command.remote)
        ),
        purpose: command.purpose.into(),
    })?;
    serde_json::from_str(&output.stdout)
        .with_context(|| format!("remote command returned invalid JSON: {}", output.stderr))
}

pub(crate) fn ssh_argv(address: &str) -> Vec<&str> {
    vec![
        "ssh",
        "-o",
        "BatchMode=yes",
        "-o",
        "ConnectTimeout=15",
        "-o",
        "StrictHostKeyChecking=yes",
        address,
    ]
}

fn ssh_args(address: &str, remote: &[String]) -> Vec<String> {
    vec![
        "-o".into(),
        "BatchMode=yes".into(),
        "-o".into(),
        "ConnectTimeout=15".into(),
        "-o".into(),
        "StrictHostKeyChecking=yes".into(),
        address.into(),
        shell_join(remote),
    ]
}

fn shell_join(argv: &[String]) -> String {
    argv.iter()
        .map(|arg| shell_quote(arg))
        .collect::<Vec<_>>()
        .join(" ")
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
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
mod tests {
    use super::*;

    #[test]
    fn stdin_wrapper_preserves_executable_argument() -> Result<()> {
        let args = stdin_wrapper_args("hello", "cat", &[], ".");
        let output = std::process::Command::new("bash").args(args).output()?;
        assert!(output.status.success());
        assert_eq!(String::from_utf8_lossy(&output.stdout), "hello");
        Ok(())
    }
}
