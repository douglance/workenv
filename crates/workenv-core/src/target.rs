use crate::{
    Controller,
    adapter::{Invocation, invoke},
    dispatch::identity,
};
use anyhow::{Context, Result};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::path::PathBuf;
use workenv_platform::ExecutionSpec;
use workenv_protocol::{AdapterResponse, PROTOCOL_VERSION, ResponseStatus};

/// How long one command on a target is given, whether it travels through a
/// transport or runs directly.
const TARGET_TIMEOUT_MS: u64 = 900_000;

pub(crate) struct TargetCommand {
    pub(crate) arguments: Vec<String>,
    pub(crate) directory: PathBuf,
    pub(crate) key: String,
    pub(crate) stage: &'static str,
    pub(crate) record: bool,
}

impl Controller {
    pub(crate) fn target_command(
        &self,
        name: &str,
        command: &TargetCommand,
    ) -> Result<AdapterResponse> {
        if !command.record {
            return self.execute_target(name, command, None);
        }
        let environment = self.environment_ref(name)?;
        let digest = serde_json::to_vec(&json!({"environment":environment,
            "host":self.host_for(environment)?,"argv":command.arguments,"cwd":command.directory}))?;
        let fingerprint = format!("{:x}", Sha256::digest(digest));
        self.receipts.apply(
            &identity(name, "devenv", command.stage),
            &command.key,
            &fingerprint,
            |previous| self.execute_target(name, command, previous),
        )
    }

    fn execute_target(
        &self,
        name: &str,
        command: &TargetCommand,
        previous: Option<Value>,
    ) -> Result<AdapterResponse> {
        let environment = self.environment_ref(name)?;
        let host = self.host_for(environment)?;
        if let Some(transport) = &host.transport {
            return self.transport_command(name, transport, command, previous);
        }
        let (executable, arguments) = command
            .arguments
            .split_first()
            .context("empty target command")?;
        let output = self.executor.execute(ExecutionSpec {
            executable: executable.clone(),
            arg: arguments.to_vec(),
            cwd: Some(command.directory.clone()),
            stdin: None,
            timeout_ms: TARGET_TIMEOUT_MS,
            idempotency_key: command.key.clone(),
            purpose: format!("Workenv {} for {name}.", command.stage),
        })?;
        Ok(AdapterResponse {
            protocol_version: PROTOCOL_VERSION,
            request_id: command.key.clone(),
            status: exit_status(output.exit_code.map(i64::from)),
            data: json!({"stdout":output.stdout,"stderr":output.stderr,"exit_code":output.exit_code}),
            error: None,
            execution_id: Some(output.execution_id),
        })
    }

    fn transport_command(
        &self,
        name: &str,
        transport: &str,
        command: &TargetCommand,
        previous: Option<Value>,
    ) -> Result<AdapterResponse> {
        let call = Invocation {
            extension_id: transport,
            operation: "execute",
            environment: name,
            config: json!({}),
            // The same budget the direct path uses. Omitting it left the transport
            // to apply its own default -- orchard's is 300_000 ms -- so the identical
            // command got a third of the time simply for travelling through a
            // transport, and a slow `devenv shell` in a guest was reported as the
            // carrier writing nothing.
            input: json!({"argv":command.arguments,"cwd":command.directory,
                "timeout_ms":TARGET_TIMEOUT_MS}),
            key: command.key.clone(),
            previous,
            allow_internal: true,
        };
        let mut response = invoke(&self.root, &self.manifest, self.executor.as_ref(), &call)?;
        if response.complete() {
            response.status = exit_status(response.data["exit_code"].as_i64());
        }
        Ok(response)
    }
}

fn exit_status(code: Option<i64>) -> ResponseStatus {
    match code {
        Some(0) => ResponseStatus::Ready,
        Some(_) => ResponseStatus::Failed,
        None => ResponseStatus::Pending,
    }
}
