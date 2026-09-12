use super::*;
use std::{io::Write as _, sync::Mutex};

use anyhow::{Context as _, Result, bail};
use serde_json::json;
use sha2::{Digest as _, Sha256};
use workenv_platform::{ExecutionOutput, ExecutionSpec, Executor};

use crate::seed_stage::{SeedStage, remote_seed_stage_script, stage_controller_seed_tools_with};

#[derive(Default)]
struct RecordingExecutor {
    calls: Mutex<Vec<ExecutionSpec>>,
    responses: Mutex<Vec<ExecutionOutput>>,
}

impl RecordingExecutor {
    fn with_response(output: ExecutionOutput) -> Self {
        Self {
            calls: Mutex::default(),
            responses: Mutex::new(vec![output]),
        }
    }

    fn calls(&self) -> Result<Vec<ExecutionSpec>> {
        Ok(self.calls.lock().map_err(lock_error)?.clone())
    }
}

impl Executor for RecordingExecutor {
    fn execute(&self, spec: ExecutionSpec) -> Result<ExecutionOutput> {
        self.calls.lock().map_err(lock_error)?.push(spec);
        self.responses
            .lock()
            .map_err(lock_error)?
            .pop()
            .context("missing executor response")
    }
}

#[test]
fn controller_path_hash_mismatch_stops_before_executor() -> Result<()> {
    let root = temp_path("bootstrap-controller-seed-hash");
    fs::create_dir_all(&root)?;
    let seed = root.join("workenv-project");
    fs::write(&seed, b"actual")?;
    let request = remote_request(seed_tool_config(&seed, &sha256_hex(b"expected"))?)?;
    let config = BootstrapConfig::from_request(&request)?;
    let executor = RecordingExecutor::default();

    let Err(error) = stage_controller_seed_tools_with(&request, &config, &executor) else {
        bail!("hash mismatch should fail");
    };

    assert!(error.to_string().contains("sha256 mismatch"));
    assert!(executor.calls()?.is_empty());
    fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn local_controller_path_uses_verified_local_path_without_executor() -> Result<()> {
    let bytes = b"local bootstrap binary";
    let sha = sha256_hex(bytes);
    let root = temp_path("bootstrap-controller-seed-local");
    fs::create_dir_all(&root)?;
    let seed = root.join("workenv-project");
    fs::write(&seed, bytes)?;
    let mut request = request("bootstrap")?;
    request.config = seed_tool_config(&seed, &sha)?;
    let config = BootstrapConfig::from_request(&request)?;
    let executor = RecordingExecutor::default();

    let SeedStage::Ready(staged) = stage_controller_seed_tools_with(&request, &config, &executor)?
    else {
        bail!("expected staged config");
    };

    assert_eq!(staged.seed_tools[0].source, path_arg(&seed)?);
    assert!(executor.calls()?.is_empty());
    fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn remote_controller_path_upload_uses_stdin_and_safe_argv() -> Result<()> {
    let bytes = b"bootstrap binary bytes";
    let sha = sha256_hex(bytes);
    let root = temp_path("bootstrap-controller-seed-remote");
    fs::create_dir_all(&root)?;
    let seed = root.join("workenv-project");
    fs::write(&seed, bytes)?;
    let request = remote_request(seed_tool_config(&seed, &sha)?)?;
    let config = BootstrapConfig::from_request(&request)?;
    let target_path = format!("/home/test/.cache/workenv/seeds/{sha}");
    let executor = RecordingExecutor::with_response(output(Some(0), &target_path));

    let SeedStage::Ready(staged) = stage_controller_seed_tools_with(&request, &config, &executor)?
    else {
        bail!("expected staged config");
    };

    assert_eq!(staged.seed_tools[0].source, target_path);
    assert_safe_upload_spec(&executor.calls()?[0], bytes, &sha);
    fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn remote_controller_path_upload_accepts_uppercase_sha_and_normalizes_key() -> Result<()> {
    let bytes = b"bootstrap binary bytes with uppercase config hash";
    let sha = sha256_hex(bytes);
    let root = temp_path("bootstrap-controller-seed-uppercase");
    fs::create_dir_all(&root)?;
    let seed = root.join("workenv-project");
    fs::write(&seed, bytes)?;
    let request = remote_request(seed_tool_config(&seed, &sha.to_ascii_uppercase())?)?;
    let config = BootstrapConfig::from_request(&request)?;
    let target_path = format!("/home/test/.cache/workenv/seeds/{sha}");
    let executor = RecordingExecutor::with_response(output(Some(0), &target_path));

    let SeedStage::Ready(staged) = stage_controller_seed_tools_with(&request, &config, &executor)?
    else {
        bail!("expected staged config");
    };

    assert_eq!(config.seed_tools[0].sha256, sha);
    assert_eq!(staged.seed_tools[0].source, target_path);
    assert_eq!(staged.seed_tools[0].sha256, sha);
    assert_safe_upload_spec(&executor.calls()?[0], bytes, &sha);
    fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn pending_remote_seed_stage_returns_execution_id_with_stable_key() -> Result<()> {
    let bytes = b"pending bootstrap binary";
    let sha = sha256_hex(bytes);
    let root = temp_path("bootstrap-controller-seed-pending");
    fs::create_dir_all(&root)?;
    let seed = root.join("workenv-project");
    fs::write(&seed, bytes)?;
    let request = remote_request(seed_tool_config(&seed, &sha)?)?;
    let config = BootstrapConfig::from_request(&request)?;
    let executor = RecordingExecutor::with_response(output(None, ""));

    let SeedStage::Pending(response) =
        stage_controller_seed_tools_with(&request, &config, &executor)?
    else {
        bail!("expected pending response");
    };

    let spec = &executor.calls()?[0];
    assert_eq!(response.execution_id.as_deref(), Some("seed-stage-exec"));
    assert_eq!(
        spec.idempotency_key,
        format!("workenv-bootstrap:bootstrap-test:stage-seed-workenv-project:{sha}")
    );
    fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn target_seed_stage_script_validates_sha_before_atomic_rename() -> Result<()> {
    let bytes = b"target checked bytes";
    let sha = sha256_hex(bytes);
    let root = temp_path("bootstrap-target-seed-stage");
    fs::create_dir_all(&root)?;

    let output = Command::new("bash")
        .arg("-c")
        .arg(remote_seed_stage_script(&sha))
        .env("HOME", &root)
        .output_with_stdin(bytes)?;

    assert!(output.status.success());
    assert_eq!(
        fs::read(root.join(".cache/workenv/seeds").join(&sha))?,
        bytes
    );
    assert_eq!(
        String::from_utf8(output.stdout)?.trim(),
        staged_path(&root, &sha)
    );

    let failed = Command::new("bash")
        .arg("-c")
        .arg(remote_seed_stage_script(&sha))
        .env("HOME", &root)
        .output_with_stdin(b"wrong")?;
    assert_eq!(failed.status.code(), Some(3));
    fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn invalid_seed_identity_fails_config_parse() -> Result<()> {
    for name in [
        "../workenv-project",
        ".hidden",
        "-dash",
        "workenv'project",
        "workenv\nproject",
        "workenv%project",
        "workenv project",
    ] {
        let bad_name = seed_tool_json(name, "/tmp/seed", &sha256_hex(b"x"));
        assert!(
            BootstrapConfig::from_request(&remote_request(bad_name)?).is_err(),
            "expected {name:?} to be rejected"
        );
    }
    let bad_sha = seed_tool_json("workenv-project", "/tmp/seed", "not-a-sha");

    assert!(BootstrapConfig::from_request(&remote_request(bad_sha)?).is_err());
    Ok(())
}

trait CommandInputExt {
    fn output_with_stdin(&mut self, input: &[u8]) -> Result<std::process::Output>;
}

impl CommandInputExt for Command {
    fn output_with_stdin(&mut self, input: &[u8]) -> Result<std::process::Output> {
        let mut child = self
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()?;
        child
            .stdin
            .take()
            .context("missing child stdin")?
            .write_all(input)?;
        Ok(child.wait_with_output()?)
    }
}

fn seed_tool_config(path: &std::path::Path, sha: &str) -> Result<serde_json::Value> {
    Ok(seed_tool_json("workenv-project", path_arg(path)?, sha))
}

fn path_arg(path: &std::path::Path) -> Result<String> {
    path.to_str()
        .map(ToOwned::to_owned)
        .context("path is not UTF-8")
}

fn seed_tool_json(name: &str, controller_path: impl Into<String>, sha: &str) -> serde_json::Value {
    json!({"seed_tools":[{"name":name,"controller_path":controller_path.into(),"sha256":sha}]})
}

fn remote_request(config: serde_json::Value) -> Result<AdapterRequest> {
    let mut request = request("bootstrap")?;
    request.target.address = Some("workenv-test.example".to_owned());
    request.config = config;
    Ok(request)
}

fn output(exit_code: Option<i32>, stdout: &str) -> ExecutionOutput {
    ExecutionOutput {
        stdout: format!("{stdout}\n"),
        stderr: String::new(),
        exit_code,
        execution_id: "seed-stage-exec".to_owned(),
    }
}

fn assert_safe_upload_spec(spec: &ExecutionSpec, bytes: &[u8], sha: &str) {
    assert_eq!(spec.executable, "ssh");
    assert_eq!(spec.stdin.as_deref(), Some(bytes));
    assert!(spec.arg.iter().any(|arg| arg == "workenv-test.example"));
    assert!(
        spec.arg
            .iter()
            .all(|arg| !arg.contains("bootstrap binary bytes"))
    );
    assert!(spec.arg.iter().all(|arg| !arg.contains("controller_path")));
    assert!(spec.arg.iter().all(|arg| !arg.contains("/tmp/seed")));
    assert!(spec.idempotency_key.ends_with(sha));
}

fn sha256_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn staged_path(root: &std::path::Path, sha: &str) -> String {
    root.join(".cache/workenv/seeds")
        .join(sha)
        .to_string_lossy()
        .into_owned()
}

fn lock_error<T>(_: std::sync::PoisonError<T>) -> anyhow::Error {
    anyhow::anyhow!("lock poisoned")
}
