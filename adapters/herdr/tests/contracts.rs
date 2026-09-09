//! Herdr adapter contract tests.
use std::sync::Mutex;

use anyhow::Result;
use serde_json::{Value, json};
use workenv_adapter_herdr::handle_with;
use workenv_platform::{ExecutionOutput, ExecutionSpec, Executor};
use workenv_protocol::{AdapterRequest, PROTOCOL_VERSION, ResponseStatus, Target};

#[test]
fn register_rejects_same_label_wrong_session() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let runner = Outputs::new(vec![json!([
        {"label":"workenv-01","target":"exedev@worker","session":"default","enabled":true}
    ])]);
    let result = handle_with(&request(temp.path(), "register"), &runner)?;
    assert_eq!(result.status, ResponseStatus::Failed);
    assert_eq!(
        runner
            .calls
            .lock()
            .map_err(|_| anyhow::anyhow!("mutex poisoned"))?
            .len(),
        1
    );
    Ok(())
}

#[test]
fn register_adds_missing_machine_with_remote_session() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let runner = Outputs::new(vec![
        json!([]),
        json!({}),
        json!([
            {"label":"workenv-01","target":"exedev@worker","session":"workenv","enabled":true}
        ]),
    ]);
    let result = handle_with(&request(temp.path(), "register"), &runner)?;
    assert_eq!(result.status, ResponseStatus::Changed);
    let calls = runner
        .calls
        .lock()
        .map_err(|_| anyhow::anyhow!("mutex poisoned"))?;
    assert_eq!(calls[1].arg[5], "--remote-session");
    assert_eq!(calls[1].arg[6], "workenv");
    Ok(())
}

#[test]
fn register_does_not_change_when_add_fails() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let runner = Outputs::new_with_codes(vec![
        (json!([]), Some(0), ""),
        (json!({}), Some(1), "denied"),
    ]);
    let result = handle_with(&request(temp.path(), "register"), &runner)?;
    assert_eq!(result.status, ResponseStatus::Failed);
    assert_eq!(result.data["status"], "registration_failed");
    Ok(())
}

#[test]
fn register_propagates_pending_add_execution() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let runner = Outputs::new_with_codes(vec![(json!([]), Some(0), ""), (json!({}), None, "")]);
    let result = handle_with(&request(temp.path(), "register"), &runner)?;
    assert_eq!(result.status, ResponseStatus::Pending);
    assert_eq!(result.execution_id, Some("execution-1".to_string()));
    assert_eq!(result.data["status"], "registration_pending");
    Ok(())
}

#[test]
fn inspect_requires_detached_daemon_capability() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let status = json!({"running":true,"compatible":true,"version":"0.9.0","protocol":22,"server_binary_stale":false,"capabilities":{}});
    let runner = Outputs::new(vec![status]);
    let result = handle_with(&request(temp.path(), "inspect"), &runner)?;
    assert_eq!(result.status, ResponseStatus::Failed);
    Ok(())
}

fn request(path: &std::path::Path, operation: &str) -> AdapterRequest {
    AdapterRequest {
        protocol_version: PROTOCOL_VERSION,
        request_id: "request-1".to_string(),
        extension: "herdr".to_string(),
        operation: operation.to_string(),
        target: Target {
            environment: "dev".to_string(),
            host: "workenv-01".to_string(),
            address: Some("exedev@worker".to_string()),
            directory: path.to_path_buf(),
            system: "x86_64-linux".to_string(),
            source: ".".to_string(),
            profiles: Vec::new(),
        },
        config: json!({"session":"workenv"}),
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
        Self::new_with_codes(
            values
                .into_iter()
                .map(|value| (value, Some(0), ""))
                .collect(),
        )
    }

    fn new_with_codes(values: Vec<(Value, Option<i32>, &str)>) -> Self {
        Self {
            calls: Mutex::new(Vec::new()),
            values: Mutex::new(
                values
                    .into_iter()
                    .enumerate()
                    .map(|(index, (value, exit_code, stderr))| OutputValue {
                        value,
                        exit_code,
                        execution_id: format!("execution-{index}"),
                        stderr: stderr.to_string(),
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
            stderr: value.stderr,
            exit_code: value.exit_code,
            execution_id: value.execution_id,
        })
    }
}

struct OutputValue {
    value: Value,
    exit_code: Option<i32>,
    execution_id: String,
    stderr: String,
}
