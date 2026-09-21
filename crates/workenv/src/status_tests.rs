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
