//! Every adapter module declares the executable all of them actually run.
//!
//! `runtime_inputs` is consulted by `adapter_invocation::runs_unwrapped`, which
//! skips the devenv shell only when every declared name resolves on PATH. The
//! list is therefore a safety mechanism, and an incomplete one fails silently:
//! nine of the ten adapter modules did not declare `apoc`, while every shell-out
//! in every adapter goes through `Command::new("apoc")` in
//! workenv-platform/src/execution_code.rs. Because `/usr/bin/ssh` resolves on
//! every macOS and Linux host, the providers took the unwrapped path
//! unconditionally, so a controller without `apoc` on PATH got an opaque failure
//! instead of the shell.
//!
//! The executor's name is read out of the Rust that calls it rather than written
//! here, so the two sides of this check come from different files. Asserting a
//! literal "apoc" against the Nix would only prove the modules agree with a
//! string in this test.
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

fn repository_root() -> PathBuf {
    let mut root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    root.pop();
    root.pop();
    root
}

/// The executable every adapter shell-out is launched through.
fn executor_name(root: &Path) -> Result<String> {
    let path = root.join("crates/workenv-platform/src/execution_code.rs");
    let source =
        std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    let marker = "Command::new(\"";
    let start = source
        .find(marker)
        .with_context(|| format!("no Command::new(\"..\") in {}", path.display()))?
        + marker.len();
    let rest = &source[start..];
    let end = rest
        .find('"')
        .context("unterminated Command::new literal")?;
    Ok(rest[..end].to_owned())
}

/// The `runtime_inputs` list a module declares, if it declares one.
fn declared_inputs(source: &str) -> Option<Vec<String>> {
    let start = source.find("runtime_inputs = [")? + "runtime_inputs = [".len();
    let rest = &source[start..];
    let end = rest.find(']')?;
    Some(
        rest[..end]
            .split('"')
            .skip(1)
            .step_by(2)
            .map(ToOwned::to_owned)
            .collect(),
    )
}

/// Adapter directory names, which are also their module file stems.
fn adapter_names(root: &Path) -> Result<Vec<String>> {
    let adapters = root.join("adapters");
    let mut names = Vec::new();
    for entry in
        std::fs::read_dir(&adapters).with_context(|| format!("reading {}", adapters.display()))?
    {
        let entry = entry?;
        if entry.path().is_dir() {
            names.push(entry.file_name().to_string_lossy().into_owned());
        }
    }
    names.sort();
    Ok(names)
}

#[test]
fn every_adapter_module_declares_the_executor_it_runs_through() -> Result<()> {
    let root = repository_root();
    let executor = executor_name(&root)?;
    let mut checked = Vec::new();
    let mut missing = Vec::new();

    for name in adapter_names(&root)? {
        let module = root.join("modules").join(format!("{name}.nix"));
        let source = std::fs::read_to_string(&module)
            .with_context(|| format!("reading {}", module.display()))?;
        // A module with no list at all is "not stated", which keeps the shell and
        // is always safe; a module that does state one must state this.
        let Some(inputs) = declared_inputs(&source) else {
            continue;
        };
        checked.push(name.clone());
        if !inputs.contains(&executor) {
            missing.push(format!("{name} declares {inputs:?}"));
        }
    }

    if !missing.is_empty() {
        bail!(
            "these modules run every command through `{executor}` but do not declare it: {missing:#?}"
        );
    }
    // Neither side vacuous: an empty sweep would satisfy the check above, and a
    // parser that silently returned no names would too.
    // Eight adapters since Orchard replaced Lima and the herdr wrapper went with it.
    // This guard fired when they were removed, which is what it is for.
    if checked.len() < 8 {
        bail!(
            "only {} modules were checked, so the sweep is not covering the adapter set: {checked:?}",
            checked.len()
        );
    }
    if executor != "apoc" {
        bail!("the executor this repository uses changed to {executor}");
    }
    Ok(())
}
