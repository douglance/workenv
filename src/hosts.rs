use std::path::{Path, PathBuf};

use anyhow::{bail, Context as _, Result};
use serde_json::Value;

use crate::Context;

const DEFAULT_REMOTE_ROOT: &str = "/home/exedev/workenv";
const LEGACY_DEVENV: &str = "/usr/local/bin/devenv";

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ResolvedTransport {
    Local,
    Ssh { target: String },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ResolvedTools {
    Native,
    Devenv { executable: String },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Provider {
    ExeDev,
    Existing,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Lifetime {
    Static,
    Ephemeral,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedWorker {
    pub name: String,
    pub host_id: String,
    pub transport: ResolvedTransport,
    pub root: PathBuf,
    pub environment_root: PathBuf,
    pub session: String,
    pub tools: ResolvedTools,
    pub provider: Provider,
    pub lifetime: Lifetime,
}

#[derive(Clone, Debug)]
pub struct RemoteCommandSpec {
    pub argv: Vec<String>,
    pub stdin: Option<Vec<u8>>,
    pub cwd: Option<PathBuf>,
    pub tools_env: bool,
    pub key: String,
    pub purpose: String,
    pub timeout_ms: u64,
}

pub fn resolve(ctx: &Context, worker: &str) -> Result<ResolvedWorker> {
    let name = ctx.worker_name(worker)?;
    resolve_worker(&ctx.fleet, &name)
}

pub fn worker_ssh_target(fleet: &Value, worker: &str) -> Result<String> {
    match resolve_worker(fleet, worker)?.transport {
        ResolvedTransport::Ssh { target } => Ok(target),
        ResolvedTransport::Local => {
            bail!("worker {worker} uses local transport and has no SSH target")
        }
    }
}

pub fn validate_fleet(fleet: &Value) -> Result<()> {
    let hosts = fleet.get("hosts").and_then(Value::as_object);
    if let Some(hosts) = hosts {
        let mut ids = std::collections::HashSet::new();
        for (id, spec) in hosts {
            validate_name(id).with_context(|| format!("hosts.{id} is invalid"))?;
            if !ids.insert(id) {
                bail!("Duplicate host id {id:?}");
            }
            validate_host_spec(id, spec)?;
        }
    }

    let workers = fleet["workers"]
        .as_array()
        .context("fleet.json requires a workers array")?;
    let mut names = std::collections::HashSet::new();
    let mut claims: Vec<ResolvedWorker> = Vec::new();
    for worker in workers {
        let name = worker["name"].as_str().context("Worker requires a name")?;
        validate_worker_name(name)?;
        if !names.insert(name) {
            bail!("Invalid or duplicate worker name {name:?}");
        }
        for field in ["cpus", "memory_gb", "disk_gb"] {
            if worker[field].as_u64().unwrap_or(0) == 0 {
                bail!("{name}.{field} must be a positive integer");
            }
        }
        if let Some(host) = worker.get("host") {
            let host = host
                .as_str()
                .with_context(|| format!("{name}.host must be a string"))?;
            if hosts.and_then(|hosts| hosts.get(host)).is_none() {
                bail!("{name}.host references unknown host {host:?}");
            }
        }
        if let Some(root) = worker.get("root") {
            let root = root
                .as_str()
                .with_context(|| format!("{name}.root must be a string"))?;
            validate_absolute_path(root).with_context(|| format!("{name}.root is invalid"))?;
        }
        if let Some(session) = worker.get("session") {
            let session = session
                .as_str()
                .with_context(|| format!("{name}.session must be a string"))?;
            validate_session(session).with_context(|| format!("{name}.session is invalid"))?;
        }
        if let Some(lifetime) = worker.get("lifetime") {
            parse_lifetime(lifetime)
                .with_context(|| format!("{name}.lifetime must be static or ephemeral"))?;
        }
        if let Some(host) = worker.get("ssh_host") {
            let host = host
                .as_str()
                .with_context(|| format!("{name}.ssh_host must be a string"))?;
            validate_ssh_host(host).with_context(|| format!("{name}.ssh_host is invalid"))?;
        }
        let resolved = resolve_worker(fleet, name)?;
        if resolved.lifetime == Lifetime::Ephemeral
            && path_contains(&resolved.root, &resolved.environment_root)
        {
            bail!("{name}.root must not contain its host environment root for ephemeral workers");
        }
        for other in &claims {
            if resolved.transport == other.transport {
                if resolved.session == other.session {
                    bail!(
                        "workers {:?} and {:?} use the same host session {:?}",
                        other.name,
                        resolved.name,
                        resolved.session
                    );
                }
                if path_contains(&resolved.root, &other.root)
                    || path_contains(&other.root, &resolved.root)
                {
                    bail!(
                        "workers {:?} and {:?} use overlapping roots on host {:?}",
                        other.name,
                        resolved.name,
                        resolved.host_id
                    );
                }
            }
        }
        claims.push(resolved);
    }

    Ok(())
}

pub fn validate_host_spec(id: &str, spec: &Value) -> Result<()> {
    let transport = spec
        .get("transport")
        .and_then(Value::as_str)
        .with_context(|| format!("hosts.{id}.transport is required"))?;
    match transport {
        "ssh" => {
            let target = spec
                .get("target")
                .and_then(Value::as_str)
                .with_context(|| format!("hosts.{id}.target is required for ssh transport"))?;
            validate_ssh_target(target)
                .with_context(|| format!("hosts.{id}.target must be user@host"))?;
        }
        "local" => {
            if spec.get("target").is_some() {
                bail!("hosts.{id}.target is only valid for ssh transport");
            }
        }
        _ => bail!("hosts.{id}.transport must be ssh or local"),
    }

    let root = spec
        .get("root")
        .and_then(Value::as_str)
        .with_context(|| format!("hosts.{id}.root is required"))?;
    validate_absolute_path(root).with_context(|| format!("hosts.{id}.root is invalid"))?;

    let tools = spec
        .get("tools")
        .and_then(Value::as_str)
        .with_context(|| format!("hosts.{id}.tools is required"))?;
    match tools {
        "native" => {}
        "devenv" => {
            if let Some(executable) = spec.get("devenv_bin") {
                let executable = executable
                    .as_str()
                    .with_context(|| format!("hosts.{id}.devenv_bin must be a string"))?;
                validate_executable_path(executable)
                    .with_context(|| format!("hosts.{id}.devenv_bin is invalid"))?;
            }
        }
        _ => bail!("hosts.{id}.tools must be native or devenv"),
    }

    if let Some(platform) = spec.get("platform") {
        match platform.as_str() {
            Some("macos" | "linux" | "auto") => {}
            Some(_) => bail!("hosts.{id}.platform must be macos, linux, or auto"),
            None => bail!("hosts.{id}.platform must be a string"),
        }
    }

    Ok(())
}

pub(crate) fn validate_worker_name(name: &str) -> Result<()> {
    validate_name(name)?;
    if name.bytes().all(|c| c.is_ascii_digit()) {
        bail!("Worker names must include a lowercase letter");
    }
    if let Some(suffix) = name.strip_prefix("workenv-") {
        if suffix.bytes().all(|c| c.is_ascii_digit())
            && (suffix.len() != 2 || suffix.parse::<u8>().is_err())
        {
            bail!("Numeric workenv worker names must use workenv-NN");
        }
    }
    Ok(())
}

pub(crate) fn resolve_worker(fleet: &Value, worker: &str) -> Result<ResolvedWorker> {
    let spec = fleet["workers"]
        .as_array()
        .and_then(|workers| {
            workers
                .iter()
                .find(|entry| entry["name"].as_str() == Some(worker))
        })
        .with_context(|| format!("Unknown worker {worker:?}; choose a worker from workenv list"))?;

    if let Some(host_id) = spec.get("host").and_then(Value::as_str) {
        let host = fleet
            .get("hosts")
            .and_then(Value::as_object)
            .and_then(|hosts| hosts.get(host_id))
            .with_context(|| format!("{worker}.host references unknown host {host_id:?}"))?;
        let environment_root = PathBuf::from(
            host.get("root")
                .and_then(Value::as_str)
                .with_context(|| format!("hosts.{host_id}.root is required"))?,
        );
        let root = spec
            .get("root")
            .and_then(Value::as_str)
            .map(PathBuf::from)
            .unwrap_or_else(|| environment_root.join("workers").join(worker));
        let transport = match host.get("transport").and_then(Value::as_str) {
            Some("local") => ResolvedTransport::Local,
            Some("ssh") => ResolvedTransport::Ssh {
                target: host
                    .get("target")
                    .and_then(Value::as_str)
                    .with_context(|| format!("hosts.{host_id}.target is required"))?
                    .to_string(),
            },
            _ => bail!("hosts.{host_id}.transport must be ssh or local"),
        };
        let tools = match host.get("tools").and_then(Value::as_str) {
            Some("native") => ResolvedTools::Native,
            Some("devenv") => ResolvedTools::Devenv {
                executable: host
                    .get("devenv_bin")
                    .and_then(Value::as_str)
                    .unwrap_or("devenv")
                    .to_string(),
            },
            _ => bail!("hosts.{host_id}.tools must be native or devenv"),
        };
        return Ok(ResolvedWorker {
            name: worker.to_string(),
            host_id: host_id.to_string(),
            transport,
            root,
            environment_root,
            session: spec
                .get("session")
                .and_then(Value::as_str)
                .map(str::to_string)
                .unwrap_or_else(|| format!("workenv-{worker}")),
            tools,
            provider: Provider::Existing,
            lifetime: spec
                .get("lifetime")
                .map(parse_lifetime)
                .transpose()?
                .unwrap_or(Lifetime::Static),
        });
    }

    let user = fleet["remote_user"].as_str().unwrap_or("exedev");
    let host = spec
        .get("ssh_host")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .unwrap_or_else(|| format!("{worker}.exe.xyz"));
    validate_ssh_host(&host).with_context(|| format!("{worker}.ssh_host is invalid"))?;
    let root = PathBuf::from(
        fleet
            .get("remote_root")
            .and_then(Value::as_str)
            .unwrap_or(DEFAULT_REMOTE_ROOT),
    );
    Ok(ResolvedWorker {
        name: worker.to_string(),
        host_id: "exe.dev".to_string(),
        transport: ResolvedTransport::Ssh {
            target: format!("{user}@{host}"),
        },
        root: root.clone(),
        environment_root: root,
        session: fleet
            .get("herdr_session")
            .and_then(Value::as_str)
            .unwrap_or("workenv")
            .to_string(),
        tools: ResolvedTools::Devenv {
            executable: LEGACY_DEVENV.to_string(),
        },
        provider: Provider::ExeDev,
        lifetime: spec
            .get("lifetime")
            .map(parse_lifetime)
            .transpose()?
            .unwrap_or(Lifetime::Static),
    })
}

pub(crate) fn validate_ssh_host(host: &str) -> Result<()> {
    if host.is_empty()
        || host.trim() != host
        || host.bytes().any(|c| c.is_ascii_whitespace())
        || host.starts_with('-')
        || host.contains('@')
        || host.contains('/')
        || host.contains('\\')
        || host.contains("://")
    {
        bail!("ssh_host must be a DNS name or IP address");
    }
    if host.parse::<std::net::IpAddr>().is_ok() {
        return Ok(());
    }
    if host.len() > 253 || host.ends_with('.') {
        bail!("ssh_host must be a DNS name or IP address");
    }
    for label in host.split('.') {
        if label.is_empty()
            || label.len() > 63
            || !label
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || c == b'-')
            || label.starts_with('-')
            || label.ends_with('-')
        {
            bail!("ssh_host must be a DNS name or IP address");
        }
    }
    Ok(())
}

fn parse_lifetime(value: &Value) -> Result<Lifetime> {
    match value.as_str() {
        Some("static") => Ok(Lifetime::Static),
        Some("ephemeral") => Ok(Lifetime::Ephemeral),
        _ => bail!("lifetime must be static or ephemeral"),
    }
}

pub(crate) fn validate_name(name: &str) -> Result<()> {
    if name.is_empty()
        || name.len() > 63
        || !name
            .bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-')
        || !name
            .bytes()
            .next()
            .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        || !name
            .bytes()
            .last()
            .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
    {
        bail!("names must use lowercase letters, digits, and dashes");
    }
    Ok(())
}

fn path_contains(parent: &Path, child: &Path) -> bool {
    parent == child || child.starts_with(parent)
}

fn validate_session(session: &str) -> Result<()> {
    if session.is_empty()
        || session.trim() != session
        || session.bytes().any(|c| c.is_ascii_whitespace())
        || session.starts_with('-')
    {
        bail!("session must be a non-empty shell-safe token");
    }
    Ok(())
}

fn validate_absolute_path(path: &str) -> Result<()> {
    if path.is_empty() || path.as_bytes().contains(&0) || !Path::new(path).is_absolute() {
        bail!("path must be absolute");
    }
    Ok(())
}

fn validate_executable_path(path: &str) -> Result<()> {
    if path.is_empty() || path.trim() != path || path.as_bytes().contains(&0) {
        bail!("executable path must be non-empty");
    }
    Ok(())
}

fn validate_ssh_target(target: &str) -> Result<()> {
    if target.is_empty()
        || target.trim() != target
        || target.bytes().any(|c| c.is_ascii_whitespace())
        || target.starts_with('-')
        || target.contains('/')
        || target.contains('\\')
        || target.contains("://")
    {
        bail!("target must be user@host");
    }
    let mut parts = target.split('@');
    let user = parts.next().unwrap_or_default();
    let host = parts.next().unwrap_or_default();
    if parts.next().is_some()
        || user.is_empty()
        || !user
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-' || c == b'.')
    {
        bail!("target must be user@host");
    }
    validate_ssh_host(host)
}
