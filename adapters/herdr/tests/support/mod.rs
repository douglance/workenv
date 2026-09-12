//! Fixtures shared by the herdr contract and diagnostics suites.
//!
//! An integration test in `tests/` is its own crate, so two files cannot share
//! private helpers without a module like this one. Split out when contracts.rs
//! reached this repository's 300-line limit.

use std::sync::Mutex;

use anyhow::Result;
use serde_json::{Value, json};
use workenv_platform::{ExecutionOutput, ExecutionSpec, Executor};
use workenv_protocol::{AdapterRequest, PROTOCOL_VERSION, Target};

pub fn request(path: &std::path::Path, operation: &str) -> AdapterRequest {
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
        config: json!({"session":"workenv","label":"workenv-01"}),
        input: json!({}),
        previous: None,
    }
}

pub struct Outputs {
    pub calls: Mutex<Vec<ExecutionSpec>>,
    pub values: Mutex<Vec<OutputValue>>,
}

impl Outputs {
    pub fn new(values: Vec<Value>) -> Self {
        Self::new_with_codes(
            values
                .into_iter()
                .map(|value| (value, Some(0), ""))
                .collect(),
        )
    }

    pub fn new_with_codes(values: Vec<(Value, Option<i32>, &str)>) -> Self {
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

pub struct OutputValue {
    pub value: Value,
    pub exit_code: Option<i32>,
    pub execution_id: String,
    pub stderr: String,
}
