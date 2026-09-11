//! Running one exact command inside a guest, as a transport.
//!
//! `workenv-core` ships a target-located extension to its host by calling the
//! host's transport with `execute`, so a host without a transport can only run
//! controller-located extensions. Every existing transport dials
//! `target.address`, which a scheduled guest does not have -- so before this,
//! an Orchard-backed host could be created and then could not run `identity`
//! or `clipboard` at all.
//!
//! The outer hop is `orchard ssh vm <name>`, chosen against measurement rather
//! than assumption. Probed against a live guest:
//!
//! | property | result |
//! |---|---|
//! | stdout cleanliness | exactly the child's bytes, no banner |
//! | stdin | passes through |
//! | exit code | **not propagated** -- `exit 7` surfaces as 1 |
//! | stderr | carries an orchard credentials banner ahead of the child's |
//!
//! The last two are why the child's result is not read off the process at all.
//! The remote script captures the child's own streams and status and prints
//! one JSON object on stdout, which is the stream measured to be clean. A
//! transport that trusted orchard's exit code would report every failure as 1
//! and every banner line as program output.
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use serde_json::{Value, json};
use workenv_platform::{ExecutionSpec, Executor};
use workenv_protocol::{AdapterRequest, AdapterResponse, ResponseStatus};

use super::response;

/// Milliseconds allowed for one guest command before the transport gives up.
const DEFAULT_TIMEOUT_MS: u64 = 300_000;

/// Exit statuses the wrapper reserves for its own failures.
///
/// Chosen above any status a child realistically returns so a wrapper fault is
/// never mistaken for the command's own result.
const WRAPPER_MKTEMP_FAILED: i64 = 121;
const WRAPPER_CHDIR_FAILED: i64 = 122;

/// Build the remote script that runs one command and reports its own result.
pub(super) fn script(argv: &[String], cwd: &str, stdin: &str) -> String {
    let command = argv
        .iter()
        .map(|arg| quote(arg))
        .collect::<Vec<_>>()
        .join(" ");
    format!(
        "set -u\n\
         d=$(mktemp -d) || exit {WRAPPER_MKTEMP_FAILED}\n\
         trap 'rm -rf \"$d\"' EXIT\n\
         printf %s {stdin} | base64 -d > \"$d/in\"\n\
         cd {cwd} || exit {WRAPPER_CHDIR_FAILED}\n\
         {command} < \"$d/in\" > \"$d/out\" 2> \"$d/err\"\n\
         c=$?\n\
         printf '{{\"exit_code\":%d,\"stdout\":\"%s\",\"stderr\":\"%s\"}}' \"$c\" \
         \"$(base64 < \"$d/out\" | tr -d '\\n')\" \"$(base64 < \"$d/err\" | tr -d '\\n')\"\n",
        stdin = quote(&BASE64.encode(stdin.as_bytes())),
        cwd = quote(cwd),
    )
}

/// The argv that carries one script into a guest.
///
/// The script travels base64-encoded. That alphabet contains no shell
/// metacharacter, so it needs no quoting of its own and cannot be re-split by
/// the guest's shell however the script itself is written.
pub(super) fn carrier_argv(guest: &str, script: &str) -> Vec<String> {
    let encoded = BASE64.encode(script.as_bytes());
    vec![
        "orchard".to_owned(),
        "ssh".to_owned(),
        "vm".to_owned(),
        guest.to_owned(),
        format!("echo {encoded} | base64 -d | bash"),
    ]
}

/// Read the transport inputs `workenv-core` sends, refusing an empty argv.
pub(super) fn plan(request: &AdapterRequest) -> Result<(Vec<String>, String, String, u64), String> {
    let argv = request
        .input
        .get("argv")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(ToOwned::to_owned)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if argv.is_empty() {
        return Err("execute needs a non-empty argv".to_owned());
    }
    let cwd = request
        .input
        .get("cwd")
        .and_then(Value::as_str)
        .unwrap_or("/")
        .to_owned();
    let stdin = request
        .input
        .get("stdin")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let timeout = request
        .input
        .get("timeout_ms")
        .and_then(Value::as_u64)
        .unwrap_or(DEFAULT_TIMEOUT_MS);
    Ok((argv, cwd, stdin, timeout))
}

/// Turn the wrapper's JSON into the `{exit_code, stdout, stderr}` core expects.
///
/// Anything that is not the wrapper's object is a transport fault, never a
/// command result. Reporting it as `exit_code: 0` with empty output would let a
/// broken tunnel read as a command that succeeded and printed nothing.
pub(super) fn interpret(raw: &str) -> Result<Value, String> {
    let parsed: Value = serde_json::from_str(raw.trim())
        .map_err(|_| format!("guest did not return a transport result: {}", excerpt(raw)))?;
    let code = parsed
        .get("exit_code")
        .and_then(Value::as_i64)
        .ok_or_else(|| "transport result has no exit_code".to_owned())?;
    match code {
        WRAPPER_MKTEMP_FAILED => return Err("guest could not create a temp dir".to_owned()),
        WRAPPER_CHDIR_FAILED => return Err("guest could not enter the target directory".to_owned()),
        _ => {}
    }
    Ok(json!({
        "exit_code": code,
        "stdout": decode(&parsed, "stdout"),
        "stderr": decode(&parsed, "stderr"),
    }))
}

/// Run one command inside the guest and report the command's own result.
pub(super) fn run(
    request: &AdapterRequest,
    guest: &str,
    executor: &dyn Executor,
) -> AdapterResponse {
    let (argv, cwd, stdin, timeout) = match plan(request) {
        Ok(plan) => plan,
        Err(error) => return response(request, ResponseStatus::Failed, json!({}), Some(&error)),
    };
    let carrier = carrier_argv(guest, &script(&argv, &cwd, &stdin));
    let spec = ExecutionSpec {
        executable: carrier[0].clone(),
        arg: carrier[1..].to_vec(),
        cwd: None,
        stdin: None,
        timeout_ms: timeout,
        // Keyed on the controller's request so a retry returns the original
        // receipt instead of running the command inside the guest twice.
        idempotency_key: format!("workenv-orchard-execute:{}", request.request_id),
        purpose: format!("Run {} in Orchard guest {guest}.", argv.join(" ")),
    };
    let output = match executor.execute(spec) {
        Ok(output) => output,
        Err(error) => {
            return response(
                request,
                ResponseStatus::Failed,
                json!({}),
                Some(&error.to_string()),
            );
        }
    };
    match interpret(&output.stdout) {
        Ok(mut data) => {
            data["guest"] = json!(guest);
            let mut answer = response(request, ResponseStatus::Ready, data, None);
            answer.execution_id = Some(output.execution_id);
            answer
        }
        Err(error) => response(request, ResponseStatus::Failed, json!({}), Some(&error)),
    }
}

/// Decode one base64 stream, treating undecodable bytes as empty.
fn decode(parsed: &Value, field: &str) -> String {
    parsed
        .get(field)
        .and_then(Value::as_str)
        .and_then(|encoded| BASE64.decode(encoded).ok())
        .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
        .unwrap_or_default()
}

/// Quote one argument for the guest's shell.
fn quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

/// A short, single-line sample of unexpected output for an error message.
fn excerpt(raw: &str) -> String {
    let flat = raw.replace('\n', " ");
    flat.chars().take(120).collect()
}
