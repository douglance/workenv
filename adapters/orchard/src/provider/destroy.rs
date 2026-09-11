//! Removing a guest, including when the bookkeeping has drifted.
use serde_json::Value;

use super::client::{Cluster, Removal};

/// Resolve which guest a teardown refers to.
///
/// The controller passes the create receipt as `previous`, but a receipt can be
/// missing: rebuilding invalidates the create fingerprint, and the environment
/// name is then the only thing left that still identifies the guest. Falling
/// back to it is what keeps teardown possible after a rebuild instead of leaking
/// the guest, which is the failure this provider exists to avoid.
pub(super) fn target(environment: &str, previous: Option<&Value>, input: &Value) -> String {
    recorded_name(previous)
        .or_else(|| {
            input
                .get("name")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned)
        })
        .unwrap_or_else(|| environment.to_owned())
}

/// Dig the guest name out of a recorded create receipt.
///
/// The receipt shape varies with how deeply the controller wrapped the response,
/// so each known nesting is tried in turn rather than assuming one.
fn recorded_name(previous: Option<&Value>) -> Option<String> {
    const PATHS: [&[&str]; 4] = [
        &["create", "response", "data", "name"],
        &["response", "data", "name"],
        &["data", "name"],
        &["name"],
    ];
    let previous = previous?;
    PATHS.iter().find_map(|path| follow(previous, path))
}

/// Walk one key path, returning the non-empty string it names.
fn follow(value: &Value, path: &[&str]) -> Option<String> {
    let mut cursor = value;
    for key in path {
        cursor = cursor.get(key)?;
    }
    cursor
        .as_str()
        .filter(|name| !name.is_empty())
        .map(ToOwned::to_owned)
}

/// What teardown did.
pub(super) enum Destroyed {
    /// The guest existed and was removed.
    Removed,
    /// The guest was already gone.
    AlreadyGone,
}

/// Remove the named guest.
pub(super) fn run<C: Cluster>(cluster: &C, name: &str) -> Result<Destroyed, String> {
    match cluster.remove(name).map_err(|error| error.to_string())? {
        Removal::Removed => Ok(Destroyed::Removed),
        Removal::Absent => Ok(Destroyed::AlreadyGone),
    }
}
