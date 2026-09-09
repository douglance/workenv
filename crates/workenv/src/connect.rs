use crate::{
    context,
    report::{Report, mcp, report},
};
use anyhow::{Context, Result, ensure};
use incurs::command::{CommandDef, TypedContext};
use serde::Deserialize;
use serde_json::Value;
use std::{io::IsTerminal, process::Command};

#[derive(Deserialize, incurs::Args)]
struct Args {
    /// Environment name from devenv configuration.
    environment: String,
}

#[derive(Deserialize, incurs::Options)]
struct Options {
    /// Return the connection descriptor even in an interactive terminal.
    print: Option<bool>,
}

pub(crate) fn command() -> CommandDef {
    CommandDef::typed::<Args, Options, (), Report, _, _>(
        "connect",
        |input: TypedContext<Args, Options, ()>| async move { report(run(&input)) },
    )
    .description("Enter the configured shell; return a descriptor for MCP or redirected output.")
    .mcp(mcp(true, false))
    .done()
}

fn run(input: &TypedContext<Args, Options, ()>) -> Result<Value> {
    let value = context::controller(&input.globals)?.environment(
        "connect",
        &input.args.environment,
        None,
    )?;
    if input.request.is_none()
        && input.options.print != Some(true)
        && !input.format_explicit
        && std::io::stdin().is_terminal()
        && std::io::stdout().is_terminal()
    {
        enter(&value)?;
    }
    Ok(value)
}

fn enter(value: &Value) -> Result<()> {
    replace_process(&mut connection_command(value)?)
}

fn connection_command(value: &Value) -> Result<Command> {
    ensure!(value["ok"] != false, "connection is not ready");
    ensure!(
        !matches!(
            value["status"].as_str(),
            Some("pending" | "failed" | "unsupported")
        ),
        "connection is not ready"
    );
    ensure!(
        !matches!(
            value.pointer("/response/status").and_then(Value::as_str),
            Some("pending" | "failed" | "unsupported")
        ),
        "connection adapter is not ready"
    );
    let descriptor = value.pointer("/response/data").unwrap_or(value);
    let entries = descriptor["argv"]
        .as_array()
        .or_else(|| descriptor["attach_argv"].as_array())
        .context("connection adapter returned no argv")?;
    let arguments: Vec<&str> = entries
        .iter()
        .map(|item| {
            item.as_str()
                .context("connection argv must contain strings")
        })
        .collect::<Result<_>>()?;
    let (executable, tail) = arguments
        .split_first()
        .context("connection argv is empty")?;
    let mut command = Command::new(executable);
    command.args(tail);
    if let Some(cwd) = descriptor["cwd"].as_str() {
        command.current_dir(cwd);
    }
    Ok(command)
}

#[cfg(unix)]
fn replace_process(command: &mut Command) -> Result<()> {
    use std::os::unix::process::CommandExt;
    // Hand the existing terminal to the client; setup executions remain APoC-owned.
    Err(command.exec()).context("enter environment connection")
}

#[cfg(not(unix))]
fn replace_process(_command: &mut Command) -> Result<()> {
    anyhow::bail!("interactive connection is supported on Linux and macOS")
}

#[cfg(test)]
mod tests {
    use super::connection_command;
    use anyhow::Result;
    use serde_json::json;

    #[test]
    fn connection_preserves_exact_arguments_and_directory() -> Result<()> {
        let command = connection_command(&json!({
            "ok":true,"argv":["ssh","user@example","literal; $HOME"],"cwd":"/tmp"
        }))?;
        assert_eq!(command.get_program(), "ssh");
        assert_eq!(
            command.get_args().collect::<Vec<_>>(),
            ["user@example", "literal; $HOME"]
        );
        assert_eq!(
            command.get_current_dir(),
            Some(std::path::Path::new("/tmp"))
        );
        Ok(())
    }

    #[test]
    fn uncertain_or_malformed_connections_cannot_enter_a_client() {
        for value in [
            json!({"status":"pending","argv":["ssh","user@example"]}),
            json!({"response":{"status":"failed","data":{"attach_argv":["herdr"]}}}),
            json!({"argv":[]}),
            json!({"argv":["ssh",7]}),
        ] {
            assert!(connection_command(&value).is_err());
        }
    }
}
