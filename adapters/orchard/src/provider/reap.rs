//! Reclaiming guests whose lease has run out.
//!
//! Orchard has no TTL of its own, so expiry is enforced here. Crucially this
//! reads live cluster state and never a receipt: the create fingerprint covers
//! the whole environment and host, so rebuilding invalidates it, and a sweeper
//! that needed a receipt would stop working exactly when guests start leaking.
use chrono::{DateTime, Utc};
use serde_json::{Value, json};

use super::client::{Cluster, Removal};

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
        .map_err(|error| error.to_string())?;
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
                .push(json!({"name": candidate.name, "reason": error.to_string()})),
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
