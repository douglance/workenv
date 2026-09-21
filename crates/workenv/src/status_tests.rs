use std::cell::Cell;

use serde_json::{Value, json};

use super::{is_ready, wait_until_ready};

fn report(ready: &str) -> Value {
    json!({ "ok": ready == "true", "conditions": [{ "type": "Ready", "status": ready }] })
}

#[test]
fn it_stops_reading_the_moment_ready_is_true() {
    let reads = Cell::new(0);
    let now = Cell::new(0);
    let result = wait_until_ready(
        || {
            reads.set(reads.get() + 1);
            Ok(report(if reads.get() >= 3 { "true" } else { "false" }))
        },
        300,
        &|| now.get(),
        &|| {
            now.set(now.get() + 5);
        },
    )
    .unwrap();
    assert_eq!(reads.get(), 3);
    assert_eq!(result["waited"]["ready"], true);
}

#[test]
fn a_timeout_returns_the_last_report_and_says_it_timed_out() {
    // The caller still needs to see which condition held it back.
    let now = Cell::new(0);
    let result = wait_until_ready(|| Ok(report("false")), 30, &|| now.get(), &|| {
        now.set(now.get() + 5);
    })
    .unwrap();
    assert_eq!(result["waited"]["ready"], false);
    assert_eq!(result["conditions"][0]["status"], "false");
    assert!(now.get() >= 30, "gave up before its timeout");
}

#[test]
fn only_a_true_ready_condition_counts() {
    assert!(is_ready(&report("true")));
    assert!(!is_ready(&report("false")));
    assert!(!is_ready(&report("unknown")));
    // `ok` alone is not readiness: a report with no conditions is not ready.
    assert!(!is_ready(&json!({ "ok": true })));
}

#[test]
fn each_condition_change_is_recorded_with_when_it_happened() {
    // Applied flips first, then Ready: the record says which part took the time.
    let reads = Cell::new(0);
    let now = Cell::new(0);
    let reports = [
        json!({"conditions": [{"type": "Applied", "status": "false", "reason": "environment_not_applied"},
                              {"type": "Ready", "status": "false", "reason": "Applied is false"}]}),
        json!({"conditions": [{"type": "Applied", "status": "true", "reason": "applied_and_current"},
                              {"type": "Ready", "status": "false", "reason": "IntegrationsReady is false"}]}),
        json!({"conditions": [{"type": "Applied", "status": "true", "reason": "applied_and_current"},
                              {"type": "Ready", "status": "true", "reason": "all_conditions_true"}]}),
    ];
    let result = wait_until_ready(
        || {
            let index = reads.get().min(2);
            reads.set(reads.get() + 1);
            Ok(reports[index].clone())
        },
        300,
        &|| now.get(),
        &|| {
            now.set(now.get() + 5);
        },
    )
    .unwrap();
    let transitions = result["waited"]["transitions"].as_array().unwrap();
    assert_eq!(transitions.len(), 2, "{transitions:?}");
    assert_eq!(transitions[0]["type"], "Applied");
    assert_eq!(transitions[0]["from"], "false");
    assert_eq!(transitions[0]["to"], "true");
    assert_eq!(transitions[0]["at_seconds"], 5);
    assert_eq!(transitions[1]["type"], "Ready");
    assert_eq!(transitions[1]["at_seconds"], 10);
}

#[test]
fn a_status_that_never_moves_records_no_transitions() {
    let now = Cell::new(0);
    let result = wait_until_ready(|| Ok(report("false")), 20, &|| now.get(), &|| {
        now.set(now.get() + 5);
    })
    .unwrap();
    assert_eq!(result["waited"]["transitions"], json!([]));
}
