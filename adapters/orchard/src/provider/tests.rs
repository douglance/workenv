//! Orchard inventory tests.
use super::handle_with;
use super::test_support::{FakeCluster, request, worker};
use serde_json::json;
use workenv_protocol::ResponseStatus;

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
    // Not `create` or `destroy`: both are supported now, and this test went red
    // when they landed, which is the behaviour wanted from it.
    let cluster = FakeCluster::new();
    let response = handle_with(&request("teleport"), &cluster);
    assert_eq!(response.status, ResponseStatus::Unsupported);
}
