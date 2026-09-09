use anyhow::{Context, Result};
use serde_json::Value;
use workenv_protocol::AdapterRequest;

const DEFAULT_NIX_VERSION: &str = "2.35.2";
const DEFAULT_NIX_INSTALL_URL: &str = "https://releases.nixos.org/nix/nix-2.35.2/install";
const DEFAULT_NIX_INSTALL_SHA256: &str =
    "9adda97297d9e8ab360df95c729eabff4f4f93d6db091953c3a68f29e3fb130c";
const DEFAULT_DEVENV_VERSION: &str = "2.3.0";
const DEFAULT_DEVENV_FLAKE: &str = "github:cachix/devenv/e0781f7bee573eefcab4a7d2788fd9b455560ca2";

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
}

impl SeedTool {
    fn from_value(value: &Value) -> Result<Self> {
        Ok(Self {
            name: required(value, "name", "seed_tools[].name")?.to_owned(),
            source: string(value, "path")
                .or_else(|| string(value, "install_url"))
                .or_else(|| string(value, "url"))
                .context("seed_tools[] requires path, install_url, or url")?
                .to_owned(),
            sha256: required(value, "sha256", "seed_tools[].sha256")?.to_owned(),
        })
    }
}

fn required<'a>(value: &'a Value, key: &str, label: &str) -> Result<&'a str> {
    string(value, key).with_context(|| format!("{label} is required"))
}

fn string<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value.get(key).and_then(Value::as_str)
}
