use anyhow::Result;
use serde_json::json;
use std::sync::Mutex;
use workenv_platform::Executor;
use workenv_protocol::{PROTOCOL_VERSION, Target};

use super::*;

#[test]
fn config_keeps_stable_node_id_and_empty_peers() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let request = request(temp.path());
    let first = configure(&request)?;
    let second = configure(&request)?;
    assert_eq!(first.status, ResponseStatus::Changed);
    assert_eq!(second.status, ResponseStatus::Ready);
    let config = fs::read_to_string(temp.path().join(".state/clipboard/config/config.json"))?;
    assert!(config.contains("\"peers\":[]"));
    Ok(())
}

#[test]
fn config_preserves_existing_peers_without_explicit_replacement() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let config_dir = temp.path().join(".state/clipboard/config");
    fs::create_dir_all(&config_dir)?;
    fs::write(
        config_dir.join("config.json"),
        serde_json::to_vec(&json!({
            "version":1,
            "node_id":"old-node",
            "node_name":"old",
            "peers":[{"name":"peer-a","address":"peer.example"}],
            "max_bytes":1,
            "poll_interval_ms":1,
            "headless_x11":false
        }))?,
    )?;
    let request = request(temp.path());
    let result = configure(&request)?;
    assert_eq!(result.status, ResponseStatus::Changed);
    let config: Value = serde_json::from_slice(&fs::read(config_dir.join("config.json"))?)?;
    assert_eq!(config["peers"][0]["name"], "peer-a");
    Ok(())
}

#[test]
fn inspect_uses_ssh_clipboard_status() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let runner = OneOutput::new(json!({"running":true}));
    let result = inspect(&request(temp.path()), &runner)?;
    assert_eq!(result.status, ResponseStatus::Ready);
    let calls = runner
        .calls
        .lock()
        .map_err(|_| anyhow::anyhow!("mutex poisoned"))?;
    assert_eq!(calls[0].executable, "ssh-clipboard");
    assert_eq!(calls[0].arg, ["status", "--json"]);
    Ok(())
}

fn request(path: &std::path::Path) -> AdapterRequest {
    AdapterRequest {
        protocol_version: PROTOCOL_VERSION,
        request_id: "request-1".to_string(),
        extension: "clipboard".to_string(),
        operation: "config".to_string(),
        target: Target {
            environment: "dev".to_string(),
            host: "worker".to_string(),
            address: None,
            directory: path.to_path_buf(),
            system: "x86_64-linux".to_string(),
            source: ".".to_string(),
            profiles: Vec::new(),
        },
        config: json!({}),
        input: json!({}),
        previous: None,
    }
}

struct OneOutput {
    calls: Mutex<Vec<ExecutionSpec>>,
    value: Value,
}

impl OneOutput {
    fn new(value: Value) -> Self {
        Self {
            calls: Mutex::new(Vec::new()),
            value,
        }
    }
}

impl Executor for OneOutput {
    fn execute(&self, spec: ExecutionSpec) -> Result<ExecutionOutput> {
        self.calls
            .lock()
            .map_err(|_| anyhow::anyhow!("mutex poisoned"))?
            .push(spec);
        Ok(ExecutionOutput {
            stdout: serde_json::to_string(&self.value)?,
            stderr: String::new(),
            exit_code: Some(0),
            execution_id: String::new(),
        })
    }
}

#[test]
fn peers_supplied_in_input_replace_what_the_existing_config_holds() -> Result<()> {
    // `peers` is read from config and then from input, and it was honoured for a
    // long time without ever being declared in modules/clipboard.nix. Declaring
    // it there without pinning it here would leave the schema's word for it as
    // the only evidence.
    let temp = tempfile::tempdir()?;
    let config_dir = temp.path().join(".state/clipboard/config");
    fs::create_dir_all(&config_dir)?;
    fs::write(
        config_dir.join("config.json"),
        serde_json::to_vec(&json!({
            "version":1,"node_id":"old-node","node_name":"old",
            "peers":[{"name":"peer-a","address":"peer.example"}],
            "max_bytes":1,"poll_interval_ms":1,"headless_x11":false
        }))?,
    )?;
    let mut request = request(temp.path());
    request.input = json!({"peers":[{"name":"peer-b","address":"other.example"}]});
    configure(&request)?;
    let config: Value = serde_json::from_slice(&fs::read(config_dir.join("config.json"))?)?;
    assert_eq!(config["peers"][0]["name"], "peer-b");
    assert_eq!(config["peers"].as_array().map(Vec::len), Some(1));
    Ok(())
}

#[test]
fn replace_in_input_clears_the_peers_already_on_disk() -> Result<()> {
    // The other newly declared pair. Without it, `replace` and `replace_existing`
    // are two schema entries nothing proves the adapter reads.
    let temp = tempfile::tempdir()?;
    let config_dir = temp.path().join(".state/clipboard/config");
    fs::create_dir_all(&config_dir)?;
    fs::write(
        config_dir.join("config.json"),
        serde_json::to_vec(&json!({
            "version":1,"node_id":"old-node","node_name":"old",
            "peers":[{"name":"peer-a","address":"peer.example"}],
            "max_bytes":1,"poll_interval_ms":1,"headless_x11":false
        }))?,
    )?;
    let mut request = request(temp.path());
    request.input = json!({"replace": true});
    configure(&request)?;
    let config: Value = serde_json::from_slice(&fs::read(config_dir.join("config.json"))?)?;
    assert_eq!(config["peers"], json!([]));
    Ok(())
}

#[test]
fn node_name_in_input_names_the_node() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let mut request = request(temp.path());
    request.input = json!({"node_name": "from-input"});
    configure(&request)?;
    let config: Value = serde_json::from_slice(&fs::read(
        temp.path().join(".state/clipboard/config/config.json"),
    )?)?;
    assert_eq!(config["node_name"], "from-input");
    Ok(())
}
