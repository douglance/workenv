//! Reclaiming guests whose lease has run out.
//!
//! Orchard has no TTL of its own, so expiry is enforced here. Crucially this
//! reads live cluster state and never a receipt: the create fingerprint covers
//! the whole environment and host, so rebuilding invalidates it, and a sweeper
//! that needed a receipt would stop working exactly when guests start leaking.
use chrono::{DateTime, Utc};
use serde_json::{Value, json};
use workenv_protocol::{AdapterRequest, AdapterResponse, ResponseStatus};

use super::client::{Cluster, Removal};
use super::{failed, response};

/// One guest considered for reaping.
struct Candidate {
    name: String,
    age_seconds: i64,
}

/// What a sweep did.
pub(super) struct Swept {
    pub(super) reaped: Vec<String>,
    pub(super) kept: Vec<String>,
    pub(super) skipped: Vec<Value>,
}

/// Remove guests older than the lease whose name carries the prefix.
///
/// A guest whose age cannot be read is skipped and reported, never reaped. An
/// unreadable timestamp must not be treated as infinitely old, or one malformed
/// record takes the fleet with it.
pub(super) fn run<C: Cluster>(
    cluster: &C,
    prefix: &str,
    lease_seconds: i64,
    now: DateTime<Utc>,
    dry_run: bool,
) -> Result<Swept, String> {
    let guests = cluster
        .collection("vms")
        .map_err(|error| format!("{error:#}"))?;
    let mut swept = Swept {
        reaped: Vec::new(),
        kept: Vec::new(),
        skipped: Vec::new(),
    };
    for guest in &guests {
        let Some(name) = guest.get("name").and_then(Value::as_str) else {
            swept
                .skipped
                .push(json!({"name": null, "reason": "guest has no name"}));
            continue;
        };
        if !name.starts_with(prefix) {
            continue;
        }
        let Some(candidate) = candidate(name, guest, now) else {
            swept
                .skipped
                .push(json!({"name": name, "reason": "createdAt missing or unparseable"}));
            continue;
        };
        if candidate.age_seconds < lease_seconds {
            swept.kept.push(candidate.name);
            continue;
        }
        if dry_run {
            swept.reaped.push(candidate.name);
            continue;
        }
        match cluster.remove(&candidate.name) {
            Ok(Removal::Removed | Removal::Absent) => swept.reaped.push(candidate.name),
            Err(error) => swept
                .skipped
                .push(json!({"name": candidate.name, "reason": format!("{error:#}")})),
        }
    }
    Ok(swept)
}

/// Read a guest's age, or nothing when its timestamp cannot be trusted.
fn candidate(name: &str, guest: &Value, now: DateTime<Utc>) -> Option<Candidate> {
    let created = guest.get("createdAt").and_then(Value::as_str)?;
    let created = DateTime::parse_from_rfc3339(created)
        .ok()?
        .with_timezone(&Utc);
    Some(Candidate {
        name: name.to_owned(),
        age_seconds: (now - created).num_seconds(),
    })
}

/// Answer `reap` by removing guests whose lease has run out.
pub(super) fn answer<C: Cluster>(request: &AdapterRequest, cluster: &C) -> AdapterResponse {
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
    match run(cluster, &prefix, lease, chrono::Utc::now(), dry_run) {
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
