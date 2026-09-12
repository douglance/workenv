//! What destroy requires of a create receipt before it tears anything down.
//!
//! A sibling of `environment_tests.rs` only because that file reached this
//! repository's 300-line limit. These cover `Environment::destroy`'s ownership
//! gate and the create fingerprint, both of which were mutable with the whole
//! suite green.
use super::controller::Controller;
use super::test_support::*;
use anyhow::{Context as _, Result};
use serde_json::json;

/// A create that really records an unowned resource, so destroy reaches the
/// ownership gate instead of failing before it.
///
/// `destroy_rejects_unowned_provider` above records no receipt at all, so it is
/// refused earlier with "has no recorded resource creation" and never exercises
/// `owned != true`. Verified by mutation: both halves of that gate could be
/// weakened with the whole suite green.
fn adopted_create(request_id: &str) -> workenv_protocol::AdapterResponse {
    workenv_protocol::AdapterResponse {
        data: json!({"owned": false, "resource_id": "vm-1"}),
        ..response(request_id)
    }
}

#[test]
fn destroy_refuses_a_resource_this_environment_only_adopted() -> Result<()> {
    let root = tempfile::tempdir()?;
    let executor = std::sync::Arc::new(MockExecutor::default());
    executor
        .responses
        .lock()
        .map_err(lock_error)?
        .push(adopted_create("create-1:dev:create:0:setup"));
    let controller = Controller::with_executor(
        root.path().to_path_buf(),
        provider_manifest(true),
        executor.clone(),
    )?;
    controller.environment("create", "dev", Some("create-1"))?;
    let error = controller
        .environment("destroy", "dev", Some("destroy-1"))
        .err()
        .context("destroy accepted an adopted resource")?;
    assert!(
        error.to_string().contains("adopted"),
        "unexpected error: {error}"
    );
    // The adapter must never have been asked to destroy it.
    assert_eq!(executor.calls.lock().map_err(lock_error)?.len(), 1);
    Ok(())
}

#[test]
fn destroy_refuses_a_receipt_that_names_no_resource() -> Result<()> {
    // The other half of the same gate: `owned: true` with no `resource_id` would
    // ask the adapter to destroy a resource the controller cannot name.
    let root = tempfile::tempdir()?;
    let executor = std::sync::Arc::new(MockExecutor::default());
    executor
        .responses
        .lock()
        .map_err(lock_error)?
        .push(workenv_protocol::AdapterResponse {
            data: json!({"owned": true}),
            ..response("create-1:dev:create:0:setup")
        });
    let controller = Controller::with_executor(
        root.path().to_path_buf(),
        provider_manifest(true),
        executor.clone(),
    )?;
    controller.environment("create", "dev", Some("create-1"))?;
    let error = controller
        .environment("destroy", "dev", Some("destroy-1"))
        .err()
        .context("destroy accepted a receipt with no resource id")?;
    assert!(
        error.to_string().contains("owned resource"),
        "unexpected error: {error}"
    );
    assert_eq!(executor.calls.lock().map_err(lock_error)?.len(), 1);
    Ok(())
}

#[test]
fn destroy_refuses_a_receipt_that_never_claimed_ownership() -> Result<()> {
    // `owned` absent entirely, which is what an older or third-party adapter
    // produces. Written because `!= true` and `== false` are indistinguishable on
    // a real boolean: only a missing field separates them, and `== false` would
    // let this receipt through.
    let root = tempfile::tempdir()?;
    let executor = std::sync::Arc::new(MockExecutor::default());
    executor
        .responses
        .lock()
        .map_err(lock_error)?
        .push(workenv_protocol::AdapterResponse {
            data: json!({"resource_id": "vm-1"}),
            ..response("create-1:dev:create:0:setup")
        });
    let controller = Controller::with_executor(
        root.path().to_path_buf(),
        provider_manifest(true),
        executor.clone(),
    )?;
    controller.environment("create", "dev", Some("create-1"))?;
    let error = controller
        .environment("destroy", "dev", Some("destroy-1"))
        .err()
        .context("destroy accepted a receipt that never claimed ownership")?;
    assert!(
        error.to_string().contains("adopted"),
        "unexpected error: {error}"
    );
    assert_eq!(executor.calls.lock().map_err(lock_error)?.len(), 1);
    Ok(())
}

#[test]
fn rebuilding_the_adapter_does_not_strand_the_resource() -> Result<()> {
    // The create fingerprint included the adapter's /nix/store path, and editing
    // any .rs file in this workspace changes it. Destroy requires the recorded
    // fingerprint to match, so one source edit made teardown impossible and left
    // the claimed resource alive until its lease expired.
    let root = tempfile::tempdir()?;
    let executor = std::sync::Arc::new(MockExecutor::default());
    executor.responses.lock().map_err(lock_error)?.extend([
        owned_create("create-1:dev:create:0:setup"),
        response("destroy-1:dev:destroy:0:setup"),
    ]);
    let controller = Controller::with_executor(
        root.path().to_path_buf(),
        provider_manifest(true),
        executor.clone(),
    )?;
    controller.environment("create", "dev", Some("create-1"))?;

    // Same contract, rebuilt binary: only the store path differs.
    let mut rebuilt = provider_manifest(true);
    if let Some(extension) = rebuilt.extensions.get_mut("setup") {
        extension.executable = std::path::PathBuf::from("/nix/store/rebuilt-setup/bin/setup");
    }
    let after_rebuild =
        Controller::with_executor(root.path().to_path_buf(), rebuilt, executor.clone())?;
    let value = after_rebuild.environment("destroy", "dev", Some("destroy-1"))?;
    assert_eq!(value["ok"], true);
    Ok(())
}

#[test]
fn changing_the_adapter_contract_still_invalidates_the_receipt() -> Result<()> {
    // The guard the fix must not remove: a different declared contract is a
    // different operation, and its receipt must not be reused.
    let root = tempfile::tempdir()?;
    let executor = std::sync::Arc::new(MockExecutor::default());
    executor
        .responses
        .lock()
        .map_err(lock_error)?
        .push(owned_create("create-1:dev:create:0:setup"));
    let controller = Controller::with_executor(
        root.path().to_path_buf(),
        provider_manifest(true),
        executor.clone(),
    )?;
    controller.environment("create", "dev", Some("create-1"))?;

    let mut changed = provider_manifest(true);
    if let Some(extension) = changed.extensions.get_mut("setup") {
        extension.version = "9.9.9".to_owned();
    }
    let after_change =
        Controller::with_executor(root.path().to_path_buf(), changed, executor.clone())?;
    let error = after_change
        .environment("destroy", "dev", Some("destroy-1"))
        .err()
        .context("a changed contract reused the old receipt")?;
    assert!(
        error.to_string().contains("fingerprint") || error.to_string().contains("no recorded"),
        "unexpected error: {error}"
    );
    Ok(())
}

fn owned_create(request_id: &str) -> workenv_protocol::AdapterResponse {
    workenv_protocol::AdapterResponse {
        data: json!({"owned": true, "resource_id": "vm-1"}),
        ..response(request_id)
    }
}
