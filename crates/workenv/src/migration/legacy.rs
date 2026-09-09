use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

use anyhow::{Context as _, Result};
use serde_json::{Value, json};
use workenv_platform::read_json;
use workenv_protocol::{Binding, Environment, Host};

use super::{flags::ExtensionFlags, profiles};
use model::{Fleet, FleetHost, Worker};

mod model;

const DEFAULT_REMOTE_ROOT: &str = "/home/exedev/workenv";
const EXEDEV_EXTENSION: &str = "workenv.exedev";
const HERDR_EXTENSION: &str = "workenv.herdr";
const IDENTITY_EXTENSION: &str = "workenv.identity";
const SSH_EXTENSION: &str = "workenv.ssh";

#[derive(Clone, Debug)]
pub(super) struct Plan {
    pub hosts: BTreeMap<String, Host>,
    pub environments: BTreeMap<String, Environment>,
    pub profiles: Vec<Value>,
    pub warnings: Vec<Value>,
    pub omitted: Vec<Value>,
    pub flags: ExtensionFlags,
}

impl Plan {
    pub fn load(root: &Path) -> Result<Self> {
        let fleet: Fleet = read_json(&root.join("fleet.json"))?;
        let profiles = profiles::read(root)?;
        let mut plan = Self::new(profiles);
        plan.add_existing_hosts(&fleet);
        for worker in &fleet.workers {
            plan.add_worker(root, &fleet, worker)?;
        }
        plan.add_omissions(&fleet);
        Ok(plan)
    }

    fn new(profiles: Vec<Value>) -> Self {
        Self {
            hosts: BTreeMap::new(),
            environments: BTreeMap::new(),
            profiles,
            warnings: Vec::new(),
            omitted: Vec::new(),
            flags: ExtensionFlags::default(),
        }
    }

    fn add_existing_hosts(&mut self, fleet: &Fleet) {
        for (name, host) in &fleet.hosts {
            self.add_existing_host(name, host);
        }
    }

    fn add_existing_host(&mut self, name: &str, host: &FleetHost) {
        let system = system_for_host(name, host, &mut self.warnings);
        let (transport, address) = match host.transport.as_str() {
            "local" => (None, None),
            "ssh" => {
                self.flags.enable("ssh");
                (Some(SSH_EXTENSION.to_owned()), host.target.clone())
            }
            other => {
                self.warn(name, &format!("unsupported legacy transport {other}"));
                (Some(other.to_owned()), host.target.clone())
            }
        };
        self.hosts.insert(
            name.to_owned(),
            Host {
                address,
                transport,
                provider: None,
                system,
            },
        );
    }

    fn add_worker(&mut self, root: &Path, fleet: &Fleet, worker: &Worker) -> Result<()> {
        let host = worker
            .host
            .clone()
            .unwrap_or_else(|| self.add_exedev_host(fleet, worker));
        let directory = worker_directory(fleet, worker)?;
        let session = worker_session(fleet, worker);
        self.flags.enable("herdr");
        let source = source_for_worker(root, fleet, worker);
        let integrations = self.integrations(root, worker, &session);
        self.environments.insert(
            worker.name.clone(),
            Environment {
                host,
                directory,
                source,
                profiles: Vec::new(),
                ephemeral: worker.lifetime.as_deref() == Some("ephemeral"),
                integrations,
                connection: Some(binding(HERDR_EXTENSION, json!({ "session": session }))),
            },
        );
        Ok(())
    }

    fn add_exedev_host(&mut self, fleet: &Fleet, worker: &Worker) -> String {
        self.flags.enable("exedev");
        self.flags.enable("ssh");
        let user = fleet.remote_user.as_deref().unwrap_or("exedev");
        let address = worker.ssh_host.as_ref().map_or_else(
            || format!("{user}@{}.exe.xyz", worker.name),
            |host| format!("{user}@{host}"),
        );
        self.hosts
            .insert(worker.name.clone(), exedev_host(fleet, worker, address));
        worker.name.clone()
    }

    fn integrations(&mut self, root: &Path, worker: &Worker, session: &str) -> Vec<Binding> {
        let mut integrations = vec![binding(HERDR_EXTENSION, json!({ "session": session }))];
        if !self.profiles.is_empty() || worker.profile.is_some() {
            self.flags.enable("identity");
            integrations.push(binding(IDENTITY_EXTENSION, identity_config(root, worker)));
        }
        integrations
    }

    fn add_omissions(&mut self, fleet: &Fleet) {
        if fleet.projects.is_some() {
            self.omitted.push(json!({
                "artifact": "projects",
                "reason": "legacy project task templates are left in fleet.json for review"
            }));
        }
        if fleet.tasks.is_some() {
            self.omitted.push(json!({
                "artifact": "tasks",
                "reason": "legacy task state is runtime data and is not converted"
            }));
        }
    }

    fn warn(&mut self, name: &str, message: &str) {
        self.warnings
            .push(json!({ "host": name, "warning": message }));
    }
}

fn identity_config(root: &Path, worker: &Worker) -> Value {
    let mut config = json!({ "profiles_root": root.join("profiles") });
    if let Some(profile) = &worker.profile {
        config["profile"] = json!(profile);
    }
    config
}

fn exedev_host(fleet: &Fleet, worker: &Worker, address: String) -> Host {
    Host {
        address: Some(address),
        transport: Some(SSH_EXTENSION.to_owned()),
        provider: Some(binding(
            EXEDEV_EXTENSION,
            json!({
                "name": &worker.name,
                "adopt": true,
                "owned": false,
                "resource_created": false,
                "region": fleet.region.as_deref().unwrap_or(""),
                "cpus": worker.cpus,
                "memory_gb": worker.memory_gb,
                "disk_gb": worker.disk_gb
            }),
        )),
        system: "x86_64-linux".to_owned(),
    }
}

fn source_for_worker(root: &Path, fleet: &Fleet, worker: &Worker) -> String {
    let Some(fleet_host) = worker
        .host
        .as_deref()
        .and_then(|name| fleet.hosts.get(name))
    else {
        return path_source(fleet.remote_root.as_deref().unwrap_or(DEFAULT_REMOTE_ROOT));
    };
    if fleet_host.transport == "local" {
        return path_source(root);
    }
    path_source(&fleet_host.root)
}

fn path_source(path: impl AsRef<Path>) -> String {
    format!("path:{}", path.as_ref().to_string_lossy())
}

fn worker_directory(fleet: &Fleet, worker: &Worker) -> Result<PathBuf> {
    if let Some(root) = &worker.root {
        return Ok(PathBuf::from(root));
    }
    if let Some(host) = worker.host.as_deref() {
        let host_root = fleet.hosts.get(host).with_context(|| {
            format!(
                "worker {} references missing legacy host {host}",
                worker.name
            )
        })?;
        return Ok(PathBuf::from(&host_root.root)
            .join("workers")
            .join(&worker.name));
    }
    Ok(PathBuf::from(
        fleet.remote_root.as_deref().unwrap_or(DEFAULT_REMOTE_ROOT),
    ))
}

fn worker_session(fleet: &Fleet, worker: &Worker) -> String {
    worker.session.clone().unwrap_or_else(|| {
        worker.host.as_ref().map_or_else(
            || {
                fleet
                    .herdr_session
                    .clone()
                    .unwrap_or_else(|| "workenv".to_owned())
            },
            |_| format!("workenv-{}", worker.name),
        )
    })
}

fn system_for_host(name: &str, host: &FleetHost, warnings: &mut Vec<Value>) -> String {
    match (host.transport.as_str(), host.platform.as_deref()) {
        ("local", Some("macos")) => local_darwin_system(),
        (_, Some("macos")) => {
            warnings.push(json!({
                "host": name,
                "warning": "remote macOS host architecture is unknown; review system"
            }));
            "REVIEW_REQUIRED_DARWIN_SYSTEM".to_owned()
        }
        (_, Some("linux")) => "x86_64-linux".to_owned(),
        _ => {
            warnings.push(json!({
                "host": name,
                "warning": "legacy host platform is unknown; review system"
            }));
            "REVIEW_REQUIRED_SYSTEM".to_owned()
        }
    }
}

fn local_darwin_system() -> String {
    match std::env::consts::ARCH {
        "aarch64" => "aarch64-darwin",
        "x86_64" => "x86_64-darwin",
        _ => "REVIEW_REQUIRED_DARWIN_SYSTEM",
    }
    .to_owned()
}

fn binding(extension: &str, config: Value) -> Binding {
    Binding {
        extension: extension.to_owned(),
        config,
    }
}
