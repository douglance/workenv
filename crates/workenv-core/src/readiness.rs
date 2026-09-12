use crate::{Controller, devenv::command_argv, target::TargetCommand};
use anyhow::{Context, Result};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::path::PathBuf;
use uuid::Uuid;
use workenv_platform::{read_json, write_json_atomic};
use workenv_protocol::{AdapterResponse, ResponseStatus};

impl Controller {
    pub(crate) fn inspect_applied(&self, name: &str) -> Result<AdapterResponse> {
        let mut response = self.probe_devenv(name)?;
        if !response.complete() {
            return Ok(response);
        }
        let path = self.applied_path(name);
        if !path.exists() {
            return Ok(needs_apply(response, "environment_not_applied"));
        }
        let applied: Value = read_json(&path)?;
        let environment = self.environment_ref(name)?;
        if applied["environment"] != json!(environment)
            || applied["host"] != json!(self.host_for(environment)?)
        {
            return Ok(needs_apply(response, "environment_configuration_changed"));
        }
        response = self.inspect_shell_identity(name)?;
        if !response.complete() {
            return Ok(response);
        }
        let current: Value = serde_json::from_str(
            response.data["stdout"]
                .as_str()
                .context("missing shell identity")?,
        )?;
        if current["shell.drvPath"].as_str().is_none()
            || current["shell.drvPath"] != applied["shell_derivation"]
        {
            return Ok(needs_apply(response, "devenv_configuration_changed"));
        }
        response = self.inspect_profile(name, &applied)?;
        if response.status == ResponseStatus::Failed {
            return Ok(needs_apply(response, "applied_package_profile_unavailable"));
        }
        if response.complete() {
            response.data = applied;
        }
        Ok(response)
    }

    pub(crate) fn record_applied(&self, name: &str, state: &Value) -> Result<bool> {
        let path = self.applied_path(name);
        if path.exists() && read_json::<Value>(&path)? == *state {
            return Ok(false);
        }
        write_json_atomic(&path, state)?;
        Ok(true)
    }

    fn applied_path(&self, name: &str) -> PathBuf {
        let digest = format!("{:x}", Sha256::digest(name.as_bytes()));
        self.root
            .join(".state/workenv-core/applied")
            .join(format!("{digest}.json"))
    }

    fn inspect_shell_identity(&self, name: &str) -> Result<AdapterResponse> {
        let mut arguments = command_argv(self.environment_ref(name)?, "eval");
        arguments.push("shell.drvPath".into());
        self.inspect_command(name, arguments)
    }

    fn inspect_profile(&self, name: &str, applied: &Value) -> Result<AdapterResponse> {
        let profile = applied["package_profile"]
            .as_str()
            .context("missing applied profile")?;
        self.inspect_command(name, vec!["test".into(), "-d".into(), profile.into()])
    }

    fn inspect_command(&self, name: &str, arguments: Vec<String>) -> Result<AdapterResponse> {
        self.target_command(
            name,
            &TargetCommand {
                arguments,
                directory: self.environment_ref(name)?.directory.clone(),
                key: format!("devenv-inspect:{}", Uuid::new_v4()),
                stage: "inspect",
                record: false,
            },
        )
    }
}

fn needs_apply(mut response: AdapterResponse, reason: &str) -> AdapterResponse {
    response.status = ResponseStatus::Pending;
    response.data = json!({"reason":reason,"next_operation":"environment apply",
        "observation":response.data});
    response.execution_id = None;
    response
}

#[cfg(test)]
#[path = "readiness_tests.rs"]
mod tests;

#[cfg(test)]
mod needs_apply_tests {
    use super::needs_apply;
    use serde_json::json;
    use workenv_protocol::{AdapterResponse, PROTOCOL_VERSION, ResponseStatus};

    #[test]
    fn a_not_applied_answer_carries_no_execution_to_poll() {
        // The observation's execution has finished; keeping its id on a Pending
        // answer invites a caller to poll a completed execution and read its result
        // as this one's. Nothing asserted the id was cleared.
        let observed = AdapterResponse {
            protocol_version: PROTOCOL_VERSION,
            request_id: "status-1".to_owned(),
            status: ResponseStatus::Ready,
            data: json!({"devenv": "2.3.0"}),
            error: None,
            execution_id: Some("exec-finished".to_owned()),
        };
        let answer = needs_apply(observed, "environment_not_applied");
        assert_eq!(answer.status, ResponseStatus::Pending);
        assert_eq!(answer.execution_id, None);
        assert_eq!(answer.data["reason"], json!("environment_not_applied"));
        assert_eq!(answer.data["observation"]["devenv"], json!("2.3.0"));
    }
}
