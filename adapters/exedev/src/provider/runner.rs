use serde_json::Value;
use sha2::{Digest, Sha256};
use workenv_platform::{ApocExecutor, ExecutionSpec, Executor};

pub(super) trait Runner {
    fn run(&mut self, args: &[String]) -> ProviderResult<Value>;
}

pub(super) type ProviderResult<T> = std::result::Result<T, String>;

#[derive(Default)]
pub(super) struct SshRunner;

impl Runner for SshRunner {
    fn run(&mut self, args: &[String]) -> ProviderResult<Value> {
        let output = ApocExecutor::new(std::env::current_dir().map_err(|e| e.to_string())?)
            .execute(ExecutionSpec {
                executable: "ssh".into(),
                arg: ssh_args(args),
                cwd: std::env::current_dir().ok(),
                stdin: None,
                timeout_ms: 180_000,
                idempotency_key: format!("workenv-exedev:v2:{}", digest_args(args)),
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

fn digest_args(args: &[String]) -> String {
    let body = serde_json::to_vec(args).unwrap_or_else(|_| Vec::new());
    format!("{:x}", Sha256::digest(body))
}

fn stderr_message(stderr: &str, fallback: &str) -> String {
    let text = stderr.trim().to_owned();
    if text.is_empty() {
        fallback.into()
    } else {
        format!("{fallback}: {text}")
    }
}
