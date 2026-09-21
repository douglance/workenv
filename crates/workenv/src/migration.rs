use std::path::PathBuf;

use anyhow::Result;
use incurs::command::{CommandDef, TypedContext};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::{
    context::{self, mutation_key},
    report::{Report, mcp, report},
};

mod flags;
mod legacy;
mod profiles;
mod receipt;
mod render;
mod shipped;

#[derive(Deserialize, incurs::Options)]
struct Options {
    /// Write the proposed Nix module to this new file.
    output: Option<String>,
    /// Stable operation key required when --output is supplied.
    idempotency_key: Option<String>,
}

/// Build the migration command.
#[must_use]
pub(crate) fn command() -> CommandDef {
    CommandDef::typed::<(), Options, (), Report, _, _>(
        "migrate",
        |input: TypedContext<(), Options, ()>| async move {
            report(run(&input.globals, input.options))
        },
    )
    .description("Propose a reviewable Nix module from the legacy fleet.json.")
    .mcp(mcp(false, false))
    .done()
}

fn run(globals: &Value, options: Options) -> Result<Value> {
    let root = context::root(globals)?;
    let mut plan = legacy::Plan::load(&root)?;
    if let Some(available) = shipped::extensions(&root)? {
        shipped::prune(&mut plan, &available);
    }
    let proposed_nix = render::module(&plan)?;
    let base = base_report(&root, &plan, &proposed_nix);
    if let Some(output) = options.output {
        let key = mutation_key(options.idempotency_key.as_deref())?;
        return receipt::write(&root, &PathBuf::from(output), key, base);
    }
    Ok(base)
}

fn base_report(root: &std::path::Path, plan: &legacy::Plan, proposed_nix: &str) -> Value {
    json!({
        "ok": true,
        "status": "proposed",
        "root": root,
        "source_fleet": root.join("fleet.json"),
        "profiles_metadata": plan.profiles,
        "proposed_nix": proposed_nix,
        "warnings": plan.warnings,
        "omitted": plan.omitted,
    })
}
