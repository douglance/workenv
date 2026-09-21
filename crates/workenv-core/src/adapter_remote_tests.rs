use std::{
    collections::VecDeque,
    path::{Path, PathBuf},
    sync::Mutex,
};

use serde_json::{Value, json};
use workenv_platform::ExecutionOutput;
use workenv_protocol::{Operation, PROTOCOL_VERSION, ResponseStatus};

use super::*;

// Named without a shared prefix: clippy rejects one, and these are three kinds
// of transport answer rather than three transports.
pub(super) enum MockExecution {
    Pending(Value, String),
    Target(AdapterResponse),
    /// Exactly this `data`, so a test can answer off-contract on purpose.
    Raw(Value, Option<String>),
}

pub(super) struct MockExecutor {
    outputs: Mutex<VecDeque<MockExecution>>,
    requests: Mutex<Vec<AdapterRequest>>,
}

impl MockExecutor {
    pub(super) fn new(outputs: Vec<MockExecution>) -> Self {
        Self {
            outputs: Mutex::new(outputs.into()),
            requests: Mutex::new(Vec::new()),
        }
    }
}

impl Executor for MockExecutor {
    fn execute(&self, spec: ExecutionSpec) -> Result<ExecutionOutput> {
        let stdin = spec.stdin.context("transport request stdin missing")?;
        let request: AdapterRequest = serde_json::from_slice(&stdin)?;
        self.requests
            .lock()
            .map_err(lock_error)?
            .push(request.clone());
        let output = self
            .outputs
            .lock()
            .map_err(lock_error)?
            .pop_front()
            .context("missing mock output")?;
        match output {
            MockExecution::Pending(data, execution_id) => output_for(&AdapterResponse {
                protocol_version: PROTOCOL_VERSION,
                request_id: request.request_id,
                status: ResponseStatus::Pending,
                data,
                error: None,
                execution_id: Some(execution_id),
            }),
            MockExecution::Target(target) => transport_output(&request, &target),
            MockExecution::Raw(data, error) => output_for(&AdapterResponse {
                protocol_version: PROTOCOL_VERSION,
                request_id: request.request_id,
                status: ResponseStatus::Changed,
                data,
                error,
                execution_id: None,
            }),
        }
    }
}

#[test]
fn transport_pending_retry_reuses_exact_transport_request_payload() -> Result<()> {
    let executor = MockExecutor::new(vec![
        MockExecution::Pending(json!({"remote": "running"}), "transport-exec-1".to_owned()),
        MockExecution::Target(target_response("remote-1", ResponseStatus::Changed)),
    ]);
    let mut call = invocation("remote-1");
    let first = invoke(Path::new("/tmp"), &manifest(), &executor, &call)?;
    assert_eq!(first.status, ResponseStatus::Pending);
    call.previous = Some(json!(first));
    let second = invoke(Path::new("/tmp"), &manifest(), &executor, &call)?;
    assert_eq!(second.status, ResponseStatus::Changed);
    let requests = executor.requests.lock().map_err(lock_error)?;
    assert_eq!(requests.len(), 2);
    assert_eq!(json!(requests[0]), json!(requests[1]));
    assert_eq!(requests[0].request_id, "remote-1:transport");
    Ok(())
}

#[test]
fn inner_target_pending_retry_uses_fresh_transport_id_and_stable_target_id() -> Result<()> {
    let executor = MockExecutor::new(vec![
        MockExecution::Target(target_response("remote-2", ResponseStatus::Pending)),
        MockExecution::Target(target_response("remote-2", ResponseStatus::Changed)),
    ]);
    let mut call = invocation("remote-2");
    let first = invoke(Path::new("/tmp"), &manifest(), &executor, &call)?;
    assert_eq!(first.status, ResponseStatus::Pending);
    call.previous = Some(json!(first.clone()));
    let second = invoke(Path::new("/tmp"), &manifest(), &executor, &call)?;
    assert_eq!(second.status, ResponseStatus::Changed);
    let requests = executor.requests.lock().map_err(lock_error)?;
    assert_eq!(requests[0].request_id, "remote-2:transport");
    assert!(requests[1].request_id.starts_with("remote-2:transport:"));
    assert_ne!(requests[0].request_id, requests[1].request_id);
    let first_target = target_request(&requests[0])?;
    let second_target = target_request(&requests[1])?;
    assert_eq!(first_target.request_id, "remote-2");
    assert_eq!(second_target.request_id, "remote-2");
    assert!(first_target.previous.is_none());
    assert_eq!(second_target.previous, Some(json!(first)));
    Ok(())
}

fn transport_output(request: &AdapterRequest, target: &AdapterResponse) -> Result<ExecutionOutput> {
    output_for(&AdapterResponse {
        protocol_version: PROTOCOL_VERSION,
        request_id: request.request_id.clone(),
        status: ResponseStatus::Changed,
        data: json!({
            "exit_code": 0,
            "stdout": serde_json::to_string(target)?,
            "stderr": "",
            "execution_id": "remote-exec-1",
        }),
        error: None,
        execution_id: None,
    })
}

fn output_for(response: &AdapterResponse) -> Result<ExecutionOutput> {
    Ok(ExecutionOutput {
        stdout: serde_json::to_string(response)?,
        stderr: String::new(),
        exit_code: Some(0),
        execution_id: response
            .execution_id
            .clone()
            .unwrap_or_else(|| "transport-process-1".to_owned()),
    })
}

fn target_request(request: &AdapterRequest) -> Result<AdapterRequest> {
    let stdin = request
        .input
        .get("stdin")
        .and_then(Value::as_str)
        .context("transport input stdin missing")?;
    Ok(serde_json::from_str(stdin)?)
}

fn target_response(request_id: &str, status: ResponseStatus) -> AdapterResponse {
    AdapterResponse {
        protocol_version: PROTOCOL_VERSION,
        request_id: request_id.to_owned(),
        status,
        data: json!({"ok": status == ResponseStatus::Changed}),
        error: None,
        execution_id: (status == ResponseStatus::Pending).then(|| "target-exec-1".to_owned()),
    }
}

pub(super) fn manifest() -> Manifest {
    Manifest {
        schema_version: PROTOCOL_VERSION,
        hosts: [("remote".to_owned(), host())].into(),
        environments: [("dev".to_owned(), environment())].into(),
        extensions: [
            (
                "adapter".to_owned(),
                extension("status", Location::Target, false),
            ),
            (
                "transport".to_owned(),
                extension("execute", Location::Controller, true),
            ),
        ]
        .into(),
    }
}

fn host() -> Host {
    Host {
        address: Some("ssh://example".to_owned()),
        transport: Some("transport".to_owned()),
        provider: None,
        system: "x86_64-linux".to_owned(),
    }
}

fn environment() -> Environment {
    Environment {
        host: "remote".to_owned(),
        directory: PathBuf::from("/work"),
        source: "path:.".to_owned(),
        profiles: Vec::new(),
        ephemeral: false,
        integrations: Vec::new(),
        connection: None,
        agent: None,
    }
}

fn extension(operation: &str, location: Location, internal: bool) -> Extension {
    Extension {
        version: "1.0.0".to_owned(),
        protocol_version: PROTOCOL_VERSION,
        executable: PathBuf::from(format!("/nix/store/bin/{operation}")),
        location,
        systems: Vec::new(),
        runtime_inputs: None,
        operations: [(
            operation.to_owned(),
            Operation {
                description: operation.to_owned(),
                location: None,
                mutating: false,
                internal,
                input_schema: json!(true),
                output_schema: json!(true),
            },
        )]
        .into(),
    }
}

pub(super) fn invocation(key: &str) -> Invocation<'static> {
    Invocation {
        extension_id: "adapter",
        operation: "status",
        environment: "dev",
        config: Value::Null,
        input: Value::Null,
        key: key.to_owned(),
        previous: None,
        allow_internal: false,
    }
}

fn lock_error<T>(_: std::sync::PoisonError<T>) -> anyhow::Error {
    anyhow::anyhow!("test lock poisoned")
}
