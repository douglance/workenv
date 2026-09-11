use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use serde_json::json;
use workenv_platform::{ExecutionOutput, ExecutionSpec, Executor, shell_join};
use workenv_protocol::{AdapterRequest, AdapterResponse, ResponseStatus};

use crate::profile_files::{profile_root, spec, string};
use crate::profile_response::{output_json, pending_response, response};

pub(crate) fn login(request: &AdapterRequest, runner: &impl Executor) -> Result<AdapterResponse> {
    let login = LoginContext::new(request)?;
    let pane = match create_login_pane(request, runner, &login)? {
        CommandOutcome::Ready(pane) => pane,
        outcome => return Ok(login.workspace_response(request, outcome)),
    };
    let output = start_login_command(request, runner, &login, &pane)?;
    Ok(login.command_response(request, &pane, output))
}

fn start_login_command(
    request: &AdapterRequest,
    runner: &impl Executor,
    login: &LoginContext,
    pane: &str,
) -> Result<ExecutionOutput> {
    let command = shell_join(&profile_exec(&login.root, login.login_argv.clone()));
    runner.execute(ExecutionSpec {
        executable: "herdr".to_string(),
        arg: vec![
            "--session".to_string(),
            login.session.clone(),
            "pane".to_string(),
            "run".to_string(),
            pane.to_string(),
            command,
        ],
        cwd: Some(request.target.directory.clone()),
        stdin: None,
        idempotency_key: format!("{}:identity-login-command", request.request_id),
        purpose: "Start an interactive login in the selected identity profile.".to_string(),
        timeout_ms: 60_000,
    })
}

fn create_login_pane(
    request: &AdapterRequest,
    runner: &impl Executor,
    login: &LoginContext,
) -> Result<CommandOutcome> {
    let output = runner.execute(ExecutionSpec {
        executable: "herdr".to_string(),
        arg: vec![
            "--session".to_string(),
            login.session.clone(),
            "workspace".to_string(),
            "create".to_string(),
            "--cwd".to_string(),
            login.root.display().to_string(),
            "--label".to_string(),
            format!("{} {} login", login.profile, login.service),
            "--focus".to_string(),
        ],
        cwd: Some(request.target.directory.clone()),
        stdin: None,
        idempotency_key: format!("{}:identity-login-pane", request.request_id),
        purpose: "Open a profile-scoped Herdr login workspace.".to_string(),
        timeout_ms: 60_000,
    })?;
    if output.exit_code.is_none() {
        return Ok(CommandOutcome::Pending(output.execution_id));
    }
    if output.exit_code != Some(0) {
        return Ok(CommandOutcome::Failed {
            execution_id: output.execution_id,
            stderr: output.stderr,
        });
    }
    let created = output_json(&output)?;
    let pane = created
        .pointer("/result/root_pane/pane_id")
        .or_else(|| created.pointer("/root_pane/pane_id"))
        .and_then(serde_json::Value::as_str)
        .map(ToOwned::to_owned)
        .context("Herdr did not return a login pane ID")?;
    Ok(CommandOutcome::Ready(pane))
}

fn profile_exec(root: &std::path::Path, argv: Vec<String>) -> Vec<String> {
    let mut command = vec![
        "direnv".to_string(),
        "exec".to_string(),
        root.display().to_string(),
    ];
    command.extend(argv);
    command
}

fn login_argv(service: &str) -> Result<Vec<String>> {
    match service {
        "github" => Ok(vec![
            "gh",
            "auth",
            "login",
            "--hostname",
            "github.com",
            "--git-protocol",
            "https",
            "--web",
        ]),
        "codex" => Ok(vec!["codex", "login", "--device-auth"]),
        "claude" => Ok(vec!["claude", "auth", "login"]),
        _ => bail!("service must be github, codex, or claude"),
    }
    .map(|items| items.into_iter().map(ToOwned::to_owned).collect())
}

enum CommandOutcome {
    Ready(String),
    Pending(String),
    Failed {
        execution_id: String,
        stderr: String,
    },
}

struct LoginContext {
    profile: String,
    service: String,
    root: PathBuf,
    session: String,
    login_argv: Vec<String>,
}

impl LoginContext {
    fn new(request: &AdapterRequest) -> Result<Self> {
        let spec = spec(request)?;
        let service = string(&request.input, "service").unwrap_or_else(|| "github".to_string());
        let login_argv = login_argv(&service)?;
        Ok(Self {
            root: profile_root(request, &spec.name),
            profile: spec.name,
            session: string(&request.config, "session").unwrap_or_else(|| "workenv".to_string()),
            service,
            login_argv,
        })
    }

    fn workspace_response(
        &self,
        request: &AdapterRequest,
        outcome: CommandOutcome,
    ) -> AdapterResponse {
        match outcome {
            CommandOutcome::Ready(pane) => response(
                request,
                ResponseStatus::Ready,
                "login_workspace_ready",
                json!({"profile":self.profile,"service":self.service,"session":self.session,"pane_id":pane}),
            ),
            CommandOutcome::Pending(execution_id) => pending_response(
                request,
                execution_id,
                "login_workspace_pending",
                json!({"profile":self.profile,"service":self.service,"session":self.session}),
            ),
            CommandOutcome::Failed {
                execution_id,
                stderr,
            } => response(
                request,
                ResponseStatus::Failed,
                "login_workspace_failed",
                json!({"profile":self.profile,"service":self.service,"session":self.session,"execution_id":execution_id,"stderr":stderr}),
            ),
        }
    }

    fn command_response(
        &self,
        request: &AdapterRequest,
        pane: &str,
        output: ExecutionOutput,
    ) -> AdapterResponse {
        if output.exit_code.is_none() {
            return pending_response(
                request,
                output.execution_id,
                "login_command_pending",
                json!({"profile":self.profile,"service":self.service,"pane_id":pane,"session":self.session}),
            );
        }
        if output.exit_code != Some(0) {
            return response(
                request,
                ResponseStatus::Failed,
                "login_command_failed",
                json!({"profile":self.profile,"service":self.service,"pane_id":pane,"session":self.session,"execution_id":output.execution_id,"stderr":output.stderr}),
            );
        }
        response(
            request,
            ResponseStatus::Changed,
            "login_started",
            json!({"profile":self.profile,"service":self.service,"pane_id":pane,"session":self.session}),
        )
    }
}

#[cfg(test)]
#[path = "profile_login_tests.rs"]
mod tests;
