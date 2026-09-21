//! Orchard reap tests.
use super::handle_with;
use super::test_support::{FakeCluster, request_with};
use serde_json::json;
use workenv_protocol::ResponseStatus;

/// A guest with an explicit age in seconds.
fn aged(name: &str, age_seconds: i64) -> serde_json::Value {
    let created = chrono::Utc::now() - chrono::Duration::seconds(age_seconds);
    json!({"name": name, "status": "running", "createdAt": created.to_rfc3339()})
}

fn reap(
    input: serde_json::Value,
    guests: Vec<serde_json::Value>,
) -> (ResponseStatus, serde_json::Value, Vec<String>) {
    let cluster = FakeCluster::new().with("vms", guests);
    let response = handle_with(&request_with("reap", input, None), &cluster);
    let removed = cluster.removed.borrow().clone();
    (response.status, response.data, removed)
}

#[test]
fn a_guest_past_its_lease_is_reaped() {
    let (status, data, removed) = reap(
        json!({"lease_seconds": 3600, "name_prefix": "wkv-"}),
        vec![aged("wkv-old", 7200)],
    );
    assert_eq!(status, ResponseStatus::Changed);
    assert_eq!(data["reaped"], json!(["wkv-old"]));
    assert_eq!(removed, vec!["wkv-old"]);
}

#[test]
fn a_guest_inside_its_lease_is_kept() {
    let (status, data, removed) = reap(
        json!({"lease_seconds": 3600, "name_prefix": "wkv-"}),
        vec![aged("wkv-young", 60)],
    );
    assert_eq!(
        status,
        ResponseStatus::Ready,
        "keeping everything is not a change"
    );
    assert_eq!(data["kept"], json!(["wkv-young"]));
    assert!(removed.is_empty());
}

#[test]
fn guests_outside_the_prefix_are_never_touched() {
    // The prefix is the ownership boundary. Another team's guest on the same
    // cluster must survive a sweep no matter how old it is.
    let (_, data, removed) = reap(
        json!({"lease_seconds": 60, "name_prefix": "wkv-"}),
        vec![aged("someone-elses-build-box", 99999)],
    );
    assert!(removed.is_empty(), "reaped a guest outside the prefix");
    assert_eq!(data["reaped"], json!([]));
    assert_eq!(data["kept"], json!([]));
}

#[test]
fn reap_without_a_prefix_is_refused() {
    // Defaulting the prefix to empty would make an omitted setting mean
    // "every guest in the cluster is mine to delete".
    let (status, _, removed) = reap(json!({"lease_seconds": 60}), vec![aged("wkv-old", 9999)]);
    assert_eq!(status, ResponseStatus::Failed);
    assert!(removed.is_empty());
}

#[test]
fn an_unreadable_timestamp_is_skipped_not_reaped() {
    // An unparseable date must not read as infinitely old, or one malformed
    // record takes the whole fleet with it.
    let (_, data, removed) = reap(
        json!({"lease_seconds": 60, "name_prefix": "wkv-"}),
        vec![json!({"name": "wkv-garbled", "createdAt": "not a date"})],
    );
    assert!(
        removed.is_empty(),
        "reaped a guest whose age could not be read"
    );
    assert_eq!(data["skipped"][0]["name"], json!("wkv-garbled"));
}

#[test]
fn a_guest_with_no_timestamp_is_skipped() {
    let (_, data, removed) = reap(
        json!({"lease_seconds": 60, "name_prefix": "wkv-"}),
        vec![json!({"name": "wkv-notime"})],
    );
    assert!(removed.is_empty());
    assert_eq!(data["skipped"][0]["name"], json!("wkv-notime"));
}

#[test]
fn a_dry_run_reports_without_removing() {
    let (status, data, removed) = reap(
        json!({"lease_seconds": 60, "name_prefix": "wkv-", "dry_run": true}),
        vec![aged("wkv-old", 9999)],
    );
    assert_eq!(status, ResponseStatus::Ready);
    assert_eq!(data["reaped"], json!(["wkv-old"]));
    assert!(removed.is_empty(), "a dry run must not remove anything");
}

#[test]
fn the_lease_boundary_reaps_rather_than_keeps() {
    // The direction of this comparison is covered above; the boundary itself was
    // not, so `<` could become `<=` unnoticed. That is not cosmetic: a lease is a
    // promise the guest is gone once it elapses, and `<=` keeps a guest that has
    // exactly reached it, so a slot held at the boundary never frees on the sweep
    // that was supposed to free it.
    //
    // `aged` builds `createdAt` from the clock, so by the time reap reads it the
    // guest is a shade older than asked for -- which is what makes "exactly at the
    // lease" testable at all from outside.
    let (status, data, removed) = reap(
        json!({"lease_seconds": 600, "name_prefix": "wkv-"}),
        vec![aged("wkv-exactly-due", 600)],
    );
    assert_eq!(status, ResponseStatus::Changed);
    assert_eq!(
        data["reaped"],
        json!(["wkv-exactly-due"]),
        "a guest that has reached its lease was kept instead of reaped"
    );
    assert_eq!(removed, vec!["wkv-exactly-due".to_owned()]);
}

#[test]
fn one_second_short_of_the_lease_is_still_kept() {
    // The paired control, so the test above cannot be satisfied by a sweep that
    // simply reaps everything.
    let (status, data, removed) = reap(
        json!({"lease_seconds": 600, "name_prefix": "wkv-"}),
        vec![aged("wkv-nearly-due", 598)],
    );
    assert_eq!(status, ResponseStatus::Ready);
    assert_eq!(data["kept"], json!(["wkv-nearly-due"]));
    assert!(removed.is_empty(), "reaped a guest still inside its lease");
}

#[test]
fn a_parked_guest_is_kept_past_its_lease() {
    // Parked on purpose, with someone's unfinished work in it: age alone must
    // not take it. The unparked guest of the same age is the control.
    let (status, data, removed) = reap(
        json!({"lease_seconds": 3600, "name_prefix": "wkv-", "keep": ["wkv-parked"]}),
        vec![aged("wkv-parked", 7200), aged("wkv-abandoned", 7200)],
    );
    assert_eq!(status, ResponseStatus::Changed);
    assert_eq!(data["reaped"], json!(["wkv-abandoned"]));
    assert_eq!(data["kept"], json!(["wkv-parked"]));
    assert_eq!(removed, vec!["wkv-abandoned"]);
}
