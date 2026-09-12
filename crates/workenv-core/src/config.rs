use anyhow::{Context, Result, bail};
use serde_json::Value;
use std::path::Path;
use uuid::Uuid;
use workenv_platform::{ExecutionSpec, Executor};
use workenv_protocol::Manifest;

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
