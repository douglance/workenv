use super::controller::{CallOptions, Controller};
use super::test_support::*;
use anyhow::{Context as _, Result};
use serde_json::{Value, json};
use workenv_protocol::Binding;
use workenv_protocol::PROTOCOL_VERSION;

#[test]
fn parse_manifest_output_reads_manifest_json_attr() -> Result<()> {
    let manifest = integration_manifest(false);
    let manifest_json = serde_json::to_string(&manifest)?;
    let output = json!({"workenv.manifestJSON": manifest_json});
    let parsed = super::controller::parse_manifest_output(&serde_json::to_string(&output)?)?;
    assert_eq!(parsed.schema_version, PROTOCOL_VERSION);
    assert!(parsed.environments.contains_key("dev"));
    Ok(())
}

#[test]
fn manifest_evaluation_uses_upstream_json_default() -> Result<()> {
    let root = tempfile::tempdir()?;
    let executor = MockExecutor::default();
    let encoded = serde_json::to_string(&integration_manifest(false))?;
    executor
        .responses
        .lock()
        .map_err(lock_error)?
        .push(raw_response(
            &json!({"workenv.manifestJSON":encoded}).to_string(),
        ));
    super::config::load(root.path(), &executor)?;
    let calls = executor.calls.lock().map_err(lock_error)?;
    assert_eq!(calls[0].executable, "devenv");
    assert_eq!(calls[0].arg, ["eval", "workenv.manifestJSON"]);
    Ok(())
}

#[test]
fn mutating_extension_call_replays_completed_receipt() -> Result<()> {
    let root = tempfile::tempdir()?;
    let executor = std::sync::Arc::new(MockExecutor::default());
    executor
        .responses
        .lock()
        .map_err(lock_error)?
        .push(response("key-1"));
    let controller = Controller::with_executor(
        root.path().to_path_buf(),
        integration_manifest(false),
        executor.clone(),
    )?;
    let options = CallOptions {
        environment: "dev".to_owned(),
        input: json!({"name":"one"}),
        key: Some("key-1".to_owned()),
    };
    controller.extension_call("setup", "create", options.clone())?;
    controller.extension_call("setup", "create", options)?;
    assert_eq!(executor.calls.lock().map_err(lock_error)?.len(), 1);
    Ok(())
}

#[test]
fn mutating_call_rejects_same_key_for_different_input() -> Result<()> {
    let root = tempfile::tempdir()?;
    let executor = std::sync::Arc::new(MockExecutor::default());
    executor
        .responses
        .lock()
        .map_err(lock_error)?
        .push(response("key-1"));
    let controller = Controller::with_executor(
        root.path().to_path_buf(),
        integration_manifest(false),
        executor,
    )?;
    controller.extension_call(
        "setup",
        "create",
        CallOptions {
            environment: "dev".to_owned(),
            input: json!({"name":"one"}),
            key: Some("key-1".to_owned()),
        },
    )?;
    let error = controller
        .extension_call(
            "setup",
            "create",
            CallOptions {
                environment: "dev".to_owned(),
                input: json!({"name":"two"}),
                key: Some("key-1".to_owned()),
            },
        )
        .err()
        .context("same key unexpectedly succeeded")?;
    assert!(error.to_string().contains("different operation"));
    Ok(())
}

#[test]
fn mutating_call_with_pending_receipt_reinvokes_with_previous() -> Result<()> {
    let root = tempfile::tempdir()?;
    let executor = std::sync::Arc::new(MockExecutor::default());
    executor
        .responses
        .lock()
        .map_err(lock_error)?
        .extend([pending_response("key-1"), response("key-1")]);
    let controller = Controller::with_executor(
        root.path().to_path_buf(),
        integration_manifest(false),
        executor.clone(),
    )?;
    let options = CallOptions {
        environment: "dev".to_owned(),
        input: json!({"name":"one"}),
        key: Some("key-1".to_owned()),
    };
    controller.extension_call("setup", "create", options.clone())?;
    controller.extension_call("setup", "create", options)?;
    let calls = executor.calls.lock().map_err(lock_error)?;
    assert_eq!(calls.len(), 2);
    let request: workenv_protocol::AdapterRequest =
        serde_json::from_slice(calls[1].stdin.as_ref().context("missing stdin")?)?;
    assert_eq!(
        request.previous.context("missing previous")?["status"],
        "pending"
    );
    Ok(())
}

#[test]
fn extension_call_uses_configured_binding_config() -> Result<()> {
    let root = tempfile::tempdir()?;
    let executor = std::sync::Arc::new(MockExecutor::default());
    executor
        .responses
        .lock()
        .map_err(lock_error)?
        .push(response("read-1"));
    let mut manifest = integration_manifest(false);
    manifest
        .environments
        .get_mut("dev")
        .context("missing environment")?
        .integrations[0]
        .config = json!({"profile":"workenv-dev"});
    let controller =
        Controller::with_executor(root.path().to_path_buf(), manifest, executor.clone())?;
    controller.extension_call(
        "setup",
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
    assert_eq!(request.config, json!({"profile":"workenv-dev"}));
    Ok(())
}

#[test]
fn extension_call_rejects_ambiguous_configured_bindings() -> Result<()> {
    let root = tempfile::tempdir()?;
    let executor = std::sync::Arc::new(MockExecutor::default());
    let mut manifest = manifest(false);
    let environment = manifest
        .environments
        .get_mut("dev")
        .context("missing environment")?;
    environment.integrations.push(Binding {
        extension: "setup".to_owned(),
        config: json!({"profile":"second"}),
    });
    let controller = Controller::with_executor(root.path().to_path_buf(), manifest, executor)?;
    let error = controller
        .extension_call(
            "setup",
            "status",
            CallOptions {
                environment: "dev".to_owned(),
                input: Value::Null,
                key: Some("read-1".to_owned()),
            },
        )
        .err()
        .context("ambiguous binding unexpectedly succeeded")?;
    assert!(error.to_string().contains("ambiguous bindings"));
    Ok(())
}
