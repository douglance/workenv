//! Migration command behavior for legacy fleet conversion.
use anyhow::{Context, Result};
use serde_json::json;

#[path = "migration/support.rs"]
mod support;

use support::{
    basic_fleet, controller_from_evaluated_manifest, invoke, manifest_from_proposal, path_source,
    write_fixture_fleet, write_fleet, write_profile,
};

#[tokio::test]
async fn default_report_proposes_adopted_nix_without_writing_state() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let root = temp.path().join("fleet-${literal}");
    std::fs::create_dir_all(&root)?;
    write_fixture_fleet(&root)?;
    write_profile(&root)?;

    let (code, report, _) = invoke(&root, &[]).await?;

    assert_eq!(code, Some(0));
    assert_eq!(report["status"], "proposed");
    assert!(
        report["proposed_nix"]
            .as_str()
            .context("missing Nix")?
            .contains("\\\"extension\\\": \\\"workenv.exedev\\\"")
    );
    assert!(
        report["proposed_nix"]
            .as_str()
            .context("missing Nix")?
            .contains("workenv-06.example.ts.net")
    );
    assert!(
        !report["proposed_nix"]
            .as_str()
            .context("missing Nix")?
            .contains("imports =")
    );
    assert!(
        report["proposed_nix"]
            .as_str()
            .context("missing Nix")?
            .contains("\\${literal}")
    );
    assert_eq!(report["omitted"][0]["artifact"], "projects");
    assert!(!root.join(".state").exists());
    Ok(())
}

#[tokio::test]
async fn migrated_manifest_uses_declared_extensions_and_target_sources() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let root = temp.path().join("fleet");
    std::fs::create_dir_all(&root)?;
    write_fixture_fleet(&root)?;
    write_profile(&root)?;

    let (code, report, _) = invoke(&root, &[]).await?;
    let proposed = report["proposed_nix"].as_str().context("missing Nix")?;
    let manifest = manifest_from_proposal(proposed)?;

    assert_eq!(code, Some(0));
    assert_eq!(
        manifest.hosts["workenv-02"].transport.as_deref(),
        Some("workenv.ssh")
    );
    assert_eq!(
        manifest.hosts["workenv-02"]
            .provider
            .as_ref()
            .context("missing provider")?
            .extension,
        "workenv.exedev"
    );
    assert_eq!(
        manifest.environments["workenv-02"]
            .connection
            .as_ref()
            .context("missing connection")?
            .extension,
        "workenv.herdr"
    );
    assert_eq!(
        manifest.environments["workenv-02"].source,
        "path:/home/exedev/workenv"
    );
    assert_eq!(
        manifest.environments["linux-static"].source,
        "path:/home/exedev/workenv"
    );
    assert_eq!(
        manifest.environments["mac-static"].source,
        path_source(&root.canonicalize()?)
    );

    workenv_core::Controller::from_manifest(&root, manifest)?;
    Ok(())
}

#[test]
#[ignore = "requires WORKENV_MANIFEST_JSON_FILE captured from devenv eval"]
fn evaluated_manifest_json_constructs_controller() -> Result<()> {
    let path = std::env::var("WORKENV_MANIFEST_JSON_FILE")
        .context("WORKENV_MANIFEST_JSON_FILE is required")?;
    controller_from_evaluated_manifest(&path)
}

#[tokio::test]
async fn output_write_is_exclusive_and_replays_by_receipt() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let root = temp.path();
    write_fleet(root, &basic_fleet())?;
    let output = root.join("migrated.nix");

    let (code, written, _) = invoke(
        root,
        &[
            "--output",
            &output.to_string_lossy(),
            "--idempotency-key",
            "write-1",
        ],
    )
    .await?;
    let first_text = std::fs::read_to_string(&output)?;

    let (second_code, replayed, _) = invoke(
        root,
        &[
            "--output",
            &output.to_string_lossy(),
            "--idempotency-key",
            "write-1",
        ],
    )
    .await?;

    assert_eq!(code, Some(0));
    assert_eq!(second_code, Some(0));
    assert_eq!(written["status"], "written");
    assert_eq!(replayed["status"], "replayed");
    assert_eq!(first_text, written["proposed_nix"]);
    assert_eq!(std::fs::read_to_string(&output)?, first_text);
    Ok(())
}

#[tokio::test]
async fn a_second_key_finding_the_same_proposal_reports_it_unchanged() -> Result<()> {
    // Two keys aimed at one destination. The second writes nothing, because the
    // file already holds exactly this proposal, and used to say `written`
    // anyway -- two receipts claiming a write where only one happened.
    let temp = tempfile::tempdir()?;
    let root = temp.path();
    write_fleet(root, &basic_fleet())?;
    let output = root.join("migrated.nix");
    let path = output.to_string_lossy().into_owned();

    let (first_code, first, _) =
        invoke(root, &["--output", &path, "--idempotency-key", "key-a"]).await?;
    let after_first = std::fs::read_to_string(&output)?;
    let (second_code, second, _) =
        invoke(root, &["--output", &path, "--idempotency-key", "key-b"]).await?;

    assert_eq!(first_code, Some(0));
    assert_eq!(second_code, Some(0));
    assert_eq!(first["status"], "written");
    assert_eq!(second["status"], "unchanged");
    // The destination is untouched, which is the thing the status now states.
    assert_eq!(std::fs::read_to_string(&output)?, after_first);
    Ok(())
}

#[tokio::test]
async fn output_requires_idempotency_key_before_writing() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let root = temp.path();
    write_fleet(root, &basic_fleet())?;
    let output = root.join("missing-key.nix");

    let (code, _, text) = invoke(root, &["--output", &output.to_string_lossy()]).await?;

    assert_ne!(code, Some(0));
    assert!(text.contains("idempotency-key is required"), "{text}");
    assert!(!output.exists());
    Ok(())
}

#[tokio::test]
async fn handwritten_output_is_not_overwritten() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let root = temp.path();
    write_fleet(root, &basic_fleet())?;
    let output = root.join("existing.nix");
    std::fs::write(&output, "# handwritten\n")?;

    let (code, _, text) = invoke(
        root,
        &[
            "--output",
            &output.to_string_lossy(),
            "--idempotency-key",
            "write-2",
        ],
    )
    .await?;

    assert_ne!(code, Some(0));
    assert!(text.contains("refusing to overwrite"), "{text}");
    assert_eq!(std::fs::read_to_string(&output)?, "# handwritten\n");
    Ok(())
}

#[tokio::test]
async fn remote_macos_hosts_require_system_review() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let root = temp.path();
    write_fleet(
        root,
        &json!({
            "schema_version": 1,
            "workers": [{
                "name": "mac-one",
                "host": "mac-ssh",
                "cpus": 2,
                "memory_gb": 8,
                "disk_gb": 50
            }],
            "hosts": {
                "mac-ssh": {
                    "transport": "ssh",
                    "target": "operator@mac.example",
                    "root": "/Users/operator/workenv",
                    "platform": "macos"
                }
            }
        }),
    )?;

    let (code, report, _) = invoke(root, &[]).await?;
    let proposed = report["proposed_nix"].as_str().context("missing Nix")?;

    assert_eq!(code, Some(0));
    assert!(proposed.contains("REVIEW_REQUIRED_DARWIN_SYSTEM"));
    assert!(proposed.contains("/Users/operator/workenv/workers/mac-one"));
    assert_eq!(report["warnings"][0]["host"], "mac-ssh");
    Ok(())
}

/// The migrated identity binding must name the directory key the adapter reads.
///
/// It did not: migration emitted `profiles_root` and the adapter has always read
/// `profiles_dir`, so a migrated fleet silently resolved profiles under
/// `$HOME/.config/workenv/profiles` instead of the directory migration had just
/// populated -- and unknown binding-config keys raise no error, so the only
/// symptom was a profile that was not found. The key is spelled out here rather
/// than taken from the shared constant, so that renaming the constant fails this
/// test instead of quietly moving both sides together.
#[tokio::test]
async fn migrated_identity_binding_points_at_the_migrated_profile_directory() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let root = temp.path().join("fleet");
    std::fs::create_dir_all(&root)?;
    write_fixture_fleet(&root)?;
    write_profile(&root)?;

    let (code, report, _) = invoke(&root, &[]).await?;
    let proposed = report["proposed_nix"].as_str().context("missing Nix")?;

    assert_eq!(code, Some(0));
    assert!(
        !proposed.contains("profiles_root"),
        "migration still emits the key the identity adapter does not read"
    );
    let expected = root.join("profiles");
    let expected = expected.to_string_lossy();
    assert!(
        proposed.contains("profiles_dir") && proposed.contains(expected.as_ref()),
        "migrated identity config does not point at {expected}"
    );
    Ok(())
}
