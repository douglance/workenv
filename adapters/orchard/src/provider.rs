//! Orchard cluster provider logic.
mod access;
#[cfg(test)]
#[path = "provider/access_tests.rs"]
mod access_tests;
mod carrier;
mod client;
#[cfg(test)]
// Test-only, and only these: a fixture that cannot unwrap says less than one that
// panics loudly when the fixture is wrong.
#[allow(clippy::expect_used, clippy::unwrap_used)]
#[path = "provider/client_tests.rs"]
mod client_tests;
mod create;
mod credentials;
#[cfg(test)]
// Test-only, and only these: a credential test that cannot unwrap its own
// fixture says less than one that panics loudly when the fixture is wrong.
#[allow(clippy::expect_used, clippy::unwrap_used)]
#[path = "provider/credentials_tests.rs"]
mod credentials_tests;
mod destroy;
mod execute;
#[cfg(test)]
// Test-only, and only these: a transport test that cannot unwrap its own
// fixtures says less than one that panics loudly when a fixture is wrong.
#[allow(clippy::expect_used, clippy::unwrap_used)]
#[path = "provider/execute_budget_tests.rs"]
mod execute_budget_tests;
#[cfg(test)]
// Same reasoning as its sibling above.
#[allow(clippy::expect_used, clippy::unwrap_used)]
#[path = "provider/execute_tests.rs"]
mod execute_tests;
mod inventory;
#[cfg(test)]
#[path = "provider/lifecycle_tests.rs"]
mod lifecycle_tests;
mod network;
mod ready;
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
use workenv_platform::ApocExecutor;
use workenv_protocol::{AdapterRequest, AdapterResponse, ResponseStatus};

use access::guest_name;
use client::{Cluster, HttpCluster};

/// An executor rooted at the controller's own working directory.
fn executor() -> ApocExecutor {
    ApocExecutor::new(std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from(".")))
}

/// Controller base URL used when the binding declares none.
const DEFAULT_CONTROLLER: &str = "http://127.0.0.1:6120";

/// Dispatch one request against the configured controller.
pub(crate) fn handle(request: &AdapterRequest) -> AdapterResponse {
    let base = controller_url(request);
    match HttpCluster::new(base) {
        Ok(cluster) => handle_with(request, &cluster),
        Err(error) => failed(request, &format!("{error:#}")),
    }
}

/// Dispatch with an injected cluster so tests need no controller.
fn handle_with<C: Cluster>(request: &AdapterRequest, cluster: &C) -> AdapterResponse {
    let executor = executor();
    let probe = ready::MarkerProbe::new(&executor);
    dispatch(request, cluster, &ready::WallClock::start(), &|guest| {
        probe.present(guest)
    })
}

/// Dispatch with time and the setup probe injected too, so a test can make a
/// guest take minutes to provision without taking minutes itself.
fn dispatch<C: Cluster>(
    request: &AdapterRequest,
    cluster: &C,
    clock: &dyn ready::Clock,
    provisioned: &dyn Fn(&str) -> bool,
) -> AdapterResponse {
    match request.operation.as_str() {
        "inventory" => report(request, cluster),
        "create" => provision(request, cluster, clock, provisioned),
        "destroy" => destroy::answer(request, cluster),
        "reap" => reap::answer(request, cluster),
        // Reaching a guest needs no cluster read: the controller tunnels by
        // name, so this answers from the request alone.
        "connect" => access::connect(request),
        // The transport hop. A target-located extension reaches its guest
        // through here, which is what lets a host declare no address at all.
        "execute" => execute::run(request, &guest_name(request), &executor()),
        _ => response(
            request,
            ResponseStatus::Unsupported,
            json!({}),
            Some("unsupported Orchard provider operation"),
        ),
    }
}

/// Answer `create` by asking the controller to schedule a guest.
fn provision<C: Cluster>(
    request: &AdapterRequest,
    cluster: &C,
    clock: &dyn ready::Clock,
    provisioned: &dyn Fn(&str) -> bool,
) -> AdapterResponse {
    let spec = match create::spec(
        &request.target.environment,
        &request.target.system,
        &request.config,
        &request.input,
    ) {
        Ok(spec) => spec,
        Err(error) => return failed(request, &error),
    };
    match create::run(cluster, &spec, clock, provisioned) {
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

/// Shape one guest for a receipt, always carrying what teardown needs.
///
/// `owned` and `resource_id` are what `Environment::destroy` actually gates on
/// (`workenv-core/src/environment.rs:114`): a receipt without both makes it bail
/// `environment <name> was adopted or has no verified owned resource`. Neither
/// field was set here, so `environment down` could never tear down an Orchard
/// guest and the integration cleanup behind that gate never ran either.
///
/// `owned` is true for every create outcome, including an already-running guest.
/// Orchard has no adoption flag the way Lima does, and the guest is named after
/// the environment, so a guest under that name is this environment's own -- from
/// an earlier `up`, in the ordinary case. Reporting `owned: false` for the
/// already-running outcome would leave exactly the original bug in place for the
/// second and every later `up`. The cost of this choice is that a foreign guest
/// colliding on the environment's name is treated as this environment's, which
/// is already how `create` treats it before reaching here.
fn guest_data(name: &str, guest: &Value) -> Value {
    let mut data = inventory::guest(guest);
    if let Some(object) = data.as_object_mut() {
        // The controller may answer a create with an empty echo; the name is the
        // one field teardown cannot do without, so it is set from the request.
        object.insert("name".into(), json!(name));
        object.insert("owned".into(), json!(true));
        object.insert("resource_id".into(), json!(name));
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
        // `{error:#}`, not `{error}`: anyhow's Display prints only the outermost
        // layer, so an unreachable controller reported "reading workers: requesting
        // http://127.0.0.1:6120/v1/workers" and dropped the `Connection refused`
        // underneath it -- leaving DNS, TLS, timeout and refusal indistinguishable.
        Err(error) => return failed(request, &format!("reading workers: {error:#}")),
    };
    let guests = match cluster.collection("vms") {
        Ok(guests) => guests,
        Err(error) => return failed(request, &format!("reading vms: {error:#}")),
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
pub(super) fn response(
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
