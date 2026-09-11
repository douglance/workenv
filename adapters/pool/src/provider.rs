//! Capacity-ranked placement across declared sites.
mod backend;
mod create;
mod destroy;
mod model;
mod rank;
mod spec;

use anyhow::Result;
use serde_json::{Value, json};
use workenv_protocol::{AdapterRequest, AdapterResponse, ResponseStatus};

use backend::{BackendResult, ProcessRunner, Site, SiteRunner};
use model::response;
use spec::{Spec, spec};

/// Milliseconds allowed for a single backend command.
const SITE_TIMEOUT_MS: u64 = 180_000;

/// Dispatch one request against the declared sites.
pub(crate) fn handle(request: &AdapterRequest) -> Result<AdapterResponse> {
    // A malformed binding is propagated rather than dressed as a response:
    // `serve` already renders a handler error as a failed response with the
    // message attached, and one rendering of that is enough.
    let resolved = spec(request)?;
    let runner = ProcessRunner::new(request.request_id.clone(), SITE_TIMEOUT_MS);
    let mut provider = Provider::new(runner);
    Ok(handle_with(request, &resolved, &mut provider))
}

/// Dispatch with an injected runner so tests need no backend.
fn handle_with<R: SiteRunner>(
    request: &AdapterRequest,
    resolved: &Spec,
    provider: &mut Provider<R>,
) -> AdapterResponse {
    match request.operation.as_str() {
        "inventory" => provider.inventory_response(request, resolved),
        "create" => create::run(provider, request, resolved),
        "destroy" => destroy::run(provider, request, resolved),
        _ => response(
            request,
            ResponseStatus::Unsupported,
            json!({}),
            Some("unsupported placement operation"),
        ),
    }
}

/// Issues backend verbs and interprets their JSON.
struct Provider<R> {
    runner: R,
}

impl<R: SiteRunner> Provider<R> {
    fn new(runner: R) -> Self {
        Self { runner }
    }

    /// What a site can currently offer.
    fn capacity(&mut self, site: &Site) -> BackendResult<Value> {
        self.runner.run(site, &["capacity".into(), "--json".into()])
    }

    /// Take a unit on this site. No slot is pinned: the site chooses.
    fn place(&mut self, site: &Site, resolved: &Spec, request_id: &str) -> BackendResult<Value> {
        self.runner.run(
            site,
            &[
                "place".into(),
                "--request-id".into(),
                request_id.to_owned(),
                "--system".into(),
                resolved.requirement.system.clone(),
                "--cpus".into(),
                resolved.requirement.cpus.to_string(),
                "--memory-gb".into(),
                resolved.requirement.memory_gb.to_string(),
                "--disk-gb".into(),
                resolved.requirement.disk_gb.to_string(),
                "--hostname".into(),
                resolved.name.clone(),
                "--lease-seconds".into(),
                resolved.lease_seconds.to_string(),
                "--json".into(),
            ],
        )
    }

    /// Join the placed guest to the tailnet under the environment name.
    ///
    /// The site performs this, not the pool: a guest's native address routes
    /// only from its own host, and the pool deliberately knows nothing about
    /// how any site reaches what it runs.
    fn enroll(&mut self, site: &Site, resolved: &Spec, placement: &Value) -> BackendResult<Value> {
        let native = placement
            .get("vm_name")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        self.runner.run(
            site,
            &[
                "enroll".into(),
                "--native-id".into(),
                native,
                "--hostname".into(),
                resolved.name.clone(),
                "--tailnet-suffix".into(),
                resolved.tailnet_suffix.clone(),
                "--json".into(),
            ],
        )
    }

    /// Give the unit back, refused by the site when the identity has drifted.
    fn release(&mut self, site: &Site, identity: &Value) -> BackendResult<Value> {
        let native = model::identity_field(identity, "native_id").unwrap_or_default();
        let claim = model::identity_field(identity, "claim_uuid").unwrap_or_default();
        self.runner.run(
            site,
            &[
                "release".into(),
                "--native-id".into(),
                native.to_owned(),
                "--claim-uuid".into(),
                claim.to_owned(),
                "--json".into(),
            ],
        )
    }

    /// Observe one site, reporting the failure rather than propagating it.
    fn observe(&mut self, site: &Site) -> Value {
        let probed = self
            .capacity(site)
            .and_then(|capacity| Ok((capacity, self.inventory(site)?)));
        match probed {
            Ok((capacity, listed)) => json!({
                "site": site.name, "ok": true,
                "capacity": capacity, "vms": listed.get("vms").cloned()
            }),
            Err(error) => json!({"site": site.name, "ok": false, "error": error}),
        }
    }

    fn inventory_response(&mut self, request: &AdapterRequest, resolved: &Spec) -> AdapterResponse {
        let sites: Vec<Value> = resolved
            .sites
            .iter()
            .map(|site| self.observe(site))
            .collect();
        let degraded = sites.iter().any(|entry| entry["ok"] != json!(true));
        // One unreachable site degrades the report but does not fail it: an
        // observation that refuses to say anything is worth less than a partial one.
        AdapterResponse::new(
            request,
            ResponseStatus::Ready,
            json!({"degraded": degraded, "sites": sites, "environment": resolved.name}),
        )
    }

    fn inventory(&mut self, site: &Site) -> BackendResult<Value> {
        self.runner
            .run(site, &["inventory".into(), "--json".into()])
    }
}

#[cfg(test)]
mod teardown_tests;
#[cfg(test)]
mod tests;
