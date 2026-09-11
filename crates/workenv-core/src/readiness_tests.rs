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
