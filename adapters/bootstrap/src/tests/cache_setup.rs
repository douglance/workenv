use super::*;
use anyhow::{Context as _, Result};
use std::{
    fs::{self, Permissions},
    os::unix::fs::PermissionsExt,
    process::Command,
};
#[test]
fn devenv_cache_setup_preserves_existing_nix_conf_and_is_idempotent() -> Result<()> {
    let root = temp_path("bootstrap-nix-conf-cache");
    fs::create_dir_all(&root)?;
    let conf = root.join("nix.conf");
    fs::write(
        &conf,
        b"# keep this file
experimental-features = nix-command flakes
substituters = https://cache.nixos.org/
",
    )?;
    let bin = root.join("bin");
    fs::create_dir_all(&bin)?;
    write_helper(
        &bin.join("systemctl"),
        r#"#!/bin/sh
printf '%s\n' "$*" >> "$SYSTEMCTL_LOG"
"#,
    )?;
    write_helper(&bin.join("uname"), "#!/bin/sh\necho Linux\n")?;

    let conf_arg = path_arg(&conf)?;
    let script = format!(
        "{}\nconfigure_devenv_cachix {conf_arg}\nconfigure_devenv_cachix {conf_arg}\n",
        scripts::devenv_cache_setup_script()
    );
    let output = Command::new("bash")
        .arg("-lc")
        .arg(script)
        .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
        .env("SYSTEMCTL_LOG", root.join("systemctl.log"))
        .output()?;

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stdout.is_empty());
    assert!(
        String::from_utf8_lossy(&output.stderr).is_empty(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let updated = fs::read_to_string(&conf)?;
    assert_preserved_nix_config(&updated);
    assert_official_devenv_cache_config(&updated);
    let systemctl_log = fs::read_to_string(root.join("systemctl.log")).unwrap_or_default();
    assert!(systemctl_log.lines().count() <= 1);
    fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn devenv_cache_setup_skips_host_config_without_admin() -> Result<()> {
    let root = temp_path("bootstrap-nix-conf-no-admin");
    fs::create_dir_all(&root)?;
    let nix_dir = root.join("etc").join("nix");
    fs::create_dir_all(&nix_dir)?;
    fs::set_permissions(&nix_dir, Permissions::from_mode(0o555))?;
    let conf = nix_dir.join("nix.conf");
    let bin = root.join("bin");
    fs::create_dir_all(&bin)?;
    write_helper(
        &bin.join("id"),
        r#"#!/bin/sh
if [ "$1" = -u ]; then echo 501; else /usr/bin/id "$@"; fi
"#,
    )?;
    write_helper(&bin.join("sudo"), "#!/bin/sh\nexit 1\n")?;
    write_helper(
        &bin.join("systemctl"),
        r#"#!/bin/sh
printf systemctl >> "$SYSTEMCTL_LOG"
"#,
    )?;

    let script = format!(
        "{}\nmaybe_configure_devenv_cachix {}\n",
        scripts::devenv_cache_setup_script(),
        path_arg(&conf)?
    );
    let output = Command::new("bash")
        .arg("-lc")
        .arg(script)
        .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
        .env("SYSTEMCTL_LOG", root.join("systemctl.log"))
        .output()?;

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stdout.is_empty());
    assert!(!conf.exists());
    assert!(!root.join("systemctl.log").exists());
    fs::set_permissions(&nix_dir, Permissions::from_mode(0o755))?;
    let stderr = String::from_utf8(output.stderr)?;
    assert!(stderr.contains("skipping devenv Cachix setup"));
    assert!(!stderr.contains("devenv.cachix.org-1:w1cLUi8dv3hnoSPGAuibQv+f9TZLr6cv/Hm9XgU50cw="));
    fs::remove_dir_all(root)?;
    Ok(())
}

fn assert_preserved_nix_config(updated: &str) {
    assert!(updated.contains("experimental-features = nix-command flakes"));
    assert!(updated.contains("substituters = https://cache.nixos.org/"));
}

fn assert_official_devenv_cache_config(updated: &str) {
    assert_eq!(
        updated
            .matches("extra-substituters = https://devenv.cachix.org")
            .count(),
        1
    );
    assert_eq!(
        updated
            .matches("extra-trusted-public-keys = devenv.cachix.org-1:w1cLUi8dv3hnoSPGAuibQv+f9TZLr6cv/Hm9XgU50cw=")
            .count(),
        1
    );
    assert!(!updated.contains("trusted-users"));
    assert!(!updated.contains("trusted = true"));
}

fn path_arg(path: &std::path::Path) -> Result<String> {
    path.to_str().map(shell_arg).context("path is not UTF-8")
}
