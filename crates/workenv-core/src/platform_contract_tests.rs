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
