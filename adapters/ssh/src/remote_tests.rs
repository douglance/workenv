use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
};

use anyhow::{Context, Result, anyhow};
use serde_json::{Value, json};
use workenv_platform::{ExecutionOutput, ExecutionSpec, Executor};
use workenv_protocol::{AdapterRequest, PROTOCOL_VERSION, Target};

use super::*;

#[derive(Default)]
struct RecordingExecutor {
    outputs: Mutex<VecDeque<ExecutionOutput>>,
    specs: Arc<Mutex<Vec<ExecutionSpec>>>,
}

impl RecordingExecutor {
    fn new(outputs: Vec<Value>) -> Result<Self> {
        let outputs = outputs
            .into_iter()
            .enumerate()
            .map(|(index, value)| {
                Ok(ExecutionOutput {
                    stdout: serde_json::to_string(&value)?,
                    stderr: String::new(),
                    exit_code: Some(0),
                    execution_id: format!("ssh-exec-{index}"),
                })
            })
            .collect::<Result<VecDeque<_>>>()?;
        Ok(Self {
            outputs: Mutex::new(outputs),
            specs: Arc::new(Mutex::new(Vec::new())),
        })
    }

    fn specs(&self) -> Result<Vec<ExecutionSpec>> {
        Ok(self
            .specs
            .lock()
            .map_err(|_| anyhow!("spec lock is poisoned"))?
            .clone())
    }
}

impl Executor for RecordingExecutor {
    fn execute(&self, spec: ExecutionSpec) -> Result<ExecutionOutput> {
        self.specs
            .lock()
            .map_err(|_| anyhow!("spec lock is poisoned"))?
            .push(spec);
        self.outputs
            .lock()
            .map_err(|_| anyhow!("output lock is poisoned"))?
            .pop_front()
            .context("missing fake output")
    }
}

#[test]
fn wait_timeout_returns_pending_and_retry_uses_fresh_wait_key() -> Result<()> {
    let request = request("remote-req");
    let argv = vec!["devenv".into(), "shell".into()];
    let executor = RecordingExecutor::new(vec![
        json!({"id":"remote-1"}),
        json!({"code":"TIMEOUT","message":"wait expired"}),
        json!({"id":"remote-1"}),
        json!({"outcome":"pending"}),
    ])?;

    let first = execute_remote_with(remote_execute(&request, &argv), &executor, ".".as_ref())?;
    let second = execute_remote_with(remote_execute(&request, &argv), &executor, ".".as_ref())?;

    assert_eq!(first.execution_id.as_deref(), Some("remote-1"));
    assert_eq!(second.execution_id.as_deref(), Some("remote-1"));
    let specs = executor.specs()?;
    assert_eq!(specs.len(), 4);
    assert_eq!(specs[0].idempotency_key, specs[2].idempotency_key);
    assert_ne!(specs[1].idempotency_key, specs[3].idempotency_key);
    assert!(remote_shell(&specs[1]).contains("'--timeout-ms' '25000'"));
    assert!(
        !specs
            .iter()
            .any(|spec| remote_shell(spec).contains("'logs'"))
    );
    Ok(())
}

#[test]
fn completed_retries_keep_start_stable_and_refresh_wait_and_logs() -> Result<()> {
    let request = request("remote-req");
    let argv = vec!["true".into()];
    let executor = RecordingExecutor::new(vec![
        json!({"id":"remote-1"}),
        json!({"outcome":"passed","result":{"exit_code":0}}),
        json!({"stdout":"one","stderr":""}),
        json!({"id":"remote-1"}),
        json!({"outcome":"passed","result":{"exit_code":0}}),
        json!({"stdout":"two","stderr":""}),
    ])?;

    let first = execute_remote_with(remote_execute(&request, &argv), &executor, ".".as_ref())?;
    let second = execute_remote_with(remote_execute(&request, &argv), &executor, ".".as_ref())?;

    assert_eq!(first.data["stdout"], "one");
    assert_eq!(second.data["stdout"], "two");
    let specs = executor.specs()?;
    assert_eq!(specs[0].idempotency_key, specs[3].idempotency_key);
    assert_ne!(specs[1].idempotency_key, specs[4].idempotency_key);
    assert_ne!(specs[2].idempotency_key, specs[5].idempotency_key);
    Ok(())
}

#[test]
fn stdin_wrapper_preserves_executable_argument() -> Result<()> {
    let args = stdin_wrapper_args("hello", "cat", &[], ".");
    let output = std::process::Command::new("bash").args(args).output()?;
    assert!(output.status.success());
    assert_eq!(String::from_utf8_lossy(&output.stdout), "hello");
    Ok(())
}

fn remote_execute<'a>(request: &'a AdapterRequest, argv: &'a [String]) -> RemoteExecute<'a> {
    RemoteExecute {
        address: "host.example",
        request,
        argv,
        cwd: "/work",
        purpose: "test remote execution",
        timeout: 300_000,
    }
}

fn request(request_id: &str) -> AdapterRequest {
    AdapterRequest {
        protocol_version: PROTOCOL_VERSION,
        request_id: request_id.into(),
        extension: "ssh".into(),
        operation: "execute".into(),
        config: json!({}),
        input: json!({}),
        previous: None,
        target: Target {
            environment: "env".into(),
            host: "host".into(),
            address: Some("host.example".into()),
            directory: "/work".into(),
            system: "x86_64-linux".into(),
            source: ".".into(),
            profiles: vec![],
        },
    }
}

fn remote_shell(spec: &ExecutionSpec) -> &str {
    spec.arg.last().map_or("", String::as_str)
}
