//! Orchard cluster provider logic.
mod client;
mod inventory;
#[cfg(test)]
#[path = "provider/tests.rs"]
mod tests;

use serde_json::{Value, json};
use workenv_protocol::{AdapterRequest, AdapterResponse, ResponseStatus};

use client::{Cluster, HttpCluster};

/// Controller base URL used when the binding declares none.
const DEFAULT_CONTROLLER: &str = "http://127.0.0.1:6120";

/// Dispatch one request against the configured controller.
pub(crate) fn handle(request: &AdapterRequest) -> AdapterResponse {
    let base = controller_url(request);
    match HttpCluster::new(base) {
        Ok(cluster) => handle_with(request, &cluster),
        Err(error) => failed(request, &error.to_string()),
    }
}

/// Dispatch with an injected cluster so tests need no controller.
fn handle_with<C: Cluster>(request: &AdapterRequest, cluster: &C) -> AdapterResponse {
    match request.operation.as_str() {
        "inventory" => report(request, cluster),
        _ => response(
            request,
            ResponseStatus::Unsupported,
            json!({}),
            Some("unsupported Orchard provider operation"),
        ),
    }
}

/// Answer `inventory` from live controller state.
///
/// Both collections must be readable. Reporting workers while the VM read
/// failed would present a cluster that looks idle because the guests are
/// invisible, and a caller cannot tell that from a cluster that is idle.
fn report<C: Cluster>(request: &AdapterRequest, cluster: &C) -> AdapterResponse {
    let workers = match cluster.collection("workers") {
        Ok(workers) => workers,
        Err(error) => return failed(request, &format!("reading workers: {error}")),
    };
    let guests = match cluster.collection("vms") {
        Ok(guests) => guests,
        Err(error) => return failed(request, &format!("reading vms: {error}")),
    };
    let data = json!({
        "workers": workers.iter().map(inventory::worker).collect::<Vec<_>>(),
        "guests": guests.iter().map(inventory::guest).collect::<Vec<_>>(),
        "totals": inventory::totals(&workers),
        "worker_count": workers.len(),
        "guest_count": guests.len(),
        "pending_count": inventory::pending(&guests),
    });
    response(request, ResponseStatus::Ready, data, None)
}

/// Shape a failure that names what could not be read.
fn failed(request: &AdapterRequest, message: &str) -> AdapterResponse {
    response(request, ResponseStatus::Failed, json!({}), Some(message))
}

/// Resolve the controller URL from the binding configuration.
fn controller_url(request: &AdapterRequest) -> String {
    request
        .config
        .get("controller_url")
        .and_then(Value::as_str)
        .filter(|url| !url.is_empty())
        .unwrap_or(DEFAULT_CONTROLLER)
        .to_owned()
}

/// Build one protocol response.
fn response(
    request: &AdapterRequest,
    status: ResponseStatus,
    data: Value,
    error: Option<&str>,
) -> AdapterResponse {
    AdapterResponse {
        protocol_version: request.protocol_version,
        request_id: request.request_id.clone(),
        status,
        data,
        error: error.map(ToOwned::to_owned),
        execution_id: None,
    }
}
