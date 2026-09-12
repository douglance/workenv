//! The transport's tuning constants and its unfinished-command answer.
//!
//! A sibling of `execute_tests.rs` only because that file is at the 300-line limit.
//! Everything here was found by mutation testing: each constant below could be
//! changed, and the unfinished case could be reported as a terminal failure, with
//! the whole suite green.
use std::sync::Mutex;

use anyhow::Result;
use serde_json::json;
use workenv_platform::{ExecutionOutput, ExecutionSpec, Executor};
use workenv_protocol::ResponseStatus;

use super::execute::{plan, run};
use super::test_support::request_with;

/// An executor whose answers are scripted per attempt.
struct ScriptedExecutor {
    answers: Mutex<Vec<ExecutionOutput>>,
    seen: Mutex<Vec<ExecutionSpec>>,
}

impl ScriptedExecutor {
    fn new(answers: Vec<ExecutionOutput>) -> Self {
        Self {
            answers: Mutex::new(answers),
            seen: Mutex::new(Vec::new()),
        }
    }

    fn attempts(&self) -> usize {
        self.seen.lock().expect("recorded specs").len()
    }
}

impl Executor for ScriptedExecutor {
    fn execute(&self, spec: ExecutionSpec) -> Result<ExecutionOutput> {
        self.seen.lock().expect("recorded specs").push(spec);
        let mut answers = self.answers.lock().expect("scripted answers");
        Ok(if answers.is_empty() {
            unfinished()
        } else {
            answers.remove(0)
        })
    }
}

/// What `APoC` returns for a command it has not seen finish.
fn unfinished() -> ExecutionOutput {
    ExecutionOutput {
        stdout: String::new(),
        stderr: String::new(),
        exit_code: None,
        execution_id: "exec-unfinished".to_owned(),
    }
}

/// A carrier that failed during port-forward setup, before running anything.
fn setup_failure() -> ExecutionOutput {
    ExecutionOutput {
        stdout: String::new(),
        stderr: "failed to setup port-forwarding to the VM".to_owned(),
        exit_code: Some(1),
        execution_id: "exec-setup".to_owned(),
    }
}

fn finished(stdout: &str) -> ExecutionOutput {
    ExecutionOutput {
        stdout: stdout.to_owned(),
        stderr: String::new(),
        exit_code: Some(0),
        execution_id: "exec-done".to_owned(),
    }
}

#[test]
fn a_command_that_has_not_finished_is_pending_not_failed() {
    // Reporting Failed closed the receipt while the work was still running in the
    // guest: the reader was sent to the tunnel with "guest did not return a
    // transport result", and the next attempt ran the command a second time.
    let executor = ScriptedExecutor::new(vec![unfinished()]);
    let response = run(
        &request_with("execute", json!({"argv": ["true"]}), None),
        "g",
        &executor,
    );
    assert_eq!(response.status, ResponseStatus::Pending);
    assert_eq!(
        response.execution_id.as_deref(),
        Some("exec-unfinished"),
        "a pending answer with no execution id cannot be resumed"
    );
    assert_eq!(
        executor.attempts(),
        1,
        "an unfinished command must not be re-run"
    );
}

#[test]
fn the_transport_honours_the_budget_it_is_given() {
    // target.rs budgets 900_000 ms for a command on a target and now passes it in.
    // The transport used to substitute its own 300_000 ms default regardless.
    let (_, _, _, timeout) = plan(&request_with(
        "execute",
        json!({"argv": ["true"], "timeout_ms": 900_000}),
        None,
    ))
    .expect("plan");
    assert_eq!(timeout, 900_000);
}

#[test]
fn the_default_budget_is_five_minutes() {
    // Pinned because nothing pinned it: the default could be set to 3 ms, which
    // times out every guest command, with the suite green.
    let (_, _, _, timeout) =
        plan(&request_with("execute", json!({"argv": ["true"]}), None)).expect("plan");
    assert_eq!(timeout, 300_000);
}

#[test]
fn a_setup_failure_is_retried_exactly_twice_before_giving_up() {
    // The retry count is load-bearing and measurement-justified, and nothing pinned
    // it: setting RETRIES to 0 silently removed the documented behaviour.
    let executor = ScriptedExecutor::new(vec![setup_failure(), setup_failure(), setup_failure()]);
    let response = run(
        &request_with("execute", json!({"argv": ["true"]}), None),
        "g",
        &executor,
    );
    assert_eq!(response.status, ResponseStatus::Failed);
    assert_eq!(
        executor.attempts(),
        3,
        "one initial attempt plus two retries"
    );
}

#[test]
fn a_retry_that_succeeds_stops_retrying() {
    let executor = ScriptedExecutor::new(vec![
        setup_failure(),
        finished(&json!({"exit_code": 0, "stdout": "", "stderr": ""}).to_string()),
    ]);
    let response = run(
        &request_with("execute", json!({"argv": ["true"]}), None),
        "g",
        &executor,
    );
    assert_eq!(response.status, ResponseStatus::Ready);
    assert_eq!(executor.attempts(), 2);
}

#[test]
fn the_last_attempt_reports_the_real_failure_not_a_generic_one() {
    // Dropping `attempt < RETRIES` from the retry test made the loop fall through
    // to "the carrier never started the command", discarding the real error and the
    // execution id -- the opposite of what the retry handling was added to fix.
    let executor = ScriptedExecutor::new(vec![setup_failure(), setup_failure(), setup_failure()]);
    let response = run(
        &request_with("execute", json!({"argv": ["true"]}), None),
        "g",
        &executor,
    );
    let error = response.error.unwrap_or_default();
    assert!(
        error.contains("port-forwarding"),
        "the carrier's own failure was discarded: {error}"
    );
    assert_eq!(
        response.execution_id.as_deref(),
        Some("exec-setup"),
        "the execution id was discarded, so there is no record to read"
    );
}

/// The wrapper's own JSON, as the guest prints it.
fn wrapper_result(code: i64) -> String {
    json!({"exit_code": code, "stdout": "", "stderr": ""}).to_string()
}

#[test]
fn both_wrapper_failures_are_faults_not_command_results() {
    // The existing test covers only status 122, so the 121 arm could be deleted
    // with the suite green -- and then a guest that could not even create its temp
    // directory would report 121 as *the command's own exit code*, which is the
    // precise confusion the surrounding doc comment says must never happen.
    for (code, expected) in [(121_i64, "temp dir"), (122_i64, "target directory")] {
        let executor = ScriptedExecutor::new(vec![finished(&wrapper_result(code))]);
        let response = run(
            &request_with("execute", json!({"argv": ["true"]}), None),
            "g",
            &executor,
        );
        assert_eq!(
            response.status,
            ResponseStatus::Failed,
            "wrapper status {code} was reported as a command result"
        );
        let error = response.error.unwrap_or_default();
        assert!(
            error.contains(expected),
            "wrapper status {code} gave an unhelpful message: {error}"
        );
    }
}

#[test]
fn a_wrapper_status_that_is_not_a_wrapper_failure_is_the_commands_own_code() {
    // The other side of the same boundary: 120 and 123 are ordinary exit codes and
    // must pass straight through, or an unlucky command looks like a broken guest.
    for code in [120_i64, 123] {
        let executor = ScriptedExecutor::new(vec![finished(&wrapper_result(code))]);
        let response = run(
            &request_with("execute", json!({"argv": ["true"]}), None),
            "g",
            &executor,
        );
        assert_eq!(response.status, ResponseStatus::Ready);
        assert_eq!(response.data["exit_code"], json!(code));
    }
}
