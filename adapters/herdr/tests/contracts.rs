//! Herdr adapter contract tests.
mod support;

use anyhow::Result;
use serde_json::json;
use support::{Outputs, request};
use workenv_adapter_herdr::handle_with;
use workenv_protocol::ResponseStatus;

#[test]
fn cleanup_rechecks_registration_on_retry() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let mut request = request(temp.path(), "cleanup");
    request.previous = Some(json!({"data":{
        "profile_id":"profile-1", "target":"exedev@worker", "session":"workenv"
    }}));
    let runner = Outputs::new(vec![json!([]), json!([])]);
    assert_eq!(
        handle_with(&request, &runner)?.status,
        ResponseStatus::Ready
    );
    assert_eq!(
        handle_with(&request, &runner)?.status,
        ResponseStatus::Ready
    );
    let calls = runner
        .calls
        .lock()
        .map_err(|_| anyhow::anyhow!("mutex poisoned"))?;
    assert_ne!(calls[0].idempotency_key, calls[1].idempotency_key);
    Ok(())
}

#[test]
fn register_rejects_same_label_wrong_session() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let runner = Outputs::new(vec![json!([
        {"id":"other-profile","label":"workenv-01","target":"exedev@worker","session":"default","enabled":true}
    ])]);
    let result = handle_with(&request(temp.path(), "register"), &runner)?;
    assert_eq!(result.status, ResponseStatus::Failed);
    assert_eq!(
        runner
            .calls
            .lock()
            .map_err(|_| anyhow::anyhow!("mutex poisoned"))?
            .len(),
        1
    );
    Ok(())
}

#[test]
fn register_adds_missing_machine_with_remote_session() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let runner = Outputs::new(vec![
        json!([]),
        json!({}),
        json!([
            {"id":"profile-1","label":"workenv-01","target":"exedev@worker","session":"workenv","enabled":true}
        ]),
    ]);
    let result = handle_with(&request(temp.path(), "register"), &runner)?;
    assert_eq!(result.status, ResponseStatus::Changed);
    assert_eq!(result.data["profile_id"], "profile-1");
    let calls = runner
        .calls
        .lock()
        .map_err(|_| anyhow::anyhow!("mutex poisoned"))?;
    assert_eq!(calls[1].arg[5], "--remote-session");
    assert_eq!(calls[1].arg[6], "workenv");
    assert!(calls.iter().all(|call| call.cwd.is_none()));
    Ok(())
}

#[test]
fn register_does_not_change_when_add_fails() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let runner = Outputs::new_with_codes(vec![
        (json!([]), Some(0), ""),
        (json!({}), Some(1), "denied"),
    ]);
    let result = handle_with(&request(temp.path(), "register"), &runner)?;
    assert_eq!(result.status, ResponseStatus::Failed);
    assert_eq!(result.data["status"], "registration_failed");
    Ok(())
}

#[test]
fn register_propagates_pending_add_execution() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let runner = Outputs::new_with_codes(vec![(json!([]), Some(0), ""), (json!({}), None, "")]);
    let result = handle_with(&request(temp.path(), "register"), &runner)?;
    assert_eq!(result.status, ResponseStatus::Pending);
    assert_eq!(result.execution_id, Some("execution-1".to_string()));
    assert_eq!(result.data["status"], "registration_pending");
    Ok(())
}

#[test]
fn cleanup_removes_exact_previous_profile_id() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let mut request = request(temp.path(), "cleanup");
    request.previous = Some(
        json!({"data":{"profile_id":"profile-1","target":"exedev@worker","session":"workenv"}}),
    );
    let runner = Outputs::new(vec![
        json!([
            {"id":"profile-1","label":"workenv-01","target":"exedev@worker","session":"workenv","enabled":true},
            {"id":"profile-2","label":"workenv-01","target":"other","session":"workenv","enabled":true}
        ]),
        json!({}),
        json!([
            {"id":"profile-2","label":"workenv-01","target":"other","session":"workenv","enabled":true}
        ]),
    ]);
    let result = handle_with(&request, &runner)?;
    assert_eq!(result.status, ResponseStatus::Changed);
    assert_eq!(result.data["profile_id"], "profile-1");
    let calls = runner
        .calls
        .lock()
        .map_err(|_| anyhow::anyhow!("mutex poisoned"))?;
    assert_eq!(calls[0].cwd, None);
    assert_eq!(calls[1].arg, ["machine", "remove", "profile-1"]);
    assert_eq!(calls[1].cwd, None);
    Ok(())
}

#[test]
fn cleanup_is_ready_when_exact_profile_already_missing() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let mut request = request(temp.path(), "cleanup");
    request.previous = Some(
        json!({"data":{"profile_id":"profile-1","target":"exedev@worker","session":"workenv"}}),
    );
    let runner = Outputs::new(vec![json!([
        {"id":"profile-2","label":"workenv-01","target":"other","session":"workenv","enabled":true}
    ])]);
    let result = handle_with(&request, &runner)?;
    assert_eq!(result.status, ResponseStatus::Ready);
    assert_eq!(result.data["profile_id"], "profile-1");
    assert_eq!(
        runner
            .calls
            .lock()
            .map_err(|_| anyhow::anyhow!("mutex poisoned"))?
            .len(),
        1
    );
    Ok(())
}

#[test]
fn cleanup_refuses_without_previous_profile_id() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let result = handle_with(&request(temp.path(), "cleanup"), &Outputs::new(vec![]))?;
    assert_eq!(result.status, ResponseStatus::Failed);
    assert_eq!(result.data["status"], "herdr_cleanup_blocked");
    Ok(())
}

#[test]
fn cleanup_refuses_reused_profile_id_with_different_target() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let mut request = request(temp.path(), "cleanup");
    request.previous = Some(
        json!({"data":{"profile_id":"profile-1","target":"exedev@worker","session":"workenv"}}),
    );
    let runner = Outputs::new(vec![json!([
        {"id":"profile-1","label":"workenv-01","target":"other","session":"workenv","enabled":true}
    ])]);
    let result = handle_with(&request, &runner)?;
    assert_eq!(result.status, ResponseStatus::Failed);
    assert_eq!(result.data["status"], "herdr_cleanup_identity_mismatch");
    assert_eq!(
        runner
            .calls
            .lock()
            .map_err(|_| anyhow::anyhow!("mutex poisoned"))?
            .len(),
        1
    );
    Ok(())
}
