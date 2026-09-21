use std::sync::Mutex;

use anyhow::{Context as _, Result};
use serde_json::{Value, json};
use workenv_platform::{ExecutionOutput, ExecutionSpec, Executor};
use workenv_protocol::{
    AdapterResponse, Binding, Environment, Extension, Host, Location, Manifest, Operation,
    PROTOCOL_VERSION, ResponseStatus,
};

#[derive(Default)]
pub(super) struct MockExecutor {
    pub(super) calls: Mutex<Vec<ExecutionSpec>>,
    pub(super) responses: Mutex<Vec<AdapterResponse>>,
}

impl Executor for MockExecutor {
    fn execute(&self, spec: ExecutionSpec) -> Result<ExecutionOutput> {
        let mut response = self.responses.lock().map_err(lock_error)?.remove(0);
        bind_response(&spec, &mut response)?;
        let stdout = if spec.stdin.is_some() {
            serde_json::to_string(&response)?
        } else {
            response.data["stdout"]
                .as_str()
                .unwrap_or_default()
                .to_owned()
        };
        self.calls.lock().map_err(lock_error)?.push(spec);
        Ok(ExecutionOutput {
            stdout,
            stderr: String::new(),
            exit_code: Some(0),
            execution_id: "exec-1".to_owned(),
        })
    }
}

fn bind_response(spec: &ExecutionSpec, response: &mut AdapterResponse) -> Result<()> {
    let Some(input) = &spec.stdin else {
        return Ok(());
    };
    let request: workenv_protocol::AdapterRequest = serde_json::from_slice(input)?;
    response.request_id.clone_from(&request.request_id);
    if let Some(inner) = request.input["stdin"].as_str() {
        let inner: workenv_protocol::AdapterRequest = serde_json::from_str(inner)?;
        let mut decoded: AdapterResponse = serde_json::from_str(
            response.data["stdout"]
                .as_str()
                .context("missing transport stdout")?,
        )?;
        decoded.request_id = inner.request_id;
        response.data["stdout"] = json!(serde_json::to_string(&decoded)?);
    }
    Ok(())
}

pub(super) fn manifest(ephemeral: bool) -> Manifest {
    Manifest {
        schema_version: PROTOCOL_VERSION,
        hosts: [("local".to_owned(), host(None))].into(),
        environments: [("dev".to_owned(), environment(ephemeral))].into(),
        extensions: [("setup".to_owned(), setup_extension(Location::Controller))].into(),
    }
}

pub(super) fn integration_manifest(ephemeral: bool) -> Manifest {
    Manifest {
        schema_version: PROTOCOL_VERSION,
        hosts: [("local".to_owned(), host_without_provider(None))].into(),
        environments: [("dev".to_owned(), environment(ephemeral))].into(),
        extensions: [("setup".to_owned(), setup_extension(Location::Controller))].into(),
    }
}

pub(super) fn provider_manifest(ephemeral: bool) -> Manifest {
    let mut environment = environment(ephemeral);
    environment.integrations.clear();
    Manifest {
        schema_version: PROTOCOL_VERSION,
        hosts: [("local".to_owned(), host(None))].into(),
        environments: [("dev".to_owned(), environment)].into(),
        extensions: [("setup".to_owned(), setup_extension(Location::Controller))].into(),
    }
}

pub(super) fn target_manifest() -> Manifest {
    let mut environment = environment(false);
    environment.integrations = vec![Binding {
        extension: "target-tool".to_owned(),
        config: Value::Null,
    }];
    Manifest {
        schema_version: PROTOCOL_VERSION,
        hosts: [("local".to_owned(), host_without_provider(Some("ssh")))].into(),
        environments: [("dev".to_owned(), environment)].into(),
        extensions: [
            ("target-tool".to_owned(), setup_extension(Location::Target)),
            ("ssh".to_owned(), transport_extension()),
        ]
        .into(),
    }
}

pub(super) fn system_manifest(
    location: Location,
    systems: Vec<String>,
    host_system: String,
) -> Manifest {
    let mut host = host_without_provider(None);
    host.system = host_system;
    let mut extension = setup_extension(location);
    extension.systems = systems;
    Manifest {
        schema_version: PROTOCOL_VERSION,
        hosts: [("local".to_owned(), host)].into(),
        environments: [("dev".to_owned(), environment(false))].into(),
        extensions: [("setup".to_owned(), extension)].into(),
    }
}

pub(super) fn current_system() -> String {
    match (std::env::consts::ARCH, std::env::consts::OS) {
        ("aarch64", "macos") => "aarch64-darwin".to_owned(),
        ("x86_64", "macos") => "x86_64-darwin".to_owned(),
        ("aarch64", "linux") => "aarch64-linux".to_owned(),
        ("x86_64", "linux") => "x86_64-linux".to_owned(),
        (arch, os) => format!("{arch}-{os}"),
    }
}

pub(super) fn alternate_system() -> String {
    if current_system() == "x86_64-linux" {
        "aarch64-darwin".to_owned()
    } else {
        "x86_64-linux".to_owned()
    }
}

pub(super) fn host(transport: Option<&str>) -> Host {
    Host {
        address: Some("builder@example".to_owned()),
        transport: transport.map(str::to_owned),
        provider: Some(Binding {
            extension: "setup".to_owned(),
            config: Value::Null,
        }),
        system: "aarch64-darwin".to_owned(),
    }
}

pub(super) fn host_without_provider(transport: Option<&str>) -> Host {
    Host {
        address: Some("builder@example".to_owned()),
        transport: transport.map(str::to_owned),
        provider: None,
        system: "aarch64-darwin".to_owned(),
    }
}

pub(super) fn environment(ephemeral: bool) -> Environment {
    Environment {
        host: "local".to_owned(),
        directory: std::path::PathBuf::from("/tmp/dev"),
        source: "path:.".to_owned(),
        profiles: Vec::new(),
        ephemeral,
        integrations: vec![Binding {
            extension: "setup".to_owned(),
            config: Value::Null,
        }],
        connection: None,
        agent: None,
    }
}

pub(super) fn setup_extension(location: Location) -> Extension {
    Extension {
        version: "1.0.0".to_owned(),
        protocol_version: PROTOCOL_VERSION,
        executable: std::path::PathBuf::from("/nix/store/bin/target-tool"),
        location,
        systems: Vec::new(),
        runtime_inputs: None,
        operations: [
            ("apply".to_owned(), operation(true, json!(true))),
            ("bootstrap".to_owned(), operation(true, json!(true))),
            (
                "create".to_owned(),
                operation(true, json!({"type":"object"})),
            ),
            ("destroy".to_owned(), operation(true, json!(true))),
            ("inspect".to_owned(), operation(false, json!(true))),
            ("status".to_owned(), operation(false, json!(true))),
            ("connect".to_owned(), operation(false, json!(true))),
        ]
        .into(),
    }
}

pub(super) fn transport_extension() -> Extension {
    Extension {
        version: "1.0.0".to_owned(),
        protocol_version: PROTOCOL_VERSION,
        executable: std::path::PathBuf::from("/nix/store/bin/transport"),
        location: Location::Controller,
        systems: Vec::new(),
        runtime_inputs: None,
        operations: [
            (
                "connect".to_owned(),
                Operation {
                    description: "connect".to_owned(),
                    location: None,
                    mutating: false,
                    internal: false,
                    input_schema: json!(true),
                    output_schema: json!({"type":"object"}),
                },
            ),
            (
                "execute".to_owned(),
                Operation {
                    description: "execute".to_owned(),
                    location: None,
                    mutating: true,
                    internal: true,
                    input_schema: json!({"type":"object"}),
                    output_schema: json!({"type":"object"}),
                },
            ),
        ]
        .into(),
    }
}

pub(super) fn operation(mutating: bool, input_schema: Value) -> Operation {
    Operation {
        description: "operation".to_owned(),
        location: None,
        mutating,
        internal: false,
        input_schema,
        output_schema: json!({"type":"object"}),
    }
}

pub(super) fn response(request_id: &str) -> AdapterResponse {
    AdapterResponse {
        protocol_version: PROTOCOL_VERSION,
        request_id: request_id.to_owned(),
        status: ResponseStatus::Changed,
        data: json!({"ok": true}),
        error: None,
        execution_id: None,
    }
}

pub(super) fn raw_response(stdout: &str) -> AdapterResponse {
    AdapterResponse {
        data: json!({"stdout":stdout}),
        ..response("raw")
    }
}

pub(super) fn descriptor_response(request_id: &str) -> AdapterResponse {
    AdapterResponse {
        data: json!({"attach_argv":["herdr","--remote","builder@example"],"cwd":"/tmp/dev"}),
        ..response(request_id)
    }
}

pub(super) fn pending_response(request_id: &str) -> AdapterResponse {
    AdapterResponse {
        protocol_version: PROTOCOL_VERSION,
        request_id: request_id.to_owned(),
        status: ResponseStatus::Pending,
        data: json!({"status":"waiting"}),
        error: None,
        execution_id: Some("exec-pending".to_owned()),
    }
}

pub(super) fn transport_response(request_id: &str) -> AdapterResponse {
    let stdout = serde_json::to_string(&response(request_id)).unwrap_or_default();
    AdapterResponse {
        data: json!({
            "stdout": stdout,
            "stderr": "",
            "exit_code": 0,
            "execution_id": "remote-exec",
        }),
        ..response(&format!("{request_id}:transport"))
    }
}

pub(super) fn lock_error<T>(_: std::sync::PoisonError<T>) -> anyhow::Error {
    anyhow::anyhow!("mock lock poisoned")
}
