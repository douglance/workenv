//! Orchard create and destroy tests.
use super::handle_with;
use super::test_support::{FakeCluster, pending, request, request_with, running};
use serde_json::json;
use workenv_protocol::ResponseStatus;

#[test]
fn create_reports_changed_once_the_guest_runs() {
    let cluster = FakeCluster::new().guests(vec![None, Some(running("env"))]);
    let response = handle_with(&request("create"), &cluster);
    assert_eq!(response.status, ResponseStatus::Changed);
    assert_eq!(response.data["name"], json!("env"));
    assert_eq!(cluster.created.borrow().len(), 1);
}

#[test]
fn create_on_an_already_running_guest_is_ready_not_changed() {
    // Re-running create must not claim a change it did not make, or every
    // apply reads as a rebuild and nothing downstream can trust `changed`.
    let cluster = FakeCluster::new().guests(vec![Some(running("env"))]);
    let response = handle_with(&request("create"), &cluster);
    assert_eq!(response.status, ResponseStatus::Ready);
    assert!(
        cluster.created.borrow().is_empty(),
        "an existing running guest must not be re-created"
    );
}

#[test]
fn create_names_the_guest_for_the_environment_not_the_worker() {
    let cluster = FakeCluster::new().guests(vec![None, Some(running("env"))]);
    handle_with(&request("create"), &cluster);
    assert_eq!(cluster.created.borrow()[0]["name"], json!("env"));
}

#[test]
fn create_attaches_no_labels_of_its_own() {
    // Labels are scheduling constraints: a labelled guest only lands on a
    // worker carrying the same label. Attaching bookkeeping made guests
    // unschedulable -- pending forever with an empty status message, on a
    // cluster with ample free capacity.
    let cluster = FakeCluster::new().guests(vec![None, Some(running("env"))]);
    let req = request_with("create", json!({"lease_seconds": 3600}), None);
    handle_with(&req, &cluster);
    let body = cluster.created.borrow()[0].clone();
    assert!(
        body.get("labels").is_none(),
        "no labels unless asked for; got {body}"
    );
}

#[test]
fn explicit_labels_are_passed_through_to_pin_placement() {
    let cluster = FakeCluster::new().guests(vec![None, Some(running("env"))]);
    let req = request_with("create", json!({"labels": {"machine": "box-03"}}), None);
    handle_with(&req, &cluster);
    assert_eq!(
        cluster.created.borrow()[0]["labels"]["machine"],
        json!("box-03")
    );
}

#[test]
fn a_guest_that_vanishes_while_starting_fails_rather_than_pending() {
    // Vanishing means something else deleted it. Reporting pending would hide
    // that and leave the caller waiting for a guest nobody is going to start.
    let cluster = FakeCluster::new().guests(vec![Some(pending("env")), None]);
    let response = handle_with(&request("create"), &cluster);
    assert_eq!(response.status, ResponseStatus::Failed);
    assert!(response.error.unwrap_or_default().contains("disappeared"));
}

#[test]
fn destroy_removes_and_reports_changed() {
    let cluster = FakeCluster::new();
    let response = handle_with(&request("destroy"), &cluster);
    assert_eq!(response.status, ResponseStatus::Changed);
    assert_eq!(cluster.removed.borrow()[0], "env");
}

#[test]
fn destroy_of_an_absent_guest_is_ready_not_failed() {
    // Teardown's goal is absence. A retried destroy must converge, not fail
    // forever because the first attempt already succeeded.
    let cluster = FakeCluster::new().absent_on_remove();
    let response = handle_with(&request("destroy"), &cluster);
    assert_eq!(response.status, ResponseStatus::Ready);
    assert_eq!(response.data["removed"], json!(false));
}

#[test]
fn destroy_prefers_the_name_recorded_in_the_create_receipt() {
    let cluster = FakeCluster::new();
    let previous = json!({"create": {"response": {"data": {"name": "recorded-guest"}}}});
    let response = handle_with(
        &request_with("destroy", json!({}), Some(previous)),
        &cluster,
    );
    assert_eq!(response.status, ResponseStatus::Changed);
    assert_eq!(cluster.removed.borrow()[0], "recorded-guest");
}

#[test]
fn destroy_falls_back_to_the_environment_name_when_the_receipt_is_gone() {
    // Rebuilding invalidates the create fingerprint, so the receipt can be
    // missing exactly when teardown matters most. Without this fallback the
    // guest leaks, which is the failure this provider exists to avoid.
    let cluster = FakeCluster::new();
    let response = handle_with(&request_with("destroy", json!({}), None), &cluster);
    assert_eq!(response.status, ResponseStatus::Changed);
    assert_eq!(cluster.removed.borrow()[0], "env");
}

#[test]
fn a_failed_removal_is_reported_not_swallowed() {
    let cluster = FakeCluster::new().failing("remove");
    let response = handle_with(&request("destroy"), &cluster);
    assert_eq!(response.status, ResponseStatus::Failed);
}

#[test]
fn the_guest_platform_is_derived_from_the_declared_system() {
    // Orchard defaults os to darwin. A Linux image with os=darwin is scheduled
    // onto nothing and sits pending with an empty status message, which is the
    // least diagnosable failure this provider can produce.
    let cluster = FakeCluster::new().guests(vec![None, Some(running("env"))]);
    let mut req = request("create");
    req.target.system = "aarch64-linux".into();
    handle_with(&req, &cluster);
    let body = cluster.created.borrow()[0].clone();
    assert_eq!(body["os"], json!("linux"));
    assert_eq!(body["arch"], json!("arm64"));
}

#[test]
fn an_intel_system_maps_to_amd64() {
    let cluster = FakeCluster::new().guests(vec![None, Some(running("env"))]);
    let mut req = request("create");
    req.target.system = "x86_64-linux".into();
    handle_with(&req, &cluster);
    assert_eq!(cluster.created.borrow()[0]["arch"], json!("amd64"));
}

#[test]
fn create_declares_the_worker_slot_it_consumes() {
    // Orchard's API does not default this; only its CLI fills it in. A body
    // written against the API without it produces a guest the scheduler never
    // places, pending forever with an empty status message.
    let cluster = FakeCluster::new().guests(vec![None, Some(running("env"))]);
    handle_with(&request("create"), &cluster);
    assert_eq!(
        cluster.created.borrow()[0]["resources"]["org.cirruslabs.tart-vms"],
        json!(1)
    );
}
