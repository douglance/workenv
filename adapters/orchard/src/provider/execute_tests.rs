//! Orchard transport tests.
use std::sync::Mutex;

use anyhow::{Result, anyhow};
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use serde_json::json;
use workenv_platform::{ExecutionOutput, ExecutionSpec, Executor};
use workenv_protocol::ResponseStatus;

use super::carrier::Carrier;
use super::execute::{carrier_argv, detail, interpret, never_started, plan, run, script};
use super::test_support::request_with;

/// An executor that returns canned output and records what it was asked to run.
struct FakeExecutor {
    stdout: String,
    fail: bool,
    seen: Mutex<Vec<ExecutionSpec>>,
}

impl FakeExecutor {
    fn returning(stdout: &str) -> Self {
        Self {
            stdout: stdout.to_owned(),
            fail: false,
            seen: Mutex::new(Vec::new()),
        }
    }

    fn failing() -> Self {
        Self {
            stdout: String::new(),
            fail: true,
            seen: Mutex::new(Vec::new()),
        }
    }

    fn only_spec(&self) -> ExecutionSpec {
        let seen = self.seen.lock().expect("recorded specs");
        assert_eq!(seen.len(), 1, "expected exactly one execution");
        seen[0].clone()
    }
}

impl Executor for FakeExecutor {
    fn execute(&self, spec: ExecutionSpec) -> Result<ExecutionOutput> {
        self.seen.lock().expect("recorded specs").push(spec);
        if self.fail {
            return Err(anyhow!("controller tunnel is down"));
        }
        Ok(ExecutionOutput {
            stdout: self.stdout.clone(),
            stderr: String::new(),
            exit_code: Some(0),
            execution_id: "exec-1".to_owned(),
        })
    }
}

/// The wrapper's own JSON, as the guest would print it.
fn wrapper(code: i64, stdout: &str, stderr: &str) -> String {
    json!({
        "exit_code": code,
        "stdout": BASE64.encode(stdout),
        "stderr": BASE64.encode(stderr),
    })
    .to_string()
}

#[test]
fn the_commands_own_exit_code_survives() {
    // Measured against a live guest: `orchard ssh vm` reports 1 for a child
    // that exited 7. Reading the code off the process would make every failure
    // look identical, so the guest reports its own and this must carry it.
    let data = interpret(&wrapper(7, "", "")).expect("a transport result");
    assert_eq!(data["exit_code"], json!(7));
}

#[test]
fn output_streams_come_back_separated_and_decoded() {
    let data = interpret(&wrapper(0, "out-here\n", "err-here\n")).expect("a transport result");
    assert_eq!(data["stdout"], json!("out-here\n"));
    assert_eq!(data["stderr"], json!("err-here\n"));
}

#[test]
fn output_that_is_not_a_transport_result_is_a_fault_not_a_success() {
    // A broken tunnel prints something that is not the wrapper's object. If
    // that were read as exit_code 0 with empty output, a command that never ran
    // would be indistinguishable from one that succeeded silently.
    let error = interpret("no credentials specified or found").expect_err("a transport fault");
    assert!(
        error.contains("did not return a transport result"),
        "{error}"
    );
}

#[test]
fn a_wrapper_failure_is_never_reported_as_the_commands_result() {
    // 122 is the wrapper's "could not enter the target directory". Passing it
    // through as an exit code would blame the command for the transport.
    let error = interpret(&wrapper(122, "", "")).expect_err("a wrapper fault");
    assert!(error.contains("target directory"), "{error}");
}

#[test]
fn the_carrier_argument_carries_no_shell_metacharacter() {
    // The script travels base64-encoded precisely so the guest's shell cannot
    // re-split it. The check is a round trip, not a scan: decoding the carried
    // segment must reproduce the script byte for byte, which is false the
    // moment any of it is sent raw. An earlier version of this test looked at
    // only the first space-delimited token, so a raw script beginning "set -u"
    // read as clean -- it passed against a deliberately broken carrier.
    let carried = script(&["echo".into(), "a b; rm -rf /".into()], "/tmp", "");
    let argv = carrier_argv("g", &carried);
    let payload = argv.last().expect("a carrier argument");
    let encoded = payload
        .strip_prefix("echo ")
        .and_then(|rest| rest.split_once(" |"))
        .map(|(encoded, _)| encoded)
        .expect("an encoded segment");
    let decoded = BASE64.decode(encoded).expect("the segment must be base64");
    assert_eq!(
        String::from_utf8(decoded).expect("utf8"),
        carried,
        "the carrier does not round-trip the script"
    );
    // And the dangerous text must not appear literally anywhere in the argv.
    assert!(
        !payload.contains("rm -rf /"),
        "the raw script reached the carrier: {payload}"
    );
}

#[test]
fn an_argument_cannot_escape_into_the_guests_shell() {
    // Single quotes in an argument are the classic break-out. The quoted form
    // must contain the closing-and-reopening sequence, not a bare quote.
    let built = script(&["echo".into(), "it's".into()], "/tmp", "");
    assert!(built.contains(r"'it'\''s'"), "{built}");
}

#[test]
fn stdin_is_carried_as_data_not_as_script() {
    let built = script(&["cat".into()], "/tmp", "payload");
    assert!(built.contains(&BASE64.encode("payload")), "{built}");
    // The literal text must not appear, or a payload containing shell syntax
    // would be executed rather than read.
    assert!(!built.contains("payload\n"), "{built}");
}

#[test]
fn an_empty_argv_is_refused_before_anything_runs() {
    let req = request_with("execute", json!({"argv": []}), None);
    let executor = FakeExecutor::returning("");
    let response = run(&req, "g", &executor);
    assert_eq!(response.status, ResponseStatus::Failed);
    assert!(
        executor.seen.lock().expect("recorded specs").is_empty(),
        "a refused request must not reach the guest"
    );
}

#[test]
fn a_retry_reuses_the_execution_identity() {
    // Without a stable key a retried transport runs the command inside the
    // guest a second time, which is not safe for anything that mutates.
    let req = request_with("execute", json!({"argv": ["true"]}), None);
    let executor = FakeExecutor::returning(&wrapper(0, "", ""));
    run(&req, "g", &executor);
    assert!(
        executor
            .only_spec()
            .idempotency_key
            .contains(&req.request_id),
        "execution key is not bound to the request"
    );
}

#[test]
fn an_unreachable_controller_fails_rather_than_reporting_success() {
    let req = request_with("execute", json!({"argv": ["true"]}), None);
    let response = run(&req, "g", &FakeExecutor::failing());
    assert_eq!(response.status, ResponseStatus::Failed);
}

#[test]
fn a_running_command_reports_ready_with_the_guest_named() {
    let req = request_with("execute", json!({"argv": ["true"]}), None);
    let response = run(&req, "g", &FakeExecutor::returning(&wrapper(0, "hi", "")));
    assert_eq!(response.status, ResponseStatus::Ready);
    assert_eq!(response.data["stdout"], json!("hi"));
    assert_eq!(response.data["guest"], json!("g"));
}

#[test]
fn the_plan_defaults_the_timeout_rather_than_running_unbounded() {
    let req = request_with("execute", json!({"argv": ["true"]}), None);
    let (_, _, _, timeout) = plan(&req).expect("a plan");
    assert!(timeout > 0, "a transport with no timeout can hang forever");
}

#[test]
fn a_setup_failure_with_no_output_is_retryable() {
    // Measured: `orchard ssh` sets up a port-forward before running anything and
    // intermittently fails it with a WebSocket 500, after which six consecutive
    // attempts succeeded. The command provably never ran, so a retry cannot
    // double-execute it.
    assert!(never_started(
        "",
        "ssh command failed: failed to setup port-forwarding to the VM \"g\": \
         failed to WebSocket dial: expected handshake response status code 101 but got 500"
    ));
}

#[test]
fn any_output_at_all_makes_it_unsafe_to_retry() {
    // Output means the wrapper started, so the command may have run. Retrying a
    // mutating command here would run it twice.
    assert!(!never_started(
        "partial output",
        "failed to setup port-forwarding"
    ));
}

#[test]
fn an_unrecognised_failure_is_not_retried() {
    // Only failures known to happen before the command starts are retried.
    // Treating every empty result as retryable would repeat a command that was
    // killed midway.
    assert!(!never_started("", "permission denied"));
}

#[test]
fn the_failure_message_carries_the_carriers_own_stderr() {
    // "guest did not return a transport result" alone cost real debugging time
    // twice, because it named neither the cause nor anywhere to look.
    let text = detail("no result", "failed to WebSocket dial", 1);
    assert!(text.contains("failed to WebSocket dial"), "{text}");
    assert!(text.contains("attempts: 2"), "{text}");
}

#[test]
fn a_silent_carrier_says_so_rather_than_trailing_off() {
    let text = detail("no result", "   ", 0);
    assert!(text.contains("wrote nothing to stderr"), "{text}");
}

#[test]
fn each_attempt_gets_its_own_execution_identity() {
    // APoC returns the ORIGINAL receipt for a repeated idempotency key, so a
    // retry sharing the first attempt's key would replay that first failure
    // forever rather than actually trying again. The key must move per attempt
    // while still being derived from the controller's request.
    let req = request_with("execute", json!({"argv": ["true"]}), None);
    let carrier = Carrier::for_tests(&req, "g", vec!["true".to_owned()]);
    let first = carrier.spec(0).idempotency_key;
    let second = carrier.spec(1).idempotency_key;
    assert_ne!(first, second, "a retry would replay the first receipt");
    assert!(first.contains(&req.request_id), "{first}");
}
