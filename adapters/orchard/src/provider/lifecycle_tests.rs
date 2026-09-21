//! Orchard create and destroy tests.
use super::handle_with;
use super::test_support::{
    FakeClock, FakeCluster, handle_now, pending, request, request_with, running,
};
use serde_json::json;
use workenv_protocol::ResponseStatus;

#[test]
fn create_reports_changed_once_the_guest_runs() {
    let cluster = FakeCluster::new().guests(vec![None, Some(running("env"))]);
    let response = handle_now(&request("create"), &cluster);
    assert_eq!(response.status, ResponseStatus::Changed);
    assert_eq!(response.data["name"], json!("env"));
    assert_eq!(cluster.created.borrow().len(), 1);
}

#[test]
fn create_on_an_already_running_guest_is_ready_not_changed() {
    // Re-running create must not claim a change it did not make, or every
    // apply reads as a rebuild and nothing downstream can trust `changed`.
    let cluster = FakeCluster::new().guests(vec![Some(running("env"))]);
    let response = handle_now(&request("create"), &cluster);
    assert_eq!(response.status, ResponseStatus::Ready);
    assert!(
        cluster.created.borrow().is_empty(),
        "an existing running guest must not be re-created"
    );
}

#[test]
fn create_names_the_guest_for_the_environment_not_the_worker() {
    let cluster = FakeCluster::new().guests(vec![None, Some(running("env"))]);
    handle_now(&request("create"), &cluster);
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
    handle_now(&req, &cluster);
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
    handle_now(&req, &cluster);
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
    let response = handle_now(&request("create"), &cluster);
    assert_eq!(response.status, ResponseStatus::Failed);
    assert!(response.error.unwrap_or_default().contains("disappeared"));
}

/// The exact condition `Environment::destroy` gates on, asserted against the
/// real create response.
///
/// Spelled out here rather than as a field check so it cannot drift from
/// `workenv-core/src/environment.rs:114`. Both halves of that gate were
/// unsatisfiable for Orchard: `inventory::guest` emits neither field, so every
/// `environment down` on an Orchard guest bailed "was adopted or has no verified
/// owned resource", and the integration cleanup behind that gate never ran.
fn destroy_gate_would_accept(data: &serde_json::Value) -> bool {
    !(data["owned"] != json!(true) || data["resource_id"].as_str().is_none())
}

#[test]
fn a_created_guest_can_actually_be_torn_down() {
    let cluster = FakeCluster::new().guests(vec![None, Some(running("env"))]);
    let response = handle_now(&request("create"), &cluster);
    assert_eq!(response.status, ResponseStatus::Changed);
    assert!(
        destroy_gate_would_accept(&response.data),
        "create receipt is rejected by the destroy ownership gate: {}",
        response.data
    );
    assert_eq!(response.data["resource_id"], json!("env"));
}

#[test]
fn a_second_up_still_leaves_a_tearable_down() {
    // The already-running outcome is the one that matters most: it is what every
    // `up` after the first reports, and reporting `owned: false` here would keep
    // teardown impossible for exactly that case.
    let cluster = FakeCluster::new().guests(vec![Some(running("env"))]);
    let response = handle_now(&request("create"), &cluster);
    assert_eq!(response.status, ResponseStatus::Ready);
    assert!(
        destroy_gate_would_accept(&response.data),
        "an already-running guest cannot be torn down: {}",
        response.data
    );
}

#[test]
fn a_create_that_times_out_is_still_tearable() {
    // A timed-out create may well have left a guest running, so the pending
    // receipt has to carry ownership too or the leak is unreachable.
    let cluster = FakeCluster::new().guests(vec![Some(pending("env"))]);
    let response = handle_now(&request("create"), &cluster);
    assert_eq!(response.status, ResponseStatus::Pending);
    assert!(
        destroy_gate_would_accept(&response.data),
        "a pending create leaves an unreachable guest: {}",
        response.data
    );
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
    handle_now(&req, &cluster);
    let body = cluster.created.borrow()[0].clone();
    assert_eq!(body["os"], json!("linux"));
    assert_eq!(body["arch"], json!("arm64"));
}

#[test]
fn an_intel_system_maps_to_amd64() {
    let cluster = FakeCluster::new().guests(vec![None, Some(running("env"))]);
    let mut req = request("create");
    req.target.system = "x86_64-linux".into();
    handle_now(&req, &cluster);
    assert_eq!(cluster.created.borrow()[0]["arch"], json!("amd64"));
}

#[test]
fn create_declares_the_worker_slot_it_consumes() {
    // Orchard's API does not default this; only its CLI fills it in. A body
    // written against the API without it produces a guest the scheduler never
    // places, pending forever with an empty status message.
    let cluster = FakeCluster::new().guests(vec![None, Some(running("env"))]);
    handle_now(&request("create"), &cluster);
    assert_eq!(
        cluster.created.borrow()[0]["resources"]["org.cirruslabs.tart-vms"],
        json!(1)
    );
}

#[test]
fn an_unreachable_controller_reports_the_whole_cause_chain() {
    // `{error}` on an anyhow error prints only its outermost layer, so an
    // unreachable controller reported "reading workers: controller unreachable"
    // and dropped the `connection refused` underneath -- leaving refusal, DNS,
    // TLS and timeout failures indistinguishable in the one place a reader looks.
    let cluster = FakeCluster::new().failing("workers");
    let response = handle_with(&request("inventory"), &cluster);
    assert_eq!(response.status, ResponseStatus::Failed);
    let error = response.error.unwrap_or_default();
    assert!(
        error.contains("reading workers"),
        "lost the outer context: {error}"
    );
    assert!(
        error.contains("connection refused"),
        "the root cause is still being dropped: {error}"
    );
}

#[test]
fn a_running_guest_whose_setup_never_finishes_is_pending_and_still_tearable() {
    // Running is not ready: the startup script installs Nix after boot. A create
    // that waited out its budget on a guest still provisioning must say pending,
    // and must still carry what teardown gates on, or the guest leaks.
    let cluster = FakeCluster::new().guests(vec![None, Some(running("env"))]);
    let req = request_with("create", json!({"startup_script": "install things"}), None);
    let clock = FakeClock::default();
    let response = super::dispatch(&req, &cluster, &clock, &|_| false);
    assert_eq!(response.status, ResponseStatus::Pending);
    assert!(
        response
            .error
            .as_deref()
            .is_some_and(|e| e.contains(".provisioned")),
        "the reason names what it waited for: {:?}",
        response.error
    );
    assert!(
        destroy_gate_would_accept(&response.data),
        "{}",
        response.data
    );
}

#[test]
fn a_fenced_runner_is_created_behind_softnet() {
    let cluster = FakeCluster::new().guests(vec![None, Some(running("env"))]);
    let mut req = request("create");
    req.config = json!({"network": {"isolated": true}});
    let response = handle_now(&req, &cluster);
    assert_eq!(response.status, ResponseStatus::Changed);
    assert_eq!(cluster.created.borrow()[0]["netSoftnet"], json!(true));
}

#[test]
fn a_malformed_fence_creates_nothing() {
    // The alternative is a guest created unfenced while its manifest says it is
    // fenced -- the one outcome this must never produce.
    let cluster = FakeCluster::new().guests(vec![None, Some(running("env"))]);
    let mut req = request("create");
    req.config = json!({"network": {"isolate": true}});
    let response = handle_now(&req, &cluster);
    assert_eq!(response.status, ResponseStatus::Failed);
    assert!(cluster.created.borrow().is_empty());
}
