use crate::{
    context,
    report::{Report, mcp, report},
};
use incurs::{
    cli::Cli,
    command::{CommandDef, TypedContext},
};
use serde::Deserialize;
use serde_json::json;

#[derive(Deserialize, incurs::Args)]
struct EnvironmentArgs {
    /// Environment name from the devenv configuration.
    environment: String,
}

#[derive(Deserialize, incurs::Options)]
struct Options {
    /// Stable operation key; required for create, apply, destroy, up, and down.
    idempotency_key: Option<String>,
}

pub(crate) fn commands() -> Cli {
    let mut group = Cli::create("environment")
        .description("Prepare and access explicitly configured development environments.")
        .command("list", list())
        .command("connect", crate::connect::command());
    for (name, description, mutating) in [
        ("status", "Inspect environment readiness.", false),
        (
            "plan",
            "Inspect the explicit setup operations for an environment.",
            false,
        ),
        (
            "create",
            "Provision the configured environment resource.",
            true,
        ),
        (
            "apply",
            "Apply devenv configuration and enabled setup integrations.",
            true,
        ),
        (
            "destroy",
            "Destroy an explicitly owned disposable environment resource.",
            true,
        ),
        (
            "up",
            "Provision, apply, and register the configured environment lifecycle.",
            true,
        ),
        (
            "down",
            "Destroy and clean up the configured owned disposable environment lifecycle.",
            true,
        ),
    ] {
        group = group.command(name, operation(name, description, mutating));
    }
    group
}

fn list() -> CommandDef {
    CommandDef::typed::<(), (), (), Report, _, _>(
        "list",
        |input: TypedContext<(), (), ()>| async move {
            report(context::controller(&input.globals).map(
                |controller| json!({"ok":true,"environments":controller.manifest().environments}),
            ))
        },
    )
    .description("List environments declared in devenv.")
    .mcp(mcp(true, false))
    .done()
}

fn operation(name: &'static str, description: &'static str, mutating: bool) -> CommandDef {
    CommandDef::typed::<EnvironmentArgs, Options, (), Report, _, _>(
        name,
        move |input: TypedContext<EnvironmentArgs, Options, ()>| async move {
            report(perform(name, mutating, &input))
        },
    )
    .description(description)
    .mcp(mcp(!mutating, matches!(name, "destroy" | "down")))
    .done()
}

fn perform(
    name: &str,
    mutating: bool,
    input: &TypedContext<EnvironmentArgs, Options, ()>,
) -> anyhow::Result<serde_json::Value> {
    let key = input.options.idempotency_key.as_deref();
    if mutating {
        context::mutation_key(key)?;
    }
    context::controller(&input.globals)?.environment(name, &input.args.environment, key)
}
