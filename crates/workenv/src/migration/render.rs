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

fn flags(plan: &Plan) -> String {
    [
        (
            plan.flags.contains("exedev"),
            "  workenv.exedev.enable = true;\n",
        ),
        (
            plan.flags.contains("herdr"),
            "  workenv.herdr.enable = true;\n",
        ),
        (
            plan.flags.contains("identity"),
            "  workenv.identity.enable = true;\n",
        ),
        (plan.flags.contains("ssh"), "  workenv.ssh.enable = true;\n"),
    ]
    .into_iter()
    .filter_map(|(enabled, line)| enabled.then_some(line))
    .collect()
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
