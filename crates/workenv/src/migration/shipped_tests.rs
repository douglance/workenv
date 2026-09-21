use std::collections::BTreeSet;

use serde_json::json;
use workenv_protocol::{Binding, Environment, Host};

use super::*;

fn set(names: &[&str]) -> BTreeSet<String> {
    names.iter().map(|name| (*name).to_owned()).collect()
}

fn enabled(plan: &Plan) -> Vec<&'static str> {
    plan.flags.names().copied().collect()
}

fn binding(extension: &str) -> Binding {
    Binding {
        extension: extension.to_owned(),
        config: json!({}),
    }
}

fn plan_with(integration: &str, connection: &str, provider: &str) -> Plan {
    let mut plan = Plan::empty();
    plan.flags.enable("ssh");
    plan.environments.insert(
        "one".to_owned(),
        Environment {
            host: "box".to_owned(),
            directory: "/tmp/one".into(),
            source: "path:/tmp".to_owned(),
            profiles: Vec::new(),
            ephemeral: false,
            integrations: vec![binding(integration), binding("workenv.ssh")],
            connection: Some(binding(connection)),
            agent: None,
        },
    );
    plan.hosts.insert(
        "box".to_owned(),
        Host {
            address: Some("user@box".to_owned()),
            transport: Some("workenv.ssh".to_owned()),
            provider: Some(binding(provider)),
            system: "x86_64-linux".to_owned(),
        },
    );
    plan
}

#[test]
fn the_shipped_set_is_read_from_the_modules_directory() -> anyhow::Result<()> {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(std::path::Path::parent)
        .expect("repository root");
    let found = extensions(root)?.expect("this repository carries a modules directory");
    // Derived, so the assertion is about the rule rather than about a count:
    // a module that exists must appear, and one that was deleted must not.
    assert!(found.contains("workenv.ssh"), "{found:?}");
    assert!(!found.contains("workenv.herdr"), "{found:?}");
    Ok(())
}

#[test]
fn a_root_with_no_modules_directory_answers_nothing_rather_than_an_empty_set() -> anyhow::Result<()>
{
    // An empty set would prune every binding out of the proposal, which is the
    // opposite of leaving a root alone that cannot answer the question.
    let elsewhere = tempfile::tempdir()?;
    assert!(extensions(elsewhere.path())?.is_none());
    Ok(())
}

#[test]
fn a_binding_naming_an_unshipped_extension_is_dropped_and_recorded() {
    let mut plan = plan_with("workenv.herdr", "workenv.herdr", "workenv.lima");
    plan.flags.enable("herdr");
    prune(&mut plan, &set(&["workenv.ssh"]));

    let environment = &plan.environments["one"];
    assert_eq!(environment.integrations.len(), 1);
    assert_eq!(environment.integrations[0].extension, "workenv.ssh");
    assert!(environment.connection.is_none());
    assert!(plan.hosts["box"].provider.is_none());

    let omitted: Vec<&str> = plan
        .omitted
        .iter()
        .filter_map(|entry| entry["artifact"].as_str())
        .collect();
    assert_eq!(omitted, vec!["workenv.herdr", "workenv.lima"]);
    assert_eq!(enabled(&plan), vec!["ssh"]);
}

#[test]
fn a_plan_naming_only_shipped_extensions_is_left_alone() {
    let mut plan = plan_with("workenv.ssh", "workenv.ssh", "workenv.ssh");
    prune(&mut plan, &set(&["workenv.ssh"]));

    assert_eq!(plan.environments["one"].integrations.len(), 2);
    assert!(plan.environments["one"].connection.is_some());
    assert!(plan.hosts["box"].provider.is_some());
    assert!(plan.omitted.is_empty());
    assert_eq!(enabled(&plan), vec!["ssh"]);
}
