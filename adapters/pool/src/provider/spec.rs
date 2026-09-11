//! Resolved settings for one placement request.
use anyhow::{Result, bail};
use serde_json::Value;
use workenv_protocol::AdapterRequest;

use super::backend::Site;
use super::rank::Requirement;

/// Default lease, held below the boundary at which a tagged ephemeral node
/// converts to a standard device.
const DEFAULT_LEASE_SECONDS: u64 = 14_400;

/// One request's resolved placement settings.
#[derive(Clone, Debug)]
pub(super) struct Spec {
    /// Environment name; also the tailnet hostname and the resource id.
    pub(super) name: String,
    pub(super) tailnet_suffix: String,
    pub(super) tailnet_user: String,
    pub(super) requirement: Requirement,
    pub(super) lease_seconds: u64,
    pub(super) sites: Vec<Site>,
}

impl Spec {
    /// The address this environment answers at, wherever it lands.
    pub(super) fn address(&self) -> String {
        format!(
            "{}@{}.{}",
            self.tailnet_user, self.name, self.tailnet_suffix
        )
    }

    /// The tailnet DNS name enrollment must produce, exactly.
    pub(super) fn dns_name(&self) -> String {
        format!("{}.{}", self.name, self.tailnet_suffix)
    }
}

/// Resolve a spec from binding config, then operation input, then the target.
pub(super) fn spec(request: &AdapterRequest) -> Result<Spec> {
    let name = request.target.host.clone();
    if name.trim().is_empty() {
        bail!("placement requires a host name to use as the environment identity");
    }
    let Some(tailnet_suffix) = field(request, "tailnet_suffix").map(str::to_owned) else {
        bail!("provider config tailnet_suffix is required");
    };
    let sites = sites(request)?;
    if sites.is_empty() {
        bail!("provider config backends must declare at least one site");
    }
    let system = field(request, "required_system")
        .map_or_else(|| request.target.system.clone(), str::to_owned);
    Ok(Spec {
        requirement: Requirement {
            system,
            cpus: shape(request, "cpus").unwrap_or(2),
            memory_gb: shape(request, "memory_gb").unwrap_or(4),
            disk_gb: shape(request, "disk_gb").unwrap_or(30),
        },
        tailnet_user: field(request, "tailnet_user")
            .unwrap_or("exedev")
            .to_owned(),
        lease_seconds: number(request, "lease_seconds").unwrap_or(DEFAULT_LEASE_SECONDS),
        tailnet_suffix,
        sites,
        name,
    })
}

/// Read the declared sites. A site is a name plus the argv that serves it.
fn sites(request: &AdapterRequest) -> Result<Vec<Site>> {
    let Some(items) = request.config.get("backends").and_then(Value::as_array) else {
        return Ok(Vec::new());
    };
    let mut sites = Vec::new();
    for item in items {
        let Some(name) = item.get("site").and_then(Value::as_str) else {
            bail!("every backend entry must declare a site name");
        };
        let run: Vec<String> = item
            .get("run")
            .and_then(Value::as_array)
            .map(|argv| {
                argv.iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default();
        if run.is_empty() {
            bail!("backend {name} must declare a non-empty run command");
        }
        sites.push(Site {
            name: name.to_owned(),
            run,
        });
    }
    Ok(sites)
}

fn field<'a>(request: &'a AdapterRequest, key: &str) -> Option<&'a str> {
    request
        .config
        .get(key)
        .and_then(Value::as_str)
        .or_else(|| request.input.get(key).and_then(Value::as_str))
}

fn number(request: &AdapterRequest, key: &str) -> Option<u64> {
    request
        .config
        .get(key)
        .and_then(Value::as_u64)
        .or_else(|| request.input.get(key).and_then(Value::as_u64))
        .filter(|value| *value > 0)
}

fn shape(request: &AdapterRequest, key: &str) -> Option<u64> {
    request
        .config
        .get("shape")
        .and_then(|shape| shape.get(key))
        .and_then(Value::as_u64)
        .or_else(|| number(request, key))
}
