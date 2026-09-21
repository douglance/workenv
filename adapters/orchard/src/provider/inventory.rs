//! Reporting what the cluster currently holds.
use serde_json::{Map, Value, json};

/// Reshape one worker record into the inventory contract.
///
/// `last_seen` is carried through verbatim and deliberately not interpreted:
/// the controller measures it, and a second opinion computed here would be a
/// second source of truth that can disagree with the scheduler's own view.
pub(super) fn worker(record: &Value) -> Value {
    json!({
        "name": text(record, "name"),
        "last_seen": text(record, "last_seen"),
        "scheduling_paused": record.get("scheduling_paused")
            .and_then(Value::as_bool).unwrap_or(false),
        "resources": record.get("resources").cloned().unwrap_or_else(|| json!({})),
        "labels": record.get("labels").cloned().unwrap_or_else(|| json!({})),
    })
}

/// Reshape one VM record into the inventory contract.
pub(super) fn guest(record: &Value) -> Value {
    json!({
        "name": text(record, "name"),
        "status": text(record, "status"),
        "status_message": text(record, "status_message"),
        "worker": text(record, "worker"),
        "image": text(record, "image"),
        // The controller spells this one camelCase while `scheduled_at` and
        // `started_at` beside it are snake_case. Reading `created_at` silently
        // returns empty, which is indistinguishable from a guest with no age.
        "created_at": text(record, "createdAt"),
        "resources": record.get("resources").cloned().unwrap_or_else(|| json!({})),
        // Read from the controller's record, so a receipt or an inventory says
        // what fence the guest actually runs behind rather than what was asked.
        "network": super::network::observed(record),
    })
}

/// Total each declared resource across every worker.
///
/// Only resources a worker actually reports are counted. A worker silent about
/// a dimension contributes nothing to it rather than zero, so a missing reading
/// cannot be mistaken for measured emptiness.
pub(super) fn totals(workers: &[Value]) -> Value {
    let mut sums: Map<String, Value> = Map::new();
    for (key, amount) in workers.iter().flat_map(readings) {
        let running = sums.get(&key).and_then(Value::as_u64).unwrap_or(0);
        sums.insert(key, json!(running + amount));
    }
    Value::Object(sums)
}

/// Numeric resource readings from one worker, skipping any that will not parse.
fn readings(worker: &Value) -> Vec<(String, u64)> {
    let Some(resources) = worker.get("resources").and_then(Value::as_object) else {
        return Vec::new();
    };
    resources
        .iter()
        .filter_map(|(key, value)| value.as_u64().map(|amount| (key.clone(), amount)))
        .collect()
}

/// Count guests the scheduler has not placed.
///
/// A pending guest is the scheduler declining to overcommit, which is a normal
/// and correct state. It is surfaced separately so a caller can tell "the
/// cluster is full" from "the cluster is broken".
pub(super) fn pending(guests: &[Value]) -> usize {
    guests
        .iter()
        .filter(|guest| guest.get("status").and_then(Value::as_str) == Some("pending"))
        .count()
}

/// Read a string field, defaulting to empty rather than failing the report.
fn text(record: &Value, field: &str) -> String {
    record
        .get(field)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()
}
