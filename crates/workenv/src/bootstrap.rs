use crate::{
    context,
    report::{Report, mcp, report},
};
use incurs::command::{CommandDef, TypedContext};
use serde::Deserialize;
use workenv_core::Controller;

#[derive(Deserialize, incurs::Args)]
struct Args {
    /// Environment name declared in the bootstrap seed or devenv configuration.
    environment: String,
}

#[derive(Deserialize, incurs::Options)]
struct Options {
    /// Stable key for this bootstrap attempt.
    idempotency_key: String,
    /// Explicit manifest exported by devenv on a prepared controller, for initial bootstrap only.
    manifest: Option<String>,
}

pub(crate) fn command() -> CommandDef {
    CommandDef::typed::<Args, Options, (), Report, _, _>(
        "bootstrap",
        |input: TypedContext<Args, Options, ()>| async move { report(run(&input)) },
    )
    .description("Bootstrap prerequisites through the explicitly configured adapter.")
    .mcp(mcp(false, false))
    .done()
}

fn run(input: &TypedContext<Args, Options, ()>) -> anyhow::Result<serde_json::Value> {
    let key = context::mutation_key(Some(&input.options.idempotency_key))?;
    let root = context::root(&input.globals)?;
    let controller = match &input.options.manifest {
        Some(path) => {
            let manifest = serde_json::from_slice(&std::fs::read(path)?)?;
            Controller::from_manifest(&root, manifest)?
        }
        None => Controller::load(&root)?,
    };
    controller.environment("bootstrap", &input.args.environment, Some(key))
}
