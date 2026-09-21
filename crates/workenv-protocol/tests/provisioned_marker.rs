//! The pool's setup script must write the file the providers wait for.
//!
//! The two sides are a Nix string that ends up in a shell script and a Rust
//! constant that both provider adapters poll for. If they drift, nothing fails
//! loudly: every create waits out its whole budget for a file that is never
//! written and comes back pending, and a second `up` does the same. The check
//! reads the Nix source rather than evaluating it, so it fails when the literal
//! in the file changes.
use std::path::PathBuf;

use anyhow::{Context, Result};
use workenv_protocol::PROVISIONED_MARKER;

#[test]
fn the_pool_setup_script_writes_the_marker_the_providers_wait_for() -> Result<()> {
    let mut root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    root.pop();
    root.pop();
    let path = root.join("presets/pool.nix");
    let source =
        std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    let writes = source
        .lines()
        .map(str::trim)
        .filter(|line| line.starts_with("touch "))
        .map(|line| line.trim_start_matches("touch ").trim())
        .collect::<Vec<_>>();
    anyhow::ensure!(
        writes.contains(&PROVISIONED_MARKER),
        "presets/pool.nix touches {writes:?}, but the providers wait for {PROVISIONED_MARKER}"
    );
    Ok(())
}
