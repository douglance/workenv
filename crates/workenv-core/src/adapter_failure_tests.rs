//! How an adapter execution that did not succeed is reported.
use serde_json::json;
use workenv_platform::ExecutionOutput;
use workenv_protocol::{AdapterRequest, PROTOCOL_VERSION, ResponseStatus};

/// One request, shaped only enough to exercise the failure reporting.
fn failing_request() -> AdapterRequest {
    serde_json::from_value(json!({
        "protocol_version": PROTOCOL_VERSION,
        "request_id": "req-1",
        "extension": "workenv.orchard",
        "operation": "create",
        "target": {
            "environment": "env", "host": "host", "address": null,
            "directory": "/tmp", "system": "aarch64-linux",
            "source": "path:/tmp", "profiles": []
        },
        "config": {}, "input": {}, "previous": null
    }))
    .expect("a request fixture")
}

fn output(exit_code: Option<i32>, stdout: &str, stderr: &str) -> ExecutionOutput {
    ExecutionOutput {
        stdout: stdout.to_owned(),
        stderr: stderr.to_owned(),
        exit_code,
        execution_id: "exec-9".to_owned(),
    }
}

#[test]
fn a_silent_adapter_failure_still_says_what_failed_and_where_to_look() {
    // The old message was "adapter exited unsuccessfully: " and nothing else,
    // which named neither the adapter, the operation, the status, nor the
    // execution to read. That exact string hid three unrelated causes in one
    // session, so every one of those has to be present.
    let error =
        crate::adapter_response::response_from_output(&output(Some(1), "", ""), &failing_request())
            .expect_err("a failure");
    let text = error.to_string();
    for expected in ["workenv.orchard", "create", "1", "exec-9", "wrote nothing"] {
        assert!(
            text.contains(expected),
            "{expected:?} missing from {text:?}"
        );
    }
}

#[test]
fn stderr_is_quoted_when_the_adapter_managed_to_say_something() {
    let failed = output(Some(2), "", "controller unreachable");
    let error = crate::adapter_response::response_from_output(&failed, &failing_request())
        .expect_err("a failure");
    assert!(error.to_string().contains("controller unreachable"));
}

#[test]
fn stdout_is_used_when_only_stdout_has_the_reason() {
    // An adapter that prints its complaint to stdout and exits non-zero is not
    // silent, and reporting it as silent sends the reader to the wrong place.
    let failed = output(Some(3), "unsupported operation", "");
    let error = crate::adapter_response::response_from_output(&failed, &failing_request())
        .expect_err("a failure");
    assert!(error.to_string().contains("unsupported operation"));
}

#[test]
fn a_still_running_adapter_is_pending_rather_than_a_failure() {
    // No exit code means APoC has not seen the process finish. Reporting that
    // as a failure is what made a slow-but-healthy adapter look broken.
    let response =
        crate::adapter_response::response_from_output(&output(None, "", ""), &failing_request())
            .expect("a pending response");
    assert_eq!(response.status, ResponseStatus::Pending);
}
