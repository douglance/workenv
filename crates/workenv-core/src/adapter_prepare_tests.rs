use std::{path::Path, path::PathBuf, sync::Mutex};

use anyhow::{Context as _, Result};
use serde_json::{Value, json};
use workenv_platform::{ExecutionOutput, ExecutionSpec, Executor};
use workenv_protocol::{Operation, PROTOCOL_VERSION, ResponseStatus};

use super::*;

struct MockExecutor {
    calls: Mutex<Vec<ExecutionSpec>>,
}

impl MockExecutor {
    const fn ready() -> Self {
        Self {
            calls: Mutex::new(Vec::new()),
        }
    }
}

impl Executor for MockExecutor {
    fn execute(&self, spec: ExecutionSpec) -> Result<ExecutionOutput> {
        let request = request_from_stdin(&spec)?;
        let response = if request.extension == "transport" {
            transport_output(&request, self.calls.lock().map_err(lock_error)?.is_empty())?
        } else {
            response_for_request(&request, ResponseStatus::Changed)?
        };
        self.calls.lock().map_err(lock_error)?.push(spec);
        Ok(response)
    }
}

#[test]
fn prepare_local_target_runs_adapter_without_devenv_shell() -> Result<()> {
    let executor = MockExecutor::ready();
    invoke(
        Path::new("/tmp"),
        &manifest(false),
        &executor,
        &call("prepare", "local-prepare"),
    )?;
    let calls = executor.calls.lock().map_err(lock_error)?;
    assert_eq!(calls[0].executable, "/nix/store/bin/project-adapter");
    assert!(calls[0].arg.is_empty());
    assert!(calls[0].stdin.is_some());
    Ok(())
}

#[test]
fn apply_local_target_still_runs_through_devenv_shell() -> Result<()> {
    let executor = MockExecutor::ready();
    invoke(
        Path::new("/tmp"),
        &manifest(false),
        &executor,
        &call("apply", "local-apply"),
    )?;
    let calls = executor.calls.lock().map_err(lock_error)?;
    assert_eq!(calls[0].executable, "devenv");
    assert_eq!(calls[0].arg[..3], ["shell", "--from", "path:."]);
    assert!(calls[0].arg.iter().any(|arg| arg == "project-adapter"));
    Ok(())
}

#[test]
fn prepare_remote_target_sends_raw_seeded_adapter_basename() -> Result<()> {
    let executor = MockExecutor::ready();
    invoke(
        Path::new("/tmp"),
        &manifest(true),
        &executor,
        &call("prepare", "remote-prepare"),
    )?;
    let request = transport_requests(&executor)?.remove(0);
    assert_eq!(request.input["argv"], json!(["project-adapter"]));
    assert_eq!(request.input["cwd"], json!("/work"));
    Ok(())
}

#[test]
fn prepare_remote_pending_retry_reuses_transport_request() -> Result<()> {
    let executor = MockExecutor::ready();
    let mut invocation = call("prepare", "remote-pending");
    let first = invoke(Path::new("/tmp"), &manifest(true), &executor, &invocation)?;
    assert_eq!(first.status, ResponseStatus::Pending);
    invocation.previous = Some(json!(first));
    let second = invoke(Path::new("/tmp"), &manifest(true), &executor, &invocation)?;
    assert_eq!(second.status, ResponseStatus::Changed);
    let requests = transport_requests(&executor)?;
    assert_eq!(requests.len(), 2);
    assert_eq!(json!(requests[0]), json!(requests[1]));
    assert_eq!(requests[0].input["argv"], json!(["project-adapter"]));
    Ok(())
}

fn response_for_request(
    request: &AdapterRequest,
    status: ResponseStatus,
) -> Result<ExecutionOutput> {
    output_for(&AdapterResponse {
        protocol_version: PROTOCOL_VERSION,
        request_id: request.request_id.clone(),
        status,
        data: json!({"ok": status == ResponseStatus::Changed}),
        error: None,
        execution_id: None,
    })
}

fn transport_output(request: &AdapterRequest, first_call: bool) -> Result<ExecutionOutput> {
    if request.request_id == "remote-pending:transport" && first_call {
        return output_for(&AdapterResponse {
            protocol_version: PROTOCOL_VERSION,
            request_id: request.request_id.clone(),
            status: ResponseStatus::Pending,
            data: json!({"remote":"running"}),
            error: None,
            execution_id: Some("transport-exec-1".to_owned()),
        });
    }
    let target = request.input["stdin"]
        .as_str()
        .context("missing target stdin")?;
    let target: AdapterRequest = serde_json::from_str(target)?;
    let response = AdapterResponse {
        protocol_version: PROTOCOL_VERSION,
        request_id: target.request_id,
        status: ResponseStatus::Changed,
        data: json!({"ok":true}),
        error: None,
        execution_id: None,
    };
    output_for(&AdapterResponse::new(
        request,
        ResponseStatus::Changed,
        json!({"exit_code":0,"stdout":serde_json::to_string(&response)?,"stderr":""}),
    ))
}

fn request_from_stdin(spec: &ExecutionSpec) -> Result<AdapterRequest> {
    serde_json::from_slice(spec.stdin.as_ref().context("missing stdin")?).map_err(Into::into)
}

fn output_for(response: &AdapterResponse) -> Result<ExecutionOutput> {
    Ok(ExecutionOutput {
        stdout: serde_json::to_string(response)?,
        stderr: String::new(),
        exit_code: Some(0),
        execution_id: response
            .execution_id
            .clone()
            .unwrap_or_else(|| "exec-1".into()),
    })
}

fn transport_requests(executor: &MockExecutor) -> Result<Vec<AdapterRequest>> {
    executor
        .calls
        .lock()
        .map_err(lock_error)?
        .iter()
        .map(request_from_stdin)
        .collect()
}

fn manifest(remote: bool) -> Manifest {
    Manifest {
        schema_version: PROTOCOL_VERSION,
        hosts: [("host".to_owned(), host(remote))].into(),
        environments: [("dev".to_owned(), environment())].into(),
        extensions: extensions(remote),
    }
}

fn extensions(remote: bool) -> std::collections::BTreeMap<String, Extension> {
    let mut extensions: std::collections::BTreeMap<String, Extension> =
        [("project".to_owned(), target_extension())].into();
    if remote {
        extensions.insert("transport".to_owned(), transport_extension());
    }
    extensions
}

fn host(remote: bool) -> Host {
    Host {
        address: remote.then(|| "builder@example".to_owned()),
        transport: remote.then(|| "transport".to_owned()),
        provider: None,
        system: "x86_64-linux".to_owned(),
    }
}

fn environment() -> Environment {
    Environment {
        host: "host".to_owned(),
        directory: PathBuf::from("/work"),
        source: "path:.".to_owned(),
        profiles: Vec::new(),
        ephemeral: false,
        integrations: Vec::new(),
        connection: None,
    }
}

fn target_extension() -> Extension {
    Extension {
        version: "1.0.0".to_owned(),
        protocol_version: PROTOCOL_VERSION,
        executable: PathBuf::from("/nix/store/bin/project-adapter"),
        location: Location::Target,
        systems: Vec::new(),
        runtime_inputs: None,
        operations: ["prepare", "apply"].into_iter().map(operation).collect(),
    }
}

fn transport_extension() -> Extension {
    Extension {
        version: "1.0.0".to_owned(),
        protocol_version: PROTOCOL_VERSION,
        executable: PathBuf::from("/nix/store/bin/transport"),
        location: Location::Controller,
        systems: Vec::new(),
        runtime_inputs: None,
        operations: [operation("execute")].into(),
    }
}

fn operation(name: &str) -> (String, Operation) {
    (
        name.to_owned(),
        Operation {
            description: name.to_owned(),
            location: None,
            mutating: true,
            internal: name == "execute",
            input_schema: json!(true),
            output_schema: json!(true),
        },
    )
}

fn call(operation: &'static str, key: &str) -> Invocation<'static> {
    Invocation {
        extension_id: "project",
        operation,
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
