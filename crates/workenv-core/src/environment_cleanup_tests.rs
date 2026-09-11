use super::controller::Controller;
use super::test_support::*;
use anyhow::{Context as _, Result};
use serde_json::json;
use std::time::Duration;
use workenv_protocol::{Binding, Location, ResponseStatus};
#[test]
fn destroy_runs_integration_cleanup_after_provider_destroy() -> Result<()> {
    let root = tempfile::tempdir()?;
    let executor = std::sync::Arc::new(MockExecutor::default());
    let mut created = response("create");
    created.data = json!({"owned":true,"resource_id":"vm-1","provider_id":"provider-1"});
    let mut old = response("register");
    old.data = json!({"enrollment_id":"old-integration"});
    let mut registered = response("register");
    registered.data = json!({"enrollment_id":"integration-1"});
    executor.responses.lock().map_err(lock_error)?.extend([
        old,
        created,
        registered,
        response("destroy"),
        response("cleanup"),
    ]);
    let controller = Controller::with_executor(
        root.path().into(),
        provider_with_cleanup_manifest(Location::Controller),
        executor.clone(),
    )?;
    controller.setup_binding(&cleanup_binding(), "register", "dev", Some("old-register"))?;
    pause_for_receipt_order();
    controller.environment("create", "dev", Some("create-1"))?;
    pause_for_receipt_order();
    controller.setup_binding(&cleanup_binding(), "register", "dev", Some("register-1"))?;
    let value = controller.environment("destroy", "dev", Some("destroy-1"))?;
    assert_eq!(value["ok"], true);
    assert_eq!(value["results"][0]["extension"], "provider");
    assert_eq!(value["results"][1]["extension"], "cleanup");
    let calls = executor.calls.lock().map_err(lock_error)?;
    let request = request_from_call(&calls[4])?;
    assert_register_cleanup_request(&request);
    Ok(())
}
fn assert_register_cleanup_request(request: &workenv_protocol::AdapterRequest) {
    assert_eq!(request.operation, "cleanup");
    assert_eq!(
        request
            .previous
            .as_ref()
            .and_then(|value| value["data"]["enrollment_id"].as_str()),
        Some("integration-1")
    );
    assert_eq!(
        request.input["provider_create"]["data"]["provider_id"],
        "provider-1"
    );
    assert_eq!(request.input["provider_destroy"]["status"], "changed");
    assert_eq!(
        request.input["integration_receipts"][0]["operation"],
        "register"
    );
    assert_eq!(
        request.input["integration_receipts"][0]["response"]["data"]["enrollment_id"],
        "integration-1"
    );
    assert_eq!(request.input["receipt"]["enrollment_id"], "integration-1");
}
#[test]
fn destroy_runs_target_integration_cleanup_on_controller() -> Result<()> {
    let root = tempfile::tempdir()?;
    let executor = std::sync::Arc::new(MockExecutor::default());
    let mut created = response("create");
    created.data = json!({"owned":true,"resource_id":"vm-1"});
    let mut destroyed = response("destroy");
    destroyed.status = ResponseStatus::Ready;
    destroyed.data = json!({"destroyed":true});
    executor.responses.lock().map_err(lock_error)?.extend([
        created,
        destroyed,
        response("cleanup"),
    ]);
    let controller = Controller::with_executor(
        root.path().into(),
        provider_with_cleanup_manifest(Location::Target),
        executor.clone(),
    )?;
    controller.environment("create", "dev", Some("create-1"))?;
    controller.environment("destroy", "dev", Some("destroy-1"))?;
    let calls = executor.calls.lock().map_err(lock_error)?;
    let request = request_from_call(&calls[2])?;
    assert_eq!(request.operation, "cleanup");
    assert_eq!(calls[2].arg, ["shell", "--", "/nix/store/bin/target-tool"]);
    Ok(())
}
#[test]
fn destroy_skips_cleanup_while_provider_destroy_is_pending() -> Result<()> {
    let root = tempfile::tempdir()?;
    let executor = std::sync::Arc::new(MockExecutor::default());
    let mut created = response("create");
    created.data = json!({"owned":true,"resource_id":"vm-1"});
    executor
        .responses
        .lock()
        .map_err(lock_error)?
        .extend([created, pending_response("destroy")]);
    let controller = Controller::with_executor(
        root.path().into(),
        provider_with_cleanup_manifest(Location::Controller),
        executor.clone(),
    )?;
    controller.environment("create", "dev", Some("create-1"))?;
    let value = controller.environment("destroy", "dev", Some("destroy-1"))?;
    assert_eq!(value["ok"], false);
    assert_eq!(value["status"], "pending");
    assert_eq!(executor.calls.lock().map_err(lock_error)?.len(), 2);
    Ok(())
}
#[test]
fn destroy_retries_failed_cleanup_without_reinvoking_provider() -> Result<()> {
    let root = tempfile::tempdir()?;
    let executor = std::sync::Arc::new(MockExecutor::default());
    let mut created = response("create");
    created.data = json!({"owned":true,"resource_id":"vm-1"});
    let mut registered = response("register");
    registered.data = json!({"enrollment_id":"integration-1"});
    let mut replacement = response("register");
    replacement.data = json!({"enrollment_id":"replacement-integration"});
    let mut failed = response("cleanup");
    failed.status = workenv_protocol::ResponseStatus::Failed;
    failed.error = Some("offline".to_owned());
    failed.data = json!({"enrollment_id":"integration-1"});
    executor.responses.lock().map_err(lock_error)?.extend([
        created,
        registered,
        response("destroy"),
        failed,
        replacement,
        response("cleanup"),
    ]);
    let controller = Controller::with_executor(
        root.path().into(),
        provider_with_cleanup_manifest(Location::Controller),
        executor.clone(),
    )?;
    controller.environment("create", "dev", Some("create-1"))?;
    pause_for_receipt_order();
    controller.setup_binding(&cleanup_binding(), "register", "dev", Some("register-1"))?;
    let first = controller.environment("destroy", "dev", Some("destroy-1"))?;
    pause_for_receipt_order();
    controller.setup_binding(&cleanup_binding(), "register", "dev", Some("register-2"))?;
    let second = controller.environment("destroy", "dev", Some("destroy-1"))?;
    assert_eq!(first["ok"], false);
    assert_eq!(second["ok"], true);
    let calls = executor.calls.lock().map_err(lock_error)?;
    assert_eq!(calls.len(), 6);
    let request = request_from_call(&calls[5])?;
    assert_eq!(request.operation, "cleanup");
    let error = request
        .previous
        .as_ref()
        .and_then(|value| value["error"].as_str());
    assert_eq!(error, Some("offline"));
    assert_eq!(request.input["receipt"]["enrollment_id"], "integration-1");
    Ok(())
}
#[test]
fn destroy_rejects_duplicate_cleanup_bindings_for_same_extension() -> Result<()> {
    let root = tempfile::tempdir()?;
    let executor = std::sync::Arc::new(MockExecutor::default());
    let mut created = response("create");
    created.data = json!({"owned":true,"resource_id":"vm-1"});
    executor
        .responses
        .lock()
        .map_err(lock_error)?
        .extend([created, response("destroy")]);
    let mut manifest = provider_with_cleanup_manifest(Location::Controller);
    manifest
        .environments
        .get_mut("dev")
        .context("dev environment")?
        .integrations
        .push(Binding {
            extension: "cleanup".to_owned(),
            config: json!({"scope":"second"}),
        });
    let controller = Controller::with_executor(root.path().into(), manifest, executor)?;
    controller.environment("create", "dev", Some("create-1"))?;
    let error = controller
        .environment("destroy", "dev", Some("destroy-1"))
        .err()
        .context("duplicate cleanup bindings unexpectedly succeeded")?;
    assert!(error.to_string().contains("multiple cleanup bindings"));
    Ok(())
}
#[test]
fn destroy_attempts_later_cleanup_when_earlier_cleanup_fails() -> Result<()> {
    let root = tempfile::tempdir()?;
    let executor = std::sync::Arc::new(MockExecutor::default());
    let mut created = response("create");
    created.data = json!({"owned":true,"resource_id":"vm-1"});
    let mut failed = response("cleanup");
    failed.status = ResponseStatus::Failed;
    executor.responses.lock().map_err(lock_error)?.extend([
        created,
        response("destroy"),
        failed,
        response("cleanup"),
    ]);
    let controller = Controller::with_executor(
        root.path().into(),
        provider_with_two_cleanup_integrations()?,
        executor.clone(),
    )?;
    controller.environment("create", "dev", Some("create-1"))?;
    let value = controller.environment("destroy", "dev", Some("destroy-1"))?;
    assert_eq!(value["ok"], false);
    assert_eq!(value["status"], "failed");
    assert_eq!(value["results"].as_array().map(Vec::len), Some(3));
    let calls = executor.calls.lock().map_err(lock_error)?;
    assert_eq!(calls.len(), 4);
    assert_eq!(request_from_call(&calls[3])?.extension, "cleanup2");
    Ok(())
}
fn provider_with_cleanup_manifest(cleanup_location: Location) -> workenv_protocol::Manifest {
    let mut environment = environment(true);
    environment.integrations = vec![Binding {
        extension: "cleanup".to_owned(),
        config: json!({"scope":"external"}),
    }];
    let mut provider = setup_extension(Location::Controller);
    provider.operations.remove("apply");
    provider.operations.remove("bootstrap");
    let mut cleanup = setup_extension(cleanup_location);
    cleanup.operations.clear();
    let mut register = operation(true, json!({}));
    register.location = Some(Location::Controller);
    cleanup.operations.insert("register".to_owned(), register);
    let mut cleanup_operation = operation(true, json!({"type":"object"}));
    cleanup_operation.location = Some(Location::Controller);
    cleanup
        .operations
        .insert("cleanup".to_owned(), cleanup_operation);
    workenv_protocol::Manifest {
        schema_version: workenv_protocol::PROTOCOL_VERSION,
        hosts: [("local".to_owned(), cleanup_host())].into(),
        environments: [("dev".to_owned(), environment)].into(),
        extensions: [
            ("provider".to_owned(), provider),
            ("cleanup".to_owned(), cleanup),
        ]
        .into(),
    }
}
fn cleanup_host() -> workenv_protocol::Host {
    workenv_protocol::Host {
        provider: Some(Binding {
            extension: "provider".to_owned(),
            config: json!({"provider":"disposable"}),
        }),
        ..host_without_provider(None)
    }
}
fn provider_with_two_cleanup_integrations() -> Result<workenv_protocol::Manifest> {
    let mut manifest = provider_with_cleanup_manifest(Location::Controller);
    let environment = manifest
        .environments
        .get_mut("dev")
        .context("dev environment")?;
    environment.integrations.push(Binding {
        extension: "cleanup2".to_owned(),
        config: json!({}),
    });
    let cleanup = manifest
        .extensions
        .get("cleanup")
        .context("cleanup extension")?
        .clone();
    manifest.extensions.insert("cleanup2".to_owned(), cleanup);
    Ok(manifest)
}
fn cleanup_binding() -> Binding {
    Binding {
        extension: "cleanup".to_owned(),
        config: json!({"scope":"external"}),
    }
}
fn pause_for_receipt_order() {
    std::thread::sleep(Duration::from_millis(10));
}
fn request_from_call(
    call: &workenv_platform::ExecutionSpec,
) -> Result<workenv_protocol::AdapterRequest> {
    let input = call.stdin.as_ref().context("missing stdin")?;
    serde_json::from_slice(input).context("decode adapter request")
}
