use anyhow::{Context, Result, bail};
use serde::Deserialize;
use serde_json::Value;
use std::{
    path::{Path, PathBuf},
    sync::OnceLock,
};
use workenv_core::Controller;

static SERVER_ROOT: OnceLock<PathBuf> = OnceLock::new();

#[derive(Deserialize, incurs::Options)]
pub(crate) struct Globals {
    /// Directory containing the devenv configuration.
    pub root: Option<String>,
}

/// Bind the MCP server to its explicitly selected configuration root.
///
/// # Errors
/// Returns an error if the root cannot be resolved or was already set.
pub fn set_server_root(root: &Path) -> Result<()> {
    SERVER_ROOT
        .set(root.canonicalize()?)
        .map_err(|_| anyhow::anyhow!("MCP root already set"))
}

pub(crate) fn root(globals: &Value) -> Result<PathBuf> {
    let options: Globals = serde_json::from_value(globals.clone())?;
    if let Some(path) = options.root {
        return Ok(Path::new(&path).canonicalize()?);
    }
    if let Some(path) = SERVER_ROOT.get() {
        return Ok(path.clone());
    }
    if let Some(path) = std::env::var_os("WORKENV_ROOT") {
        return Ok(PathBuf::from(path).canonicalize()?);
    }
    let cwd = std::env::current_dir()?;
    cwd.ancestors()
        .find(|path| path.join("devenv.nix").is_file())
        .map(Path::to_path_buf)
        .context("No devenv configuration found; pass --root")
}

pub(crate) fn controller(globals: &Value) -> Result<Controller> {
    Controller::load(&root(globals)?)
}

pub(crate) fn mutation_key(key: Option<&str>) -> Result<&str> {
    match key {
        Some(key) if !key.trim().is_empty() => Ok(key),
        _ => bail!("--idempotency-key is required; reuse the key to inspect the same operation"),
    }
}
