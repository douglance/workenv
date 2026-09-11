use super::controller::Controller;
use super::test_support::*;
use anyhow::{Context as _, Result};
use serde_json::json;
use workenv_protocol::{Binding, ResponseStatus};

#[test]
fn up_orders_create_apply_then_register_and_replays_same_key() -> Result<()> {
    let root = tempfile::tempdir()?;
    let executor = std::sync::Arc::new(MockExecutor::default());
    executor.responses.lock().map_err(lock_error)?.extend([
        response("create"),
        response("bootstrap"),
        raw_response("ready"),
        response("prepare"),
        raw_response("/nix/store/example-devenv-profile"),
        raw_response("{\"shell.drvPath\":\"/nix/store/example-shell.drv\"}"),
        response("apply"),
        response("register"),
    ]);
    let controller =
        Controller::with_executor(root.path().into(), lifecycle_manifest()?, executor.clone())?;
    let value = controller.environment("up", "dev", Some("up-1"))?;
    assert_eq!(value["ok"], true);
    assert_eq!(
        operations(&executor)?,
        [
            "create",
            "bootstrap",
            "directory",
            "prepare",
            "shell",
            "identity",
            "apply",
            "register"
        ]
    );
    controller.environment("up", "dev", Some("up-1"))?;
    assert_eq!(executor.calls.lock().map_err(lock_error)?.len(), 8);
    Ok(())
}

#[test]
fn up_stops_before_apply_when_create_is_pending() -> Result<()> {
    let root = tempfile::tempdir()?;
    let executor = std::sync::Arc::new(MockExecutor::default());
    executor
        .responses
        .lock()
        .map_err(lock_error)?
        .push(pending_response("create"));
    let controller =
        Controller::with_executor(root.path().into(), lifecycle_manifest()?, executor.clone())?;
    let value = controller.environment("up", "dev", Some("up-1"))?;
    assert_eq!(value["status"], "pending");
    assert_eq!(executor.calls.lock().map_err(lock_error)?.len(), 1);
    Ok(())
}

#[test]
fn up_stops_before_register_when_apply_is_pending() -> Result<()> {
    let root = tempfile::tempdir()?;
    let executor = std::sync::Arc::new(MockExecutor::default());
    executor
        .responses
        .lock()
        .map_err(lock_error)?
        .extend([response("create"), pending_response("bootstrap")]);
    let controller =
        Controller::with_executor(root.path().into(), lifecycle_manifest()?, executor.clone())?;
    let value = controller.environment("up", "dev", Some("up-1"))?;
    assert_eq!(value["status"], "pending");
    assert_eq!(operations(&executor)?, ["create", "bootstrap"]);
    Ok(())
}

#[test]
fn apply_stops_before_realize_when_prepare_is_pending() -> Result<()> {
    let root = tempfile::tempdir()?;
    let executor = std::sync::Arc::new(MockExecutor::default());
    executor.responses.lock().map_err(lock_error)?.extend([
        response("bootstrap"),
        raw_response("ready"),
        pending_response("prepare"),
    ]);
    let controller =
        Controller::with_executor(root.path().into(), lifecycle_manifest()?, executor.clone())?;
    let value = controller.environment("apply", "dev", Some("apply-1"))?;
    assert_eq!(value["status"], "pending");
    assert_eq!(
        operations(&executor)?,
        ["bootstrap", "directory", "prepare"]
    );
    Ok(())
}

#[test]
fn up_retries_failed_register_without_reinvoking_create_or_apply() -> Result<()> {
    let root = tempfile::tempdir()?;
    let executor = std::sync::Arc::new(MockExecutor::default());
    let mut failed = response("register");
    failed.status = ResponseStatus::Failed;
    failed.error = Some("not ready".to_owned());
    executor.responses.lock().map_err(lock_error)?.extend([
        response("create"),
        response("bootstrap"),
        raw_response("ready"),
        response("prepare"),
        raw_response("/nix/store/example-devenv-profile"),
        raw_response("{\"shell.drvPath\":\"/nix/store/example-shell.drv\"}"),
        response("apply"),
        failed,
        response("register"),
    ]);
    let controller =
        Controller::with_executor(root.path().into(), lifecycle_manifest()?, executor.clone())?;
    assert_eq!(
        controller.environment("up", "dev", Some("up-1"))?["ok"],
        false
    );
    assert_eq!(
        controller.environment("up", "dev", Some("up-1"))?["ok"],
        true
    );
    let calls = executor.calls.lock().map_err(lock_error)?;
    assert_eq!(calls.len(), 9);
    assert_eq!(request_operation(&calls[8])?, "register");
    Ok(())
}

#[test]
fn up_allows_no_register_integrations() -> Result<()> {
    let root = tempfile::tempdir()?;
    let executor = std::sync::Arc::new(MockExecutor::default());
    executor.responses.lock().map_err(lock_error)?.extend([
        response("create"),
        response("bootstrap"),
        raw_response("ready"),
        raw_response("/nix/store/example-devenv-profile"),
        raw_response("{\"shell.drvPath\":\"/nix/store/example-shell.drv\"}"),
        response("apply"),
    ]);
    let controller =
        Controller::with_executor(root.path().into(), no_register_manifest(), executor.clone())?;
    let value = controller.environment("up", "dev", Some("up-1"))?;
    assert_eq!(value["ok"], true);
    assert_eq!(executor.calls.lock().map_err(lock_error)?.len(), 6);
    Ok(())
}

#[test]
fn up_rejects_connection_register_binding_missing_from_integrations() -> Result<()> {
    let root = tempfile::tempdir()?;
    let executor = std::sync::Arc::new(MockExecutor::default());
    let mut manifest = lifecycle_manifest()?;
    manifest
        .environments
        .get_mut("dev")
        .context("dev")?
        .integrations
        .clear();
    manifest
        .environments
        .get_mut("dev")
        .context("dev")?
        .connection = Some(Binding {
        extension: "setup".to_owned(),
        config: json!({}),
    });
    let controller = Controller::with_executor(root.path().into(), manifest, executor.clone())?;
    let error = controller
        .environment("up", "dev", Some("up-1"))
        .err()
        .context("up unexpectedly succeeded")?;
    assert!(
        error
            .to_string()
            .contains("add the same binding to integrations")
    );
    assert_eq!(executor.calls.lock().map_err(lock_error)?.len(), 0);
    Ok(())
}

#[test]
fn down_uses_destroy_cleanup_path() -> Result<()> {
    let root = tempfile::tempdir()?;
    let executor = std::sync::Arc::new(MockExecutor::default());
    let mut created = response("create");
    created.data = json!({"owned":true,"resource_id":"vm-1"});
    executor.responses.lock().map_err(lock_error)?.extend([
        created,
        response("register"),
        response("destroy"),
        response("cleanup"),
    ]);
    let controller =
        Controller::with_executor(root.path().into(), cleanup_manifest()?, executor.clone())?;
    controller.environment("create", "dev", Some("create-1"))?;
    controller.setup_binding(&cleanup_binding(), "register", "dev", Some("register-1"))?;
    let value = controller.environment("down", "dev", Some("down-1"))?;
    assert_eq!(value["operation"], "down");
    assert_eq!(
        request_operation(&executor.calls.lock().map_err(lock_error)?[3])?,
        "cleanup"
    );
    Ok(())
}

fn lifecycle_manifest() -> Result<workenv_protocol::Manifest> {
    let mut manifest = manifest(true);
    let operations = &mut manifest
        .extensions
        .get_mut("setup")
        .context("setup extension")?
        .operations;
    operations.insert("prepare".to_owned(), operation(true, json!({})));
    operations.insert("register".to_owned(), operation(true, json!({})));
    Ok(manifest)
}

fn no_register_manifest() -> workenv_protocol::Manifest {
    manifest(true)
}

fn cleanup_manifest() -> Result<workenv_protocol::Manifest> {
    let mut manifest = lifecycle_manifest()?;
    manifest
        .extensions
        .get_mut("setup")
        .context("setup extension")?
        .operations
        .insert(
            "cleanup".to_owned(),
            operation(true, json!({"type":"object"})),
        );
    Ok(manifest)
}

fn cleanup_binding() -> Binding {
    Binding {
        extension: "setup".to_owned(),
        config: json!({}),
    }
}

fn operations(executor: &MockExecutor) -> Result<Vec<String>> {
    executor
        .calls
        .lock()
        .map_err(lock_error)?
        .iter()
        .map(call_operation)
        .collect()
}

fn call_operation(call: &workenv_platform::ExecutionSpec) -> Result<String> {
    if call.stdin.is_some() {
        request_operation(call)
    } else if call.arg.first().is_some_and(|arg| arg == "-c") {
        Ok("directory".to_owned())
    } else if call.arg.iter().any(|arg| arg == "printenv") {
        Ok("shell".to_owned())
    } else {
        Ok("identity".to_owned())
    }
}

fn request_operation(call: &workenv_platform::ExecutionSpec) -> Result<String> {
    let input = call.stdin.as_ref().context("missing stdin")?;
    Ok(serde_json::from_slice::<workenv_protocol::AdapterRequest>(input)?.operation)
}
