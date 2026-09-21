use std::{
    collections::VecDeque,
    path::{Path, PathBuf},
    sync::Mutex,
};

use serde_json::{Value, json};
use workenv_platform::ExecutionOutput;
use workenv_protocol::{Operation, PROTOCOL_VERSION, ResponseStatus};

use super::*;

#[derive(Default)]
struct MockExecutor {
    execute_outputs: Mutex<VecDeque<ExecutionOutput>>,
    observe_outputs: Mutex<VecDeque<ExecutionOutput>>,
    executed: Mutex<Vec<ExecutionSpec>>,
    observed: Mutex<Vec<String>>,
}

impl MockExecutor {
    fn with_execute(output: ExecutionOutput) -> Self {
        Self {
            execute_outputs: Mutex::new(VecDeque::from([output])),
            ..Self::default()
        }
    }

    fn with_observe(output: ExecutionOutput) -> Self {
        Self {
            observe_outputs: Mutex::new(VecDeque::from([output])),
            ..Self::default()
        }
    }
}

impl Executor for MockExecutor {
    fn execute(&self, spec: ExecutionSpec) -> Result<ExecutionOutput> {
        self.executed.lock().map_err(lock_error)?.push(spec);
        self.execute_outputs
            .lock()
            .map_err(lock_error)?
            .pop_front()
            .context("missing execute output")
    }

    fn observe(
        &self,
        execution_id: &str,
        _purpose: &str,
        _timeout_ms: u64,
    ) -> Result<ExecutionOutput> {
        self.observed
            .lock()
            .map_err(lock_error)?
            .push(execution_id.to_owned());
        self.observe_outputs
            .lock()
            .map_err(lock_error)?
            .pop_front()
            .context("missing observe output")
    }
}

#[test]
fn complete_response_is_validated_against_output_schema() -> Result<()> {
    let response = AdapterResponse {
        protocol_version: PROTOCOL_VERSION,
        request_id: "read-1".to_owned(),
        status: ResponseStatus::Ready,
        data: Value::Null,
        error: None,
        execution_id: None,
    };
    let executor = MockExecutor::with_execute(output_for(&response)?);
    let error = invoke(
        Path::new("/tmp"),
        &manifest(Location::Controller),
        &executor,
        &invocation(),
    )
    .err()
    .context("invalid adapter output unexpectedly succeeded")?;
    assert!(error.to_string().contains("adapter output"));
    Ok(())
}

#[test]
fn retained_adapter_execution_is_observed_without_relaunch() -> Result<()> {
    let response = AdapterResponse {
        protocol_version: PROTOCOL_VERSION,
        request_id: "read-1".to_owned(),
        status: ResponseStatus::Changed,
        data: json!({"ok": true}),
        error: None,
        execution_id: Some("exec-1".to_owned()),
    };
    let executor = MockExecutor::with_observe(output_for(&response)?);
    let mut call = invocation();
    call.previous = Some(json!({
        "protocol_version": PROTOCOL_VERSION,
        "request_id": "read-1",
        "status": "pending",
        "data": {"kind": "adapter_execution", "request_id": "read-1"},
        "error": null,
        "execution_id": "exec-1",
    }));
    let observed = invoke(
        Path::new("/tmp"),
        &manifest(Location::Controller),
        &executor,
        &call,
    )?;
    assert_eq!(observed.status, ResponseStatus::Changed);
    assert!(executor.executed.lock().map_err(lock_error)?.is_empty());
    assert_eq!(*executor.observed.lock().map_err(lock_error)?, ["exec-1"]);
    Ok(())
}

#[test]
fn completed_pending_response_reenters_adapter_with_fresh_process_key() -> Result<()> {
    let response = AdapterResponse {
        protocol_version: PROTOCOL_VERSION,
        request_id: "read-1".to_owned(),
        status: ResponseStatus::Changed,
        data: json!({"ok": true}),
        error: None,
        execution_id: None,
    };
    let executor = MockExecutor::with_execute(output_for(&response)?);
    let mut call = invocation();
    call.previous = Some(json!({
        "protocol_version": PROTOCOL_VERSION,
        "request_id": "read-1",
        "status": "pending",
        "data": {"provider_request": "still-running"},
        "error": null,
        "execution_id": "provider-exec-1",
    }));
    invoke(
        Path::new("/tmp"),
        &manifest(Location::Controller),
        &executor,
        &call,
    )?;
    let executed = executor.executed.lock().map_err(lock_error)?;
    assert_eq!(executed.len(), 1);
    assert!(
        executed[0]
            .idempotency_key
            .starts_with("read-1:adapter-invocation:")
    );
    assert_ne!(executed[0].idempotency_key, "read-1");
    Ok(())
}

#[test]
fn target_argv_uses_shell_from_source_profiles_then_adapter_basename() -> Result<()> {
    let mut call = invocation();
    call.key = "target-1".to_owned();
    let response = AdapterResponse {
        protocol_version: PROTOCOL_VERSION,
        request_id: "target-1".to_owned(),
        status: ResponseStatus::Changed,
        data: json!({"ok": true}),
        error: None,
        execution_id: None,
    };
    let executor = MockExecutor::with_execute(output_for(&response)?);
    let manifest = manifest(Location::Target);
    invoke(Path::new("/tmp"), &manifest, &executor, &call)?;
    let executed = executor.executed.lock().map_err(lock_error)?;
    assert_eq!(executed[0].executable, "devenv");
    assert_eq!(
        executed[0].arg,
        [
            "shell",
            "--from",
            "path:/source",
            "--profile",
            "default",
            "--profile",
            "tools",
            "--",
            "adapter"
        ]
    );
    Ok(())
}

fn output_for(response: &AdapterResponse) -> Result<ExecutionOutput> {
    Ok(ExecutionOutput {
        stdout: serde_json::to_string(response)?,
        stderr: String::new(),
        exit_code: Some(0),
        execution_id: response
            .execution_id
            .clone()
            .unwrap_or_else(|| "exec-1".to_owned()),
    })
}

fn manifest(location: Location) -> Manifest {
    Manifest {
        schema_version: PROTOCOL_VERSION,
        hosts: [("local".to_owned(), host())].into(),
        environments: [("dev".to_owned(), environment())].into(),
        extensions: [("adapter".to_owned(), extension(location))].into(),
    }
}

fn host() -> Host {
    Host {
        address: None,
        transport: None,
        provider: None,
        system: crate::validate::execution_system_for(Location::Controller, "unused")
            .unwrap_or_else(|_| "x86_64-linux".to_owned()),
    }
}

fn environment() -> Environment {
    Environment {
        host: "local".to_owned(),
        directory: PathBuf::from("/tmp"),
        source: "path:/source".to_owned(),
        profiles: vec!["default".to_owned(), "tools".to_owned()],
        ephemeral: false,
        integrations: Vec::new(),
        connection: None,
        agent: None,
    }
}

fn extension(location: Location) -> Extension {
    Extension {
        version: "1.0.0".to_owned(),
        protocol_version: PROTOCOL_VERSION,
        executable: PathBuf::from("/nix/store/bin/adapter"),
        location,
        systems: Vec::new(),
        runtime_inputs: None,
        operations: [(
            "status".to_owned(),
            Operation {
                description: "status".to_owned(),
                location: None,
                mutating: false,
                internal: false,
                input_schema: json!(true),
                output_schema: json!({"type":"object"}),
            },
        )]
        .into(),
    }
}

fn invocation() -> Invocation<'static> {
    Invocation {
        extension_id: "adapter",
        operation: "status",
        environment: "dev",
        config: Value::Null,
        input: Value::Null,
        key: "read-1".to_owned(),
        previous: None,
        allow_internal: false,
    }
}

fn lock_error<T>(_: std::sync::PoisonError<T>) -> anyhow::Error {
    anyhow::anyhow!("test lock poisoned")
}
