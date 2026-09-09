use anyhow::{Context, Result, bail};
use serde_json::Value;
use std::path::Path;
use uuid::Uuid;
use workenv_platform::{ExecutionSpec, Executor};
use workenv_protocol::Manifest;

pub(crate) fn load(root: &Path, executor: &dyn Executor) -> Result<Manifest> {
    let output = executor.execute(ExecutionSpec {
        executable: "devenv".into(),
        arg: vec!["eval".into(), "workenv.manifestJSON".into()],
        cwd: Some(root.to_path_buf()),
        stdin: None,
        timeout_ms: 300_000,
        idempotency_key: format!("workenv-manifest:{}", Uuid::new_v4()),
        purpose: "Evaluate the Workenv manifest through devenv.".into(),
    })?;
    if output.exit_code != Some(0) {
        bail!(
            "devenv manifest evaluation did not complete successfully; execution {}: {}",
            output.execution_id,
            output.stderr
        );
    }
    parse_manifest_output(&output.stdout)
}

pub(crate) fn parse_manifest_output(stdout: &str) -> Result<Manifest> {
    let value: Value = serde_json::from_str(stdout).context("parse devenv manifest output")?;
    let source = value["workenv.manifestJSON"]
        .as_str()
        .context("devenv output omitted workenv.manifestJSON")?;
    serde_json::from_str(source).context("parse workenv manifest")
}
