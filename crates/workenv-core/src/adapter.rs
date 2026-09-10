//! Adapter invocation and response validation.
use std::path::Path;

use anyhow::{Context as _, Result, bail};
use serde_json::{Value, json};
use workenv_platform::{ExecutionSpec, Executor};
use workenv_protocol::{
    AdapterRequest, AdapterResponse, Binding, Environment, Extension, Host, Location, Manifest,
    Target,
};

use crate::{
    adapter_response::{outer_execution_id, response_from_output, validate_response},
    validate,
};

#[path = "adapter_target.rs"]
mod adapter_target;
#[path = "adapter_transport.rs"]
mod adapter_transport;

use adapter_target::{local_target_command, target_command};
use adapter_transport::{pending_transport, request_for_transport, transport_response};

pub(crate) struct Invocation<'a> {
    pub(crate) extension_id: &'a str,
    pub(crate) operation: &'a str,
    pub(crate) environment: &'a str,
    pub(crate) config: Value,
    pub(crate) input: Value,
    pub(crate) key: String,
    pub(crate) previous: Option<Value>,
    pub(crate) allow_internal: bool,
}

pub(crate) struct BindingInvocation<'a> {
    pub(crate) binding: &'a Binding,
    pub(crate) operation: &'a str,
    pub(crate) environment: &'a str,
    pub(crate) input: Value,
    pub(crate) key: String,
    pub(crate) previous: Option<Value>,
}

pub(crate) fn binding_call(call: BindingInvocation<'_>) -> Invocation<'_> {
    Invocation {
        extension_id: &call.binding.extension,
        operation: call.operation,
        environment: call.environment,
        config: call.binding.config.clone(),
        input: call.input,
        key: call.key,
        previous: call.previous,
        allow_internal: false,
    }
}

pub(crate) fn invoke(
    root: &Path,
    manifest: &Manifest,
    executor: &dyn Executor,
    call: &Invocation<'_>,
) -> Result<AdapterResponse> {
    let runtime = Runtime {
        root,
        manifest,
        executor,
    };
    let env = environment(manifest, call.environment)?;
    let host = host(manifest, &env.host)?;
    let extension = extension_by_id(manifest, call.extension_id)?;
    let operation = operation(extension, call.operation)?;
    let location = validate::operation_location(extension, operation);
    if operation.internal && !call.allow_internal {
        bail!("operation {} is internal", call.operation);
    }
    validate::instance(&operation.input_schema, &call.input, "adapter input")?;
    let system = validate::execution_system_for(location, &host.system)?;
    if !validate::supports_system(extension, &system) {
        bail!("{} does not support {system}", call.extension_id);
    }
    let request = AdapterRequest {
        protocol_version: manifest.schema_version,
        request_id: call.key.clone(),
        extension: call.extension_id.to_owned(),
        operation: call.operation.to_owned(),
        target: target(call.environment, env, host),
        config: call.config.clone(),
        input: call.input.clone(),
        previous: call.previous.clone(),
    };
    let response = match location {
        Location::Controller => invoke_controller(&runtime, extension, &request)?,
        Location::Target => invoke_target(&runtime, host, extension, &request)?,
    };
    validate_response(extension, call.operation, &request, response)
}

struct Runtime<'a> {
    root: &'a Path,
    manifest: &'a Manifest,
    executor: &'a dyn Executor,
}

fn target(environment: &str, env: &Environment, host: &Host) -> Target {
    Target {
        environment: environment.to_owned(),
        host: env.host.clone(),
        address: host.address.clone(),
        directory: env.directory.clone(),
        system: host.system.clone(),
        source: env.source.clone(),
        profiles: env.profiles.clone(),
    }
}

fn invoke_controller(
    runtime: &Runtime<'_>,
    extension: &Extension,
    request: &AdapterRequest,
) -> Result<AdapterResponse> {
    invoke_controller_with_previous(runtime, extension, request, request.previous.as_ref())
}

fn invoke_controller_with_previous(
    runtime: &Runtime<'_>,
    extension: &Extension,
    request: &AdapterRequest,
    previous: Option<&Value>,
) -> Result<AdapterResponse> {
    let (executable, arg) = controller_command(extension);
    let spec = ExecutionSpec {
        executable,
        arg,
        cwd: Some(runtime.root.to_path_buf()),
        stdin: Some(serde_json::to_vec(request)?),
        timeout_ms: 300_000,
        idempotency_key: execution_key(request, previous),
        purpose: format!(
            "Run workenv adapter {} {}.",
            request.extension, request.operation
        ),
    };
    execute_or_observe(runtime.executor, request, spec, previous)
}

fn controller_command(extension: &Extension) -> (String, Vec<String>) {
    let executable = extension.executable.to_string_lossy().into_owned();
    if extension.executable.starts_with("/nix/store") {
        (
            "devenv".to_owned(),
            vec!["shell".to_owned(), "--".to_owned(), executable],
        )
    } else {
        (executable, Vec::new())
    }
}

fn invoke_target(
    runtime: &Runtime<'_>,
    host: &Host,
    extension: &Extension,
    request: &AdapterRequest,
) -> Result<AdapterResponse> {
    let Some(transport_id) = &host.transport else {
        return invoke_local_target(runtime.executor, extension, request);
    };
    let transport = extension_by_id(runtime.manifest, transport_id)?;
    if transport.location != Location::Controller {
        bail!("transport extension {transport_id} must run on the controller");
    }
    if let Some(pending) = pending_transport(request.previous.as_ref(), &request.request_id)? {
        let previous = json!(pending.response);
        let output =
            invoke_controller_with_previous(runtime, transport, &pending.request, Some(&previous))?;
        return transport_response(&output, request, &pending.request);
    }
    let input = transport_input(extension, request)?;
    let fresh = request.previous.is_some();
    let transport_request = request_for_transport(request, transport_id, input, fresh);
    let output = invoke_controller_with_previous(runtime, transport, &transport_request, None)?;
    transport_response(&output, request, &transport_request)
}

fn invoke_local_target(
    executor: &dyn Executor,
    extension: &Extension,
    request: &AdapterRequest,
) -> Result<AdapterResponse> {
    let (executable, arg) = local_target_command(extension, request)?;
    let spec = ExecutionSpec {
        executable,
        arg,
        cwd: Some(request.target.directory.clone()),
        stdin: Some(serde_json::to_vec(request)?),
        timeout_ms: 300_000,
        idempotency_key: execution_key(request, request.previous.as_ref()),
        purpose: format!(
            "Run target workenv adapter {} {}.",
            request.extension, request.operation
        ),
    };
    execute_or_observe(executor, request, spec, request.previous.as_ref())
}

fn execute_or_observe(
    executor: &dyn Executor,
    request: &AdapterRequest,
    spec: ExecutionSpec,
    previous: Option<&Value>,
) -> Result<AdapterResponse> {
    let output = match outer_execution_id(previous, &request.request_id) {
        Some(execution_id) => executor.observe(execution_id, &spec.purpose, spec.timeout_ms)?,
        None => executor.execute(spec)?,
    };
    response_from_output(&output, request)
}

fn execution_key(request: &AdapterRequest, previous: Option<&Value>) -> String {
    if previous.is_some() && outer_execution_id(previous, &request.request_id).is_none() {
        format!(
            "{}:adapter-invocation:{}",
            request.request_id,
            uuid::Uuid::new_v4()
        )
    } else {
        request.request_id.clone()
    }
}

fn transport_input(extension: &Extension, request: &AdapterRequest) -> Result<Value> {
    Ok(json!({
        "argv": target_command(extension, request)?,
        "cwd": request.target.directory,
        "stdin": serde_json::to_string(request)?,
    }))
}

fn environment<'a>(manifest: &'a Manifest, name: &str) -> Result<&'a Environment> {
    manifest
        .environments
        .get(name)
        .with_context(|| format!("unknown environment {name}"))
}

fn host<'a>(manifest: &'a Manifest, name: &str) -> Result<&'a Host> {
    manifest
        .hosts
        .get(name)
        .with_context(|| format!("unknown host {name}"))
}

fn extension_by_id<'a>(manifest: &'a Manifest, id: &str) -> Result<&'a Extension> {
    manifest
        .extensions
        .get(id)
        .with_context(|| format!("unknown extension {id}"))
}

fn operation<'a>(extension: &'a Extension, name: &str) -> Result<&'a workenv_protocol::Operation> {
    extension
        .operations
        .get(name)
        .with_context(|| format!("unknown operation {name}"))
}

#[cfg(test)]
#[path = "adapter_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "adapter_remote_tests.rs"]
mod remote_tests;

#[cfg(test)]
#[path = "adapter_prepare_tests.rs"]
mod prepare_tests;

#[cfg(test)]
#[path = "adapter_controller_tests.rs"]
mod controller_tests;
