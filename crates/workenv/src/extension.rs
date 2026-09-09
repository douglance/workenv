use crate::{
    context,
    report::{Report, mcp, report},
};
use incurs::{
    cli::Cli,
    command::{CommandDef, TypedContext},
};
use serde::Deserialize;
use workenv_core::CallOptions;

#[derive(Deserialize, incurs::Args)]
struct ExtensionArgs {
    /// Extension ID from the evaluated devenv configuration.
    extension: String,
}

#[derive(Deserialize, incurs::Args)]
struct CallArgs {
    /// Extension ID.
    extension: String,
    /// Declared public operation name.
    operation: String,
}

#[derive(Deserialize, incurs::Options)]
struct CallFlags {
    /// Target environment name.
    environment: String,
    /// Operation input encoded as JSON.
    input: Option<String>,
    /// Stable key required for operations that change state.
    idempotency_key: Option<String>,
}

pub(crate) fn commands() -> Cli {
    Cli::create("extension")
        .description("Inspect optional devenv modules and call their declared native adapters.")
        .command("list", list())
        .command("inspect", inspect())
        .command("check", check())
        .command("call", call())
}

fn list() -> CommandDef {
    CommandDef::typed::<(), (), (), Report, _, _>(
        "list",
        |input: TypedContext<(), (), ()>| async move {
            report(
                context::controller(&input.globals).and_then(|controller| controller.extensions()),
            )
        },
    )
    .description("List enabled extensions and capabilities.")
    .mcp(mcp(true, false))
    .done()
}

fn inspect() -> CommandDef {
    CommandDef::typed::<ExtensionArgs, (), (), Report, _, _>(
        "inspect",
        |input: TypedContext<ExtensionArgs, (), ()>| async move {
            report(
                context::controller(&input.globals)
                    .and_then(|controller| controller.extension_inspect(&input.args.extension)),
            )
        },
    )
    .description("Read an extension's operation schemas and configuration.")
    .mcp(mcp(true, false))
    .done()
}

fn check() -> CommandDef {
    CommandDef::typed::<(), (), (), Report, _, _>(
        "check",
        |input: TypedContext<(), (), ()>| async move {
            report(
                context::controller(&input.globals)
                    .and_then(|controller| controller.extension_check(None)),
            )
        },
    )
    .description("Validate enabled adapter contracts and executable availability.")
    .mcp(mcp(true, false))
    .done()
}

fn call() -> CommandDef {
    CommandDef::typed::<CallArgs, CallFlags, (), Report, _, _>(
        "call",
        |input: TypedContext<CallArgs, CallFlags, ()>| async move {
            report((|| {
                let body = serde_json::from_str(input.options.input.as_deref().unwrap_or("{}"))?;
                let options = CallOptions {
                    environment: input.options.environment,
                    input: body,
                    key: input.options.idempotency_key,
                };
                context::controller(&input.globals)?.extension_call(
                    &input.args.extension,
                    &input.args.operation,
                    options,
                )
            })())
        },
    )
    .description(
        "Call a schema-declared integration action; internal transport actions are excluded.",
    )
    .mcp(mcp(false, true))
    .done()
}
