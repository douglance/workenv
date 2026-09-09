//! Public extension lifecycle acceptance using a built Workenv CLI and standalone adapter.
use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
};

use anyhow::{Context as _, Result, bail};
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};

enum ExtensionState<'a> {
    Disabled,
    Enabled { version: &'a str, adapter: &'a Path },
}

#[test]
#[ignore = "requires APoC, WORKENV_BIN, and WORKENV_EXTERNAL_ADAPTER"]
fn public_extension_lifecycle_runs_external_binaries_without_rebuilding_core() -> Result<()> {
    let workenv = env_path("WORKENV_BIN")?;
    let adapter = env_path("WORKENV_EXTERNAL_ADAPTER")?;
    let root = tempfile::tempdir()?;
    install_devenv_fixture(root.path())?;
    let adapter_v1 = copy_executable(&adapter, &root.path().join("adapter-v1"))?;
    let adapter_v2 = copy_executable(&adapter, &root.path().join("adapter-v2"))?;
    let original_core_hash = sha256_file(&workenv)?;
    println!("workenv_sha256={original_core_hash}");

    write_manifest(
        root.path(),
        &ExtensionState::Enabled {
            version: "1.0.0",
            adapter: &adapter_v1,
        },
    )?;
    let first = discover_and_invoke(&workenv, root.path(), "1.0.0")?;
    println!("registered_version=1.0.0 executable={}", first.executable);
    assert_core_unchanged(&workenv, &original_core_hash)?;

    write_manifest(root.path(), &ExtensionState::Disabled)?;
    let disabled = extension_list(&workenv, root.path())?;
    assert!(
        disabled.as_object().is_some_and(serde_json::Map::is_empty),
        "{disabled}"
    );
    println!("disabled_extensions={disabled}");
    assert_core_unchanged(&workenv, &original_core_hash)?;

    write_manifest(
        root.path(),
        &ExtensionState::Enabled {
            version: "2.0.0",
            adapter: &adapter_v2,
        },
    )?;
    let updated = discover_and_invoke(&workenv, root.path(), "2.0.0")?;
    println!("updated_version=2.0.0 executable={}", updated.executable);
    assert_ne!(updated.executable, first.executable);
    assert_core_unchanged(&workenv, &original_core_hash)?;

    write_manifest(
        root.path(),
        &ExtensionState::Enabled {
            version: "1.0.0",
            adapter: &adapter_v1,
        },
    )?;
    let rolled_back = discover_and_invoke(&workenv, root.path(), "1.0.0")?;
    println!(
        "rolled_back_version=1.0.0 executable={}",
        rolled_back.executable
    );
    assert_eq!(rolled_back.executable, first.executable);
    assert_core_unchanged(&workenv, &original_core_hash)?;
    Ok(())
}

struct InvocationResult {
    executable: String,
}

fn discover_and_invoke(
    workenv: &Path,
    root: &Path,
    expected_version: &str,
) -> Result<InvocationResult> {
    let extensions = extension_list(workenv, root)?;
    assert!(
        extensions.get("example.independent").is_some(),
        "{extensions}"
    );

    let inspected = workenv_json(
        workenv,
        root,
        ["extension", "inspect", "example.independent"],
    )?;
    assert_eq!(inspected["version"], expected_version);
    let executable = inspected["executable"]
        .as_str()
        .context("inspect omitted executable")?
        .to_owned();

    let checked = workenv_json(workenv, root, ["extension", "check"])?;
    assert_eq!(checked["ok"], true, "{checked}");

    let called = workenv_json(
        workenv,
        root,
        [
            "extension",
            "call",
            "example.independent",
            "inspect",
            "--environment",
            "test",
            "--input",
            "{}",
        ],
    )?;
    assert_eq!(called["status"], "ready", "{called}");
    assert_eq!(called["data"]["message"], "independent native extension");
    assert_eq!(called["data"]["environment"], "test");
    assert_eq!(called["data"]["executable"], executable);
    Ok(InvocationResult { executable })
}

fn extension_list(workenv: &Path, root: &Path) -> Result<Value> {
    workenv_json(workenv, root, ["extension", "list"])
}

fn workenv_json<const N: usize>(workenv: &Path, root: &Path, args: [&str; N]) -> Result<Value> {
    let mut command_args: Vec<String> = args.into_iter().map(ToOwned::to_owned).collect();
    command_args.extend([
        "--root".to_owned(),
        root.to_string_lossy().into_owned(),
        "--format".to_owned(),
        "json".to_owned(),
    ]);
    let output = Command::new(workenv)
        .args(command_args)
        .env("PATH", fixture_path(root))
        .output()
        .with_context(|| format!("run {}", workenv.display()))?;
    parse_json_output(&output)
}

fn parse_json_output(output: &Output) -> Result<Value> {
    if !output.status.success() {
        bail!(
            "workenv failed stdout={} stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    serde_json::from_slice(&output.stdout).with_context(|| {
        format!(
            "parse workenv JSON: {}",
            String::from_utf8_lossy(&output.stdout)
        )
    })
}

fn write_manifest(root: &Path, state: &ExtensionState<'_>) -> Result<()> {
    let manifest = json!({
        "schema_version": 1,
        "hosts": {"local": {"system": current_system(), "address": null, "transport": null, "provider": null}},
        "environments": {"test": {
            "host": "local",
            "directory": root,
            "source": "path:.",
            "profiles": [],
            "ephemeral": false,
            "integrations": [],
            "connection": null
        }},
        "extensions": extensions(state),
    });
    let output = json!({"workenv.manifestJSON": serde_json::to_string(&manifest)?});
    fs::write(
        root.join("workenv-manifest-output.json"),
        serde_json::to_vec(&output)?,
    )?;
    fs::write(
        root.join("devenv.nix"),
        "# manifest supplied by test devenv fixture\n",
    )?;
    Ok(())
}

fn extensions(state: &ExtensionState<'_>) -> Value {
    match state {
        ExtensionState::Disabled => json!({}),
        ExtensionState::Enabled { version, adapter } => json!({
            "example.independent": {
                "version": version,
                "protocol_version": 1,
                "executable": adapter,
                "location": "controller",
                "systems": [],
                "operations": {"inspect": {
                    "description": "Inspect the independent adapter",
                    "mutating": false,
                    "internal": false,
                    "input_schema": {"type": "object", "additionalProperties": false},
                    "output_schema": {
                        "type": "object",
                        "required": ["message", "version", "environment", "executable"],
                        "properties": {
                            "message": {"type": "string"},
                            "version": {"type": "string"},
                            "environment": {"type": "string"},
                            "executable": {"type": "string"}
                        },
                        "additionalProperties": false
                    }
                }}
            }
        }),
    }
}

fn install_devenv_fixture(root: &Path) -> Result<()> {
    let bin = root.join("fixture-bin");
    fs::create_dir_all(&bin)?;
    let script = bin.join("devenv");
    fs::write(
        &script,
        r#"#!/bin/sh
if [ "$1" = "eval" ] && [ "$2" = "workenv.manifestJSON" ]; then
  cat "$PWD/workenv-manifest-output.json"
else
  echo "unsupported devenv fixture command: $*" >&2
  exit 2
fi
"#,
    )?;
    make_executable(&script)
}

fn fixture_path(root: &Path) -> String {
    let existing = std::env::var("PATH").unwrap_or_default();
    format!("{}:{existing}", root.join("fixture-bin").display())
}

fn copy_executable(source: &Path, target: &Path) -> Result<PathBuf> {
    fs::copy(source, target).with_context(|| format!("copy {}", source.display()))?;
    make_executable(target)?;
    Ok(target.to_path_buf())
}

fn make_executable(path: &Path) -> Result<()> {
    let mut permissions = fs::metadata(path)?.permissions();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        permissions.set_mode(0o755);
    }
    fs::set_permissions(path, permissions)?;
    Ok(())
}

fn assert_core_unchanged(workenv: &Path, expected: &str) -> Result<()> {
    assert_eq!(sha256_file(workenv)?, expected);
    Ok(())
}

fn sha256_file(path: &Path) -> Result<String> {
    let bytes = fs::read(path).with_context(|| format!("read {}", path.display()))?;
    Ok(format!("{:x}", Sha256::digest(bytes)))
}

fn env_path(name: &str) -> Result<PathBuf> {
    std::env::var_os(name)
        .map(PathBuf::from)
        .context(format!("{name} is required"))
}

fn current_system() -> &'static str {
    match (std::env::consts::ARCH, std::env::consts::OS) {
        ("aarch64", "macos") => "aarch64-darwin",
        ("x86_64", "macos") => "x86_64-darwin",
        ("aarch64", "linux") => "aarch64-linux",
        ("x86_64", "linux") => "x86_64-linux",
        _ => "unknown",
    }
}
