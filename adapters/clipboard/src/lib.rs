//! Native adapter for ssh-clipboard configuration and status.

use std::{fs, path::PathBuf};

use anyhow::{Context, Result, bail};
use serde_json::{Map, Value, json};
use uuid::Uuid;
use workenv_platform::{ApocExecutor, ExecutionOutput, ExecutionSpec, Executor};
use workenv_protocol::{AdapterRequest, AdapterResponse, ResponseStatus};

/// Handle one clipboard adapter request using `APoC` for command execution.
///
/// # Errors
///
/// Returns an error when the current directory or adapter operation fails.
pub fn handle(request: &AdapterRequest) -> Result<AdapterResponse> {
    let root = std::env::current_dir()?;
    let runner = ApocExecutor::new(root);
    handle_with(request, &runner)
}

/// Handle one clipboard adapter request with an injected command runner.
///
/// # Errors
///
/// Returns an error when configuration, inspection, or request validation fails.
pub fn handle_with(request: &AdapterRequest, runner: &impl Executor) -> Result<AdapterResponse> {
    match request.operation.as_str() {
        "config" | "apply" => configure(request),
        "inspect" => inspect(request, runner),
        operation => bail!("unsupported clipboard operation {operation}"),
    }
}

fn configure(request: &AdapterRequest) -> Result<AdapterResponse> {
    let roots = Roots::from_request(request);
    fs::create_dir_all(&roots.config)?;
    fs::create_dir_all(&roots.state)?;
    private_dir(&roots.config)?;
    private_dir(&roots.state)?;
    let node_id = stable_node_id(&roots)?;
    let config_path = roots.config.join("config.json");
    let peers = config_peers(request, &config_path)?;
    let config = clipboard_config(request, &node_id, &peers);
    let changed = atomic_json(&config_path, &config)?;
    let mut data = Map::new();
    data.insert("status".to_string(), json!("clipboard_configured"));
    data.insert("config_path".to_string(), json!(config_path));
    data.insert("state_path".to_string(), json!(roots.state));
    data.insert("node_id".to_string(), json!(node_id));
    Ok(response_status(request, changed, data))
}

fn inspect(request: &AdapterRequest, runner: &impl Executor) -> Result<AdapterResponse> {
    let output = runner.execute(ExecutionSpec {
        executable: "ssh-clipboard".to_string(),
        arg: vec!["status".to_string(), "--json".to_string()],
        cwd: Some(request.target.directory.clone()),
        stdin: None,
        idempotency_key: format!("{}:clipboard-status", request.request_id),
        purpose: "Inspect ssh-clipboard status.".to_string(),
        timeout_ms: 60_000,
    })?;
    let mut data = Map::new();
    data.insert("exit_code".to_string(), json!(output.exit_code));
    data.insert("execution_id".to_string(), json!(output.execution_id));
    if output.exit_code == Some(0) {
        data.insert("status".to_string(), json!("clipboard_ready"));
        data.insert("clipboard".to_string(), output_json(&output)?);
        return Ok(response(request, ResponseStatus::Ready, data));
    }
    data.insert("status".to_string(), json!("clipboard_not_ready"));
    data.insert("stderr".to_string(), json!(safe_text(&output.stderr)));
    Ok(response(request, ResponseStatus::Failed, data))
}

fn clipboard_config(request: &AdapterRequest, node_id: &str, peers: &Value) -> Value {
    let node_name = optional_string(&request.config, "node_name")
        .or_else(|| optional_string(&request.input, "node_name"))
        .unwrap_or_else(|| request.target.host.clone());
    json!({
        "version": 1,
        "node_id": node_id,
        "node_name": node_name,
        "peers": peers,
        "max_bytes": 268_435_456_u64,
        "poll_interval_ms": 75_u64,
        "headless_x11": cfg!(target_os = "linux"),
    })
}

fn config_peers(request: &AdapterRequest, config_path: &std::path::Path) -> Result<Value> {
    if let Some(peers) = request
        .config
        .get("peers")
        .or_else(|| request.input.get("peers"))
        .filter(|value| value.is_array())
    {
        return Ok(peers.clone());
    }
    if explicit_replacement(request) || !config_path.is_file() {
        return Ok(json!([]));
    }
    let existing: Value = serde_json::from_slice(&fs::read(config_path)?)?;
    Ok(existing
        .get("peers")
        .filter(|value| value.is_array())
        .cloned()
        .unwrap_or_else(|| json!([])))
}

fn explicit_replacement(request: &AdapterRequest) -> bool {
    bool_field(&request.config, "replace_existing")
        || bool_field(&request.config, "replace")
        || bool_field(&request.input, "replace_existing")
        || bool_field(&request.input, "replace")
}

fn bool_field(value: &Value, key: &str) -> bool {
    value.get(key).and_then(Value::as_bool).unwrap_or(false)
}

fn response(
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

fn optional_string(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
}

fn field_path(value: &Value, key: &str, default: &std::path::Path) -> PathBuf {
    optional_string(value, key).map_or_else(|| default.to_path_buf(), PathBuf::from)
}

fn atomic_json(path: &std::path::Path, value: &Value) -> Result<bool> {
    if path.exists() && serde_json::from_slice::<Value>(&fs::read(path)?)? == *value {
        return Ok(false);
    }
    let parent = path.parent().context("JSON path has no parent")?;
    fs::create_dir_all(parent)?;
    let temp = parent.join(format!(".{}.tmp", Uuid::new_v4()));
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

fn stable_node_id(roots: &Roots) -> Result<String> {
    let path = roots.state.join("node-id");
    if path.exists() {
        return Ok(fs::read_to_string(path)?.trim().to_string());
    }
    let id = Uuid::new_v4().to_string();
    write_private(&path, id.as_bytes())?;
    Ok(id)
}

fn response_status(
    request: &AdapterRequest,
    changed: bool,
    data: Map<String, Value>,
) -> AdapterResponse {
    let status = if changed {
        ResponseStatus::Changed
    } else {
        ResponseStatus::Ready
    };
    response(request, status, data)
}

struct Roots {
    config: PathBuf,
    state: PathBuf,
}

impl Roots {
    fn from_request(request: &AdapterRequest) -> Self {
        let base = request.target.directory.join(".state/clipboard");
        Self {
            config: field_path(&request.config, "config_dir", &base.join("config")),
            state: field_path(&request.config, "state_dir", &base.join("state")),
        }
    }
}

fn private_dir(path: &std::path::Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

fn write_private(path: &std::path::Path, bytes: &[u8]) -> Result<()> {
    let parent = path.parent().context("private file has no parent")?;
    fs::create_dir_all(parent)?;
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    std::io::Write::write_all(&mut options.open(path)?, bytes)?;
    Ok(())
}

fn safe_text(text: &str) -> String {
    text.trim().to_string()
}

fn output_json(output: &ExecutionOutput) -> Result<Value> {
    serde_json::from_str(&output.stdout).context("stdout was not JSON")
}

#[cfg(test)]
#[path = "lib_tests.rs"]
mod tests;
