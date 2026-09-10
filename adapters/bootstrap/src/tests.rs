use super::*;
mod cache_setup;
mod controller_seed;
use std::{
    fs::{self, Permissions},
    os::unix::fs::PermissionsExt,
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};
use workenv_protocol::{PROTOCOL_VERSION, Target};

#[test]
fn privilege_required_is_failed_without_execution_id() -> Result<()> {
    let request = request("bootstrap")?;
    let config = BootstrapConfig::from_request(&request)?;
    let response = response(
        &request,
        ResponseStatus::Failed,
        privilege_required(&request, &config),
        Some("bootstrap requires passwordless sudo or root"),
    );
    assert_eq!(response.status, ResponseStatus::Failed);
    assert_eq!(response.data["status"], "privilege_required");
    assert!(response.execution_id.is_none());
    Ok(())
}

#[test]
fn completed_install_missing_requirements_is_failed_without_execution_id() -> Result<()> {
    let request = request("bootstrap")?;
    let report = json!({"ready":false,"status":"needs_setup"});
    let response = response(
        &request,
        ResponseStatus::Failed,
        report,
        Some("bootstrap install completed but prerequisites are still missing"),
    );
    assert_eq!(response.status, ResponseStatus::Failed);
    assert!(response.execution_id.is_none());
    Ok(())
}

#[test]
fn inspect_missing_requirements_remains_pending() -> Result<()> {
    let request = request("inspect")?;
    let response = AdapterResponse::new(
        &request,
        ResponseStatus::Pending,
        json!({"ready":false,"status":"needs_setup"}),
    );
    assert_eq!(response.status, ResponseStatus::Pending);
    assert!(response.execution_id.is_none());
    Ok(())
}

#[test]
fn nix_ready_user_paths_do_not_require_privilege() -> Result<()> {
    let root = temp_path("bootstrap-user-paths");
    let prefix = root.join("missing").join("prefix");
    let link_dir = root.join("missing").join("bin");
    fs::create_dir_all(&root)?;
    let config = config_with_paths(&prefix, &link_dir)?;
    let script =
        scripts::can_install_without_privilege_script(&config, &json!({"nix":{"ok":true}}));

    assert!(run_bash(&script)?.success());
    fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn missing_nix_still_requires_privilege() -> Result<()> {
    let root = temp_path("bootstrap-missing-nix");
    let prefix = root.join("prefix");
    let link_dir = root.join("bin");
    fs::create_dir_all(&root)?;
    let config = config_with_paths(&prefix, &link_dir)?;
    let script =
        scripts::can_install_without_privilege_script(&config, &json!({"nix":{"ok":false}}));

    assert!(!run_bash(&script)?.success());
    fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn protected_prefix_still_requires_privilege() -> Result<()> {
    let root = temp_path("bootstrap-protected-prefix");
    let prefix = root.join("protected").join("prefix");
    let link_dir = root.join("bin");
    fs::create_dir_all(root.join("protected"))?;
    fs::create_dir_all(&link_dir)?;
    let mut permissions = fs::metadata(root.join("protected"))?.permissions();
    permissions.set_readonly(true);
    fs::set_permissions(root.join("protected"), permissions)?;
    let config = config_with_paths(&prefix, &link_dir)?;
    let script =
        scripts::can_install_without_privilege_script(&config, &json!({"nix":{"ok":true}}));

    assert!(!run_bash(&script)?.success());
    fs::set_permissions(root.join("protected"), Permissions::from_mode(0o755))?;
    fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn seed_install_creates_new_user_link_dir_when_devenv_is_ready() -> Result<()> {
    let root = temp_path("bootstrap-seed-link-dir");
    let prefix = root.join("prefix");
    let link_dir = root.join("nested").join("bin");
    let seed = root.join("seed-source");
    fs::create_dir_all(&root)?;
    fs::write(&seed, b"canary\n")?;
    let mut request = request("bootstrap")?;
    request.config = json!({"prefix":prefix,"link_dir":link_dir,"seed_tools":[{
        "name":"workenv-canary-seed","path":seed,"sha256":digest("canary\n")}]});
    let script = format!(
        "nix() {{ echo 'nix (Nix) 2.35.2'; }}\n\
         devenv() {{ echo 'devenv 2.3.0'; }}\n{}",
        scripts::install_script(&BootstrapConfig::from_request(&request)?)
    );

    assert!(
        Command::new("bash")
            .arg("-lc")
            .arg(script)
            .status()?
            .success()
    );
    assert_eq!(fs::read(link_dir.join("workenv-canary-seed"))?, b"canary\n");
    fs::remove_dir_all(root)?;
    Ok(())
}

fn request(operation: &str) -> Result<AdapterRequest> {
    Ok(AdapterRequest {
        protocol_version: PROTOCOL_VERSION,
        request_id: "bootstrap-test".to_owned(),
        extension: "workenv.bootstrap".to_owned(),
        operation: operation.to_owned(),
        target: Target {
            environment: "test".to_owned(),
            host: "local".to_owned(),
            address: None,
            directory: std::env::current_dir()?,
            system: "aarch64-darwin".to_owned(),
            source: ".".to_owned(),
            profiles: vec![],
        },
        config: json!({}),
        input: json!({}),
        previous: None,
    })
}

fn config_with_paths(
    prefix: &std::path::Path,
    link_dir: &std::path::Path,
) -> Result<BootstrapConfig> {
    let mut request = request("bootstrap")?;
    request.config = json!({"prefix":prefix,"link_dir":link_dir});
    BootstrapConfig::from_request(&request)
}

fn run_bash(script: &str) -> Result<std::process::ExitStatus> {
    Ok(Command::new("bash").arg("-lc").arg(script).status()?)
}

fn write_helper(path: &std::path::Path, body: &str) -> Result<()> {
    fs::write(path, body)?;
    fs::set_permissions(path, Permissions::from_mode(0o755))?;
    Ok(())
}

fn shell_arg(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn temp_path(name: &str) -> std::path::PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos());
    std::env::temp_dir().join(format!("workenv-{name}-{nonce}"))
}
