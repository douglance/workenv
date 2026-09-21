//! The network fence a guest is created behind.
//!
//! Enforced by Softnet on the worker Mac, outside the guest. That is the point:
//! a runner's agent has passwordless sudo, so any rule set up inside the guest
//! is a rule the agent can delete. Softnet's own default, once isolation is on,
//! lets a guest reach globally routable addresses and the vmnet gateway -- the
//! hosting Mac's bridge address, which is how DNS works -- and blocks the rest:
//! the LAN, other guests, and non-routable ranges such as a tailnet's. `allow`
//! and `block` adjust that by CIDR; the longest prefix wins and a prefix both
//! allowed and blocked is blocked. Hostnames cannot be expressed, because the
//! filter sees addresses.
use std::net::Ipv4Addr;

use serde_json::{Map, Value, json};

/// The keys a fence may carry. Anything else is refused, not ignored: a
/// misspelled `isolate` silently ignored would leave a runner unfenced while
/// its manifest reads as fenced.
const KEYS: [&str; 3] = ["isolated", "allow", "block"];

/// Translate a declared fence into the VM-spec fields Orchard reads.
///
/// # Errors
/// Returns a description of the first malformed part. A fence that cannot be
/// read is a create that must not happen, because the alternative is a guest
/// created without the fence it was declared with.
pub(super) fn softnet_fields(network: &Value) -> Result<Map<String, Value>, String> {
    let object = network
        .as_object()
        .ok_or_else(|| format!("network must be an object, got {network}"))?;
    if let Some(unknown) = object.keys().find(|key| !KEYS.contains(&key.as_str())) {
        return Err(format!(
            "network has unknown key {unknown:?}; expected one of {KEYS:?}"
        ));
    }
    let mut fields = Map::new();
    match object.get("isolated") {
        None | Some(Value::Bool(false)) => {}
        Some(Value::Bool(true)) => {
            fields.insert("netSoftnet".into(), json!(true));
        }
        Some(other) => return Err(format!("network.isolated must be a boolean, got {other}")),
    }
    for (key, field) in [("allow", "netSoftnetAllow"), ("block", "netSoftnetBlock")] {
        let listed = object.get(key).map(|list| cidrs(key, list)).transpose()?;
        if let Some(listed) = listed.filter(|listed| !listed.is_empty()) {
            fields.insert(field.into(), json!(listed));
        }
    }
    Ok(fields)
}

/// The fence a guest record reports, in the same shape it was declared in.
///
/// Read back from the controller rather than echoed from the request, so a
/// receipt states what the cluster holds, not what was asked for.
pub(super) fn observed(guest: &Value) -> Value {
    let list = |field: &str| guest.get(field).cloned().unwrap_or_else(|| json!([]));
    let allow = list("netSoftnetAllow");
    let block = list("netSoftnetBlock");
    let listed = |value: &Value| value.as_array().is_some_and(|items| !items.is_empty());
    // Orchard treats a non-empty allow or block list as isolation on its own
    // (`SoftnetEnabled` in its VM spec), so this must too.
    let isolated = guest.get("netSoftnet").and_then(Value::as_bool) == Some(true)
        || guest.get("net-softnet").and_then(Value::as_bool) == Some(true)
        || listed(&allow)
        || listed(&block);
    json!({ "isolated": isolated, "allow": allow, "block": block })
}

fn cidrs(key: &str, list: &Value) -> Result<Vec<String>, String> {
    let items = list
        .as_array()
        .ok_or_else(|| format!("network.{key} must be a list of CIDRs, got {list}"))?;
    items
        .iter()
        .map(|item| {
            item.as_str()
                .filter(|text| is_ipv4_cidr(text))
                .map(ToOwned::to_owned)
                .ok_or_else(|| format!("network.{key} entry {item} is not an IPv4 CIDR"))
        })
        .collect()
}

/// `a.b.c.d/n` with a real address and a prefix of at most 32. Softnet filters
/// IPv4 only, so an IPv6 range would be a rule that is never applied.
fn is_ipv4_cidr(text: &str) -> bool {
    let Some((address, prefix)) = text.split_once('/') else {
        return false;
    };
    address.parse::<Ipv4Addr>().is_ok() && prefix.parse::<u8>().is_ok_and(|bits| bits <= 32)
}

#[cfg(test)]
#[path = "network_tests.rs"]
mod tests;
