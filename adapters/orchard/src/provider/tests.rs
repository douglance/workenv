//! Orchard provider tests.
use super::{Cluster, handle_with};
use anyhow::{Result, anyhow};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::path::PathBuf;
use workenv_protocol::{AdapterRequest, ResponseStatus, Target};

/// A cluster that answers from a fixed table, or fails a named collection.
struct FakeCluster {
    collections: HashMap<String, Vec<Value>>,
    failing: Option<String>,
}

impl FakeCluster {
    fn new() -> Self {
        Self {
            collections: HashMap::new(),
            failing: None,
        }
    }

    fn with(mut self, name: &str, items: Vec<Value>) -> Self {
        self.collections.insert(name.to_owned(), items);
        self
    }

    fn failing(mut self, name: &str) -> Self {
        self.failing = Some(name.to_owned());
        self
    }
}

impl Cluster for FakeCluster {
    fn collection(&self, name: &str) -> Result<Vec<Value>> {
        if self.failing.as_deref() == Some(name) {
            return Err(anyhow!("controller unreachable"));
        }
        Ok(self.collections.get(name).cloned().unwrap_or_default())
    }
}

fn request(operation: &str) -> AdapterRequest {
    AdapterRequest {
        protocol_version: 1,
        request_id: "observation:test".into(),
        extension: "workenv.orchard".into(),
        operation: operation.into(),
        target: Target {
            environment: "env".into(),
            host: "host".into(),
            address: None,
            directory: PathBuf::from("/tmp"),
            system: "aarch64-darwin".into(),
            source: ".".into(),
            profiles: Vec::new(),
        },
        config: json!({}),
        input: json!({}),
        previous: None,
    }
}

fn worker(name: &str, cores: u64, vms: u64) -> Value {
    json!({
        "name": name,
        "last_seen": "2026-09-10T22:39:36-04:00",
        "resources": {"org.cirruslabs.logical-cores": cores, "org.cirruslabs.tart-vms": vms},
    })
}

#[test]
fn inventory_reports_workers_and_guests() {
    let cluster = FakeCluster::new()
        .with("workers", vec![worker("mac-1", 14, 2)])
        .with("vms", vec![json!({"name": "g1", "status": "running"})]);
    let response = handle_with(&request("inventory"), &cluster);
    assert_eq!(response.status, ResponseStatus::Ready);
    assert_eq!(response.data["worker_count"], json!(1));
    assert_eq!(response.data["guest_count"], json!(1));
    assert_eq!(response.data["workers"][0]["name"], json!("mac-1"));
}

#[test]
fn totals_sum_across_workers() {
    let cluster = FakeCluster::new()
        .with(
            "workers",
            vec![worker("mac-1", 14, 2), worker("mac-2", 8, 2)],
        )
        .with("vms", vec![]);
    let response = handle_with(&request("inventory"), &cluster);
    assert_eq!(
        response.data["totals"]["org.cirruslabs.logical-cores"],
        json!(22)
    );
    assert_eq!(response.data["totals"]["org.cirruslabs.tart-vms"], json!(4));
}

#[test]
fn unreadable_resource_values_are_skipped_not_counted_as_zero() {
    // The empty-resources case does not exercise this: the loop body never
    // runs, so a mutant that counts unreadable values as zero survives it.
    // A key that is present but not a number is what actually distinguishes
    // "skipped" from "measured as zero", and zero would understate a total
    // that another worker contributes to.
    // The garbled worker must be the ONLY source for its key. If another worker
    // also reports it, adding zero is a no-op and the mutant computes the same
    // total -- which is how this test passed while asserting nothing.
    let cluster = FakeCluster::new()
        .with(
            "workers",
            vec![
                json!({"name": "healthy", "resources": {"org.cirruslabs.memory-mib": 65536}}),
                json!({"name": "garbled", "resources": {"org.cirruslabs.tart-vms": "two"}}),
            ],
        )
        .with("vms", vec![]);
    let response = handle_with(&request("inventory"), &cluster);
    assert_eq!(
        response.data["totals"]["org.cirruslabs.memory-mib"],
        json!(65536),
        "a readable reading elsewhere must survive"
    );
    assert!(
        response.data["totals"]
            .get("org.cirruslabs.tart-vms")
            .is_none(),
        "an unreadable value must leave the dimension absent, not present as a measured zero; got {}",
        response.data["totals"]
    );
}

#[test]
fn a_worker_reporting_no_resources_contributes_nothing() {
    let cluster = FakeCluster::new()
        .with("workers", vec![json!({"name": "quiet", "resources": {}})])
        .with("vms", vec![]);
    let response = handle_with(&request("inventory"), &cluster);
    assert_eq!(response.data["totals"], json!({}));
}

#[test]
fn a_failed_worker_read_fails_the_report() {
    let cluster = FakeCluster::new().failing("workers").with("vms", vec![]);
    let response = handle_with(&request("inventory"), &cluster);
    assert_eq!(response.status, ResponseStatus::Failed);
    assert!(
        response
            .error
            .unwrap_or_default()
            .contains("reading workers")
    );
}

#[test]
fn a_failed_guest_read_fails_rather_than_reporting_an_empty_cluster() {
    // This is the dangerous one: workers readable, guests not. Reporting the
    // workers alone shows a cluster that looks idle because its guests are
    // invisible, which a caller cannot distinguish from one that is idle.
    let cluster = FakeCluster::new()
        .with("workers", vec![worker("mac-1", 14, 2)])
        .failing("vms");
    let response = handle_with(&request("inventory"), &cluster);
    assert_eq!(response.status, ResponseStatus::Failed);
    assert!(response.error.unwrap_or_default().contains("reading vms"));
    assert_eq!(response.data, json!({}));
}

#[test]
fn pending_guests_are_counted_separately() {
    let cluster = FakeCluster::new().with("workers", vec![]).with(
        "vms",
        vec![
            json!({"name": "a", "status": "running"}),
            json!({"name": "b", "status": "pending"}),
            json!({"name": "c", "status": "pending"}),
        ],
    );
    let response = handle_with(&request("inventory"), &cluster);
    assert_eq!(response.data["pending_count"], json!(2));
    assert_eq!(response.data["guest_count"], json!(3));
}

#[test]
fn an_unknown_operation_is_unsupported_not_failed() {
    let cluster = FakeCluster::new();
    let response = handle_with(&request("create"), &cluster);
    assert_eq!(response.status, ResponseStatus::Unsupported);
}
