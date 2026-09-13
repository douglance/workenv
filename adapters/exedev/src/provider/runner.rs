use serde_json::Value;
use sha2::{Digest, Sha256};
use uuid::Uuid;
use workenv_platform::{ApocExecutor, ExecutionSpec, Executor};

pub(super) trait Runner {
    fn observe(&mut self, args: &[String]) -> ProviderResult<Value>;

    fn mutate(&mut self, request_id: &str, args: &[String]) -> ProviderResult<Value>;

    /// The relay's stdout, unparsed.
    ///
    /// `observe` exists for the exe.dev CLI's own `--json` output and fails when
    /// the stream is not a single JSON document. The transport's stdout is not:
    /// it carries the relay's "Tip:" banner around a sentinel line, so the
    /// transport has to select its result out of the stream rather than parse
    /// the whole of it. A longer timeout too, because this one carries the
    /// caller's command rather than a control-plane call.
    fn observe_raw(&mut self, args: &[String], timeout_ms: u64) -> ProviderResult<String>;
}

pub(super) type ProviderResult<T> = std::result::Result<T, String>;

pub(super) struct SshRunner<E = ApocExecutor> {
    executor: E,
    cwd: std::path::PathBuf,
}

impl SshRunner<ApocExecutor> {
    pub(super) fn new(cwd: std::path::PathBuf) -> Self {
        Self {
            executor: ApocExecutor::new(cwd.clone()),
            cwd,
        }
    }
}

impl<E> SshRunner<E> {
    #[cfg(test)]
    pub(super) fn with_executor(executor: E, cwd: std::path::PathBuf) -> Self {
        Self { executor, cwd }
    }
}

impl<E: Executor> Runner for SshRunner<E> {
    fn observe(&mut self, args: &[String]) -> ProviderResult<Value> {
        self.run(
            args,
            format!("workenv-exedev:v2:observe:{}", Uuid::new_v4()),
        )
    }

    fn mutate(&mut self, request_id: &str, args: &[String]) -> ProviderResult<Value> {
        self.run(args, mutation_key(request_id, args))
    }

    fn observe_raw(&mut self, args: &[String], timeout_ms: u64) -> ProviderResult<String> {
        self.run_raw(
            args,
            format!("workenv-exedev:v2:exec:{}", Uuid::new_v4()),
            timeout_ms,
        )
    }
}

impl<E: Executor> SshRunner<E> {
    /// Run through the relay and hand back stdout exactly as it arrived.
    ///
    /// The relay's own exit code is not consulted. Measured, it does not carry
    /// the child's -- so a transport that read it would report every command
    /// failure as the same status. Whether the command ran at all is decided by
    /// the presence of the sentinel line, which is the caller's job.
    fn run_raw(
        &self,
        args: &[String],
        idempotency_key: String,
        timeout_ms: u64,
    ) -> ProviderResult<String> {
        self.executor
            .execute(ExecutionSpec {
                executable: "ssh".into(),
                arg: ssh_args(args),
                cwd: Some(self.cwd.clone()),
                stdin: None,
                timeout_ms,
                idempotency_key,
                purpose: "Run one command inside an exe.dev VM through the relay.".into(),
            })
            .map(|output| output.stdout)
            .map_err(|error| error.to_string())
    }

    fn run(&self, args: &[String], idempotency_key: String) -> ProviderResult<Value> {
        let output = self
            .executor
            .execute(ExecutionSpec {
                executable: "ssh".into(),
                arg: ssh_args(args),
                cwd: Some(self.cwd.clone()),
                stdin: None,
                timeout_ms: 180_000,
                idempotency_key,
                purpose: "Run exe.dev provider command through APoC.".into(),
            })
            .map_err(|error| error.to_string())?;
        parse_output(&output)
    }
}

fn parse_output(output: &workenv_platform::ExecutionOutput) -> ProviderResult<Value> {
    let value: Value = serde_json::from_str(&output.stdout)
        .map_err(|_| stderr_message(&output.stderr, "provider returned no valid JSON"))?;
    if output.exit_code == Some(0) && value.get("error").is_none_or(Value::is_null) {
        return Ok(value);
    }
    Err(value
        .get("error")
        .and_then(Value::as_str)
        .unwrap_or("provider command failed")
        .to_owned())
}

fn ssh_args(args: &[String]) -> Vec<String> {
    let mut out = vec![
        "-o".into(),
        "BatchMode=yes".into(),
        "-o".into(),
        "ConnectTimeout=15".into(),
        "exe.dev".into(),
    ];
    out.extend(args.iter().cloned());
    out
}

fn mutation_key(request_id: &str, args: &[String]) -> String {
    let body = serde_json::json!({"request_id":request_id,"args":args});
    let bytes = serde_json::to_vec(&body).unwrap_or_else(|_| Vec::new());
    format!("workenv-exedev:v2:mutate:{:x}", Sha256::digest(bytes))
}

fn stderr_message(stderr: &str, fallback: &str) -> String {
    let text = stderr.trim().to_owned();
    if text.is_empty() {
        fallback.into()
    } else {
        format!("{fallback}: {text}")
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use anyhow::{Result, anyhow};
    use workenv_platform::{ExecutionOutput, ExecutionSpec};

    use super::*;

    #[derive(Clone, Default)]
    struct RecordingExecutor {
        specs: Arc<Mutex<Vec<ExecutionSpec>>>,
    }

    impl Executor for RecordingExecutor {
        fn execute(&self, spec: ExecutionSpec) -> Result<ExecutionOutput> {
            self.specs
                .lock()
                .map_err(|_| anyhow!("recording executor lock is poisoned"))?
                .push(spec);
            Ok(ExecutionOutput {
                stdout: "{}".into(),
                stderr: String::new(),
                exit_code: Some(0),
                execution_id: "execution-id".into(),
            })
        }
    }

    #[test]
    fn observation_keys_are_fresh_and_mutation_keys_are_request_scoped() -> Result<()> {
        let executor = RecordingExecutor::default();
        let specs = executor.specs.clone();
        let mut runner = SshRunner::with_executor(executor, std::env::current_dir()?);
        let ls = vec!["ls".into(), "--json".into()];
        let new = vec!["new".into(), "--name".into(), "workenv-01".into()];

        runner.observe(&ls).map_err(anyhow::Error::msg)?;
        runner.observe(&ls).map_err(anyhow::Error::msg)?;
        runner
            .mutate("create-1", &new)
            .map_err(anyhow::Error::msg)?;
        runner
            .mutate("create-1", &new)
            .map_err(anyhow::Error::msg)?;
        runner
            .mutate("create-2", &new)
            .map_err(anyhow::Error::msg)?;

        let calls = specs
            .lock()
            .map_err(|_| anyhow!("recording executor lock is poisoned"))?
            .clone();
        assert_ne!(calls[0].idempotency_key, calls[1].idempotency_key);
        assert_eq!(calls[2].idempotency_key, calls[3].idempotency_key);
        assert_ne!(calls[2].idempotency_key, calls[4].idempotency_key);
        Ok(())
    }
}
