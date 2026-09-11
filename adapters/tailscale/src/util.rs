use anyhow::{Context, Result};
use serde_json::{Map, Value, json};
use workenv_platform::ExecutionOutput;
use workenv_protocol::{AdapterRequest, AdapterResponse, ResponseStatus};

pub(crate) fn response(
    request: &AdapterRequest,
    status: ResponseStatus,
    mut data: Map<String, Value>,
) -> AdapterResponse {
    data.insert(
        "ok".to_string(),
        json!(matches!(
            status,
            ResponseStatus::Ready | ResponseStatus::Changed
        )),
    );
    AdapterResponse::new(request, status, Value::Object(data))
}

pub(crate) fn pending_response(
    request: &AdapterRequest,
    execution_id: String,
    mut data: Map<String, Value>,
) -> AdapterResponse {
    data.insert("ok".to_string(), json!(false));
    let mut response = AdapterResponse::new(request, ResponseStatus::Pending, Value::Object(data));
    response.execution_id = Some(execution_id);
    response
}

pub(crate) fn optional_string(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
}

pub(crate) fn redacted(text: &str) -> String {
    if text.trim().is_empty() {
        String::new()
    } else {
        "[redacted command failure]".to_string()
    }
}

pub(crate) fn output_json(output: &ExecutionOutput) -> Result<Value> {
    serde_json::from_str(&output.stdout).context("stdout was not JSON")
}
