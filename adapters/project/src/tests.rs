use std::{fs, process::Command, sync::Mutex};

use anyhow::{Result, bail};
use serde_json::json;
use tempfile::TempDir;
use workenv_platform::{ExecutionOutput, ExecutionSpec, Executor};
use workenv_protocol::{AdapterRequest, PROTOCOL_VERSION, ResponseStatus, Target};

use super::*;

#[test]
fn prepare_clones_temp_local_git_repo() -> Result<()> {
    let origin = local_repo()?;
    let target = tempfile::tempdir()?;
    let checkout = target.path().join("checkout");
    let response = handle_with(&request(origin.path(), &checkout, None), &RealGit::new())?;

    assert_eq!(response.status, ResponseStatus::Changed);
    assert_eq!(response.data["status"], "project_prepared");
    assert_eq!(fs::read_to_string(checkout.join("README.md"))?, "one\n");
    assert!(
        response.data["commit"]
            .as_str()
            .is_some_and(|v| v.len() == 40)
    );
    Ok(())
}

#[test]
fn existing_dirty_checkout_is_ready_without_reset() -> Result<()> {
    let origin = local_repo()?;
    let target = tempfile::tempdir()?;
    let checkout = target.path().join("checkout");
    let runner = RealGit::new();
    handle_with(&request(origin.path(), &checkout, None), &runner)?;
    fs::write(checkout.join("README.md"), "dirty\n")?;

    let response = handle_with(&request(origin.path(), &checkout, None), &runner)?;

    assert_eq!(response.status, ResponseStatus::Ready);
    assert_eq!(fs::read_to_string(checkout.join("README.md"))?, "dirty\n");
    Ok(())
}

#[test]
fn nonempty_non_git_directory_fails_without_overwrite() -> Result<()> {
    let origin = local_repo()?;
    let target = tempfile::tempdir()?;
    let checkout = target.path().join("checkout");
    fs::create_dir_all(&checkout)?;
    fs::write(checkout.join("keep.txt"), "keep\n")?;

    let response = handle_with(&request(origin.path(), &checkout, None), &RealGit::new())?;

    assert_eq!(response.status, ResponseStatus::Failed);
    assert_eq!(response.data["status"], "project_directory_not_checkout");
    assert_eq!(fs::read_to_string(checkout.join("keep.txt"))?, "keep\n");
    Ok(())
}

#[test]
fn mismatched_origin_fails_actionably() -> Result<()> {
    let origin = local_repo()?;
    let other = local_repo()?;
    let target = tempfile::tempdir()?;
    let checkout = target.path().join("checkout");
    let runner = RealGit::new();
    handle_with(&request(origin.path(), &checkout, None), &runner)?;

    let response = handle_with(&request(other.path(), &checkout, None), &runner)?;

    assert_eq!(response.status, ResponseStatus::Failed);
    assert_eq!(response.data["status"], "project_origin_mismatch");
    assert_eq!(
        response.data["actual_origin"],
        origin.path().to_string_lossy().as_ref()
    );
    Ok(())
}

#[test]
fn ref_selection_and_failure_are_reported() -> Result<()> {
    let origin = local_repo()?;
    git(origin.path(), ["tag", "v1"])?;
    fs::write(origin.path().join("README.md"), "two\n")?;
    git(origin.path(), ["add", "README.md"])?;
    git(origin.path(), ["commit", "-m", "two"])?;
    let target = tempfile::tempdir()?;
    let checkout = target.path().join("tagged");

    let response = handle_with(
        &request(origin.path(), &checkout, Some("v1")),
        &RealGit::new(),
    )?;
    assert_eq!(response.status, ResponseStatus::Changed);
    assert_eq!(fs::read_to_string(checkout.join("README.md"))?, "one\n");

    let bad = target.path().join("bad-ref");
    let response = handle_with(
        &request(origin.path(), &bad, Some("missing-ref")),
        &RealGit::new(),
    )?;
    assert_eq!(response.status, ResponseStatus::Failed);
    assert_eq!(response.data["status"], "project_ref_not_found");
    Ok(())
}

#[test]
fn rejects_credentials_and_git_option_injection() -> Result<()> {
    let target = tempfile::tempdir()?;
    let credential = request("https://token@example.com/repo.git", target.path(), None);
    let option = request("--upload-pack=bad", target.path(), None);

    assert!(handle_with(&credential, &RealGit::new()).is_err());
    assert!(handle_with(&option, &RealGit::new()).is_err());
    Ok(())
}

#[test]
fn pending_clone_returns_execution_id() -> Result<()> {
    let target = tempfile::tempdir()?;
    let response = handle_with(
        &request("https://example.com/repo.git", target.path(), None),
        &PendingGit,
    )?;

    assert_eq!(response.status, ResponseStatus::Pending);
    assert_eq!(response.execution_id.as_deref(), Some("pending-clone"));
    assert_eq!(response.data["status"], "project_clone_pending");
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
            .current_dir(
                spec.cwd
                    .as_deref()
                    .unwrap_or_else(|| std::path::Path::new(".")),
            )
            .output()?;
        Ok(ExecutionOutput {
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            exit_code: output.status.code(),
            execution_id: format!("git-{calls}"),
        })
    }
}

struct PendingGit;

impl Executor for PendingGit {
    fn execute(&self, spec: ExecutionSpec) -> Result<ExecutionOutput> {
        if spec.arg.first().is_some_and(|arg| arg == "clone") {
            return Ok(ExecutionOutput {
                stdout: String::new(),
                stderr: String::new(),
                exit_code: None,
                execution_id: "pending-clone".to_owned(),
            });
        }
        bail!("unexpected command")
    }
}

fn local_repo() -> Result<TempDir> {
    let dir = tempfile::tempdir()?;
    git(dir.path(), ["init"])?;
    git(dir.path(), ["config", "user.email", "test@example.com"])?;
    git(dir.path(), ["config", "user.name", "Test User"])?;
    fs::write(dir.path().join("README.md"), "one\n")?;
    git(dir.path(), ["add", "README.md"])?;
    git(dir.path(), ["commit", "-m", "one"])?;
    Ok(dir)
}

fn git<const N: usize>(cwd: &std::path::Path, args: [&str; N]) -> Result<()> {
    let status = Command::new("git").args(args).current_dir(cwd).status()?;
    if status.success() {
        return Ok(());
    }
    bail!("git fixture command failed")
}

fn request(
    repository: impl AsRef<std::path::Path>,
    directory: &std::path::Path,
    reference: Option<&str>,
) -> AdapterRequest {
    let mut config = json!({"repository":repository.as_ref().to_string_lossy()});
    if let Some(reference) = reference {
        config["ref"] = json!(reference);
    }
    AdapterRequest {
        protocol_version: PROTOCOL_VERSION,
        request_id: format!("project-test-{}", directory.display()),
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
