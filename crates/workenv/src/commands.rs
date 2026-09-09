use crate::{
    bootstrap,
    context::{self, Globals},
    environment, extension, migration,
    report::{Report, mcp, report},
};
use incurs::{
    cli::Cli,
    command::{CommandDef, TypedContext},
};

/// Construct the same environment command graph for CLI and MCP clients.
#[must_use]
pub fn build() -> Cli {
    Cli::create("workenv")
        .version(env!("CARGO_PKG_VERSION"))
        .description("Development environment setup through devenv and optional native adapters.")
        .globals::<Globals>()
        .group(environment::commands())
        .group(extension::commands())
        .command("doctor", doctor())
        .command("bootstrap", bootstrap::command())
        .command("migrate", migration::command())
}

fn doctor() -> CommandDef {
    CommandDef::typed::<(), (), (), Report, _, _>(
        "doctor",
        |input: TypedContext<(), (), ()>| async move {
            report(context::controller(&input.globals).and_then(|controller| controller.doctor()))
        },
    )
    .description("Inspect devenv, configuration, and setup prerequisites.")
    .mcp(mcp(true, false))
    .done()
}
