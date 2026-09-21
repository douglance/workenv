use std::sync::Arc;

use anyhow::Result;
use serde_json::{Value, json};

use super::{applied, fence_from, integrations_ready, ready};
use crate::{Controller, test_support::*};

fn step(extension: &str, status: &str) -> Value {
    json!({ "extension": extension, "response": { "status": status } })
}

fn find<'a>(report: &'a Value, kind: &str) -> &'a Value {
    report["conditions"]
        .as_array()
        .and_then(|all| all.iter().find(|c| c["type"] == kind))
        .unwrap_or(&Value::Null)
}

#[test]
fn an_environment_never_applied_is_not_ready_and_says_why() -> Result<()> {
    let root = tempfile::tempdir()?;
    let executor = Arc::new(MockExecutor::default());
    executor
        .responses
        .lock()
        .map_err(lock_error)?
        .push(raw_response("devenv 2.3.0"));
    let controller =
        Controller::with_executor(root.path().into(), provider_manifest(false), executor)?;
    let report = controller.environment("status", "dev", None)?;

    assert_eq!(find(&report, "Applied")["status"], "false");
    assert_eq!(
        find(&report, "Applied")["reason"],
        "environment_not_applied"
    );
    assert_eq!(find(&report, "Ready")["status"], "false");
    assert_eq!(find(&report, "Ready")["reason"], "Applied is false");
    // The top level is untouched, so exit codes are too.
    assert_eq!(report["status"], "pending");
    Ok(())
}

#[test]
fn a_provider_environment_with_no_creation_record_has_an_unknown_fence() -> Result<()> {
    // Silence here would read as "fine". An unknown fence must be visible.
    let root = tempfile::tempdir()?;
    let executor = Arc::new(MockExecutor::default());
    executor
        .responses
        .lock()
        .map_err(lock_error)?
        .push(raw_response("devenv 2.3.0"));
    let controller =
        Controller::with_executor(root.path().into(), provider_manifest(false), executor)?;
    let report = controller.environment("status", "dev", None)?;
    assert_eq!(find(&report, "Fenced")["status"], "unknown");
    assert_eq!(find(&report, "Fenced")["reason"], "no_recorded_creation");
    Ok(())
}

#[test]
fn applied_reads_true_false_and_unknown_from_the_devenv_step() {
    assert_eq!(applied(Some(&step("devenv", "ready")))["status"], "true");
    let known = json!({"response": {"status": "pending", "data": {"reason": "devenv_configuration_changed"}}});
    assert_eq!(applied(Some(&known))["status"], "false");
    assert_eq!(
        applied(Some(&known))["reason"],
        "devenv_configuration_changed"
    );
    // No reason means devenv never answered: that is not a known "not applied".
    let unanswered = json!({"response": {"status": "failed", "error": "devenv: not found"}});
    assert_eq!(applied(Some(&unanswered))["status"], "unknown");
    assert_eq!(applied(Some(&unanswered))["reason"], "devenv: not found");
}

#[test]
fn integrations_name_the_first_one_that_is_not_ready() {
    let steps = [
        step("workenv.identity", "ready"),
        step("workenv.clipboard", "pending"),
    ];
    let condition = integrations_ready(&steps);
    assert_eq!(condition["status"], "false");
    assert_eq!(condition["reason"], "workenv.clipboard is pending");
    assert_eq!(integrations_ready(&[])["reason"], "none_declared");
}

#[test]
fn ready_is_true_only_when_every_part_is() {
    let yes = json!({"type": "Applied", "status": "true"});
    let unknown = json!({"type": "IntegrationsReady", "status": "unknown"});
    assert_eq!(ready(&[&yes, &yes])["status"], "true");
    let not = ready(&[&yes, &unknown]);
    assert_eq!(not["status"], "false");
    assert_eq!(not["reason"], "IntegrationsReady is unknown");
}

#[test]
fn the_fence_is_read_from_what_the_provider_recorded() {
    assert_eq!(
        fence_from(&json!({"network": {"isolated": true}}))["status"],
        "true"
    );
    assert_eq!(
        fence_from(&json!({"network": {"isolated": false}}))["status"],
        "false"
    );
    // A provider that records no fence at all: unknown, never true by default.
    assert_eq!(fence_from(&json!({"name": "vm"}))["status"], "unknown");
}
