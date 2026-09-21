//! Asking the controller for one guest.
use serde_json::{Map, Value, json};

use super::client::Cluster;
use super::network;
use super::ready::{self, Clock};

/// The slot every Tart guest occupies on a worker.
const TART_VM_RESOURCE: &str = "org.cirruslabs.tart-vms";
/// Seconds to wait for the scheduler to place and boot a guest.
const READY_TIMEOUT_SECS: u64 = 180;
/// Seconds between readiness polls.
const POLL_SECS: u64 = 2;

/// One resolved create request.
pub(super) struct Spec {
    pub(super) name: String,
    pub(super) body: Value,
}

/// Build the create body from the binding configuration.
///
/// The guest is named for the environment, not for where it lands. Placement is
/// the scheduler's business; an identity that changed with the worker would make
/// the receipt unable to find its own guest after a reschedule.
///
/// # Errors
/// Refuses a malformed network fence rather than creating the guest without it.
pub(super) fn spec(
    environment: &str,
    system: &str,
    config: &Value,
    input: &Value,
) -> Result<Spec, String> {
    let read = |field: &str| setting(config, input, field);
    let mut body = Map::new();
    body.insert("name".into(), json!(environment));
    // Orchard defaults `os` to darwin. Leaving it unset against a Linux image
    // produces a guest no worker can ever satisfy, and it sits pending forever
    // with an empty status message -- so this is derived, not left to default.
    let (arch, os) = platform(system);
    body.insert("os".into(), read("os").unwrap_or_else(|| json!(os)));
    body.insert("arch".into(), read("arch").unwrap_or_else(|| json!(arch)));
    body.insert(
        "image".into(),
        read("image").unwrap_or_else(|| json!("ghcr.io/cirruslabs/ubuntu:latest")),
    );
    body.insert("cpu".into(), read("cpu").unwrap_or_else(|| json!(2)));
    body.insert(
        "memory".into(),
        read("memory").unwrap_or_else(|| json!(2048)),
    );
    // The API does not default this and the CLI fills it in, so a body written
    // straight against the API omits it -- and the scheduler then never places
    // the guest. It stays pending forever with an empty status message, which
    // is indistinguishable from a cluster with no spare capacity.
    body.insert(
        "resources".into(),
        read("resources").unwrap_or_else(|| json!({ TART_VM_RESOURCE: 1 })),
    );
    if let Some(disk) = read("disk_size") {
        body.insert("disk_size".into(), disk);
    }
    if let Some(script) = read("startup_script") {
        body.insert("startup_script".into(), script);
    }
    // Labels are SCHEDULING CONSTRAINTS in Orchard, not free-form metadata: a
    // labelled guest only lands on a worker carrying the same label. Attaching
    // bookkeeping here made guests unschedulable, pending forever with an empty
    // status message. So nothing is added automatically; labels are passed
    // through only when asked for, which is how a guest gets pinned to one
    // machine deliberately.
    if let Some(labels) = read("labels") {
        body.insert("labels".into(), labels);
    }
    if let Some(fence) = read("network") {
        body.extend(network::softnet_fields(&fence)?);
    }
    // The lease needs no label. The guest is named for its environment and the
    // controller records created_at, so `reap` has everything it needs from
    // live cluster state plus the lease declared on the binding.
    Ok(Spec {
        name: environment.to_owned(),
        body: Value::Object(body),
    })
}

/// Split a Nix system string into the arch and os names Orchard expects.
///
/// The manifest already declares each host's system and core validates it, so
/// deriving from it keeps one source of truth rather than adding a second
/// place where a guest's platform can be stated and disagree.
fn platform(system: &str) -> (&'static str, &'static str) {
    let (cpu, kernel) = system.split_once('-').unwrap_or(("aarch64", "linux"));
    let arch = match cpu {
        "x86_64" => "amd64",
        _ => "arm64",
    };
    let os = match kernel {
        "darwin" => "darwin",
        _ => "linux",
    };
    (arch, os)
}

/// Read a setting from the operation input, falling back to the binding config.
///
/// Input wins so a caller can override a declared default per call, which is how
/// one manifest binding serves environments of different shapes.
fn setting(config: &Value, input: &Value, field: &str) -> Option<Value> {
    input
        .get(field)
        .or_else(|| config.get(field))
        .filter(|value| !value.is_null())
        .cloned()
}

/// Outcome of asking for a guest.
pub(super) enum Created {
    /// The guest was already present and running.
    Existing(Value),
    /// The guest was created and reached `running`.
    Running(Value),
    /// Created, but not running before the deadline.
    Pending(Value, String),
}

/// Create the guest if absent, wait for it to run, then for its setup to finish.
pub(super) fn run<C: Cluster>(
    cluster: &C,
    spec: &Spec,
    clock: &dyn Clock,
    provisioned: &dyn Fn(&str) -> bool,
) -> Result<Created, String> {
    let started = match cluster.guest(&spec.name).map_err(|e| e.to_string())? {
        Some(existing) if status_of(&existing) == "running" => Created::Existing(existing),
        Some(_) => await_running(cluster, &spec.name, clock)?,
        None => {
            cluster.create(&spec.body).map_err(|e| e.to_string())?;
            await_running(cluster, &spec.name, clock)?
        }
    };
    Ok(ready::await_provisioned(started, spec, clock, provisioned))
}

/// Poll until the guest runs, or report why it did not.
fn await_running<C: Cluster>(
    cluster: &C,
    name: &str,
    clock: &dyn Clock,
) -> Result<Created, String> {
    let mut last = json!({});
    while clock.elapsed() < READY_TIMEOUT_SECS {
        // A guest that vanishes mid-wait was deleted by something else.
        // Reporting "still pending" would hide that entirely.
        let guest = cluster
            .guest(name)
            .map_err(|error| format!("{error:#}"))?
            .ok_or_else(|| format!("guest {name} disappeared while starting"))?;
        match status_of(&guest).as_str() {
            "running" => return Ok(Created::Running(guest)),
            "failed" => return Err(format!("guest {name} failed: {}", message_of(&guest))),
            _ => last = guest,
        }
        clock.sleep(POLL_SECS);
    }
    let reason = format!(
        "guest {name} was {} after {READY_TIMEOUT_SECS}s: {}",
        status_of(&last),
        message_of(&last)
    );
    Ok(Created::Pending(last, reason))
}

/// Read a guest's status, defaulting to a name that is never a real state.
fn status_of(guest: &Value) -> String {
    guest
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("unknown")
        .to_owned()
}

/// Read a guest's status message.
fn message_of(guest: &Value) -> String {
    guest
        .get("status_message")
        .and_then(Value::as_str)
        .unwrap_or("no status message")
        .to_owned()
}
