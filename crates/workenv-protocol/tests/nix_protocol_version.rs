//! The Nix modules and this crate must agree on the protocol version.
//!
//! `modules/base.nix` sets `workenv.manifest.schema_version` and defaults every
//! extension's `protocol_version`. `adapter.rs` sends the manifest's
//! `schema_version` as each request's `protocol_version`, and `server.rs`
//! refuses a request whose `protocol_version` is not `PROTOCOL_VERSION`. So a
//! divergence between the Nix literal and the Rust constant does not degrade
//! anything gracefully -- every adapter call fails, for every extension, on
//! every host.
//!
//! Nothing covered that. Measured: editing `schema_version = 1` to `2` left all
//! 265 workspace tests green and all five Nix suites green, because each side
//! only ever checked its own copy. The two sides are a Nix file and a Rust
//! constant, which is exactly the independence a check like this needs -- an
//! assertion written in Nix against a number also written in Nix proves only
//! that the modules agree with themselves.
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use workenv_protocol::PROTOCOL_VERSION;

/// Repository root, from this crate's manifest directory.
fn repository_root() -> PathBuf {
    let mut root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    root.pop();
    root.pop();
    root
}

/// The integer assigned to `name` in a Nix file, ignoring option declarations.
///
/// Deliberately not a Nix evaluation: this test must fail when the literal in
/// the file changes, and evaluating the module set would reintroduce the
/// same-source problem by asking Nix what Nix thinks.
fn nix_assignment(source: &str, name: &str) -> Result<Vec<u32>> {
    let needle = format!("{name} = ");
    let found: Vec<u32> = source
        .lines()
        .map(str::trim)
        .filter(|line| line.starts_with(&needle))
        .filter_map(|line| {
            line[needle.len()..]
                .trim_end_matches(';')
                .trim()
                .parse::<u32>()
                .ok()
        })
        .collect();
    if found.is_empty() {
        bail!("no integer assignment to `{name}` found; the declaration moved");
    }
    Ok(found)
}

#[test]
fn base_nix_declares_the_protocol_version_this_crate_implements() -> Result<()> {
    let path = repository_root().join("modules").join("base.nix");
    let source =
        std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;

    // The manifest's own version, which becomes every request's
    // `protocol_version` by way of adapter.rs.
    let schema_versions = nix_assignment(&source, "schema_version")?;
    for declared in &schema_versions {
        assert_eq!(
            *declared, PROTOCOL_VERSION,
            "modules/base.nix declares schema_version {declared} but \
             workenv_protocol::PROTOCOL_VERSION is {PROTOCOL_VERSION}; every \
             adapter call would be refused by the protocol check in server.rs"
        );
    }

    // And the per-extension default, which a module may override but which no
    // module in this repository does.
    let defaults = nix_assignment(&source, "default")?;
    assert!(
        defaults.contains(&PROTOCOL_VERSION),
        "no `default = {PROTOCOL_VERSION};` in modules/base.nix; the \
         protocol_version option default no longer matches \
         workenv_protocol::PROTOCOL_VERSION"
    );
    Ok(())
}
