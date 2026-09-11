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
