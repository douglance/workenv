use std::{fs, path::Path, process::Command, sync::Mutex};

use anyhow::{Result, bail};
use serde_json::json;
use tempfile::TempDir;
use workenv_platform::{ExecutionOutput, ExecutionSpec, Executor};
use workenv_protocol::{AdapterRequest, PROTOCOL_VERSION, ResponseStatus, Target};

use super::*;

#[test]
fn clone_from_bundle_preserves_origin_without_persisted_rewrite() -> Result<()> {
    let origin = local_repo()?;
    let target = tempfile::tempdir()?;
    let checkout = target.path().join("checkout");
    let bundle = target.path().join("origin.bundle");
    git(
        origin.path(),
        ["bundle", "create", bundle.to_str().unwrap_or(""), "HEAD"],
    )?;
    let repository = "git@github.com:example/private.git";
    let runner = RealGit::new();

    let response = handle_with(
        &request(
            repository,
            &checkout,
            Some(bundle.to_string_lossy().as_ref()),
        ),
        &runner,
    )?;

    assert_eq!(response.status, ResponseStatus::Changed);
    assert_eq!(response.data["origin"], repository);
    assert_eq!(fs::read_to_string(checkout.join("README.md"))?, "bundle\n");
    assert_eq!(
        git_stdout(&checkout, ["config", "--get", "remote.origin.url"])?,
        repository
    );
    assert!(git_stdout(&checkout, ["config", "--local", "--get-regexp", "^url\\."]).is_err());

    fs::write(checkout.join("README.md"), "dirty\n")?;
    let response = handle_with(
        &request(
            repository,
            &checkout,
            Some(bundle.to_string_lossy().as_ref()),
        ),
        &runner,
    )?;

    assert_eq!(response.status, ResponseStatus::Ready);
    assert_eq!(fs::read_to_string(checkout.join("README.md"))?, "dirty\n");
    Ok(())
}

struct RealGit {
    calls: Mutex<usize>,
}

impl RealGit {
    const fn new() -> Self {
        Self {
            calls: Mutex::new(0),
        }
    }
}

impl Executor for RealGit {
    fn execute(&self, spec: ExecutionSpec) -> Result<ExecutionOutput> {
        let mut calls = self.calls.lock().map_err(|_| anyhow::anyhow!("lock"))?;
        *calls += 1;
        let output = Command::new(&spec.executable)
            .args(&spec.arg)
            .current_dir(spec.cwd.as_deref().unwrap_or_else(|| Path::new(".")))
            .output()?;
        Ok(ExecutionOutput {
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            exit_code: output.status.code(),
            execution_id: format!("git-{calls}"),
        })
    }
}

fn local_repo() -> Result<TempDir> {
    let dir = tempfile::tempdir()?;
    git(dir.path(), ["init"])?;
    git(dir.path(), ["config", "user.email", "test@example.com"])?;
    git(dir.path(), ["config", "user.name", "Test User"])?;
    fs::write(dir.path().join("README.md"), "bundle\n")?;
    git(dir.path(), ["add", "README.md"])?;
    git(dir.path(), ["commit", "-m", "one"])?;
    Ok(dir)
}

fn git<const N: usize>(cwd: &Path, args: [&str; N]) -> Result<()> {
    let status = Command::new("git").args(args).current_dir(cwd).status()?;
    if status.success() {
        return Ok(());
    }
    bail!("git fixture command failed")
}

fn git_stdout<const N: usize>(cwd: &Path, args: [&str; N]) -> Result<String> {
    let output = Command::new("git").args(args).current_dir(cwd).output()?;
    if !output.status.success() {
        bail!("git fixture command failed")
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

fn request(repository: &str, directory: &Path, clone_from: Option<&str>) -> AdapterRequest {
    let mut config = json!({"repository": repository});
    if let Some(clone_from) = clone_from {
        config["clone_from"] = json!(clone_from);
    }
    AdapterRequest {
        protocol_version: PROTOCOL_VERSION,
        request_id: format!("project-clone-from-test-{}", directory.display()),
        extension: "workenv.project".to_owned(),
        operation: "prepare".to_owned(),
        target: Target {
            environment: "test".to_owned(),
            host: "local".to_owned(),
            address: None,
            directory: directory.to_path_buf(),
            system: "x86_64-linux".to_owned(),
            source: ".".to_owned(),
            profiles: vec![],
        },
        config,
        input: json!({}),
        previous: None,
    }
}
