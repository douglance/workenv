use anyhow::{Context as _, Result, bail};
use serde_json::{Value, json};
use workenv_protocol::{AdapterRequest, AdapterResponse, PROTOCOL_VERSION, ResponseStatus};

pub(super) struct PendingTransport {
    pub(super) request: AdapterRequest,
    pub(super) response: AdapterResponse,
}

pub(super) fn request_for_transport(
    base: &AdapterRequest,
    extension_id: &str,
    input: Value,
    fresh: bool,
) -> AdapterRequest {
    let request_id = if fresh {
        format!("{}:transport:{}", base.request_id, uuid::Uuid::new_v4())
    } else {
        format!("{}:transport", base.request_id)
    };
    AdapterRequest {
        protocol_version: PROTOCOL_VERSION,
        request_id,
        extension: extension_id.to_owned(),
        operation: "execute".to_owned(),
        target: base.target.clone(),
        config: Value::Null,
        input,
        previous: None,
    }
}

pub(super) fn pending_transport(
    previous: Option<&Value>,
    target_request_id: &str,
) -> Result<Option<PendingTransport>> {
    let Some(previous) = previous else {
        return Ok(None);
    };
    if previous["status"].as_str() != Some("pending") {
        return Ok(None);
    }
    let data = previous
        .get("data")
        .context("pending response has no data")?;
    if data["kind"].as_str() != Some("transport_pending") {
        return Ok(None);
    }
    if previous["request_id"].as_str() != Some(target_request_id) {
        bail!("transport pending response request ID mismatch");
    }
    let request = serde_json::from_value(data["request"].clone())?;
    let response = serde_json::from_value(data["response"].clone())?;
    Ok(Some(PendingTransport { request, response }))
}

pub(super) fn transport_response(
    output: &AdapterResponse,
    request: &AdapterRequest,
    transport_request: &AdapterRequest,
) -> Result<AdapterResponse> {
    let data = &output.data;
    if output.status == ResponseStatus::Pending {
        let mut pending = AdapterResponse::new(
            request,
            ResponseStatus::Pending,
            json!({
                "kind": "transport_pending",
                "request": transport_request,
                "response": output,
            }),
        );
        pending.execution_id.clone_from(&output.execution_id);
        return Ok(pending);
    }
    if data["exit_code"].as_i64() != Some(0) {
        bail!("transport execution failed: {}", data["stderr"]);
    }
    let stdout = data["stdout"]
        .as_str()
        .context("transport returned no stdout")?;
    let mut response: AdapterResponse =
        serde_json::from_str(stdout).context("target adapter returned invalid JSON")?;
    if response.execution_id.is_none() {
        response.execution_id = data["execution_id"].as_str().map(ToOwned::to_owned);
    }
    if response.request_id != request.request_id {
        bail!("target adapter response request ID mismatch");
    }
    Ok(response)
}
