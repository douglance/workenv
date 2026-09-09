use crate::{Controller, dispatch::mutation_key, target::TargetCommand};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use uuid::Uuid;
use workenv_protocol::{AdapterResponse, Environment, ResponseStatus};

const PREPARE: &str =
    "if [ -d \"$1\" ]; then printf ready; else mkdir -p \"$1\" && printf changed; fi";

impl Controller {
    pub(crate) fn prepare_directory(
        &self,
        name: &str,
        key: Option<&str>,
    ) -> Result<AdapterResponse> {
        let environment = self.environment_ref(name)?;
        let mut response = self.target_command(
            name,
            &TargetCommand {
                arguments: vec![
                    "sh".into(),
                    "-c".into(),
                    PREPARE.into(),
                    "workenv-directory".into(),
                    environment.directory.to_string_lossy().into_owned(),
                ],
                directory: "/".into(),
                key: stage_key(key, name, "directory")?,
                stage: "directory",
                record: true,
            },
        )?;
        if response.complete() && response.data["stdout"].as_str() == Some("changed") {
            response.status = ResponseStatus::Changed;
        }
        Ok(response)
    }

    pub(crate) fn realize(&self, name: &str, key: Option<&str>) -> Result<AdapterResponse> {
        let environment = self.environment_ref(name)?;
        let mut arguments = shell_argv(environment);
        arguments.extend(["--".into(), "printenv".into(), "DEVENV_PROFILE".into()]);
        let mut response = self.target_command(
            name,
            &TargetCommand {
                arguments,
                directory: environment.directory.clone(),
                key: stage_key(key, name, "shell")?,
                stage: "shell",
                record: true,
            },
        )?;
        if !response.complete() {
            return Ok(response);
        }
        let profile = output_text(&response)?;
        ensure!(
            profile.starts_with("/nix/store/") && !profile.contains('\n'),
            "devenv returned an invalid package profile"
        );
        let identity = self.shell_identity(name, key)?;
        if !identity.complete() {
            return Ok(identity);
        }
        let evaluation: Value = serde_json::from_str(output_text(&identity)?)?;
        let derivation = evaluation["shell.drvPath"]
            .as_str()
            .context("devenv omitted shell.drvPath")?;
        let state = json!({"environment":environment,"host":self.host_for(environment)?,
            "package_profile":profile,"shell_derivation":derivation});
        let changed = self.record_applied(name, &state)?;
        response.status = if changed {
            ResponseStatus::Changed
        } else {
            ResponseStatus::Ready
        };
        response.data = state;
        Ok(response)
    }

    pub(crate) fn probe_devenv(&self, name: &str) -> Result<AdapterResponse> {
        let environment = self.environment_ref(name)?;
        self.target_command(
            name,
            &TargetCommand {
                arguments: vec!["devenv".into(), "version".into()],
                directory: environment.directory.clone(),
                key: format!("devenv-probe:{}", Uuid::new_v4()),
                stage: "inspect",
                record: false,
            },
        )
    }

    fn shell_identity(&self, name: &str, key: Option<&str>) -> Result<AdapterResponse> {
        let environment = self.environment_ref(name)?;
        let mut arguments = command_argv(environment, "eval");
        arguments.push("shell.drvPath".into());
        self.target_command(
            name,
            &TargetCommand {
                arguments,
                directory: environment.directory.clone(),
                key: stage_key(key, name, "identity")?,
                stage: "identity",
                record: true,
            },
        )
    }
}

pub(crate) fn shell_argv(environment: &Environment) -> Vec<String> {
    command_argv(environment, "shell")
}

pub(crate) fn command_argv(environment: &Environment, command: &str) -> Vec<String> {
    let mut arguments = vec![
        "devenv".into(),
        command.into(),
        "--from".into(),
        environment.source.clone(),
    ];
    for profile in &environment.profiles {
        arguments.extend(["--profile".into(), profile.clone()]);
    }
    arguments
}

fn stage_key(key: Option<&str>, name: &str, stage: &str) -> Result<String> {
    Ok(format!("{}:{name}:devenv:{stage}", mutation_key(key)?))
}

fn output_text(response: &AdapterResponse) -> Result<&str> {
    response.data["stdout"]
        .as_str()
        .map(str::trim)
        .context("target returned no stdout")
}
