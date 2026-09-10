//! Herdr lifecycle cleanup contract tests.
use std::sync::Mutex;

use anyhow::Result;
use serde_json::{Value, json};
use workenv_adapter_herdr::handle_with;
use workenv_platform::{ExecutionOutput, ExecutionSpec, Executor};
use workenv_protocol::{AdapterRequest, PROTOCOL_VERSION, ResponseStatus, Target};

#[test]
fn noops_when_completed_lifecycle_never_registered() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let mut request = request(temp.path());
    request.input = completed_lifecycle_input(&[]);
    let runner = Outputs::default();

    let result = handle_with(&request, &runner)?;

    assert_eq!(result.status, ResponseStatus::Ready);
    assert_eq!(result.data["status"], "herdr_cleanup_not_registered");
    assert!(runner.calls.lock().is_ok_and(|calls| calls.is_empty()));
    Ok(())
}

#[test]
fn noops_when_completed_lifecycle_only_applied_config() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let mut request = request(temp.path());
    request.input = completed_lifecycle_input(&[json!({
        "operation": "apply",
        "response": {
            "status": "ready",
            "data": {"status": "herdr_server_ready"}
        }
    })]);
    let runner = Outputs::default();

    let result = handle_with(&request, &runner)?;

    assert_eq!(result.status, ResponseStatus::Ready);
    assert_eq!(result.data["status"], "herdr_cleanup_not_registered");
    assert!(runner.calls.lock().is_ok_and(|calls| calls.is_empty()));
    Ok(())
}

#[test]
fn blocks_register_receipt_without_profile_id() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let mut request = request(temp.path());
    request.input = completed_lifecycle_input(&[json!({
        "operation": "register",
        "response": {
            "status": "changed",
            "data": {
                "status": "registered",
                "target": "exedev@worker",
                "session": "workenv"
            }
        }
    })]);

    let result = handle_with(&request, &Outputs::default())?;

    assert_eq!(result.status, ResponseStatus::Failed);
    assert_eq!(result.data["status"], "herdr_cleanup_blocked");
    Ok(())
}

fn request(path: &std::path::Path) -> AdapterRequest {
    AdapterRequest {
        protocol_version: PROTOCOL_VERSION,
        request_id: "request-1".to_string(),
        extension: "herdr".to_string(),
        operation: "cleanup".to_string(),
        target: Target {
            environment: "dev".to_string(),
            host: "workenv-01".to_string(),
            address: Some("exedev@worker".to_string()),
            directory: path.to_path_buf(),
            system: "x86_64-linux".to_string(),
            source: ".".to_string(),
            profiles: Vec::new(),
        },
        config: json!({"session":"workenv","label":"workenv-01"}),
        input: json!({}),
        previous: None,
    }
}

fn completed_lifecycle_input(receipts: &[Value]) -> Value {
    json!({
        "provider_create": {
            "status": "changed",
            "data": {"owned": true}
        },
        "provider_destroy": {
            "status": "changed",
            "data": {}
        },
        "integration_receipts": receipts
    })
}

#[derive(Default)]
struct Outputs {
    calls: Mutex<Vec<ExecutionSpec>>,
}

impl Executor for Outputs {
    fn execute(&self, spec: ExecutionSpec) -> Result<ExecutionOutput> {
        self.calls
            .lock()
            .map_err(|_| anyhow::anyhow!("mutex poisoned"))?
            .push(spec);
        Err(anyhow::anyhow!("cleanup should not execute Herdr"))
    }
}
