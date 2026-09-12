use super::controller::{CallOptions, Controller};
use super::test_support::*;
use anyhow::{Context as _, Result};
use serde_json::{Value, json};
use workenv_protocol::Location;

#[test]
fn target_extension_uses_transport_devenv_command() -> Result<()> {
    let root = tempfile::tempdir()?;
    let executor = std::sync::Arc::new(MockExecutor::default());
    executor
        .responses
        .lock()
        .map_err(lock_error)?
        .push(transport_response("read-1"));
    let controller = Controller::with_executor(
        root.path().to_path_buf(),
        target_manifest(),
        executor.clone(),
    )?;
    controller.extension_call(
        "target-tool",
        "status",
        CallOptions {
            environment: "dev".to_owned(),
            input: Value::Null,
            key: Some("read-1".to_owned()),
        },
    )?;
    let calls = executor.calls.lock().map_err(lock_error)?;
    let request: workenv_protocol::AdapterRequest =
        serde_json::from_slice(calls[0].stdin.as_ref().context("missing stdin")?)?;
    assert_eq!(request.extension, "ssh");
    assert_eq!(request.operation, "execute");
    assert_eq!(
        request.input["argv"],
        json!(["devenv", "shell", "--from", "path:.", "--", "target-tool"])
    );
    Ok(())
}

#[test]
fn controller_extension_uses_controller_system_not_target_host() -> Result<()> {
    let root = tempfile::tempdir()?;
    let executor = std::sync::Arc::new(MockExecutor::default());
    executor
        .responses
        .lock()
        .map_err(lock_error)?
        .push(response("read-1"));
    let controller = Controller::with_executor(
        root.path().to_path_buf(),
        system_manifest(
            Location::Controller,
            vec![current_system()],
            alternate_system(),
        ),
        executor,
    )?;
    controller.extension_call(
        "setup",
        "status",
        CallOptions {
            environment: "dev".to_owned(),
            input: Value::Null,
            key: Some("read-1".to_owned()),
        },
    )?;
    Ok(())
}

#[test]
fn controller_extension_rejects_target_only_system() -> Result<()> {
    let root = tempfile::tempdir()?;
    let executor = std::sync::Arc::new(MockExecutor::default());
    let error = Controller::with_executor(
        root.path().to_path_buf(),
        system_manifest(
            Location::Controller,
            vec![alternate_system()],
            alternate_system(),
        ),
        executor,
    )
    .err()
    .context("target-only controller extension unexpectedly validated")?;
    assert!(error.to_string().contains("does not support"));
    Ok(())
}

#[test]
fn target_extension_uses_target_host_system() -> Result<()> {
    let root = tempfile::tempdir()?;
    let executor = std::sync::Arc::new(MockExecutor::default());
    executor
        .responses
        .lock()
        .map_err(lock_error)?
        .push(response("read-1"));
    let controller = Controller::with_executor(
        root.path().to_path_buf(),
        system_manifest(
            Location::Target,
            vec![alternate_system()],
            alternate_system(),
        ),
        executor,
    )?;
    controller.extension_call(
        "setup",
        "status",
        CallOptions {
            environment: "dev".to_owned(),
            input: Value::Null,
            key: Some("read-1".to_owned()),
        },
    )?;
    Ok(())
}

#[test]
fn a_stage_through_a_transport_keeps_the_direct_paths_time_budget() -> Result<()> {
    // `transport_command` sent only argv and cwd, so the transport applied its own
    // default timeout -- orchard's is 300_000 ms against the 900_000 ms this path
    // budgets directly. The same command therefore got a third of the time purely
    // for travelling through a transport, and a slow `devenv shell` inside a guest
    // came back as the carrier having written nothing.
    let root = tempfile::tempdir()?;
    let executor = std::sync::Arc::new(MockExecutor::default());
    {
        let mut responses = executor.responses.lock().map_err(lock_error)?;
        for _ in 0..6 {
            responses.push(transport_response("apply-1"));
        }
    }
    let controller = Controller::with_executor(
        root.path().to_path_buf(),
        target_manifest(),
        executor.clone(),
    )?;
    // The stage may or may not run to completion with these canned answers; what
    // matters is the transport input of whichever stage calls first.
    let _ = controller.environment("apply", "dev", Some("apply-1"));
    let calls = executor.calls.lock().map_err(lock_error)?;
    let mut budgets = Vec::new();
    for call in calls.iter() {
        let Some(stdin) = call.stdin.as_ref() else {
            continue;
        };
        let Ok(request) = serde_json::from_slice::<workenv_protocol::AdapterRequest>(stdin) else {
            continue;
        };
        if request.operation == "execute" && request.input.get("argv").is_some() {
            budgets.push(request.input["timeout_ms"].clone());
        }
    }
    assert!(
        !budgets.is_empty(),
        "no transport execute call was made, so nothing was asserted"
    );
    for budget in budgets {
        assert_eq!(
            budget,
            json!(900_000),
            "a stage reached the transport without this path's time budget"
        );
    }
    Ok(())
}
