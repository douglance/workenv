//! SSH transport adapter.
mod remote;

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use workenv_protocol::{AdapterRequest, AdapterResponse, ResponseStatus, serve};

use remote::{RemoteExecute, execute_remote, ssh_argv};

fn main() -> Result<()> {
    serve(handle)
}

fn handle(request: &AdapterRequest) -> Result<AdapterResponse> {
    match request.operation.as_str() {
        "inspect" => Ok(inspect(request)),
        "connect" => Ok(connect(request)),
        "execute" => execute(request),
        _ => Ok(response(
            request,
            ResponseStatus::Unsupported,
            json!({}),
            Some("unsupported SSH operation"),
        )),
    }
}

fn inspect(request: &AdapterRequest) -> AdapterResponse {
    let Some(address) = address(request) else {
        return unsupported(request, "SSH target address is required");
    };
    AdapterResponse::new(
        request,
        ResponseStatus::Ready,
        json!({"target":address,"directory":request.target.directory,
            "system":request.target.system,"ssh_argv":ssh_argv(address),
            "execute":"apoc execution start"}),
    )
}

fn connect(request: &AdapterRequest) -> AdapterResponse {
    let Some(address) = address(request) else {
        return unsupported(request, "SSH target address is required");
    };
    let session = str_field(&request.config, "session").unwrap_or(&request.target.environment);
    AdapterResponse::new(
        request,
        ResponseStatus::Ready,
        json!({"target":address,"session":session,"ssh_argv":ssh_argv(address),
            "attach_argv":["herdr","--remote",address,"--session",session]}),
    )
}

fn execute(request: &AdapterRequest) -> Result<AdapterResponse> {
    let Some(address) = address(request) else {
        return Ok(unsupported(request, "SSH target address is required"));
    };
    let argv = input_argv(&request.input)?;
    let cwd = str_field(&request.input, "cwd").map_or_else(
        || request.target.directory.to_string_lossy().into_owned(),
        str::to_owned,
    );
    let purpose = str_field(&request.input, "purpose")
        .unwrap_or("Run Workenv SSH adapter command through remote APoC.");
    let timeout = request.input["timeout_ms"].as_u64().unwrap_or(300_000);
    let result = execute_remote(RemoteExecute {
        address,
        request,
        argv: &argv,
        cwd: &cwd,
        purpose,
        timeout,
    })?;
    let Some(id) = result.execution_id else {
        return Ok(AdapterResponse::new(
            request,
            ResponseStatus::Ready,
            result.data,
        ));
    };
    let mut pending = AdapterResponse::new(request, ResponseStatus::Pending, result.data);
    pending.execution_id = Some(id);
    Ok(pending)
}

fn input_argv(input: &Value) -> Result<Vec<String>> {
    let Some(items) = input["argv"].as_array() else {
        bail!("execute input.argv is required");
    };
    let argv = items
        .iter()
        .map(|item| {
            item.as_str()
                .map(str::to_owned)
                .context("execute input.argv must contain strings")
        })
        .collect::<Result<Vec<_>>>()?;
    if argv.is_empty() {
        bail!("execute input.argv must not be empty");
    }
    Ok(argv)
}

fn address(request: &AdapterRequest) -> Option<&str> {
    request.target.address.as_deref()
}

fn str_field<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value.get(key).and_then(Value::as_str)
}

fn unsupported(request: &AdapterRequest, message: &str) -> AdapterResponse {
    response(
        request,
        ResponseStatus::Unsupported,
        json!({}),
        Some(message),
    )
}

fn response(
    request: &AdapterRequest,
    status: ResponseStatus,
    data: Value,
    error: Option<&str>,
) -> AdapterResponse {
    let mut response = AdapterResponse::new(request, status, data);
    response.error = error.map(str::to_owned);
    response
}
