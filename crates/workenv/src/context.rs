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
    resolve(globals).map(|(path, _)| path)
}

/// The configuration root and which of the four sources chose it.
///
/// AX's `ax ctx` answers "which control plane am I talking to". The same
/// question here had no answer: the root came from a flag, the MCP server, an
/// environment variable, or the nearest `devenv.nix` above the working directory
/// -- and the last one changes with wherever the command happens to run. From
/// this repository's root it finds the development shell, which declares no
/// environments, so `environment list` came back empty and looked like a broken
/// fleet rather than the wrong directory.
pub(crate) fn resolve(globals: &Value) -> Result<(PathBuf, &'static str)> {
    let options: Globals = serde_json::from_value(globals.clone())?;
    if let Some(path) = options.root {
        return Ok((Path::new(&path).canonicalize()?, "--root"));
    }
    if let Some(path) = SERVER_ROOT.get() {
        return Ok((path.clone(), "the MCP server's root"));
    }
    if let Some(path) = std::env::var_os("WORKENV_ROOT") {
        return Ok((PathBuf::from(path).canonicalize()?, "WORKENV_ROOT"));
    }
    let cwd = std::env::current_dir()?;
    let found =
        nearest_configuration(&cwd).context("No devenv configuration found; pass --root")?;
    Ok((found, "the nearest devenv.nix above the working directory"))
}

/// The closest directory at or above `start` that holds a `devenv.nix`.
pub(crate) fn nearest_configuration(start: &Path) -> Option<PathBuf> {
    start
        .ancestors()
        .find(|path| path.join("devenv.nix").is_file())
        .map(Path::to_path_buf)
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

#[cfg(test)]
// Test-only, and only these: a fixture that cannot unwrap says less than one
// that panics loudly when the fixture is wrong.
#[allow(clippy::expect_used, clippy::unwrap_used)]
#[path = "context_tests.rs"]
mod tests;
