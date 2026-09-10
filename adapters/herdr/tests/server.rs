//! Project session startup contracts.
use std::sync::Mutex;

use anyhow::Result;
use serde_json::{Value, json};
use workenv_adapter_herdr::handle_with;
use workenv_platform::{ExecutionOutput, ExecutionSpec, Executor};
use workenv_protocol::{AdapterRequest, PROTOCOL_VERSION, ResponseStatus, Target};

#[test]
fn generated_shell_preserves_project_source_and_profile_arguments() -> Result<()> {
    use std::{fs, os::unix::fs::PermissionsExt as _, process::Command};
    let temp = tempfile::tempdir()?;
    let project = temp.path().join("project's directory");
    let bin = temp.path().join("bin");
    fs::create_dir(&bin)?;
    let devenv = bin.join("devenv");
    fs::write(&devenv, "#!/bin/sh\nprintf '%s\\0' \"$@\"\n")?;
    fs::set_permissions(&devenv, fs::Permissions::from_mode(0o700))?;
    let mut input = request(&project);
    input.target.source = "git+file:///project's source".into();
    input.target.profiles = vec!["quoted'profile".into()];
    handle_with(&input, &Runner::new(vec![ready()]))?;
    let config: toml::Value = toml::from_str(&fs::read_to_string(
        project.join(".state/herdr/config.toml"),
    )?)?;
    let shell = config["terminal"]["default_shell"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("missing shell"))?;
    let output = Command::new(shell).env("PATH", &bin).output()?;
    assert!(output.status.success());
    let args: Vec<_> = output
        .stdout
        .split(|byte| *byte == 0)
        .filter(|arg| !arg.is_empty())
        .collect();
    let expected = [
        "shell",
        "--from",
        "git+file:///project's source",
        "--profile",
        "quoted'profile",
        "--",
        "bash",
        "--noprofile",
        "--norc",
        "-i",
    ];
    assert_eq!(args, expected.map(str::as_bytes));
    Ok(())
}

#[test]
fn apply_starts_and_verifies_the_project_session() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let runner = Runner::new(vec![json!({"running":false}), json!({}), ready()]);
    let response = handle_with(&request(temp.path()), &runner)?;
    assert_eq!(response.status, ResponseStatus::Changed);
    assert_eq!(response.data["status"], "herdr_ready");
    let calls = runner
        .calls
        .lock()
        .map_err(|_| anyhow::anyhow!("mutex poisoned"))?;
    assert_eq!(calls.len(), 3);
    assert!(calls[1].arg.iter().any(|arg| arg == "remote-client-bridge"));
    assert_eq!(calls[1].stdin, Some(Vec::new()));
    assert_eq!(calls[1].cwd.as_deref(), Some(temp.path()));
    Ok(())
}

#[test]
fn apply_preserves_a_compatible_running_session() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let runner = Runner::new(vec![ready()]);
    let response = handle_with(&request(temp.path()), &runner)?;
    assert!(response.complete());
    assert_eq!(response.data["status"], "herdr_ready");
    assert_eq!(
        runner
            .calls
            .lock()
            .map_err(|_| anyhow::anyhow!("mutex poisoned"))?
            .len(),
        1
    );
    Ok(())
}

#[test]
fn apply_does_not_replace_an_incompatible_running_session() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let mut incompatible = ready();
    incompatible["compatible"] = json!(false);
    let runner = Runner::new(vec![incompatible]);
    let response = handle_with(&request(temp.path()), &runner)?;
    assert_eq!(response.status, ResponseStatus::Failed);
    assert_eq!(
        runner
            .calls
            .lock()
            .map_err(|_| anyhow::anyhow!("mutex poisoned"))?
            .len(),
        1
    );
    Ok(())
}

fn request(directory: &std::path::Path) -> AdapterRequest {
    AdapterRequest {
        protocol_version: PROTOCOL_VERSION,
        request_id: "project-up-1".into(),
        extension: "workenv.herdr".into(),
        operation: "apply".into(),
        target: Target {
            environment: "project".into(),
            host: "vm".into(),
            address: Some("user@vm".into()),
            directory: directory.to_path_buf(),
            system: "x86_64-linux".into(),
            source: "path:/project".into(),
            profiles: vec!["development".into()],
        },
        config: json!({"session":"project"}),
        input: json!({}),
        previous: None,
    }
}

fn ready() -> Value {
    json!({"running":true,"compatible":true,"version":"0.9.0","protocol":22,
        "server_binary_stale":false,"capabilities":{"detached_server_daemon":true}})
}

struct Runner {
    calls: Mutex<Vec<ExecutionSpec>>,
    values: Mutex<Vec<Value>>,
}

impl Runner {
    fn new(values: Vec<Value>) -> Self {
        Self {
            calls: Mutex::new(Vec::new()),
            values: Mutex::new(values),
        }
    }
}

impl Executor for Runner {
    fn execute(&self, spec: ExecutionSpec) -> Result<ExecutionOutput> {
        self.calls
            .lock()
            .map_err(|_| anyhow::anyhow!("mutex poisoned"))?
            .push(spec);
        let value = self
            .values
            .lock()
            .map_err(|_| anyhow::anyhow!("mutex poisoned"))?
            .remove(0);
        Ok(ExecutionOutput {
            stdout: serde_json::to_string(&value)?,
            stderr: String::new(),
            exit_code: Some(0),
            execution_id: "execution-1".into(),
        })
    }
}
