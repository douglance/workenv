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

#[cfg(unix)]
#[test]
fn credential_must_be_a_regular_file() -> Result<()> {
    // `symlink_metadata` reports the link itself, so `is_file()` is already false
    // for a symlink; a directory is the only shape that shows the second half of
    // the guard doing work of its own.
    use std::os::unix::fs::PermissionsExt as _;
    let temp = tempfile::tempdir()?;
    let directory = temp.path().join("nib-token");
    fs::create_dir(&directory)?;
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o700))?;
    assert!(validate_private(&directory).is_err());
    Ok(())
}

#[cfg(unix)]
#[test]
fn credential_must_not_be_reached_through_a_symlink() -> Result<()> {
    // A symlinked token path lets anyone who can rewrite the link redirect the
    // read, so the link is refused even when its target is owner-only.
    use std::os::unix::fs::PermissionsExt as _;
    let temp = tempfile::tempdir()?;
    let real = temp.path().join("real-token");
    fs::write(&real, "private-token")?;
    fs::set_permissions(&real, fs::Permissions::from_mode(0o600))?;
    let link = temp.path().join("nib-token");
    std::os::unix::fs::symlink(&real, &link)?;
    assert!(validate_private(&link).is_err());
    assert!(read_private_token(&link).is_err());
    Ok(())
}

#[cfg(unix)]
#[test]
fn credential_must_be_unreadable_to_group_and_other() -> Result<()> {
    // Every mode here is owned by this user and so passes the owner half of the
    // guard: only the permission half can reject them. 0o600 must still read.
    use std::os::unix::fs::PermissionsExt as _;
    let temp = tempfile::tempdir()?;
    for mode in [0o644, 0o640, 0o604, 0o660, 0o666] {
        let path = temp.path().join(format!("token-{mode:o}"));
        fs::write(&path, "private-token")?;
        fs::set_permissions(&path, fs::Permissions::from_mode(mode))?;
        assert!(
            validate_private(&path).is_err(),
            "mode {mode:o} was accepted"
        );
        assert!(
            read_private_token(&path).is_err(),
            "mode {mode:o} was read anyway"
        );
    }
    let owner_only = temp.path().join("token-600");
    fs::write(&owner_only, "private-token")?;
    fs::set_permissions(&owner_only, fs::Permissions::from_mode(0o600))?;
    assert_eq!(read_private_token(&owner_only)?, "private-token");
    Ok(())
}

#[test]
fn token_length_cap_is_exact() {
    // store_from_stdin reads one byte past the cap so an oversized token is
    // refused rather than silently truncated to a valid-looking prefix.
    assert!(validate_token(&"a".repeat(16_384)).is_ok());
    assert!(validate_token(&"a".repeat(16_385)).is_err());
}

#[test]
fn token_must_not_be_empty_or_contain_whitespace() {
    assert!(validate_token("").is_err());
    assert!(validate_token("two words").is_err());
    assert!(validate_token("token\n").is_err());
    assert!(validate_token("private-token").is_ok());
}
