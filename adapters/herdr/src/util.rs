use std::fs;

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
    data.insert("execution_id".to_string(), json!(execution_id.clone()));
    let mut response = AdapterResponse::new(request, ResponseStatus::Pending, Value::Object(data));
    response.execution_id = Some(execution_id);
    response
}

pub(crate) fn response_status(
    request: &AdapterRequest,
    changed: bool,
    data: Map<String, Value>,
) -> AdapterResponse {
    response(
        request,
        if changed {
            ResponseStatus::Changed
        } else {
            ResponseStatus::Ready
        },
        data,
    )
}

pub(crate) fn optional_string(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
}

pub(crate) fn field_bool(value: &Value, key: &str, default: bool) -> bool {
    value.get(key).and_then(Value::as_bool).unwrap_or(default)
}

pub(crate) fn atomic_json(path: &std::path::Path, value: &Value) -> Result<bool> {
    if path.exists() && serde_json::from_slice::<Value>(&fs::read(path)?)? == *value {
        return Ok(false);
    }
    let parent = path.parent().context("JSON path has no parent")?;
    fs::create_dir_all(parent)?;
    let temp = parent.join(format!(".{}.tmp", uuid_token()));
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&temp)?;
    serde_json::to_writer(&mut file, value)?;
    std::io::Write::write_all(&mut file, b"\n")?;
    file.sync_all()?;
    fs::rename(&temp, path)?;
    fs::File::open(parent)?.sync_all()?;
    Ok(true)
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
fn excerpt(text: &str) -> String {
    let text = text.trim();
    match text.char_indices().nth(EXCERPT_CHARS) {
        Some((index, _)) => format!("{}…", &text[..index]),
        None => text.to_string(),
    }
}

const EXCERPT_CHARS: usize = 200;

fn uuid_token() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos())
        .to_string()
}

#[cfg(test)]
#[path = "util_tests.rs"]
mod tests;
