use std::{collections::BTreeMap, fs, io::Read, path::PathBuf, process::Command};

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use workenv_platform::{ExecutionSpec, Executor};
use workenv_protocol::{AdapterRequest, AdapterResponse, ResponseStatus};

/// Return package-wrapper information for Nix without embedding credentials.
///
/// # Errors
/// This currently returns no validation error, but retains `Result` for adapter API symmetry.
pub fn wrapper(request: &AdapterRequest) -> Result<AdapterResponse> {
    let executable =
        string(&request.config, "real_executable").unwrap_or_else(|| "nib-real".to_string());
    Ok(response(
        request,
        ResponseStatus::Ready,
        "nib_wrapper",
        json!({
            "wrapper_argv": ["workenv-adapter-identity", "nib-proxy", executable],
            "credential_path": "~/.config/workenv/nib-token",
            "explicit_env_wins": "NIB_AUTH_TOKEN"
        }),
    ))
}

/// Transfer a caller-referenced Nib token to the target user.
///
/// # Errors
/// Returns an error when the secret reference is invalid or the runner cannot execute.
pub fn transfer(request: &AdapterRequest, runner: &impl Executor) -> Result<AdapterResponse> {
    let token = read_token(request)?;
    verify_token(&token)?;
    let output = runner.execute(ExecutionSpec {
        executable: target_executable(request),
        arg: target_args(request),
        cwd: Some(request.target.directory.clone()),
        stdin: Some(token.into_bytes()),
        idempotency_key: format!("{}:nib-transfer", request.request_id),
        purpose: "Install Nib credential from a caller-supplied secret reference.".to_string(),
        timeout_ms: 60_000,
    })?;
    if output.exit_code.is_none() {
        return Ok(pending_response(
            request,
            output.execution_id,
            "nib_credential_pending",
            json!({"source":"caller_secret_reference"}),
        ));
    }
    if output.exit_code != Some(0) {
        return Ok(response(
            request,
            ResponseStatus::Failed,
            "auth_required",
            json!({
                "error": "Nib credential transfer did not complete",
                "execution_id": output.execution_id
            }),
        ));
    }
    let ack = serde_json::from_str(&output.stdout).context("stdout was not JSON")?;
    Ok(response(
        request,
        ResponseStatus::Changed,
        "nib_credential_ready",
        json!({
            "ack": sanitize_ack(&ack),
            "source": "caller_secret_reference"
        }),
    ))
}

/// Build the environment used by the Nib proxy.
///
/// # Errors
/// Returns an error when no explicit token is present and the token file is invalid.
pub fn credential_environment(
    path: &std::path::Path,
    inherited: &BTreeMap<String, String>,
) -> Result<BTreeMap<String, String>> {
    let mut env = inherited.clone();
    if env
        .get("NIB_AUTH_TOKEN")
        .is_some_and(|value| !value.trim().is_empty())
    {
        return Ok(env);
    }
    let token = read_private_token(path)?;
    env.insert("NIB_AUTH_TOKEN".to_string(), token);
    Ok(env)
}

/// Execute real Nib with the runtime credential loaded outside the Nix store.
///
/// # Errors
/// Returns an error when the credential cannot be loaded or the real executable is missing.
pub fn proxy(args: &[String]) -> Result<()> {
    let executable = args
        .first()
        .context("a real Nib executable path is required")?;
    let env = std::env::vars().collect::<BTreeMap<_, _>>();
    let env = credential_environment(&default_token_path(), &env)?;
    let mut command = Command::new(executable);
    command.args(&args[1..]).env_clear().envs(env);
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

/// Store a Nib token received on stdin.
///
/// # Errors
/// Returns an error when stdin is invalid or an existing different token is present.
pub fn store_from_stdin() -> Result<()> {
    let mut token = String::new();
    std::io::stdin().take(16_385).read_to_string(&mut token)?;
    let payload = store_token(&default_token_path(), token.trim())?;
    println!("{}", serde_json::to_string(&payload)?);
    Ok(())
}

fn store_token(path: &std::path::Path, token: &str) -> Result<Value> {
    validate_token(token)?;
    let directory = path.parent().context("token path has no parent")?;
    fs::create_dir_all(directory)?;
    private_dir(directory)?;
    if path.exists() {
        let existing = read_private_token(path)?;
        if existing != token {
            bail!("a different Nib credential already exists; it was preserved");
        }
        return Ok(
            json!({"status":"already_present","mode":"0600","credential_sha256":sha(token)}),
        );
    }
    write_private(path, token.as_bytes())?;
    Ok(json!({"status":"installed","mode":"0600","credential_sha256":sha(token)}))
}

fn read_token(request: &AdapterRequest) -> Result<String> {
    if let Some(name) = string(&request.config, "nib_token_env") {
        return std::env::var(&name).with_context(|| format!("{name} is not set"));
    }
    if let Some(path) = string(&request.config, "nib_token_file") {
        return read_private_token(&PathBuf::from(path));
    }
    bail!("Nib transfer requires nib_token_env or nib_token_file")
}

fn read_private_token(path: &std::path::Path) -> Result<String> {
    #[cfg(unix)]
    validate_private(path)?;
    let token = fs::read_to_string(path)?.trim().to_string();
    validate_token(&token)?;
    Ok(token)
}

#[cfg(unix)]
fn validate_private(path: &std::path::Path) -> Result<()> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        bail!("Nib credential must be an owner-only regular file");
    }
    if metadata.uid() != current_uid() || metadata.permissions().mode() & 0o077 != 0 {
        bail!("Nib credential must be an owner-only regular file");
    }
    Ok(())
}

#[cfg(unix)]
fn current_uid() -> u32 {
    std::process::Command::new("id")
        .arg("-u")
        .output()
        .ok()
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .and_then(|text| text.trim().parse().ok())
        .unwrap_or(u32::MAX)
}

fn verify_token(token: &str) -> Result<()> {
    validate_token(token)
}

fn validate_token(token: &str) -> Result<()> {
    if token.is_empty() || token.len() > 16_384 || token.chars().any(char::is_whitespace) {
        bail!("Nib credential file has an invalid format");
    }
    Ok(())
}

fn target_executable(request: &AdapterRequest) -> String {
    if request.target.address.is_some() {
        "ssh".to_string()
    } else {
        "workenv-adapter-identity".to_string()
    }
}

fn target_args(request: &AdapterRequest) -> Vec<String> {
    if let Some(address) = &request.target.address {
        return vec![
            "-o".to_string(),
            "BatchMode=yes".to_string(),
            "-o".to_string(),
            "ConnectTimeout=15".to_string(),
            address.clone(),
            "workenv-adapter-identity nib-store".to_string(),
        ];
    }
    vec!["nib-store".to_string()]
}

fn response(
    request: &AdapterRequest,
    status: ResponseStatus,
    name: &str,
    details: Value,
) -> AdapterResponse {
    let mut data = serde_json::Map::new();
    data.insert(
        "ok".to_string(),
        json!(matches!(
            status,
            ResponseStatus::Ready | ResponseStatus::Changed
        )),
    );
    data.insert("status".to_string(), json!(name));
    data.insert("details".to_string(), details);
    AdapterResponse::new(request, status, Value::Object(data))
}

fn pending_response(
    request: &AdapterRequest,
    execution_id: String,
    name: &str,
    details: Value,
) -> AdapterResponse {
    let mut response = response(request, ResponseStatus::Pending, name, details);
    response.execution_id = Some(execution_id);
    response
}

fn sanitize_ack(value: &Value) -> Value {
    json!({"status":value.get("status"),"mode":value.get("mode"),"credential_sha256":value.get("credential_sha256")})
}

fn default_token_path() -> PathBuf {
    std::env::var_os("HOME")
        .map_or_else(|| PathBuf::from("."), PathBuf::from)
        .join(".config/workenv/nib-token")
}

fn write_private(path: &std::path::Path, bytes: &[u8]) -> Result<()> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    std::io::Write::write_all(&mut options.open(path)?, bytes)?;
    Ok(())
}

fn private_dir(path: &std::path::Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

fn sha(value: &str) -> String {
    use sha2::{Digest, Sha256};
    format!("{:x}", Sha256::digest(value.as_bytes()))
}

fn string(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
}

#[cfg(test)]
#[path = "nib_tests.rs"]
mod tests;
