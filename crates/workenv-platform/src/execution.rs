//! APoC-backed exact-argv execution.
use std::{
    fs,
    io::Write as _,
    path::{Path, PathBuf},
};

use anyhow::{Context as _, Result, bail};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

use std::time::{Duration, Instant};

use crate::execution_code::{observe_execution, run_execution};

const STDIN_HELPER: &str = r#"input_file=$1; shift; exec "$@" < "$input_file""#;

/// One exact command execution request.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExecutionSpec {
    /// Executable path or name.
    pub executable: String,
    /// Exact arguments passed to the executable.
    #[serde(default)]
    pub arg: Vec<String>,
    /// Working directory for the command.
    pub cwd: Option<PathBuf>,
    /// Bytes supplied to standard input.
    pub stdin: Option<Vec<u8>>,
    /// Wall-clock timeout in milliseconds.
    pub timeout_ms: u64,
    /// Stable mutation identity for `APoC`.
    pub idempotency_key: String,
    /// Human-readable execution purpose.
    pub purpose: String,
}

/// Captured result of a bounded execution.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExecutionOutput {
    /// Captured standard output.
    pub stdout: String,
    /// Captured standard error.
    pub stderr: String,
    /// Process exit code, if `APoC` reached a terminal process result.
    pub exit_code: Option<i32>,
    /// Durable `APoC` execution identifier.
    pub execution_id: String,
}

/// Host execution interface.
pub trait Executor: Send + Sync {
    /// Execute one exact command.
    ///
    /// # Errors
    /// Returns an error when the host execution layer cannot accept, observe, or
    /// read the execution.
    fn execute(&self, spec: ExecutionSpec) -> Result<ExecutionOutput>;

    /// Observe an already accepted execution without starting it again.
    ///
    /// # Errors
    /// Returns an error when the host execution layer cannot observe or read the
    /// retained execution.
    fn observe(
        &self,
        execution_id: &str,
        purpose: &str,
        _timeout_ms: u64,
    ) -> Result<ExecutionOutput> {
        bail!("execution observation is not supported for {execution_id}: {purpose}");
    }
}

/// Executor that uses the local `APoC` daemon through Code Mode.
#[derive(Clone, Debug)]
pub struct ApocExecutor {
    root: PathBuf,
}

impl ApocExecutor {
    /// Create an executor rooted at the controller checkout.
    #[must_use]
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    /// Keep observing an accepted execution until the caller's budget is spent.
    ///
    /// Returns the last observation either way: a still-running command is not
    /// an error here, and the caller distinguishes the two by `exit_code`.
    fn observe_until(
        &self,
        started: &ExecutionOutput,
        spec: &ExecutionSpec,
    ) -> Result<ExecutionOutput> {
        let deadline = Instant::now() + Duration::from_millis(spec.timeout_ms);
        let mut latest = started.clone();
        while latest.exit_code.is_none() && Instant::now() < deadline {
            latest = self.observe_once(started, spec, deadline)?;
        }
        Ok(latest)
    }

    /// One observation, bounded by whatever remains of the caller's budget.
    fn observe_once(
        &self,
        started: &ExecutionOutput,
        spec: &ExecutionSpec,
        deadline: Instant,
    ) -> Result<ExecutionOutput> {
        let remaining = deadline.saturating_duration_since(Instant::now());
        observe_execution(
            &self.root,
            &started.execution_id,
            &spec.purpose,
            u64::try_from(remaining.as_millis()).unwrap_or(u64::MAX),
        )
    }
}

impl Executor for ApocExecutor {
    fn execute(&self, spec: ExecutionSpec) -> Result<ExecutionOutput> {
        let staged = stage_if_needed(&self.root, &spec)?;
        let mut command = staged.command_spec();
        command.executable = resolve_executable(&command.executable)?;
        let mut output = run_execution(&self.root, &command)?;
        if output.execution_id.is_empty() {
            bail!("APoC execution returned no durable ID");
        }
        // The first wait is capped at 30s inside the code-mode runner, so a
        // command that legitimately takes longer comes back with no exit code
        // and reads to the caller as a failure with empty stderr. Measured: a
        // devenv manifest with a real host set evaluates in 55s, so every fleet
        // worth having tripped this. Keep observing the execution APoC already
        // accepted until the caller's own budget is spent.
        if output.exit_code.is_none() {
            output = self.observe_until(&output, &spec)?;
        }
        if staged.is_some() && output.exit_code.is_some() {
            staged.cleanup();
        }
        Ok(output)
    }

    fn observe(
        &self,
        execution_id: &str,
        purpose: &str,
        timeout_ms: u64,
    ) -> Result<ExecutionOutput> {
        let output = observe_execution(&self.root, execution_id, purpose, timeout_ms)?;
        if output.execution_id.is_empty() {
            bail!("APoC observation returned no durable ID");
        }
        Ok(output)
    }
}

struct PreparedCommand {
    spec: ExecutionSpec,
    stdin_path: Option<PathBuf>,
}

impl PreparedCommand {
    fn command_spec(&self) -> ExecutionSpec {
        let Some(path) = &self.stdin_path else {
            return self.spec.clone();
        };
        let mut arg = vec![
            "-c".to_owned(),
            STDIN_HELPER.to_owned(),
            "workenv-stdin".to_owned(),
            path.to_string_lossy().into_owned(),
            self.spec.executable.clone(),
        ];
        arg.extend(self.spec.arg.clone());
        ExecutionSpec {
            executable: "sh".to_owned(),
            arg,
            stdin: None,
            ..self.spec.clone()
        }
    }

    fn cleanup(&self) {
        if let Some(path) = &self.stdin_path {
            let _ignored = fs::remove_file(path);
        }
    }

    const fn is_some(&self) -> bool {
        self.stdin_path.is_some()
    }
}

fn stage_if_needed(root: &Path, spec: &ExecutionSpec) -> Result<PreparedCommand> {
    let stdin_path = match &spec.stdin {
        Some(stdin) => Some(stage_stdin(root, &spec.idempotency_key, stdin)?),
        None => None,
    };
    Ok(PreparedCommand {
        spec: spec.clone(),
        stdin_path,
    })
}

fn stage_stdin(root: &Path, key: &str, stdin: &[u8]) -> Result<PathBuf> {
    let dir = root.join(".state/workenv-platform/stdin");
    fs::create_dir_all(&dir)?;
    let path = dir.join(format!("{:x}", Sha256::digest(key.as_bytes())));
    match private_new_file(&path) {
        Ok(mut file) => {
            file.write_all(stdin)?;
            file.sync_all()?;
            Ok(path)
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            ensure_existing_stdin(&path, stdin)?;
            Ok(path)
        }
        Err(error) => Err(error).with_context(|| format!("stage stdin {}", path.display())),
    }
}

fn ensure_existing_stdin(path: &Path, stdin: &[u8]) -> Result<()> {
    if fs::read(path)? == stdin {
        return Ok(());
    }
    bail!("idempotency key is already bound to different standard input");
}

fn private_new_file(path: &Path) -> std::io::Result<fs::File> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    options.open(path)
}

fn resolve_executable(executable: &str) -> Result<String> {
    let path = Path::new(executable);
    if path.is_absolute() || executable.contains(std::path::MAIN_SEPARATOR) {
        return Ok(executable.to_owned());
    }
    let path_var = std::env::var_os("PATH").context("PATH is required to resolve executable")?;
    for dir in std::env::split_paths(&path_var) {
        let candidate = dir.join(executable);
        if candidate.is_file() {
            return Ok(candidate.to_string_lossy().into_owned());
        }
    }
    bail!("executable {executable} was not found in PATH");
}

#[cfg(test)]
#[path = "execution_tests.rs"]
mod tests;
