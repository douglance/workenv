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
use workenv_platform::Executor;
use workenv_protocol::{AdapterRequest, AdapterResponse, ResponseStatus};

use super::carrier::Carrier;
use super::response;

/// Milliseconds allowed for one guest command before the transport gives up.
const DEFAULT_TIMEOUT_MS: u64 = 300_000;

/// Retries allowed when the carrier failed before running anything.
const RETRIES: u32 = 2;

/// Carrier stderr markers that mean the command never started.
///
/// Measured: `orchard ssh` answers "failed to setup port-forwarding to the VM
/// ... expected handshake response status code 101 but got 500" intermittently,
/// and six consecutive attempts succeeded immediately afterwards.
const SETUP_FAILURES: &[&str] = &[
    "failed to setup port-forwarding",
    "failed to WebSocket dial",
    "expected handshake response status code",
];

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
    let in_guest = carrier_argv(guest, &script(&argv, &cwd, &stdin));
    let carrier = Carrier::new(request, guest, argv, in_guest, timeout);
    for attempt in 0..=RETRIES {
        let output = match executor.execute(carrier.spec(attempt)) {
            Ok(output) => output,
            Err(error) => {
                return response(
                    request,
                    ResponseStatus::Failed,
                    json!({}),
                    Some(&format!("{error:#}")),
                );
            }
        };
        // APoC has not seen this command finish, so the outcome is unknown rather
        // than failed. Reporting Failed here closed the receipt while the work was
        // still running inside the guest: the reader was sent to the tunnel with
        // "guest did not return a transport result", and the next attempt ran the
        // command a second time. A slow `devenv shell` in a fresh guest reaches
        // this every time.
        if output.exit_code.is_none() {
            let mut pending = response(
                request,
                ResponseStatus::Pending,
                json!({"guest": guest, "kind": "guest_command"}),
                Some("the guest command has not finished; observe the execution to resume"),
            );
            pending.execution_id = Some(output.execution_id.clone());
            return pending;
        }
        let interpreted = interpret(&output.stdout);
        // `orchard ssh` sets up a port-forward before running anything, and that
        // setup intermittently fails with a WebSocket 500. The command provably
        // never ran, so retrying cannot double-execute it -- and without this the
        // caller saw only "guest did not return a transport result", which cost
        // real debugging time twice before it was handled here.
        let retryable = interpreted.is_err()
            && attempt < RETRIES
            && never_started(&output.stdout, &output.stderr);
        if retryable {
            continue;
        }
        return answer(request, guest, &output, interpreted, attempt);
    }
    response(
        request,
        ResponseStatus::Failed,
        json!({}),
        Some("the carrier never started the command"),
    )
}

/// Turn one finished attempt into the response, successful or not.
fn answer(
    request: &AdapterRequest,
    guest: &str,
    output: &workenv_platform::ExecutionOutput,
    interpreted: Result<Value, String>,
    attempt: u32,
) -> AdapterResponse {
    let mut answer = match interpreted {
        Ok(mut data) => {
            data["guest"] = json!(guest);
            response(request, ResponseStatus::Ready, data, None)
        }
        Err(error) => response(
            request,
            ResponseStatus::Failed,
            json!({}),
            Some(&detail(&error, &output.stderr, attempt)),
        ),
    };
    // The execution identity is carried even on failure: without it there is no
    // record to go and read, which is what made this opaque.
    answer.execution_id = Some(output.execution_id.clone());
    answer
}

/// Whether the carrier failed before the command could run.
///
/// Only then is a retry safe. Any output at all on stdout means the wrapper
/// started, so the command may have run and must not be repeated.
pub(super) fn never_started(stdout: &str, stderr: &str) -> bool {
    if !stdout.trim().is_empty() {
        return false;
    }

    SETUP_FAILURES.iter().any(|marker| stderr.contains(marker))
}

/// What to say when the guest returned nothing usable.
pub(super) fn detail(error: &str, stderr: &str, attempts: u32) -> String {
    let trimmed = stderr.trim();
    let carrier = if trimmed.is_empty() {
        "the carrier wrote nothing to stderr either".to_owned()
    } else {
        format!("carrier stderr: {}", excerpt(trimmed))
    };
    format!("{error}; {carrier}; attempts: {}", attempts + 1)
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
