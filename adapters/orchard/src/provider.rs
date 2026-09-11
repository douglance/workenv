//! Orchard cluster provider logic.
mod client;
mod create;
mod destroy;
mod inventory;
#[cfg(test)]
#[path = "provider/lifecycle_tests.rs"]
mod lifecycle_tests;
mod reap;
#[cfg(test)]
#[path = "provider/reap_tests.rs"]
mod reap_tests;
#[cfg(test)]
#[path = "provider/test_support.rs"]
mod test_support;
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
        "create" => provision(request, cluster, &sleep_seconds),
        "destroy" => teardown(request, cluster),
        "reap" => sweep(request, cluster),
        _ => response(
            request,
            ResponseStatus::Unsupported,
            json!({}),
            Some("unsupported Orchard provider operation"),
        ),
    }
}

/// Wait between readiness polls. Injected so tests do not sleep.
fn sleep_seconds(seconds: u64) {
    std::thread::sleep(std::time::Duration::from_secs(seconds));
}

/// Answer `create` by asking the controller to schedule a guest.
fn provision<C: Cluster>(
    request: &AdapterRequest,
    cluster: &C,
    sleep: &dyn Fn(u64),
) -> AdapterResponse {
    let spec = create::spec(
        &request.target.environment,
        &request.target.system,
        &request.config,
        &request.input,
    );
    match create::run(cluster, &spec, sleep) {
        // Already running is Ready, not Changed: re-running create must not
        // report a change it did not make, or every apply looks like a rebuild.
        Ok(create::Created::Existing(guest)) => response(
            request,
            ResponseStatus::Ready,
            guest_data(&spec.name, &guest),
            None,
        ),
        Ok(create::Created::Running(guest)) => response(
            request,
            ResponseStatus::Changed,
            guest_data(&spec.name, &guest),
            None,
        ),
        // Pending keeps the name in `data` so a later teardown can still find
        // the guest that a timed-out create may well have left running.
        Ok(create::Created::Pending(guest, reason)) => response(
            request,
            ResponseStatus::Pending,
            guest_data(&spec.name, &guest),
            Some(&reason),
        ),
        Err(error) => failed(request, &error),
    }
}

/// Answer `destroy` by removing the guest this environment created.
fn teardown<C: Cluster>(request: &AdapterRequest, cluster: &C) -> AdapterResponse {
    let name = destroy::target(
        &request.target.environment,
        request.previous.as_ref(),
        &request.input,
    );
    match destroy::run(cluster, &name) {
        Ok(destroy::Destroyed::Removed) => response(
            request,
            ResponseStatus::Changed,
            json!({"name": name, "removed": true}),
            None,
        ),
        Ok(destroy::Destroyed::AlreadyGone) => response(
            request,
            ResponseStatus::Ready,
            json!({"name": name, "removed": false, "reason": "already absent"}),
            None,
        ),
        Err(error) => failed(request, &error),
    }
}

/// Answer `reap` by removing guests whose lease has run out.
fn sweep<C: Cluster>(request: &AdapterRequest, cluster: &C) -> AdapterResponse {
    let read = |field: &str| {
        request
            .input
            .get(field)
            .or_else(|| request.config.get(field))
            .cloned()
    };
    let Some(lease) = read("lease_seconds").and_then(|value| value.as_i64()) else {
        return failed(request, "reap needs lease_seconds");
    };
    // A prefix is required, not defaulted to empty. Defaulting would make an
    // omitted setting mean "every guest in the cluster is mine to delete".
    let Some(prefix) = read("name_prefix").and_then(|value| value.as_str().map(ToOwned::to_owned))
    else {
        return failed(request, "reap needs name_prefix naming the guests it owns");
    };
    let dry_run = read("dry_run")
        .and_then(|value| value.as_bool())
        .unwrap_or(false);
    match reap::run(cluster, &prefix, lease, chrono::Utc::now(), dry_run) {
        Ok(swept) => {
            let data = json!({
                "reaped": swept.reaped,
                "kept": swept.kept,
                "skipped": swept.skipped,
                "dry_run": dry_run,
            });
            // Reaping nothing is a correct outcome, not a change.
            let status = if swept.reaped.is_empty() || dry_run {
                ResponseStatus::Ready
            } else {
                ResponseStatus::Changed
            };
            response(request, status, data, None)
        }
        Err(error) => failed(request, &error),
    }
}

/// Shape one guest for a receipt, always carrying the name teardown needs.
fn guest_data(name: &str, guest: &Value) -> Value {
    let mut data = inventory::guest(guest);
    if let Some(object) = data.as_object_mut() {
        // The controller may answer a create with an empty echo; the name is the
        // one field teardown cannot do without, so it is set from the request.
        object.insert("name".into(), json!(name));
    }
    data
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
