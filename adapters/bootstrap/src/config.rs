use anyhow::{Context, Result, bail};
use serde_json::Value;
use std::path::Path;
use workenv_protocol::AdapterRequest;

const DEFAULT_NIX_VERSION: &str = "2.35.2";
const DEFAULT_NIX_INSTALL_URL: &str = "https://releases.nixos.org/nix/nix-2.35.2/install";
const DEFAULT_NIX_INSTALL_SHA256: &str =
    "9adda97297d9e8ab360df95c729eabff4f4f93d6db091953c3a68f29e3fb130c";
const DEFAULT_DEVENV_VERSION: &str = "2.3.0";
const DEFAULT_DEVENV_FLAKE: &str = "github:cachix/devenv/e0781f7bee573eefcab4a7d2788fd9b455560ca2";

#[derive(Clone)]
pub(crate) struct BootstrapConfig {
    pub(crate) prefix: String,
    pub(crate) link_dir: String,
    pub(crate) nix_version: String,
    pub(crate) nix_url: String,
    pub(crate) nix_sha256: String,
    pub(crate) devenv_version: String,
    pub(crate) devenv_flake: String,
    pub(crate) seed_tools: Vec<SeedTool>,
    pub(crate) timeout_ms: u64,
}

impl BootstrapConfig {
    pub(crate) fn from_request(request: &AdapterRequest) -> Result<Self> {
        let nix = request.config.get("nix").unwrap_or(&Value::Null);
        let devenv = request.config.get("devenv").unwrap_or(&Value::Null);
        let seed_tools = request
            .config
            .get("seed_tools")
            .or_else(|| request.config.get("tools"))
            .and_then(Value::as_array)
            .map(|items| items.iter().map(SeedTool::from_value).collect())
            .transpose()?
            .unwrap_or_default();
        Ok(Self {
            prefix: string(&request.config, "prefix")
                .unwrap_or("/opt/workenv")
                .to_owned(),
            link_dir: string(&request.config, "link_dir")
                .unwrap_or("/usr/local/bin")
                .to_owned(),
            nix_version: string(nix, "version")
                .unwrap_or(DEFAULT_NIX_VERSION)
                .to_owned(),
            nix_url: string(nix, "install_url")
                .unwrap_or(DEFAULT_NIX_INSTALL_URL)
                .to_owned(),
            nix_sha256: string(nix, "install_sha256")
                .unwrap_or(DEFAULT_NIX_INSTALL_SHA256)
                .to_owned(),
            devenv_version: string(devenv, "version")
                .unwrap_or(DEFAULT_DEVENV_VERSION)
                .to_owned(),
            devenv_flake: string(devenv, "flake_ref")
                .unwrap_or(DEFAULT_DEVENV_FLAKE)
                .to_owned(),
            seed_tools,
            timeout_ms: request.config["timeout_ms"].as_u64().unwrap_or(900_000),
        })
    }
}

#[derive(Clone)]
pub(crate) struct SeedTool {
    pub(crate) name: String,
    pub(crate) source: String,
    pub(crate) sha256: String,
    pub(crate) controller_path: Option<String>,
}

impl SeedTool {
    fn from_value(value: &Value) -> Result<Self> {
        let name = required(value, "name", "seed_tools[].name")?;
        let sha256 = required(value, "sha256", "seed_tools[].sha256")?;
        validate_seed_name(name)?;
        validate_seed_sha(sha256)?;
        Ok(Self {
            name: name.to_owned(),
            source: string(value, "path")
                .or_else(|| string(value, "install_url"))
                .or_else(|| string(value, "url"))
                .or_else(|| string(value, "controller_path"))
                .context("seed_tools[] requires path, install_url, url, or controller_path")?
                .to_owned(),
            sha256: sha256.to_owned(),
            controller_path: string(value, "controller_path").map(ToOwned::to_owned),
        })
    }
}

fn required<'a>(value: &'a Value, key: &str, label: &str) -> Result<&'a str> {
    string(value, key).with_context(|| format!("{label} is required"))
}

fn string<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value.get(key).and_then(Value::as_str)
}

fn validate_seed_name(name: &str) -> Result<()> {
    if name.is_empty() || name == "." || name == ".." || name.starts_with('-') {
        bail!("seed_tools[].name must be a safe basename");
    }
    let path = Path::new(name);
    if path.components().count() != 1
        || path.file_name().and_then(|part| part.to_str()) != Some(name)
    {
        bail!("seed_tools[].name must be a safe basename");
    }
    Ok(())
}

fn validate_seed_sha(sha: &str) -> Result<()> {
    if sha.len() == 64 && sha.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Ok(());
    }
    bail!("seed_tools[].sha256 must be 64 hex characters")
}
