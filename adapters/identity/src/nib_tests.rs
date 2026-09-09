use std::sync::Mutex;

use anyhow::Result;
use workenv_platform::{ExecutionOutput, ExecutionSpec};
use workenv_protocol::{PROTOCOL_VERSION, Target};

use super::*;

#[test]
fn environment_prefers_explicit_token() -> Result<()> {
    let mut env = BTreeMap::new();
    env.insert("NIB_AUTH_TOKEN".to_string(), "explicit".to_string());
    let result = credential_environment(std::path::Path::new("missing"), &env)?;
    assert_eq!(result.get("NIB_AUTH_TOKEN"), Some(&"explicit".to_string()));
    Ok(())
}

#[test]
fn store_is_idempotent_and_preserves_different_token() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("nib-token");
    let first = store_token(&path, "private-token")?;
    let second = store_token(&path, "private-token")?;
    assert_eq!(first["status"], "installed");
    assert_eq!(second["status"], "already_present");
    assert!(store_token(&path, "other-token").is_err());
    Ok(())
}

#[test]
fn transfer_propagates_pending_execution() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let token = temp.path().join("nib-token-source");
    fs::write(&token, "private-token")?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(&token, fs::Permissions::from_mode(0o600))?;
    }
    let mut request = request(temp.path());
    request.config["nib_token_file"] = json!(token);
    let runner = PendingExecutor::default();
    let result = transfer(&request, &runner)?;
    assert_eq!(result.status, ResponseStatus::Pending);
    assert_eq!(result.execution_id, Some("pending-transfer".to_string()));
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

fn request(path: &std::path::Path) -> AdapterRequest {
    AdapterRequest {
        protocol_version: PROTOCOL_VERSION,
        request_id: "request-1".to_string(),
        extension: "identity".to_string(),
        operation: "nib_transfer".to_string(),
        target: Target {
            environment: "dev".to_string(),
            host: "workenv-01".to_string(),
            address: None,
            directory: path.to_path_buf(),
            system: "x86_64-linux".to_string(),
            source: ".".to_string(),
            profiles: Vec::new(),
        },
        config: json!({}),
        input: json!({}),
        previous: None,
    }
}

#[derive(Default)]
struct PendingExecutor {
    calls: Mutex<Vec<ExecutionSpec>>,
}

impl Executor for PendingExecutor {
    fn execute(&self, spec: ExecutionSpec) -> Result<ExecutionOutput> {
        self.calls
            .lock()
            .map_err(|_| anyhow::anyhow!("mutex poisoned"))?
            .push(spec);
        Ok(ExecutionOutput {
            stdout: String::new(),
            stderr: String::new(),
            exit_code: None,
            execution_id: "pending-transfer".to_string(),
        })
    }
}
