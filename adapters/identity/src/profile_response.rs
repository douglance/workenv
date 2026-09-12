use anyhow::{Context, Result};
use serde_json::{Value, json};
use workenv_platform::ExecutionOutput;
use workenv_protocol::{AdapterRequest, AdapterResponse, ResponseStatus};

pub(crate) fn response(
    request: &AdapterRequest,
    status: ResponseStatus,
    name: &str,
    details: Value,
) -> AdapterResponse {
    let mut data = serde_json::Map::new();
    data.insert(
        "ok".to_string(),
        json!(matches!(
            status,
            ResponseStatus::Ready | ResponseStatus::Changed
        )),
    );
    data.insert("status".to_string(), json!(name));
    data.insert("details".to_string(), details);
    AdapterResponse::new(request, status, Value::Object(data))
}

pub(crate) fn pending_response(
    request: &AdapterRequest,
    execution_id: String,
    name: &str,
    details: Value,
) -> AdapterResponse {
    let mut response = response(request, ResponseStatus::Pending, name, details);
    response.execution_id = Some(execution_id);
    response
}

pub(crate) fn output_json(output: &ExecutionOutput) -> Result<Value> {
    serde_json::from_str(&output.stdout).with_context(|| {
        format!(
            "stdout was not JSON (execution {}): stdout {:?}, stderr {:?}",
            output.execution_id,
            excerpt(&output.stdout),
            excerpt(&output.stderr)
        )
    })
}

/// Keep a diagnostic excerpt short so a large payload cannot flood the message.
pub(crate) fn excerpt(text: &str) -> String {
    let text = text.trim();
    match text.char_indices().nth(EXCERPT_CHARS) {
        Some((index, _)) => format!("{}…", &text[..index]),
        None => text.to_string(),
    }
}

const EXCERPT_CHARS: usize = 200;

#[cfg(test)]
#[path = "profile_response_tests.rs"]
mod tests;
