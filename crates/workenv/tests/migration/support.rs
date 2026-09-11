use std::{collections::BTreeMap, path::Path};

use anyhow::{Context, Result};
use serde_json::{Value, json};
use workenv_core::Controller;
use workenv_protocol::{Extension, Location, Manifest, Operation};

pub async fn invoke(root: &Path, options: &[&str]) -> Result<(Option<i32>, Value, String)> {
    let mut output = Vec::new();
    let mut argv = vec![
        "--root".to_owned(),
        root.to_string_lossy().into_owned(),
        "--json".to_owned(),
        "migrate".to_owned(),
    ];
    argv.extend(options.iter().map(|arg| (*arg).to_owned()));
    let code = workenv::build()
        .serve_to(argv, &mut output, false)
        .await
        .map_err(|error| anyhow::anyhow!(error.to_string()))?;
    let text = String::from_utf8(output)?;
    let value = serde_json::from_str(&text).unwrap_or_else(|_| json!({}));
    Ok((code, value, text))
}

pub fn manifest_from_proposal(proposed: &str) -> Result<Manifest> {
    Ok(Manifest {
        schema_version: workenv_protocol::PROTOCOL_VERSION,
        hosts: serde_json::from_str(&embedded_json(proposed, "workenv.hosts")?)?,
        environments: serde_json::from_str(&embedded_json(proposed, "workenv.environments")?)?,
        extensions: extension_contracts(),
    })
}

pub fn controller_from_evaluated_manifest(path: &str) -> Result<()> {
    let value: Value = serde_json::from_slice(&std::fs::read(path)?)?;
    let encoded = value
        .get("workenv.manifestJSON")
        .unwrap_or(&value)
        .as_str()
        .context("manifestJSON capture must be a JSON string")?;
    let manifest: Manifest = serde_json::from_str(encoded)?;

    assert_eq!(manifest.hosts.len(), 8);
    assert_eq!(manifest.environments.len(), 10);
    assert_eq!(
        manifest.hosts["workenv-02"].transport.as_deref(),
        Some("workenv.ssh")
    );
    Controller::from_manifest(&std::env::current_dir()?, manifest)?;
    Ok(())
}

pub fn basic_fleet() -> Value {
    json!({
        "schema_version": 1,
        "workers": [{
            "name": "workenv-01",
            "ssh_host": "workenv-01.tail.example",
            "cpus": 2,
            "memory_gb": 8,
            "disk_gb": 50
        }]
    })
}

pub fn write_fleet(root: &Path, value: &Value) -> Result<()> {
    std::fs::write(root.join("fleet.json"), serde_json::to_vec_pretty(value)?)?;
    Ok(())
}

pub fn write_fixture_fleet(root: &Path) -> Result<()> {
    std::fs::write(
        root.join("fleet.json"),
        include_str!("../fixtures/legacy-fleet.json"),
    )?;
    Ok(())
}

pub fn write_profile(root: &Path) -> Result<()> {
    let profiles = root.join("profiles");
    std::fs::create_dir_all(&profiles)?;
    std::fs::write(
        profiles.join("personal.json"),
        include_str!("../fixtures/profiles/personal.json"),
    )?;
    Ok(())
}

pub fn path_source(path: &Path) -> String {
    format!("path:{}", path.to_string_lossy())
}

fn embedded_json(proposed: &str, attribute: &str) -> Result<String> {
    let marker = format!("  {attribute} = builtins.fromJSON ");
    let start = proposed
        .find(&marker)
        .context("missing generated attribute")?
        + marker.len();
    let mut chars = proposed[start..].chars();
    anyhow::ensure!(
        chars.next() == Some('"'),
        "generated JSON is not a Nix string"
    );
    let mut value = String::new();
    while let Some(next) = chars.next() {
        match next {
            '"' => return Ok(value),
            '\\' => value.push(chars.next().context("unterminated Nix escape")?),
            other => value.push(other),
        }
    }
    anyhow::bail!("unterminated generated Nix string")
}

fn extension_contracts() -> BTreeMap<String, Extension> {
    [
        (
            "workenv.exedev".to_owned(),
            extension(Location::Controller, []),
        ),
        ("workenv.herdr".to_owned(), extension(Location::Target, [])),
        (
            "workenv.identity".to_owned(),
            extension(Location::Target, []),
        ),
        (
            "workenv.ssh".to_owned(),
            extension(Location::Controller, [("execute", true)]),
        ),
    ]
    .into()
}

fn extension<const N: usize>(location: Location, operations: [(&str, bool); N]) -> Extension {
    let operations = if operations.is_empty() {
        [("inspect".to_owned(), operation(false))].into()
    } else {
        operations
            .into_iter()
            .map(|(name, internal)| (name.to_owned(), operation(internal)))
            .collect()
    };
    Extension {
        version: "0.2.0".to_owned(),
        protocol_version: workenv_protocol::PROTOCOL_VERSION,
        executable: "/nix/store/workenv-adapter".into(),
        location,
        systems: Vec::new(),
        runtime_inputs: None,
        operations,
    }
}

fn operation(internal: bool) -> Operation {
    Operation {
        description: "test operation".to_owned(),
        location: None,
        mutating: false,
        internal,
        input_schema: json!(true),
        output_schema: json!(true),
    }
}
