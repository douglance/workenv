use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
    sync::Mutex,
};

use anyhow::{Result, bail};
use serde_json::json;
use tempfile::TempDir;
use workenv_platform::{ExecutionOutput, ExecutionSpec, Executor};
use workenv_protocol::{AdapterRequest, PROTOCOL_VERSION, ResponseStatus, Target};

use super::*;

#[test]
fn pending_clone_completion_continues_to_requested_ref() -> Result<()> {
    let origin = local_repo_with_tag()?;
    let target = tempfile::tempdir()?;
    let checkout = target.path().join("checkout");
    let mut request = request(origin.path(), &checkout, Some("v1"));
    request.previous = Some(pending_previous(
        &request,
        "project_clone_pending",
        "clone-exec",
    ));
    let runner = CompletingGit::clone_ready(origin.path(), &checkout);

    let response = handle_with(&request, &runner)?;

    assert_eq!(response.status, ResponseStatus::Changed);
    assert_eq!(response.data["status"], "project_prepared");
    assert_eq!(fs::read_to_string(checkout.join("README.md"))?, "one\n");
    assert_eq!(runner.observed()?, vec!["clone-exec"]);
    assert!(!runner.executed_args()?.iter().any(|arg| arg[0] == "clone"));
    assert!(
        runner
            .executed_args()?
            .iter()
            .any(|arg| arg[0] == "checkout")
    );
    Ok(())
}

#[test]
fn pending_clone_is_observed_without_reclone_or_filesystem_probe() -> Result<()> {
    let target = tempfile::tempdir()?;
    let checkout = target.path().join("checkout");
    fs::create_dir_all(checkout.join(".git"))?;
    let mut request = request("https://example.com/repo.git", &checkout, Some("feature"));
    request.previous = Some(pending_previous(
        &request,
        "project_clone_pending",
        "clone-exec",
    ));
    let runner = StillPendingGit;

    let response = handle_with(&request, &runner)?;

    assert_eq!(response.status, ResponseStatus::Pending);
    assert_eq!(response.execution_id.as_deref(), Some("clone-exec"));
    assert_eq!(response.data["status"], "project_clone_pending");
    Ok(())
}

#[test]
fn pending_checkout_completion_is_verified_without_starting_checkout() -> Result<()> {
    let origin = local_repo_with_tag()?;
    let target = tempfile::tempdir()?;
    let checkout = target.path().join("checkout");
    let origin_path = origin.path().to_string_lossy();
    git(target.path(), ["clone", "--", &origin_path, "checkout"])?;
    let mut request = request(origin.path(), &checkout, Some("v1"));
    request.previous = Some(pending_previous(
        &request,
        "project_checkout_pending",
        "checkout-exec",
    ));
    let runner = CompletingGit::checkout_ready(&checkout, "v1");

    let response = handle_with(&request, &runner)?;

    assert_eq!(response.status, ResponseStatus::Changed);
    assert_eq!(response.data["status"], "project_prepared");
    assert_eq!(fs::read_to_string(checkout.join("README.md"))?, "one\n");
    assert_eq!(runner.observed()?, vec!["checkout-exec"]);
    let executed = runner.executed_args()?;
    assert!(!executed.iter().any(|arg| arg[0] == "checkout"));
    assert!(
        executed
            .iter()
            .any(|arg| arg == &["rev-parse", "v1^{commit}"])
    );
    Ok(())
}

struct CompletingGit {
    completion: Completion,
    executed: Mutex<Vec<Vec<String>>>,
    observed: Mutex<Vec<String>>,
}

enum Completion {
    Clone(PathBuf, PathBuf),
    Checkout(PathBuf, String),
}

impl CompletingGit {
    fn clone_ready(origin: &Path, checkout: &Path) -> Self {
        Self {
            completion: Completion::Clone(origin.to_path_buf(), checkout.to_path_buf()),
            executed: Mutex::new(Vec::new()),
            observed: Mutex::new(Vec::new()),
        }
    }

    fn checkout_ready(checkout: &Path, reference: &str) -> Self {
        Self {
            completion: Completion::Checkout(checkout.to_path_buf(), reference.to_owned()),
            executed: Mutex::new(Vec::new()),
            observed: Mutex::new(Vec::new()),
        }
    }

    fn observed(&self) -> Result<Vec<String>> {
        let observed = self.observed.lock().map_err(|_| anyhow::anyhow!("lock"))?;
        Ok(observed.clone())
    }

    fn executed_args(&self) -> Result<Vec<Vec<String>>> {
        let executed = self.executed.lock().map_err(|_| anyhow::anyhow!("lock"))?;
        Ok(executed.clone())
    }
}

impl Executor for CompletingGit {
    fn execute(&self, spec: ExecutionSpec) -> Result<ExecutionOutput> {
        self.executed
            .lock()
            .map_err(|_| anyhow::anyhow!("lock"))?
            .push(spec.arg.clone());
        real_git(spec)
    }

    fn observe(
        &self,
        execution_id: &str,
        _purpose: &str,
        _timeout_ms: u64,
    ) -> Result<ExecutionOutput> {
        self.observed
            .lock()
            .map_err(|_| anyhow::anyhow!("lock"))?
            .push(execution_id.to_owned());
        match &self.completion {
            Completion::Clone(origin, checkout) => command_output(
                Command::new("git")
                    .arg("clone")
                    .arg("--")
                    .arg(origin)
                    .arg(checkout),
                execution_id,
            ),
            Completion::Checkout(checkout, reference) => command_output(
                Command::new("git")
                    .arg("checkout")
                    .arg("--detach")
                    .arg(reference)
                    .current_dir(checkout),
                execution_id,
            ),
        }
    }
}

struct StillPendingGit;

impl Executor for StillPendingGit {
    fn execute(&self, _spec: ExecutionSpec) -> Result<ExecutionOutput> {
        bail!("pending retry unexpectedly started or inspected git")
    }

    fn observe(
        &self,
        execution_id: &str,
        _purpose: &str,
        _timeout_ms: u64,
    ) -> Result<ExecutionOutput> {
        Ok(ExecutionOutput {
            stdout: String::new(),
            stderr: String::new(),
            exit_code: None,
            execution_id: execution_id.to_owned(),
        })
    }
}

fn real_git(spec: ExecutionSpec) -> Result<ExecutionOutput> {
    let mut command = Command::new(&spec.executable);
    command.args(&spec.arg);
    if let Some(cwd) = spec.cwd {
        command.current_dir(cwd);
    }
    command_output(&mut command, &spec.idempotency_key)
}

fn command_output(command: &mut Command, execution_id: &str) -> Result<ExecutionOutput> {
    let output = command.output()?;
    Ok(ExecutionOutput {
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        exit_code: output.status.code(),
        execution_id: execution_id.to_owned(),
    })
}

fn local_repo_with_tag() -> Result<TempDir> {
    let dir = tempfile::tempdir()?;
    git(dir.path(), ["init"])?;
    git(dir.path(), ["config", "user.email", "test@example.com"])?;
    git(dir.path(), ["config", "user.name", "Test User"])?;
    fs::write(dir.path().join("README.md"), "one\n")?;
    git(dir.path(), ["add", "README.md"])?;
    git(dir.path(), ["commit", "-m", "one"])?;
    git(dir.path(), ["tag", "v1"])?;
    fs::write(dir.path().join("README.md"), "two\n")?;
    git(dir.path(), ["add", "README.md"])?;
    git(dir.path(), ["commit", "-m", "two"])?;
    Ok(dir)
}

fn git<const N: usize>(cwd: &Path, args: [&str; N]) -> Result<()> {
    let status = Command::new("git").args(args).current_dir(cwd).status()?;
    if status.success() {
        return Ok(());
    }
    bail!("git fixture command failed")
}

fn pending_previous(
    request: &AdapterRequest,
    status: &str,
    execution_id: &str,
) -> serde_json::Value {
    json!({
        "protocol_version": PROTOCOL_VERSION,
        "request_id": request.request_id,
        "status": "pending",
        "data": {
            "ok": false,
            "status": status,
            "repository": request.config["repository"].clone(),
            "ref": request.config.get("ref").cloned().unwrap_or(serde_json::Value::Null),
            "path": request.target.directory.clone(),
            "execution_id": execution_id,
        },
        "error": null,
        "execution_id": execution_id,
    })
}

fn request(
    repository: impl AsRef<Path>,
    directory: &Path,
    reference: Option<&str>,
) -> AdapterRequest {
    let mut config = json!({"repository": repository.as_ref().to_string_lossy()});
    if let Some(reference) = reference {
        config["ref"] = json!(reference);
    }
    AdapterRequest {
        protocol_version: PROTOCOL_VERSION,
        request_id: format!("project-pending-test-{}", directory.display()),
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
