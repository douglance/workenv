use anyhow::Result;
use workenv_protocol::{PROTOCOL_VERSION, Target};

use super::*;

/// Replays canned answers keyed by `(site, verb)` and records every call.
struct FakeRunner {
    answers: Vec<((String, String), BackendResult<Value>)>,
    calls: Vec<(String, String)>,
}

impl FakeRunner {
    fn new(answers: Vec<((&str, &str), BackendResult<Value>)>) -> Self {
        Self {
            answers: answers
                .into_iter()
                .map(|((site, verb), value)| ((site.to_owned(), verb.to_owned()), value))
                .collect(),
            calls: vec![],
        }
    }
}

impl SiteRunner for FakeRunner {
    fn run(&mut self, site: &Site, args: &[String]) -> BackendResult<Value> {
        let verb = args.first().cloned().unwrap_or_default();
        self.calls.push((site.name.clone(), verb.clone()));
        let key = (site.name.clone(), verb);
        self.answers
            .iter()
            .find(|(candidate, _)| *candidate == key)
            .map_or_else(
                || Err(format!("no canned answer for {key:?}")),
                |(_, value)| value.clone(),
            )
    }
}

pub(super) fn capacity(systems: &[&str], free_cpus: u64, free_mem: u64, warm: u64) -> Value {
    json!({
        "systems": systems,
        "free":  {"cpus": free_cpus, "memory_gb": free_mem, "disk_gb": 100, "units": warm},
        "total": {"cpus": 8, "memory_gb": 16, "disk_gb": 200, "units": 4},
        "warm": warm
    })
}

fn placement(native: &str) -> Value {
    json!({"vm_name": native, "ssh_dest": format!("lima-{native}"),
           "instance_identity": {"claim_uuid": "8f1c", "boot_id": "3f2a"}})
}

pub(super) fn request(operation: &str, input: Value) -> AdapterRequest {
    AdapterRequest {
        protocol_version: PROTOCOL_VERSION,
        request_id: "key".into(),
        extension: "workenv.pool".into(),
        operation: operation.into(),
        config: json!({
            "tailnet_suffix": "example.ts.net",
            "tailnet_user": "exedev",
            "required_system": "aarch64-linux",
            "shape": {"cpus": 2, "memory_gb": 4, "disk_gb": 30},
            "backends": [
                {"site": "box-03", "run": ["ssh", "box-03", "workenv-lima"]},
                {"site": "dal",  "run": ["workenv-exedev"]}
            ]
        }),
        input,
        previous: None,
        target: Target {
            environment: "myproject".into(),
            host: "myproject".into(),
            address: Some("exedev@myproject.example.ts.net".into()),
            directory: "/home/exedev/workenv".into(),
            system: "aarch64-linux".into(),
            source: ".".into(),
            profiles: vec![],
        },
    }
}

pub(super) fn drive(
    req: &AdapterRequest,
    answers: Vec<((&str, &str), BackendResult<Value>)>,
) -> Result<(AdapterResponse, Vec<(String, String)>)> {
    let resolved = spec(req)?;
    let mut provider = Provider::new(FakeRunner::new(answers));
    let response = handle_with(req, &resolved, &mut provider);
    let calls = provider.runner.calls.clone();
    Ok((response, calls))
}

fn enrolled() -> Value {
    json!({"dns_name": "myproject.example.ts.net", "device_id": "nodeid-1"})
}

#[test]
fn places_on_the_site_with_the_most_headroom() -> Result<()> {
    let req = request("create", json!({}));
    let (response, calls) = drive(
        &req,
        vec![
            (
                ("box-03", "capacity"),
                Ok(capacity(&["aarch64-linux"], 3, 5, 1)),
            ),
            (
                ("dal", "capacity"),
                Ok(capacity(&["aarch64-linux"], 8, 16, 2)),
            ),
            (("dal", "place"), Ok(placement("wkv-09"))),
            (("dal", "enroll"), Ok(enrolled())),
        ],
    )?;
    assert_eq!(response.status, ResponseStatus::Changed);
    assert_eq!(response.data["owned"], json!(true));
    assert_eq!(response.data["resource_id"], "myproject");
    assert_eq!(response.data["instance_identity"]["site"], "dal");
    assert!(
        !calls
            .iter()
            .any(|(site, verb)| site == "box-03" && verb == "place")
    );
    Ok(())
}

#[test]
fn the_address_does_not_depend_on_where_it_landed() -> Result<()> {
    let req = request("create", json!({}));
    let (response, _) = drive(
        &req,
        vec![
            (
                ("box-03", "capacity"),
                Ok(capacity(&["aarch64-linux"], 8, 16, 2)),
            ),
            (
                ("dal", "capacity"),
                Ok(capacity(&["aarch64-linux"], 3, 5, 1)),
            ),
            (("box-03", "place"), Ok(placement("wkv-01"))),
            (("box-03", "enroll"), Ok(enrolled())),
        ],
    )?;
    assert_eq!(response.data["instance_identity"]["site"], "box-03");
    assert_eq!(
        response.data["address"],
        "exedev@myproject.example.ts.net"
    );
    Ok(())
}

#[test]
fn a_site_serving_another_architecture_is_not_considered() -> Result<()> {
    let req = request("create", json!({}));
    let (response, calls) = drive(
        &req,
        vec![
            (
                ("box-03", "capacity"),
                Ok(capacity(&["aarch64-linux"], 8, 16, 2)),
            ),
            (
                ("dal", "capacity"),
                Ok(capacity(&["x86_64-linux"], 8, 16, 4)),
            ),
            (("box-03", "place"), Ok(placement("wkv-01"))),
            (("box-03", "enroll"), Ok(enrolled())),
        ],
    )?;
    assert_eq!(response.data["instance_identity"]["site"], "box-03");
    assert!(
        !calls
            .iter()
            .any(|(site, verb)| site == "dal" && verb == "place")
    );
    Ok(())
}

#[test]
fn no_capacity_reports_every_candidates_numbers() -> Result<()> {
    let req = request("create", json!({}));
    let (response, calls) = drive(
        &req,
        vec![
            (
                ("box-03", "capacity"),
                Ok(capacity(&["aarch64-linux"], 1, 1, 0)),
            ),
            (
                ("dal", "capacity"),
                Ok(capacity(&["aarch64-linux"], 0, 0, 0)),
            ),
        ],
    )?;
    assert_eq!(response.status, ResponseStatus::Failed);
    assert_eq!(response.data["status"], "no_capacity");
    let candidates = response.data["candidates"]
        .as_array()
        .map_or(&[][..], |v| v);
    assert_eq!(candidates.len(), 2);
    assert!(candidates.iter().all(|c| c["eligible"] == json!(false)));
    assert!(candidates.iter().all(|c| !c["reason"].is_null()));
    assert!(!calls.iter().any(|(_, verb)| verb == "place"));
    Ok(())
}

#[test]
fn an_unreachable_site_is_pending_not_a_failover() -> Result<()> {
    let req = request("create", json!({}));
    let (response, calls) = drive(
        &req,
        vec![
            (("box-03", "capacity"), Err("host unreachable".into())),
            (
                ("dal", "capacity"),
                Ok(capacity(&["aarch64-linux"], 8, 16, 2)),
            ),
        ],
    )?;
    assert_eq!(response.status, ResponseStatus::Pending);
    assert_eq!(response.data["probe_errors"][0]["site"], "box-03");
    assert!(!calls.iter().any(|(_, verb)| verb == "place"));
    Ok(())
}

#[test]
fn a_suffixed_tailnet_name_releases_rather_than_returning_a_broken_address() -> Result<()> {
    let req = request("create", json!({}));
    let (response, calls) = drive(
        &req,
        vec![
            (
                ("box-03", "capacity"),
                Ok(capacity(&["aarch64-linux"], 8, 16, 2)),
            ),
            (
                ("dal", "capacity"),
                Ok(capacity(&["aarch64-linux"], 1, 1, 0)),
            ),
            (("box-03", "place"), Ok(placement("wkv-01"))),
            (
                ("box-03", "enroll"),
                Ok(json!({"dns_name": "myproject-1.example.ts.net"})),
            ),
            (("box-03", "release"), Ok(json!({"status": "destroyed"}))),
        ],
    )?;
    assert_eq!(response.status, ResponseStatus::Failed);
    assert_eq!(
        response.data["reported_dns"],
        "myproject-1.example.ts.net"
    );
    assert!(
        calls
            .iter()
            .any(|(site, verb)| site == "box-03" && verb == "release")
    );
    Ok(())
}
