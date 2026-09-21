use anyhow::Result;
use serde::Serialize;

use super::legacy::Plan;

pub(super) fn module(plan: &Plan) -> Result<String> {
    let hosts = json_nix(&plan.hosts)?;
    let environments = json_nix(&plan.environments)?;
    let flags = flags(plan);
    Ok(format!(
        "{{ ... }}:\n\n{{\n{flags}\n  workenv.hosts = builtins.fromJSON {hosts};\n  workenv.environments = builtins.fromJSON {environments};\n}}\n"
    ))
}

/// One line per enabled flag, read from the flag set rather than from a list
/// kept here. The list that used to live here named `herdr`, which the
/// repository no longer ships, and nothing noticed.
fn flags(plan: &Plan) -> String {
    plan.flags
        .names()
        .map(|name| format!("  workenv.{name}.enable = true;\n"))
        .collect::<Vec<_>>()
        .concat()
}

fn json_nix(value: &impl Serialize) -> Result<String> {
    let json = serde_json::to_string_pretty(value)?;
    Ok(format!("\"{}\"", nix_double_string(&json)))
}

fn nix_double_string(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    let mut chars = value.chars().peekable();
    while let Some(next) = chars.next() {
        match next {
            '\\' => escaped.push_str("\\\\"),
            '"' => escaped.push_str("\\\""),
            '$' if chars.peek() == Some(&'{') => escaped.push_str("\\$"),
            _ => escaped.push(next),
        }
    }
    escaped
}
