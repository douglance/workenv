use std::sync::Mutex;

use anyhow::Result;
use serde_json::{Value, json};
use workenv_platform::{ExecutionOutput, ExecutionSpec, Executor};
use workenv_protocol::{AdapterRequest, PROTOCOL_VERSION, ResponseStatus, Target};

use super::*;

#[test]
fn login_propagates_pending_command_execution() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let profiles = tempfile::tempdir()?;
    let request = request(temp.path(), profiles.path());
    let runner = Outputs::new(vec![
        (
            json!({"result":{"root_pane":{"pane_id":"pane-1"}}}),
            Some(0),
            "workspace",
            "",
        ),
        (json!({}), None, "login-command", ""),
    ]);
    let result = login(&request, &runner)?;
    assert_eq!(result.status, ResponseStatus::Pending);
    assert_eq!(result.execution_id, Some("login-command".to_string()));
    Ok(())
}

fn request(workenv: &std::path::Path, profiles: &std::path::Path) -> AdapterRequest {
    request_with_digest(workenv, profiles, "digest-a")
}

fn request_with_digest(
    workenv: &std::path::Path,
    profiles: &std::path::Path,
    digest: &str,
) -> AdapterRequest {
    AdapterRequest {
        protocol_version: PROTOCOL_VERSION,
        request_id: "request-1".to_string(),
        extension: "identity".to_string(),
        operation: "login".to_string(),
        target: Target {
            environment: "dev".to_string(),
            host: "workenv-01".to_string(),
            address: None,
            directory: workenv.to_path_buf(),
            system: "x86_64-linux".to_string(),
            source: ".".to_string(),
            profiles: Vec::new(),
        },
        config: json!({
            "session":"workenv",
            "profiles_dir": profiles.display().to_string(),
            "profile": {"name":"profile-a","digest":digest}
        }),
        input: json!({"service":"github"}),
        previous: None,
    }
}

struct Outputs {
    values: Mutex<Vec<OutputValue>>,
}

impl Outputs {
    fn new(values: Vec<(Value, Option<i32>, &str, &str)>) -> Self {
        Self {
            values: Mutex::new(
                values
                    .into_iter()
                    .map(|(value, exit_code, execution_id, stderr)| OutputValue {
                        value,
                        exit_code,
                        execution_id: execution_id.to_string(),
                        stderr: stderr.to_string(),
                    })
                    .collect(),
            ),
        }
    }
}

impl Executor for Outputs {
    fn execute(&self, _spec: ExecutionSpec) -> Result<ExecutionOutput> {
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
