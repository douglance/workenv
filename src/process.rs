use anyhow::{bail, Context as _, Result};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{IsTerminal, Write},
    path::{Path, PathBuf},
    process::Command,
    sync::Arc,
    time::{Duration, Instant},
};
use uuid::Uuid;

static SERVER_ROOT: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();

pub fn set_server_root(path: PathBuf) -> Result<()> {
    let path = path
        .canonicalize()
        .context("MCP workenv root does not exist")?;
    SERVER_ROOT
        .set(path)
        .map_err(|_| anyhow::anyhow!("MCP workenv root was already configured"))
}

#[derive(Clone)]
pub struct Context {
    pub root: PathBuf,
    pub state: PathBuf,
    pub fleet: Value,
    pub runtime: Arc<dyn Runtime>,
}

#[derive(Clone, Debug)]
pub struct CommandSpec {
    pub executable: String,
    pub args: Vec<String>,
    pub cwd: Option<PathBuf>,
    pub stdin: Option<Vec<u8>>,
    pub timeout_ms: u64,
    pub key: String,
    pub purpose: String,
}

#[derive(Clone, Debug, Default)]
pub struct CommandOutput {
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub exit_code: Option<i32>,
    pub execution_id: String,
}

impl CommandOutput {
    pub fn success(&self) -> Result<()> {
        match self.exit_code {
            Some(0) => Ok(()),
            Some(code) => bail!("Execution {} exited {code}: {}", self.execution_id, String::from_utf8_lossy(&self.stderr)),
            None => bail!("Execution {} is still running or its result is unknown; inspect it before retrying", self.execution_id),
        }
    }
    pub fn text(&self) -> Result<String> {
        self.success()?;
        String::from_utf8(self.stdout.clone()).context("Command output was not UTF-8")
    }
    pub fn json(&self) -> Result<Value> {
        serde_json::from_str(&self.text()?).context("Command did not return valid JSON")
    }
}

pub trait Runtime: Send + Sync {
    fn run(&self, spec: CommandSpec) -> Result<CommandOutput>;
    fn apoc(&self, method: &str, args: Value) -> Result<Value>;
}

impl Context {
    pub fn load(root: Option<&str>) -> Result<Self> {
        let root = discover_root(root)?;
        let fleet = read_json(&root.join("fleet.json"))?;
        validate_fleet(&fleet)?;
        Ok(Self {
            state: root.join(".state/controller"),
            runtime: Arc::new(ApocRuntime { root: root.clone() }),
            root,
            fleet,
        })
    }
    pub fn worker_name(&self, selector: &str) -> Result<String> {
        let selector = selector.trim();
        let number = selector
            .strip_prefix("workenv-")
            .or_else(|| selector.strip_prefix("workenv"))
            .unwrap_or(selector);
        let name = if !number.is_empty() && number.bytes().all(|c| c.is_ascii_digit()) {
            format!(
                "workenv-{:02}",
                number.parse::<u16>().context("Invalid worker number")?
            )
        } else {
            selector.to_owned()
        };
        let workers = self.fleet["workers"]
            .as_array()
            .context("fleet.json requires workers")?;
        if workers
            .iter()
            .filter(|w| w["name"].as_str() == Some(&name))
            .count()
            != 1
        {
            bail!("Unknown worker {selector:?}; choose a worker from workenv list");
        }
        Ok(name)
    }
    pub fn worker(&self, selector: &str) -> Result<&Value> {
        let name = self.worker_name(selector)?;
        self.fleet["workers"]
            .as_array()
            .and_then(|ws| ws.iter().find(|w| w["name"] == name))
            .context("Worker disappeared from fleet")
    }
    pub fn run(
        &self,
        executable: &str,
        args: Vec<String>,
        key: &str,
        purpose: &str,
        timeout_ms: u64,
    ) -> Result<CommandOutput> {
        self.runtime.run(CommandSpec {
            executable: executable.into(),
            args,
            cwd: Some(self.root.clone()),
            stdin: None,
            timeout_ms,
            key: key.into(),
            purpose: purpose.into(),
        })
    }
    pub fn ssh(
        &self,
        worker: &str,
        mut argv: Vec<String>,
        key: &str,
        purpose: &str,
        timeout_ms: u64,
    ) -> Result<CommandOutput> {
        let name = self.worker_name(worker)?;
        let user = self.fleet["remote_user"].as_str().unwrap_or("exedev");
        let root = self.fleet["remote_root"]
            .as_str()
            .unwrap_or("/home/exedev/workenv");
        if matches!(argv.first().map(String::as_str), Some("apoc" | "herdr")) {
            let invocation = if argv.first().map(String::as_str) == Some("apoc")
                && argv.get(1).map(String::as_str) == Some("execution")
                && argv.get(2).map(String::as_str) == Some("start")
            {
                let delimiter = argv
                    .iter()
                    .position(|s| s == "--")
                    .context("APoC execution start requires an argv delimiter")?;
                format!(
                    "{} --env \"PATH=$PATH\" -- {}",
                    shell_join(&argv[..delimiter]),
                    shell_join(&argv[delimiter + 1..])
                )
            } else {
                shell_join(&argv)
            };
            argv = vec![
                "bash".into(),
                "-lc".into(),
                format!(
                    "cd {} && /usr/local/bin/devenv shell -- bash -c {}",
                    shell_quote(root),
                    shell_quote(&format!("exec {invocation}"))
                ),
            ];
        }
        self.run(
            "ssh",
            vec![
                "-o".into(),
                "BatchMode=yes".into(),
                "-o".into(),
                "ConnectTimeout=15".into(),
                "-o".into(),
                "StrictHostKeyChecking=yes".into(),
                format!("{user}@{name}.exe.xyz"),
                shell_join(&argv),
            ],
            key,
            purpose,
            timeout_ms,
        )
    }
    pub fn apoc(&self, method: &str, args: Value) -> Result<Value> {
        self.runtime.apoc(method, args)
    }
}

fn discover_root(explicit: Option<&str>) -> Result<PathBuf> {
    if let Some(root) = explicit
        .map(PathBuf::from)
        .or_else(|| SERVER_ROOT.get().cloned())
        .or_else(|| std::env::var_os("WORKENV_ROOT").map(PathBuf::from))
    {
        let root = root.canonicalize().context("Workenv root does not exist")?;
        if !root.join("fleet.json").is_file() {
            bail!("{} has no fleet.json", root.display());
        }
        return Ok(root);
    }
    let cwd = std::env::current_dir()?;
    if let Some(root) = cwd
        .ancestors()
        .find(|p| p.join("fleet.json").is_file() && p.join("bootstrap").is_dir())
    {
        return Ok(root.to_path_buf());
    }
    if let Some(home) = std::env::var_os("HOME") {
        let config = PathBuf::from(home).join(".config/workenv/config.json");
        if config.exists() {
            let value = read_json(&config)?;
            let root = value["root"]
                .as_str()
                .context("Workenv config requires root")?;
            return discover_root(Some(root));
        }
    }
    bail!("Workenv configuration not found. Use --root PATH or set WORKENV_ROOT to the folder containing fleet.json")
}

fn validate_fleet(fleet: &Value) -> Result<()> {
    let workers = fleet["workers"]
        .as_array()
        .context("fleet.json requires a workers array")?;
    let mut names = std::collections::HashSet::new();
    for worker in workers {
        let name = worker["name"].as_str().context("Worker requires a name")?;
        let suffix = name
            .strip_prefix("workenv-")
            .context("Worker names must use workenv-NN")?;
        if suffix.len() != 2 || !suffix.bytes().all(|c| c.is_ascii_digit()) || !names.insert(name) {
            bail!("Invalid or duplicate worker name {name:?}");
        }
        for field in ["cpus", "memory_gb", "disk_gb"] {
            if worker[field].as_u64().unwrap_or(0) == 0 {
                bail!("{name}.{field} must be a positive integer");
            }
        }
    }
    let user = fleet["remote_user"].as_str().unwrap_or("exedev");
    if user.is_empty()
        || !user
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-')
    {
        bail!("Invalid remote_user");
    }
    Ok(())
}

pub fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}
pub fn shell_join(argv: &[String]) -> String {
    argv.iter()
        .map(|s| shell_quote(s))
        .collect::<Vec<_>>()
        .join(" ")
}
pub fn read_json(path: &Path) -> Result<Value> {
    serde_json::from_slice(&fs::read(path).with_context(|| format!("Read {}", path.display()))?)
        .with_context(|| format!("Parse {}", path.display()))
}
pub fn write_json(path: &Path, value: &Value) -> Result<()> {
    let parent = path.parent().context("State path has no parent")?;
    fs::create_dir_all(parent)?;
    let temp = parent.join(format!(".{}.tmp", Uuid::new_v4()));
    let result = (|| {
        let mut opts = fs::OpenOptions::new();
        opts.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let mut file = opts.open(&temp)?;
        file.write_all(&serde_json::to_vec_pretty(value)?)?;
        file.sync_all()?;
        fs::rename(&temp, path)?;
        fs::File::open(parent)?.sync_all()?;
        Ok(())
    })();
    if temp.exists() {
        let _ = fs::remove_file(&temp);
    }
    result
}

pub struct ApocRuntime {
    pub root: PathBuf,
}

fn execution_exit_code(value: &Value) -> Option<i32> {
    value
        .pointer("/result/exit_code")
        .or_else(|| value.pointer("/execution/result/exit_code"))
        .or_else(|| value.pointer("/execution/exit_code"))
        .or_else(|| value.get("exit_code"))
        .and_then(Value::as_i64)
        .map(|code| code as i32)
        .or_else(|| {
            if value["outcome"] == "passed" {
                Some(0)
            } else {
                None
            }
        })
}

/// Replace the APoC-owned helper process with the exact requested program.
/// A private file supplies binary stdin without passing it through shell syntax.
pub fn exec_with_stdin(args: Vec<std::ffi::OsString>) -> Result<()> {
    if args.len() < 3 || args[1] != "--" {
        bail!("Invalid stdin execution helper invocation");
    }
    let input = fs::File::open(&args[0]).context("Open staged execution input")?;
    let mut command = Command::new(&args[2]);
    command.args(&args[3..]).stdin(input);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        Err(command.exec().into())
    }
    #[cfg(not(unix))]
    {
        let status = command.status()?;
        std::process::exit(status.code().unwrap_or(1));
    }
}

impl ApocRuntime {
    fn invoke(&self, args: &[String]) -> Result<Value> {
        let output = Command::new("apoc")
            .args(args)
            .current_dir(&self.root)
            .output()
            .context("Could not run apoc; install it and run apoc daemon status")?;
        let value: Value = serde_json::from_slice(&output.stdout).with_context(|| {
            format!(
                "APoC returned invalid JSON: {}",
                String::from_utf8_lossy(&output.stderr)
            )
        })?;
        if !output.status.success() {
            bail!("APoC call failed: {}", value.get("error").unwrap_or(&value));
        }
        Ok(value)
    }
    fn code(&self, code: &str, key: &str, purpose: &str) -> Result<Value> {
        let mut state = self.invoke(&[
            "code".into(),
            "run".into(),
            code.into(),
            "--timeout-ms".into(),
            "1000".into(),
            "--idempotency-key".into(),
            key.into(),
            "--purpose".into(),
            purpose.into(),
            "--filter-output".into(),
            "id,status,result,error".into(),
            "--format".into(),
            "json".into(),
        ])?;
        let start = Instant::now();
        loop {
            match state["status"].as_str() {
                Some("completed") => return Ok(state["result"].take()),
                Some("failed" | "cancelled" | "error") => {
                    bail!("APoC code {} failed: {}", state["id"], state["error"])
                }
                _ => {}
            }
            let id = state["id"]
                .as_str()
                .context("APoC returned no durable code ID")?
                .to_owned();
            if start.elapsed() > Duration::from_secs(90) {
                bail!("APoC code {id} is still pending; inspect it before retrying");
            }
            std::thread::sleep(Duration::from_millis(500));
            state = self.invoke(&[
                "code".into(),
                "get".into(),
                id,
                "--purpose".into(),
                purpose.into(),
                "--filter-output".into(),
                "id,status,result,error".into(),
                "--format".into(),
                "json".into(),
            ])?;
        }
    }
}

impl Runtime for ApocRuntime {
    fn apoc(&self, method: &str, args: Value) -> Result<Value> {
        if method.is_empty()
            || !method
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || c == b'_')
        {
            bail!("Invalid APoC capability name");
        }
        let purpose = args["purpose"]
            .as_str()
            .unwrap_or("Run workenv orchestration.");
        let key = args["idempotency_key"]
            .as_str()
            .map(|k| format!("workenv-code:{method}:{k}"))
            .unwrap_or_else(|| format!("workenv-read:{}", Uuid::new_v4()));
        self.code(
            &format!(
                "return await apoc.{method}({});",
                serde_json::to_string(&args)?
            ),
            &key,
            purpose,
        )
    }
    fn run(&self, spec: CommandSpec) -> Result<CommandOutput> {
        if spec.key.is_empty() {
            bail!("Every managed execution requires an idempotency key");
        }
        if std::io::stderr().is_terminal() {
            eprintln!("{}", spec.purpose);
        }
        let mut executable = spec.executable;
        let mut argv = spec.args;
        let mut input_path = None;
        if let Some(input) = spec.stdin {
            let dir = self.root.join(".state/stdin");
            fs::create_dir_all(&dir)?;
            let path = dir.join(format!("{:x}", Sha256::digest(spec.key.as_bytes())));
            let mut opts = fs::OpenOptions::new();
            opts.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                opts.mode(0o600);
            }
            match opts.open(&path) {
                Ok(mut file) => {
                    file.write_all(&input)?;
                    file.sync_all()?;
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    if fs::read(&path)? != input {
                        bail!("Idempotency key is already bound to different standard input");
                    }
                }
                Err(error) => return Err(error.into()),
            }
            let mut helper_args = vec![
                "--workenv-exec-with-stdin".into(),
                path.to_string_lossy().into_owned(),
                "--".into(),
                executable,
            ];
            helper_args.extend(argv);
            executable = std::env::current_exe()?.to_string_lossy().into_owned();
            argv = helper_args;
            input_path = Some(path);
        }
        let started = self.apoc("execution_start", json!({"executable":executable,"arg":argv,"cwd":spec.cwd,"idempotency_key":spec.key,"purpose":spec.purpose,"timeout_ms":spec.timeout_ms,"artifact_bytes":16777216,"verbosity":"trace","expect_exit_code":[0]}))?;
        let id = started
            .pointer("/execution/id")
            .or_else(|| started.get("id"))
            .and_then(Value::as_str)
            .context("APoC start returned no execution ID")?
            .to_owned();
        let deadline = Instant::now() + Duration::from_millis(spec.timeout_ms);
        let terminal = loop {
            let remaining = deadline
                .saturating_duration_since(Instant::now())
                .as_millis() as u64;
            if remaining == 0 {
                break None;
            }
            let waited = self.apoc(
                "execution_wait",
                json!({"id":id,"timeout_ms":remaining.min(30000),"purpose":spec.purpose,"verbosity":"trace"}),
            )?;
            if waited["outcome"] != "pending" {
                break Some(waited);
            }
            if Instant::now() >= deadline {
                break None;
            }
        };
        let logs = self.apoc(
            "execution_logs",
            json!({"id":id,"tail_bytes":16777216,"purpose":spec.purpose}),
        )?;
        if logs["stdout_truncated"] == true || logs["stderr_truncated"] == true {
            bail!("Execution {id} output was truncated; use a file artifact for this operation");
        }
        let exit_code = terminal.as_ref().and_then(execution_exit_code);
        if terminal.is_some() {
            if let Some(path) = input_path {
                let _ = fs::remove_file(path);
            }
        }
        Ok(CommandOutput {
            stdout: logs["stdout"]
                .as_str()
                .unwrap_or_default()
                .as_bytes()
                .to_vec(),
            stderr: logs["stderr"]
                .as_str()
                .unwrap_or_default()
                .as_bytes()
                .to_vec(),
            exit_code,
            execution_id: id,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn retains_nonzero_exit_from_apoc_trace_result() {
        assert_eq!(
            execution_exit_code(&json!({"outcome":"failed","result":{"exit_code":23}})),
            Some(23)
        );
        assert_eq!(execution_exit_code(&json!({"outcome":"pending"})), None);
        assert_eq!(
            execution_exit_code(&json!({"outcome":"failed","exit_code":255})),
            Some(255)
        );
    }
    #[test]
    fn rejects_duplicate_workers_and_invalid_capacities() {
        let worker = json!({"name":"workenv-01","cpus":2,"memory_gb":8,"disk_gb":50});
        assert!(validate_fleet(&json!({"workers":[worker.clone(),worker]})).is_err());
        assert!(validate_fleet(&json!({"workers":[{"name":"workenv-01","cpus":0}]})).is_err());
    }
    #[test]
    fn state_write_is_complete_and_private() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("task.json");
        write_json(&path, &json!({"revision":"abc"})).unwrap();
        assert_eq!(read_json(&path).unwrap()["revision"], "abc");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }
    #[test]
    fn quotes_remote_shell_metacharacters_as_data() {
        assert_eq!(shell_quote("x'$(secret);"), "'x'\\''$(secret);'");
    }
    #[test]
    fn unknown_execution_is_not_success() {
        let output = CommandOutput {
            execution_id: "durable-id".into(),
            ..Default::default()
        };
        assert!(output
            .success()
            .unwrap_err()
            .to_string()
            .contains("durable-id"));
    }
}
