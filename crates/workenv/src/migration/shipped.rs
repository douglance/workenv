//! Which extensions this repository can actually provide.
//!
//! The migration used to emit whatever the legacy manifest named. `herdr` was
//! deleted along with the Lima substrate, and `migrate` went on proposing
//! `workenv.herdr.enable = true;` plus a herdr binding on every environment --
//! with `ok: true` and an empty warning list. The proposal is meant to be
//! reviewable, and a module that cannot evaluate is not reviewable.
//!
//! The set is read from `modules/` rather than written here, because a list
//! written here is the same hand-kept list that went stale in the first place.
use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
};

use anyhow::{Context as _, Result};
use serde_json::json;
use workenv_protocol::Binding;

use super::legacy::Plan;

/// Extension IDs the modules directory declares, as `workenv.<name>`.
///
/// `None` when the root carries no modules directory. A configuration root is
/// not required to be this repository -- it may import the modules from
/// somewhere else entirely -- and a root that cannot answer the question must
/// leave the proposal alone rather than prune everything out of it.
pub(super) fn extensions(root: &Path) -> Result<Option<BTreeSet<String>>> {
    let modules = root.join("modules");
    if !modules.is_dir() {
        return Ok(None);
    }
    let mut found = BTreeSet::new();
    let entries =
        std::fs::read_dir(&modules).with_context(|| format!("reading {}", modules.display()))?;
    for entry in entries {
        let path: PathBuf = entry?.path();
        if path.extension().is_some_and(|ext| ext == "nix")
            && let Some(stem) = path.file_stem().and_then(|stem| stem.to_str())
        {
            found.insert(format!("workenv.{stem}"));
        }
    }
    Ok(Some(found))
}

/// Drop every binding naming an extension this repository does not ship, and
/// record each drop so the proposal says what did not carry over.
pub(super) fn prune(plan: &mut Plan, shipped: &BTreeSet<String>) {
    let mut dropped = BTreeSet::new();
    for environment in plan.environments.values_mut() {
        environment
            .integrations
            .retain(|binding| keep(binding, shipped, &mut dropped));
        if !environment
            .connection
            .as_ref()
            .is_none_or(|binding| keep(binding, shipped, &mut dropped))
        {
            environment.connection = None;
        }
    }
    for host in plan.hosts.values_mut() {
        if !host
            .provider
            .as_ref()
            .is_none_or(|binding| keep(binding, shipped, &mut dropped))
        {
            host.provider = None;
        }
    }
    for extension in &dropped {
        // The enable flags are short names; the bindings are full extension IDs.
        plan.flags
            .disable(extension.strip_prefix("workenv.").unwrap_or(extension));
        plan.omitted.push(json!({
            "artifact": extension,
            "reason": "the legacy fleet named an extension this repository no longer ships"
        }));
    }
}

fn keep(binding: &Binding, shipped: &BTreeSet<String>, dropped: &mut BTreeSet<String>) -> bool {
    if shipped.contains(&binding.extension) {
        return true;
    }
    dropped.insert(binding.extension.clone());
    false
}

#[cfg(test)]
// Test-only, and only these: a pruning test that cannot unwrap the repository
// root it reads says less than one that panics loudly when the layout moved.
#[allow(clippy::expect_used, clippy::unwrap_used)]
#[path = "shipped_tests.rs"]
mod tests;
