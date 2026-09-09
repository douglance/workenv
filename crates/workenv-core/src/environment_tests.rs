use super::controller::Controller;
use super::test_support::*;
use anyhow::{Context as _, Result};
use serde_json::json;
use workenv_protocol::Binding;

#[test]
fn apply_runs_controller_bootstrap_before_apply() -> Result<()> {
    let root = tempfile::tempdir()?;
    let executor = std::sync::Arc::new(MockExecutor::default());
    let request_id = "apply-1:dev:bootstrap:0:setup";
    executor.responses.lock().map_err(lock_error)?.extend([
        response(request_id),
        raw_response("ready"),
        raw_response("/nix/store/example-devenv-profile"),
        raw_response("{\"shell.drvPath\":\"/nix/store/example-shell.drv\"}"),
        response("apply-1:dev:apply:0:setup"),
    ]);
    let controller = Controller::with_executor(
        root.path().to_path_buf(),
        integration_manifest(false),
        executor.clone(),
    )?;
    controller.environment("apply", "dev", Some("apply-1"))?;
    let calls = executor.calls.lock().map_err(lock_error)?;
    assert!(calls[0].idempotency_key.contains(":bootstrap:"));
    assert!(calls[1].idempotency_key.contains(":directory"));
    assert!(calls[2].idempotency_key.contains(":shell"));
    assert_eq!(calls[3].arg, ["eval", "--from", "path:.", "shell.drvPath"]);
    Ok(())
}

#[test]
fn destroy_rejects_unowned_provider() -> Result<()> {
    let root = tempfile::tempdir()?;
    let executor = std::sync::Arc::new(MockExecutor::default());
    let controller =
        Controller::with_executor(root.path().to_path_buf(), provider_manifest(true), executor)?;
    let result = controller.environment("destroy", "dev", Some("destroy-1"));
    assert!(result.is_err());
    Ok(())
}

#[test]
fn destroy_surfaces_pending_provider_create() -> Result<()> {
    let root = tempfile::tempdir()?;
    let executor = std::sync::Arc::new(MockExecutor::default());
    executor
        .responses
        .lock()
        .map_err(lock_error)?
        .push(pending_response("create-1:dev:create:0:setup"));
    let controller = Controller::with_executor(
        root.path().to_path_buf(),
        provider_manifest(true),
        executor.clone(),
    )?;
    controller.environment("create", "dev", Some("create-1"))?;
    let value = controller.environment("destroy", "dev", Some("destroy-1"))?;
    assert_eq!(value["ok"], false);
    assert_eq!(value["status"], "pending");
    assert_eq!(value["results"][0]["response"]["status"], "pending");
    assert_eq!(executor.calls.lock().map_err(lock_error)?.len(), 1);
    Ok(())
}

#[test]
fn connect_response_surfaces_not_ready_status() -> Result<()> {
    let root = tempfile::tempdir()?;
    let executor = std::sync::Arc::new(MockExecutor::default());
    executor
        .responses
        .lock()
        .map_err(lock_error)?
        .push(pending_response("connect-1"));
    let mut manifest = manifest(false);
    manifest
        .environments
        .get_mut("dev")
        .context("missing environment")?
        .connection = Some(Binding {
        extension: "setup".to_owned(),
        config: json!({"session":"dev"}),
    });
    let controller = Controller::with_executor(root.path().to_path_buf(), manifest, executor)?;
    let value = controller.environment("connect", "dev", Some("connect-1"))?;
    assert_eq!(value["ok"], false);
    assert_eq!(value["status"], "pending");
    assert_eq!(value["response"]["status"], "pending");
    Ok(())
}

#[test]
fn default_connect_returns_executable_descriptor() -> Result<()> {
    let root = tempfile::tempdir()?;
    let executor = std::sync::Arc::new(MockExecutor::default());
    let controller =
        Controller::with_executor(root.path().to_path_buf(), manifest(false), executor)?;
    let value = controller.environment("connect", "dev", None)?;
    assert_eq!(value["ok"], true);
    assert_eq!(value["status"], "ready");
    assert_eq!(
        value["argv"],
        json!(["devenv", "shell", "--from", "path:."])
    );
    assert_eq!(value["cwd"], json!("/tmp/dev"));
    Ok(())
}

#[test]
fn remote_default_connect_uses_transport_connect_descriptor() -> Result<()> {
    let root = tempfile::tempdir()?;
    let executor = std::sync::Arc::new(MockExecutor::default());
    executor
        .responses
        .lock()
        .map_err(lock_error)?
        .push(descriptor_response("connect-1"));
    let controller = Controller::with_executor(
        root.path().to_path_buf(),
        target_manifest(),
        executor.clone(),
    )?;
    let value = controller.environment("connect", "dev", Some("connect-1"))?;
    let calls = executor.calls.lock().map_err(lock_error)?;
    let request: workenv_protocol::AdapterRequest =
        serde_json::from_slice(calls[0].stdin.as_ref().context("missing stdin")?)?;
    assert_eq!(request.extension, "ssh");
    assert_eq!(request.operation, "connect");
    assert_eq!(
        value["response"]["data"]["attach_argv"],
        json!(["herdr", "--remote", "builder@example"])
    );
    Ok(())
}
#[test]
fn destroy_rejects_a_creation_receipt_from_changed_host_configuration() -> Result<()> {
    let root = tempfile::tempdir()?;
    let executor = std::sync::Arc::new(MockExecutor::default());
    let mut created = response("create");
    created.data = json!({"owned":true,"resource_id":"one"});
    executor.responses.lock().map_err(lock_error)?.push(created);
    let mut manifest = provider_manifest(true);
    Controller::with_executor(root.path().into(), manifest.clone(), executor.clone())?
        .environment("create", "dev", Some("create-1"))?;
    manifest
        .hosts
        .get_mut("local")
        .context("missing host")?
        .address = Some("different@example".into());
    let controller = Controller::with_executor(root.path().into(), manifest, executor.clone())?;
    assert!(
        controller
            .environment("destroy", "dev", Some("destroy-1"))
            .is_err()
    );
    assert_eq!(executor.calls.lock().map_err(lock_error)?.len(), 1);
    Ok(())
}
