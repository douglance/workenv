use std::{path::PathBuf, sync::Mutex};

use serde_json::{Value, json};
use workenv_platform::ExecutionOutput;
use workenv_protocol::{Operation, PROTOCOL_VERSION, ResponseStatus};

use super::*;

struct MockExecutor {
    specs: Mutex<Vec<ExecutionSpec>>,
}

impl Executor for MockExecutor {
    fn execute(&self, spec: ExecutionSpec) -> Result<ExecutionOutput> {
        let request: AdapterRequest = serde_json::from_slice(
            spec.stdin
                .as_deref()
                .context("controller request stdin missing")?,
        )?;
        self.specs.lock().map_err(lock_error)?.push(spec);
        output_for(&AdapterResponse {
            protocol_version: PROTOCOL_VERSION,
            request_id: request.request_id,
            status: ResponseStatus::Ready,
            data: json!({"ok": true}),
            error: None,
            execution_id: None,
        })
    }
}

#[test]
fn store_controller_extension_runs_through_devenv_shell() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let executor = MockExecutor {
        specs: Mutex::new(Vec::new()),
    };
    invoke(
        temp.path(),
        &manifest("/nix/store/225wgcabj8zszgzs72p9s2c9mqp8vkk8-pkg/bin/adapter"),
        &executor,
        &invocation(),
    )?;
    let specs = executor.specs.lock().map_err(lock_error)?;
    assert_eq!(specs[0].executable, "devenv");
    assert_eq!(
        specs[0].arg,
        [
            "shell",
            "--",
            "/nix/store/225wgcabj8zszgzs72p9s2c9mqp8vkk8-pkg/bin/adapter"
        ]
    );
    assert_eq!(specs[0].cwd.as_deref(), Some(temp.path()));
    Ok(())
}

#[test]
fn native_controller_extension_runs_directly_for_bootstrap_manifest() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let executor = MockExecutor {
        specs: Mutex::new(Vec::new()),
    };
    invoke(
        temp.path(),
        &manifest("/tmp/workenv-bootstrap-adapter"),
        &executor,
        &invocation(),
    )?;
    let specs = executor.specs.lock().map_err(lock_error)?;
    assert_eq!(specs[0].executable, "/tmp/workenv-bootstrap-adapter");
    assert!(specs[0].arg.is_empty());
    assert_eq!(specs[0].cwd.as_deref(), Some(temp.path()));
    Ok(())
}

fn output_for(response: &AdapterResponse) -> Result<ExecutionOutput> {
    Ok(ExecutionOutput {
        stdout: serde_json::to_string(response)?,
        stderr: String::new(),
        exit_code: Some(0),
        execution_id: "exec-1".to_owned(),
    })
}

fn manifest(executable: &str) -> Manifest {
    Manifest {
        schema_version: PROTOCOL_VERSION,
        hosts: [("local".to_owned(), host())].into(),
        environments: [("dev".to_owned(), environment())].into(),
        extensions: [("adapter".to_owned(), extension(executable))].into(),
    }
}

fn host() -> Host {
    Host {
        address: None,
        transport: None,
        provider: None,
        system: "aarch64-darwin".to_owned(),
    }
}

fn environment() -> Environment {
    Environment {
        host: "local".to_owned(),
        directory: PathBuf::from("/tmp"),
        source: "path:.".to_owned(),
        profiles: Vec::new(),
        ephemeral: false,
        integrations: Vec::new(),
        connection: None,
    }
}

fn extension(executable: &str) -> Extension {
    Extension {
        version: "1.0.0".to_owned(),
        protocol_version: PROTOCOL_VERSION,
        executable: PathBuf::from(executable),
        location: Location::Controller,
        systems: Vec::new(),
        operations: [(
            "inspect".to_owned(),
            Operation {
                description: "inspect".to_owned(),
                location: None,
                mutating: false,
                internal: false,
                input_schema: json!(true),
                output_schema: json!(true),
            },
        )]
        .into(),
    }
}

fn invocation() -> Invocation<'static> {
    Invocation {
        extension_id: "adapter",
        operation: "inspect",
        environment: "dev",
        config: Value::Null,
        input: Value::Null,
        key: "inspect-1".to_owned(),
        previous: None,
        allow_internal: false,
    }
}

fn lock_error<T>(_: std::sync::PoisonError<T>) -> anyhow::Error {
    anyhow::anyhow!("test lock poisoned")
}
