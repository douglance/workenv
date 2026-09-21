use crate::{
    bootstrap,
    context::{self, Globals},
    environment, extension, migration,
    report::{Report, mcp, report},
};
use anyhow::Result;
use incurs::{
    cli::Cli,
    command::{CommandDef, TypedContext},
};
use serde_json::{Value, json};
use workenv_core::prerequisites;

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
        |input: TypedContext<(), (), ()>| async move { report(diagnose(&input.globals)) },
    )
    .description("Inspect devenv, configuration, and setup prerequisites.")
    .mcp(mcp(true, false))
    .done()
}

/// What to do about a host that is missing one, written out because this report
/// is most often read by someone meeting the repository for the first time.
const NEXT_WHEN_MISSING: &str = concat!(
    "install the missing executables, or run ",
    "`workenv bootstrap <environment> --manifest <exported-manifest>` ",
    "on a host that has none"
);

/// Prerequisites before configuration, because reading the configuration needs
/// them.
///
/// Building the controller first meant a host without devenv got
/// `executable devenv was not found in PATH` from the command whose entire job
/// is to say what is missing -- no list, no mention of the prerequisite that was
/// present, and no route out. The report is still a failure, so the exit code is
/// unchanged; what changes is that it says something.
fn diagnose(globals: &Value) -> Result<Value> {
    let prerequisites = prerequisites::report();
    let configuration = configuration(globals);
    if !prerequisites::satisfied(&prerequisites) {
        return Ok(json!({
            "ok": false,
            "configuration": configuration,
            "prerequisites": prerequisites,
            "next": NEXT_WHEN_MISSING,
        }));
    }
    let mut diagnosis = context::controller(globals)?.doctor()?;
    diagnosis["configuration"] = configuration;
    diagnosis["prerequisites"] = prerequisites;
    Ok(diagnosis)
}

/// Which configuration root this command would use, and why. Answered without
/// devenv, so it is there even when nothing else can be.
fn configuration(globals: &Value) -> Value {
    match context::resolve(globals) {
        Ok((root, chosen_by)) => json!({ "root": root, "chosen_by": chosen_by }),
        Err(error) => json!({ "root": null, "error": format!("{error:#}") }),
    }
}
