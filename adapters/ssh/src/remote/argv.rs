use anyhow::{Context, Result};
use base64::{Engine, engine::general_purpose::STANDARD as BASE64};
use workenv_protocol::AdapterRequest;

pub(super) fn start_argv(
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

pub(super) fn stdin_wrapper_args(
    stdin: &str,
    executable: &str,
    args: &[String],
    cwd: &str,
) -> Vec<String> {
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

pub(super) fn wait_argv(id: &str, purpose: &str, timeout: u64) -> Vec<String> {
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

pub(super) fn logs_argv(id: &str, purpose: &str) -> Vec<String> {
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

pub(super) fn ssh_args(address: &str, remote: &[String]) -> Vec<String> {
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
