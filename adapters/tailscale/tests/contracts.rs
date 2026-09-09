//! Tailscale adapter contract tests.
use std::{path::PathBuf, sync::Mutex};

use anyhow::Result;
use serde_json::{Value, json};
use workenv_adapter_tailscale::handle_with;
use workenv_platform::{ExecutionOutput, ExecutionSpec, Executor};
use workenv_protocol::{AdapterRequest, PROTOCOL_VERSION, ResponseStatus, Target};

#[test]
fn inspect_requires_running_identity_tag_and_ssh_prefs() -> Result<()> {
    let runner = Outputs::new(vec![
        ready_status(),
        json!({"WantRunning":true,"RunSSH":false}),
    ]);
    let result = handle_with(&request("inspect"), &runner)?;
    assert_eq!(result.status, ResponseStatus::Failed);
    assert_eq!(
        runner
            .calls
            .lock()
            .map_err(|_| anyhow::anyhow!("mutex poisoned"))?
            .len(),
        2
    );
    Ok(())
}

#[test]
fn enroll_uses_secret_stdin_without_secret_argv() -> Result<()> {
    let temp = tempfile::NamedTempFile::new()?;
    std::fs::write(temp.path(), "tskey-secret")?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(temp.path(), std::fs::Permissions::from_mode(0o600))?;
    }
    let mut request = request("enroll");
    request.config["auth_key_file"] = json!(temp.path());
    let runner = Outputs::new(vec![
        not_running(),
        json!({}),
        json!({}),
        ready_status(),
        json!({"WantRunning":true,"RunSSH":true}),
    ]);
    let result = handle_with(&request, &runner)?;
    assert_eq!(result.status, ResponseStatus::Ready);
    let calls = runner
        .calls
        .lock()
        .map_err(|_| anyhow::anyhow!("mutex poisoned"))?;
    let enroll_call = &calls[2];
    assert_eq!(enroll_call.stdin, Some(b"tskey-secret".to_vec()));
    assert!(!serde_json::to_string(&enroll_call.arg)?.contains("tskey-secret"));
    assert!(
        !enroll_call
            .arg
            .iter()
            .any(|arg| arg.contains("'--auth-key=file:$key_file'"))
    );
    Ok(())
}

#[test]
fn inspect_propagates_pending_status_execution() -> Result<()> {
    let runner = Outputs::new_with_codes(vec![(json!({}), None)]);
    let result = handle_with(&request("inspect"), &runner)?;
    assert_eq!(result.status, ResponseStatus::Pending);
    assert_eq!(result.execution_id, Some("execution-0".to_string()));
    assert_eq!(result.data["status"], "tailscale_status_pending");
    Ok(())
}

#[test]
fn enroll_propagates_pending_execution() -> Result<()> {
    let temp = tempfile::NamedTempFile::new()?;
    std::fs::write(temp.path(), "tskey-secret")?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(temp.path(), std::fs::Permissions::from_mode(0o600))?;
    }
    let mut request = request("enroll");
    request.config["auth_key_file"] = json!(temp.path());
    let runner = Outputs::new_with_codes(vec![
        (not_running(), Some(0)),
        (json!({}), Some(0)),
        (json!({}), None),
    ]);
    let result = handle_with(&request, &runner)?;
    assert_eq!(result.status, ResponseStatus::Pending);
    assert_eq!(result.execution_id, Some("execution-2".to_string()));
    Ok(())
}

fn ready_status() -> Value {
    json!({
        "BackendState":"Running",
        "Self":{"DNSName":"workenv-01.tail.example.ts.net.","Tags":["tag:workenv"]},
        "CurrentTailnet":{"MagicDNSSuffix":"tail.example.ts.net"}
    })
}

fn not_running() -> Value {
    json!({"BackendState":"NeedsLogin","Self":{},"CurrentTailnet":{}})
}

fn request(operation: &str) -> AdapterRequest {
    AdapterRequest {
        protocol_version: PROTOCOL_VERSION,
        request_id: "request-1".to_string(),
        extension: "tailscale".to_string(),
        operation: operation.to_string(),
        target: Target {
            environment: "dev".to_string(),
            host: "workenv-01".to_string(),
            address: Some("exedev@workenv-01.exe.xyz".to_string()),
            directory: PathBuf::from("."),
            system: "x86_64-linux".to_string(),
            source: ".".to_string(),
            profiles: Vec::new(),
        },
        config: json!({"tailnet_suffix":"tail.example.ts.net","tag":"tag:workenv"}),
        input: json!({}),
        previous: None,
    }
}

struct Outputs {
    calls: Mutex<Vec<ExecutionSpec>>,
    values: Mutex<Vec<OutputValue>>,
}

impl Outputs {
    fn new(values: Vec<Value>) -> Self {
        Self::new_with_codes(values.into_iter().map(|value| (value, Some(0))).collect())
    }

    fn new_with_codes(values: Vec<(Value, Option<i32>)>) -> Self {
        Self {
            calls: Mutex::new(Vec::new()),
            values: Mutex::new(
                values
                    .into_iter()
                    .enumerate()
                    .map(|(index, (value, exit_code))| OutputValue {
                        value,
                        exit_code,
                        execution_id: format!("execution-{index}"),
                    })
                    .collect(),
            ),
        }
    }
}

impl Executor for Outputs {
    fn execute(&self, spec: ExecutionSpec) -> Result<ExecutionOutput> {
        self.calls
            .lock()
            .map_err(|_| anyhow::anyhow!("mutex poisoned"))?
            .push(spec);
        let value = self
            .values
            .lock()
            .map_err(|_| anyhow::anyhow!("mutex poisoned"))?
            .remove(0);
        Ok(ExecutionOutput {
            stdout: serde_json::to_string(&value.value)?,
            stderr: String::new(),
            exit_code: value.exit_code,
            execution_id: value.execution_id,
        })
    }
}

struct OutputValue {
    value: Value,
    exit_code: Option<i32>,
    execution_id: String,
}
