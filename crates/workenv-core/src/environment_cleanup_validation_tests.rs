use super::controller::Controller;
use super::test_support::*;
use anyhow::{Context as _, Result};
use serde_json::json;
use workenv_protocol::{Binding, Location};

#[test]
fn destroy_rejects_cleanup_that_would_run_on_target() -> Result<()> {
    let root = tempfile::tempdir()?;
    let executor = std::sync::Arc::new(MockExecutor::default());
    let mut created = response("create");
    created.data = json!({"owned":true,"resource_id":"vm-1"});
    executor
        .responses
        .lock()
        .map_err(lock_error)?
        .extend([created, response("destroy")]);
    let controller = Controller::with_executor(
        root.path().into(),
        provider_with_target_cleanup_manifest(),
        executor.clone(),
    )?;
    controller.environment("create", "dev", Some("create-1"))?;

    let error = controller
        .environment("destroy", "dev", Some("destroy-1"))
        .err()
        .context("target cleanup unexpectedly succeeded")?;
    assert!(error.to_string().contains("must run on the controller"));
    assert_eq!(executor.calls.lock().map_err(lock_error)?.len(), 2);
    Ok(())
}

#[test]
fn destroy_rejects_a_cleanup_that_is_not_mutating() -> Result<()> {
    // The sibling location guard one test up IS covered; this one was not. A
    // non-mutating cleanup runs outside the receipt machinery entirely, so it gets
    // no idempotency key and no replay protection -- it can be re-run, and its
    // outcome is never recorded. Verified by mutation: deleting this guard left all
    // the cleanup tests green.
    let root = tempfile::tempdir()?;
    let executor = std::sync::Arc::new(MockExecutor::default());
    let mut created = response("create");
    created.data = json!({"owned":true,"resource_id":"vm-1"});
    executor
        .responses
        .lock()
        .map_err(lock_error)?
        .extend([created, response("destroy")]);
    let controller = Controller::with_executor(
        root.path().into(),
        provider_with_observing_cleanup_manifest(),
        executor.clone(),
    )?;
    controller.environment("create", "dev", Some("create-1"))?;

    let error = controller
        .environment("destroy", "dev", Some("destroy-1"))
        .err()
        .context("a non-mutating cleanup unexpectedly succeeded")?;
    assert!(
        error.to_string().contains("must be mutating"),
        "unexpected error: {error}"
    );
    Ok(())
}

#[test]
fn internal_operations_are_not_mined_for_cleanup_receipts() -> Result<()> {
    // `setup_operations` filters out internal operations. Without that filter the
    // transport's own `execute` receipts are collected and handed to cleanup as
    // though they described a resource this integration created.
    let manifest = provider_with_internal_operation_manifest();
    let binding = Binding {
        extension: "cleanup".to_owned(),
        config: json!({}),
    };
    let root = tempfile::tempdir()?;
    let controller = Controller::with_executor(
        root.path().into(),
        manifest,
        std::sync::Arc::new(MockExecutor::default()),
    )?;
    let operations = super::environment_cleanup::setup_operations(&controller, &binding)?;
    assert!(
        operations.contains(&"register".to_owned()),
        "the ordinary mutating operation was dropped: {operations:?}"
    );
    assert!(
        !operations.contains(&"execute".to_owned()),
        "an internal operation was collected as setup state: {operations:?}"
    );
    Ok(())
}

fn provider_with_observing_cleanup_manifest() -> workenv_protocol::Manifest {
    let mut manifest = provider_with_target_cleanup_manifest();
    if let Some(cleanup) = manifest.extensions.get_mut("cleanup") {
        cleanup.location = Location::Controller;
        cleanup.operations.clear();
        // Mutating = false is the whole point of this fixture.
        cleanup
            .operations
            .insert("cleanup".to_owned(), operation(false, json!({})));
    }
    manifest
}

fn provider_with_internal_operation_manifest() -> workenv_protocol::Manifest {
    let mut manifest = provider_with_target_cleanup_manifest();
    if let Some(cleanup) = manifest.extensions.get_mut("cleanup") {
        cleanup.location = Location::Controller;
        cleanup.operations.clear();
        cleanup
            .operations
            .insert("cleanup".to_owned(), operation(true, json!({})));
        cleanup
            .operations
            .insert("register".to_owned(), operation(true, json!({})));
        let mut internal = operation(true, json!({}));
        internal.internal = true;
        cleanup.operations.insert("execute".to_owned(), internal);
    }
    manifest
}

fn provider_with_target_cleanup_manifest() -> workenv_protocol::Manifest {
    let mut environment = environment(true);
    environment.integrations = vec![Binding {
        extension: "cleanup".to_owned(),
        config: json!({}),
    }];
    let mut cleanup = setup_extension(Location::Target);
    cleanup.operations.clear();
    cleanup
        .operations
        .insert("cleanup".to_owned(), operation(true, json!({})));
    workenv_protocol::Manifest {
        schema_version: workenv_protocol::PROTOCOL_VERSION,
        hosts: [("local".to_owned(), cleanup_host())].into(),
        environments: [("dev".to_owned(), environment)].into(),
        extensions: [
            ("provider".to_owned(), setup_extension(Location::Controller)),
            ("cleanup".to_owned(), cleanup),
        ]
        .into(),
    }
}

fn cleanup_host() -> workenv_protocol::Host {
    workenv_protocol::Host {
        provider: Some(Binding {
            extension: "provider".to_owned(),
            config: json!({}),
        }),
        ..host_without_provider(None)
    }
}
