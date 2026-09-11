//! Running a capacity backend.
//!
//! A backend is any executable that answers four verbs and writes one JSON
//! object to stdout. The pool knows nothing about Lima, exe.dev, or any future
//! machine: it knows how to run a program and read its answer. Adding capacity
//! is writing a script, not editing this crate.
use serde_json::Value;
use sha2::{Digest, Sha256};
use workenv_platform::{ApocExecutor, ExecutionOutput, ExecutionSpec, Executor};

/// Result of one backend command.
pub(super) type BackendResult<T> = std::result::Result<T, String>;

/// One declared capacity site and the command that serves it.
#[derive(Clone, Debug)]
pub(super) struct Site {
    /// Stable name recorded in receipts so teardown finds its way home.
    pub(super) name: String,
    /// argv of the program answering the four verbs.
    pub(super) run: Vec<String>,
}

/// Runs one backend verb.
pub(super) trait SiteRunner {
    /// Invoke `site` with `args`, returning its JSON object.
    fn run(&mut self, site: &Site, args: &[String]) -> BackendResult<Value>;
}

/// Runs backend commands as local processes, supervised by the `APoC` executor.
pub(super) struct ProcessRunner {
    timeout_ms: u64,
    request_id: String,
}

impl ProcessRunner {
    /// Bind a runner to one controller request.
    pub(super) fn new(request_id: String, timeout_ms: u64) -> Self {
        Self {
            timeout_ms,
            request_id,
        }
    }
}

impl SiteRunner for ProcessRunner {
    fn run(&mut self, site: &Site, args: &[String]) -> BackendResult<Value> {
        let Some((executable, leading)) = site.run.split_first() else {
            return Err(format!("site {} declares an empty command", site.name));
        };
        let mut command: Vec<String> = leading.to_vec();
        command.extend(args.iter().cloned());
        let root = std::env::current_dir().map_err(|error| error.to_string())?;
        let output = ApocExecutor::new(root.clone())
            .execute(ExecutionSpec {
                executable: executable.clone(),
                arg: command.clone(),
                cwd: Some(root),
                stdin: None,
                timeout_ms: self.timeout_ms,
                idempotency_key: key(&self.request_id, &site.name, &command),
                purpose: format!(
                    "Run capacity backend {} through the APoC executor.",
                    site.name
                ),
            })
            .map_err(|error| error.to_string())?;
        parse(&output)
    }
}

/// Bind the execution identity to the controller request and the exact argv.
///
/// Keying on argv alone would let a replayed execution answer a different
/// operation from cache: two teardowns of the same slot share an argv, so the
/// second would return the first's success without the backend ever seeing it.
fn key(request_id: &str, site: &str, argv: &[String]) -> String {
    let body = serde_json::to_vec(argv).unwrap_or_default();
    format!(
        "workenv-pool:v1:{:x}:{site}:{:x}",
        Sha256::digest(request_id.as_bytes()),
        Sha256::digest(body)
    )
}

/// Accept one JSON object; treat a non-null `error` as failure even on exit 0.
fn parse(output: &ExecutionOutput) -> BackendResult<Value> {
    let value: Value = serde_json::from_str(&output.stdout)
        .map_err(|_| message(&output.stderr, "backend returned no valid JSON"))?;
    if output.exit_code == Some(0) && value.get("error").is_none_or(Value::is_null) {
        return Ok(value);
    }
    Err(value
        .get("error")
        .and_then(Value::as_str)
        .unwrap_or("backend command failed")
        .to_owned())
}

fn message(stderr: &str, fallback: &str) -> String {
    let text = stderr.trim();
    if text.is_empty() {
        fallback.into()
    } else {
        format!("{fallback}: {text}")
    }
}
