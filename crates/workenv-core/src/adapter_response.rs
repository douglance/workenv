use anyhow::{Context as _, Result, bail};
use serde_json::{Value, json};
use workenv_platform::ExecutionOutput;
use workenv_protocol::{
    AdapterRequest, AdapterResponse, Extension, PROTOCOL_VERSION, ResponseStatus,
};

use crate::validate;

pub(super) fn response_from_output(
    output: &ExecutionOutput,
    request: &AdapterRequest,
) -> Result<AdapterResponse> {
    if output.exit_code != Some(0) {
        if output.exit_code.is_none() {
            let mut pending = AdapterResponse::new(
                request,
                ResponseStatus::Pending,
                json!({"kind": "adapter_execution", "request_id": request.request_id}),
            );
            pending.execution_id = Some(output.execution_id.clone());
            return Ok(pending);
        }
        bail!(
            "adapter {} {} exited with {}{}; execution {}",
            request.extension,
            request.operation,
            output
                .exit_code
                .map_or_else(|| "no status".to_owned(), |code| code.to_string()),
            diagnosis(output),
            output.execution_id,
        );
    }
    let response: AdapterResponse =
        serde_json::from_str(&output.stdout).context("adapter returned invalid JSON")?;
    if response.request_id == request.request_id {
        Ok(response)
    } else {
        bail!("adapter response request ID mismatch");
    }
}

/// The most informative thing the failed execution actually said.
///
/// An adapter that dies before it can write anything leaves both streams empty,
/// and the bare message that used to be produced -- "adapter exited
/// unsuccessfully: " -- named neither the adapter, the operation, the status,
/// nor the execution to go and read. That exact string hid three different
/// causes during one session. When there is nothing to quote, say so and point
/// at the execution record instead of trailing off.
fn diagnosis(output: &ExecutionOutput) -> String {
    for (label, stream) in [("stderr", &output.stderr), ("stdout", &output.stdout)] {
        let text = stream.trim();
        if !text.is_empty() {
            let excerpt: String = text.chars().take(400).collect();
            return format!(" ({label}: {excerpt})");
        }
    }
    // A process killed by a signal is reported as 128 + the signal number, and a
    // cancelled or timed-out execution arrives here with both streams empty. Saying
    // so turns "exited with 128 and wrote nothing" -- which reads as a mysterious
    // adapter bug -- into a pointer at the budget and the execution record. Phrased
    // as consistency rather than certainty, because ExecutionOutput carries no
    // status field and a genuine exit(128) is indistinguishable from here.
    if output.exit_code.is_some_and(|code| code >= 128) {
        return " and wrote nothing to either stream, which is what a cancelled or \
                timed-out execution looks like; check the execution record for its \
                outcome"
            .to_owned();
    }
    " and wrote nothing to either stream".to_owned()
}

pub(super) fn validate_response(
    extension: &Extension,
    operation: &str,
    request: &AdapterRequest,
    response: AdapterResponse,
) -> Result<AdapterResponse> {
    if response.protocol_version != PROTOCOL_VERSION {
        bail!("adapter response protocol version is unsupported");
    }
    if response.request_id != request.request_id {
        bail!("adapter response request ID mismatch");
    }
    if response.complete() {
        let operation = extension
            .operations
            .get(operation)
            .context("operation disappeared")?;
        validate::instance(&operation.output_schema, &response.data, "adapter output")?;
    }
    Ok(response)
}

pub(super) fn outer_execution_id<'a>(
    previous: Option<&'a Value>,
    request_id: &str,
) -> Option<&'a str> {
    let previous = previous?;
    if previous["status"].as_str() != Some("pending") {
        return None;
    }
    let data = previous.get("data")?;
    if data["kind"].as_str() != Some("adapter_execution") {
        return None;
    }
    if data["request_id"].as_str() != Some(request_id) {
        return None;
    }
    previous["execution_id"].as_str()
}

#[cfg(test)]
mod outer_execution_id_tests {
    use super::outer_execution_id;
    use serde_json::json;

    fn pending(kind: &str, request_id: &str) -> serde_json::Value {
        json!({
            "status": "pending",
            "execution_id": "exec-1",
            "data": {"kind": kind, "request_id": request_id},
        })
    }

    #[test]
    fn an_execution_recorded_for_another_request_is_not_reused() {
        // Without the request_id match a pending retry attaches to a different
        // request's execution and reports that execution's result as its own.
        assert_eq!(
            outer_execution_id(Some(&pending("adapter_execution", "other")), "mine"),
            None
        );
    }

    #[test]
    fn a_pending_response_that_is_not_an_adapter_execution_is_not_reused() {
        // Without the kind guard any pending previous response carrying an
        // execution_id is treated as an adapter execution.
        assert_eq!(
            outer_execution_id(Some(&pending("transport_pending", "mine")), "mine"),
            None
        );
    }

    #[test]
    fn the_matching_execution_is_reused() {
        assert_eq!(
            outer_execution_id(Some(&pending("adapter_execution", "mine")), "mine"),
            Some("exec-1")
        );
    }
}

#[cfg(test)]
mod diagnosis_tests {
    use super::diagnosis;
    use workenv_platform::ExecutionOutput;

    fn output(exit_code: Option<i32>, stdout: &str, stderr: &str) -> ExecutionOutput {
        ExecutionOutput {
            stdout: stdout.to_owned(),
            stderr: stderr.to_owned(),
            exit_code,
            execution_id: "exec-1".to_owned(),
        }
    }

    #[test]
    fn a_silent_signal_death_is_named_rather_than_left_as_a_number() {
        // Measured in this tree: a call wrapped in a devenv shell outran its budget,
        // was cancelled, and surfaced as "exited with 128 and wrote nothing to either
        // stream" -- which reads as a mysterious adapter bug and sent the reader to
        // the adapter instead of to the budget.
        let text = diagnosis(&output(Some(128), "", ""));
        assert!(
            text.contains("cancelled") || text.contains("timed-out"),
            "a silent 128 still reads as an adapter fault: {text}"
        );
    }

    #[test]
    fn an_ordinary_silent_failure_is_not_blamed_on_cancellation() {
        // The control: exit 1 with no output is an adapter that died on its own, and
        // calling that a cancellation would send the reader somewhere useless.
        let text = diagnosis(&output(Some(1), "", ""));
        assert!(
            !text.contains("cancelled"),
            "exit 1 was misreported as a cancellation: {text}"
        );
    }

    #[test]
    fn anything_the_adapter_actually_said_wins_over_the_guess() {
        // A real message is always better than an inference about the exit code.
        let text = diagnosis(&output(Some(137), "", "orchard: connection refused"));
        assert!(
            text.contains("connection refused"),
            "lost the message: {text}"
        );
        assert!(
            !text.contains("cancelled"),
            "buried the adapter's own words under a guess: {text}"
        );
    }
}
