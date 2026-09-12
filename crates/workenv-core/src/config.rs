use anyhow::{Context, Result, bail};
use serde_json::Value;
use std::path::Path;
use uuid::Uuid;
use workenv_platform::{ExecutionSpec, Executor};
use workenv_protocol::Manifest;

/// How long a manifest evaluation is given before it is reported as unfinished.
const MANIFEST_TIMEOUT_MS: u64 = 300_000;

pub(crate) fn load(root: &Path, executor: &dyn Executor) -> Result<Manifest> {
    // 68 s on a warm tree, once per command. Reused when nothing that feeds the
    // evaluation has changed; see manifest_cache for what that covers and why a
    // cached entry is checked against disk before it is trusted.
    if let Some(cached) = crate::manifest_cache::read(root) {
        return Ok(cached);
    }
    let output = executor.execute(ExecutionSpec {
        executable: "devenv".into(),
        arg: vec!["eval".into(), "workenv.manifestJSON".into()],
        cwd: Some(root.to_path_buf()),
        stdin: None,
        timeout_ms: MANIFEST_TIMEOUT_MS,
        idempotency_key: format!("workenv-manifest:{}", Uuid::new_v4()),
        purpose: "Evaluate the Workenv manifest through devenv.".into(),
    })?;
    // A missing exit code is not a failure, and saying so saves the reader a long
    // detour. `!= Some(0)` treated "still running" as "failed", so a cold manifest
    // evaluation that outran the 300 s budget died on every command with
    // "did not complete successfully; execution <id>: " and nothing after the
    // colon -- no stderr, because there is none yet.
    if output.exit_code.is_none() {
        bail!(
            "devenv manifest evaluation is still running after {}ms; \
             inspect execution {} and retry once it finishes",
            MANIFEST_TIMEOUT_MS,
            output.execution_id
        );
    }
    if output.exit_code != Some(0) {
        bail!(
            "devenv manifest evaluation failed with exit code {}; execution {}: {}",
            output.exit_code.unwrap_or_default(),
            output.execution_id,
            output.stderr
        );
    }
    let manifest = parse_manifest_output(&output.stdout)?;
    // Cache the inner manifest JSON, not devenv's wrapper around it, so a
    // cached entry is parsed by exactly the same code as a fresh one.
    crate::manifest_cache::write(root, &manifest_source(&output.stdout)?);
    Ok(manifest)
}

/// The manifest JSON that devenv nests as a string inside its own output.
fn manifest_source(stdout: &str) -> Result<String> {
    let value: Value = serde_json::from_str(stdout).context("parse devenv manifest output")?;
    value["workenv.manifestJSON"]
        .as_str()
        .map(ToOwned::to_owned)
        .context("devenv output omitted workenv.manifestJSON")
}

pub(crate) fn parse_manifest_output(stdout: &str) -> Result<Manifest> {
    let source = manifest_source(stdout)?;
    let source = source.as_str();
    serde_json::from_str(source).context("parse workenv manifest")
}

#[cfg(test)]
mod tests {
    use super::*;
    use workenv_platform::{ExecutionOutput, ExecutionSpec};

    /// An executor that answers once with exactly this output.
    struct OneAnswer(ExecutionOutput);

    impl Executor for OneAnswer {
        fn execute(&self, _spec: ExecutionSpec) -> Result<ExecutionOutput> {
            Ok(self.0.clone())
        }
    }

    fn output(exit_code: Option<i32>, stderr: &str) -> ExecutionOutput {
        ExecutionOutput {
            stdout: String::new(),
            stderr: stderr.to_owned(),
            exit_code,
            execution_id: "exec-1".to_owned(),
        }
    }

    #[test]
    fn an_unfinished_evaluation_is_not_reported_as_a_failure() -> Result<()> {
        // `exit_code != Some(0)` treated "still running" as "failed", so a cold
        // evaluation that outran the timeout died on every command with
        // "did not complete successfully; execution exec-1: " -- nothing after the
        // colon, because a running execution has no stderr yet.
        let temp = tempfile::tempdir()?;
        let Err(error) = load(temp.path(), &OneAnswer(output(None, ""))) else {
            bail!("an unfinished evaluation was accepted");
        };
        let message = error.to_string();
        assert!(
            message.contains("still running"),
            "unfinished evaluation misreported: {message}"
        );
        assert!(message.contains("exec-1"), "no execution id: {message}");
        Ok(())
    }

    #[test]
    fn a_real_failure_still_reports_its_code_and_stderr() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let Err(error) = load(
            temp.path(),
            &OneAnswer(output(Some(1), "attribute missing")),
        ) else {
            bail!("a failing evaluation was accepted");
        };
        let message = error.to_string();
        assert!(message.contains("exit code 1"), "no exit code: {message}");
        assert!(
            message.contains("attribute missing"),
            "stderr dropped: {message}"
        );
        Ok(())
    }
}
