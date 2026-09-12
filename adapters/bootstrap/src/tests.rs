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
            .arg("-c")
            .arg(script)
            .status()?
            .success()
    );
    assert_eq!(fs::read(link_dir.join("workenv-canary-seed"))?, b"canary\n");
    fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn install_invokes_nix_by_absolute_path_when_sudo_resets_path() -> Result<()> {
    let root = temp_path("bootstrap-sudo-nix-path");
    let fake_bin = root.join("fake-bin");
    let prefix_parent = root.join("protected");
    let prefix = prefix_parent.join("prefix");
    let link_dir = root.join("bin");
    let nix_log = root.join("nix.log");
    fs::create_dir_all(&fake_bin)?;
    fs::create_dir_all(&prefix_parent)?;
    fs::create_dir_all(&link_dir)?;
    fs::set_permissions(&prefix_parent, Permissions::from_mode(0o555))?;
    write_helper(
        &fake_bin.join("sudo"),
        r#"#!/bin/sh
if [ "$1" = -n ]; then shift; fi
last=
for arg do last=$arg; done
parent=$(dirname "$last")
chmod u+w "$parent" 2>/dev/null || true
PATH=/usr/bin:/bin exec "$@"
"#,
    )?;
    write_helper(
        &fake_bin.join("nix"),
        &format!(
            r#"#!/bin/sh
if [ "$1" = --version ]; then echo 'nix (Nix) 2.35.2'; exit 0; fi
printf '%s\n' "$0 $*" >> {}
while [ "$#" -gt 0 ]; do if [ "$1" = --profile ]; then profile=$2; mkdir -p "$profile/bin"; printf '#!/bin/sh\necho devenv 2.3.0\n' > "$profile/bin/devenv"; chmod 755 "$profile/bin/devenv"; fi; shift; done
"#,
            shell_arg(&path_arg(&nix_log)?)
        ),
    )?;
    let config = config_with_paths(&prefix, &link_dir)?;
    let script = scripts::install_script(&config)
        .replace("maybe_configure_devenv_cachix /etc/nix/nix.conf", ":")
        .replace(
            r#"PATH="/nix/var/nix/profiles/default/bin:$link_dir:$PATH""#,
            &format!("PATH={}:$link_dir:$PATH", shell_arg(&path_arg(&fake_bin)?)),
        );

    // PATH is set, unlike before, so nothing ambient decides this test.
    //
    // Two things leaked in. `sudo_for_path` runs before the script rewrites PATH, so
    // it consulted the real sudo -- passwordless on a runner, not here. And the
    // version gate `devenv version | grep -F "$devenv_version"` saw whatever devenv
    // was on PATH: the gate runs inside the pinned devenv shell on CI, so the
    // version matched, the whole install block was skipped, the stub nix never ran,
    // and reading its log failed with ENOENT. It passed here only because this
    // machine's devenv (2.2.2) is older than the pin (2.3). `fake_bin` holds only
    // `sudo` and `nix`, so `devenv` does not resolve and the gate always opens.
    let status = Command::new("bash")
        .arg("-c")
        .arg(script)
        .env("PATH", format!("{}:/usr/bin:/bin", fake_bin.display()))
        .status()?;

    fs::set_permissions(&prefix_parent, Permissions::from_mode(0o755))?;
    assert!(status.success());
    assert!(fs::read_to_string(nix_log)?.starts_with(&path_arg(&fake_bin.join("nix"))?));
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
    Ok(Command::new("bash").arg("-c").arg(script).status()?)
}

fn write_helper(path: &std::path::Path, body: &str) -> Result<()> {
    fs::write(path, body)?;
    fs::set_permissions(path, Permissions::from_mode(0o755))?;
    Ok(())
}

fn path_arg(path: &std::path::Path) -> Result<String> {
    path.to_str()
        .map(ToOwned::to_owned)
        .ok_or_else(|| anyhow::anyhow!("path is not UTF-8"))
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
