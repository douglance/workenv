use crate::{Controller, test_support::*};
use anyhow::Result;
use std::sync::Arc;

#[test]
fn installed_devenv_does_not_prove_environment_was_applied() -> Result<()> {
    let root = tempfile::tempdir()?;
    let executor = Arc::new(MockExecutor::default());
    executor
        .responses
        .lock()
        .map_err(lock_error)?
        .push(raw_response("devenv 2.3.0"));
    let controller =
        Controller::with_executor(root.path().into(), provider_manifest(false), executor)?;
    let result = controller.environment("status", "dev", None)?;
    assert_eq!(result["status"], "pending");
    assert_eq!(
        result["results"][0]["response"]["data"]["reason"],
        "environment_not_applied"
    );
    Ok(())
}

#[test]
fn status_compares_current_derivation_with_applied_configuration() -> Result<()> {
    for (current, expected) in [("original", "ready"), ("changed", "pending")] {
        let root = tempfile::tempdir()?;
        let executor = Arc::new(MockExecutor::default());
        executor.responses.lock().map_err(lock_error)?.extend([
            raw_response("ready"),
            raw_response("/nix/store/profile"),
            raw_response("{\"shell.drvPath\":\"/nix/store/original.drv\"}"),
            raw_response("devenv 2.3.0"),
            raw_response(&format!(
                "{{\"shell.drvPath\":\"/nix/store/{current}.drv\"}}"
            )),
            raw_response(""),
        ]);
        let controller =
            Controller::with_executor(root.path().into(), provider_manifest(false), executor)?;
        controller.environment("apply", "dev", Some("apply"))?;
        let result = controller.environment("status", "dev", None)?;
        assert_eq!(result["status"], expected);
    }
    Ok(())
}

#[test]
fn recording_the_same_applied_state_twice_reports_no_change() -> Result<()> {
    // `record_applied` had no test at all. Inverting its comparison makes `apply`
    // never converge: a real change stops being recorded, so `status` reports
    // environment_configuration_changed forever, while an unchanged state is
    // rewritten and reported as changed.
    let root = tempfile::tempdir()?;
    let controller = Controller::with_executor(
        root.path().into(),
        provider_manifest(false),
        Arc::new(MockExecutor::default()),
    )?;
    let state = serde_json::json!({"shellDerivation": "/nix/store/a.drv"});
    assert!(
        controller.record_applied("dev", &state)?,
        "the first recording must report a change"
    );
    assert!(
        !controller.record_applied("dev", &state)?,
        "recording the identical state again reported a change"
    );
    let changed = serde_json::json!({"shellDerivation": "/nix/store/b.drv"});
    assert!(
        controller.record_applied("dev", &changed)?,
        "a genuinely changed state reported no change"
    );
    Ok(())
}
