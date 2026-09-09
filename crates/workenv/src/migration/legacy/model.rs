use std::collections::BTreeMap;

use serde::Deserialize;
use serde_json::Value;

#[derive(Deserialize)]
pub(super) struct Fleet {
    #[serde(default)]
    pub remote_user: Option<String>,
    #[serde(default)]
    pub remote_root: Option<String>,
    #[serde(default)]
    pub herdr_session: Option<String>,
    #[serde(default)]
    pub region: Option<String>,
    pub workers: Vec<Worker>,
    #[serde(default)]
    pub hosts: BTreeMap<String, FleetHost>,
    #[serde(default)]
    pub projects: Option<Value>,
    #[serde(default)]
    pub tasks: Option<Value>,
}

#[derive(Deserialize)]
pub(super) struct Worker {
    pub name: String,
    pub cpus: u64,
    pub memory_gb: u64,
    pub disk_gb: u64,
    #[serde(default)]
    pub ssh_host: Option<String>,
    #[serde(default)]
    pub host: Option<String>,
    #[serde(default)]
    pub root: Option<String>,
    #[serde(default)]
    pub session: Option<String>,
    #[serde(default)]
    pub lifetime: Option<String>,
    #[serde(default)]
    pub profile: Option<String>,
}

#[derive(Deserialize)]
pub(super) struct FleetHost {
    pub transport: String,
    pub root: String,
    #[serde(default)]
    pub target: Option<String>,
    #[serde(default)]
    pub platform: Option<String>,
}
