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
