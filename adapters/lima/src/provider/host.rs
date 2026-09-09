//! Transport to the `workenv-vm` CLI on the Lima VM host.
use serde_json::Value;
use sha2::{Digest, Sha256};
use workenv_platform::{ApocExecutor, ExecutionOutput, ExecutionSpec, Executor};

/// Result of one VM-host command.
pub(super) type HostResult<T> = std::result::Result<T, String>;

/// Runs one host-CLI subcommand on the VM host and returns its JSON object.
pub(super) trait HostRunner {
    /// Invoke the host CLI with `args`.
    fn run(&mut self, args: &[String]) -> HostResult<Value>;
}

/// Reaches the VM host over SSH, supervised by the `APoC` executor.
pub(super) struct SshHostRunner {
    vm_host: String,
    command: String,
    timeout_ms: u64,
}

impl SshHostRunner {
    /// Build a runner for one VM host and host-CLI command name.
    pub(super) fn new(vm_host: String, command: String, timeout_ms: u64) -> Self {
        Self {
            vm_host,
            command,
            timeout_ms,
        }
    }

    fn argv(&self, args: &[String]) -> Vec<String> {
        let mut out = vec![
            "-o".into(),
            "BatchMode=yes".into(),
            "-o".into(),
            "ConnectTimeout=15".into(),
            "-o".into(),
            "StrictHostKeyChecking=yes".into(),
            self.vm_host.clone(),
            self.command.clone(),
        ];
        out.extend(args.iter().cloned());
        out
    }
}

impl HostRunner for SshHostRunner {
    fn run(&mut self, args: &[String]) -> HostResult<Value> {
        let root = std::env::current_dir().map_err(|error| error.to_string())?;
        let command = self.argv(args);
        let output = ApocExecutor::new(root.clone())
            .execute(ExecutionSpec {
                executable: "ssh".into(),
                arg: command.clone(),
                cwd: Some(root),
                stdin: None,
                timeout_ms: self.timeout_ms,
                idempotency_key: format!("workenv-lima:v1:{}", digest_args(&command)),
                purpose: "Run Lima VM host command through the APoC executor.".into(),
            })
            .map_err(|error| error.to_string())?;
        parse_output(&output)
    }
}

/// Accept one JSON object on stdout; treat a non-null `error` as failure.
fn parse_output(output: &ExecutionOutput) -> HostResult<Value> {
    let value: Value = serde_json::from_str(&output.stdout)
        .map_err(|_| stderr_message(&output.stderr, "VM host returned no valid JSON"))?;
    if output.exit_code == Some(0) && value.get("error").is_none_or(Value::is_null) {
        return Ok(value);
    }
    Err(value
        .get("error")
        .and_then(Value::as_str)
        .unwrap_or("VM host command failed")
        .to_owned())
}

fn digest_args(args: &[String]) -> String {
    let body = serde_json::to_vec(args).unwrap_or_default();
    format!("{:x}", Sha256::digest(body))
}

fn stderr_message(stderr: &str, fallback: &str) -> String {
    let text = stderr.trim();
    if text.is_empty() {
        fallback.into()
    } else {
        format!("{fallback}: {text}")
    }
}
