use std::fs::{self, File, OpenOptions};

use anyhow::{bail, Context as _, Result};
use fs2::FileExt;
use serde_json::{json, Value};

use crate::{
    hosts,
    process::{read_json, shell_join, write_json},
    Context,
};

pub struct HostRegistration {
    pub local: bool,
    pub ssh: Option<String>,
    pub directory: Option<String>,
    pub tools: String,
}

pub struct WorkerRegistration {
    pub host: String,
    pub class: String,
    pub cpus: u64,
    pub memory_gb: u64,
    pub disk_gb: u64,
    pub lifetime: String,
    pub directory: Option<String>,
}

pub fn list_hosts(ctx: &Context) -> Value {
    json!({"ok":true,"status":"hosts","hosts":ctx.fleet.get("hosts").cloned().unwrap_or_else(||json!({}))})
}

fn fleet_lock(ctx: &Context) -> Result<File> {
    fs::create_dir_all(&ctx.state)?;
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(ctx.state.join("fleet.lock"))?;
    lock.try_lock_exclusive()
        .context("Another fleet configuration change is in progress")?;
    if read_json(&ctx.root.join("fleet.json"))? != ctx.fleet {
        bail!("Fleet configuration changed; retry with a new request key");
    }
    Ok(lock)
}

fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name.as_bytes()[0].is_ascii_lowercase()
        && name
            .bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-')
}

pub fn add_host(ctx: &Context, name: &str, options: HostRegistration, key: &str) -> Result<Value> {
    if !valid_name(name) {
        bail!("Host name must use lowercase letters, digits, and hyphens");
    }
    if options.local == options.ssh.is_some() {
        bail!("Choose exactly one of --local or --ssh USER@HOST");
    }
    if !matches!(options.tools.as_str(), "native" | "devenv") {
        bail!("Tools must be native or devenv");
    }
    let mut spec = json!({"transport":if options.local {"local"} else {"ssh"},"root":options.directory.as_deref().unwrap_or("/workenv-probe"),"tools":options.tools});
    if let Some(target) = &options.ssh {
        spec["target"] = json!(target);
    }
    hosts::validate_host_spec(name, &spec)?;
    let _lock = fleet_lock(ctx)?;
    let probe = r#"import json,os,pathlib,platform,shutil
system=platform.system()
print(json.dumps({'platform':{'Darwin':'macos','Linux':'linux'}.get(system,system.lower()),'architecture':platform.machine(),'home':str(pathlib.Path.home()),'cpus':os.cpu_count(),'tools':{name:shutil.which(name) for name in ['git','python3','cargo','apoc','herdr','devenv','workenv']}}))"#;
    let argv = vec!["python3".into(), "-c".into(), probe.into()];
    let observed = if let Some(target) = &options.ssh {
        ctx.run(
            "ssh",
            vec![
                "-o".into(),
                "BatchMode=yes".into(),
                "-o".into(),
                "ConnectTimeout=10".into(),
                "-o".into(),
                "StrictHostKeyChecking=yes".into(),
                target.clone(),
                shell_join(&argv),
            ],
            &format!("{key}:host-probe"),
            "Identify the registered host and its native tools.",
            30_000,
        )?
        .json()?
    } else {
        ctx.run(
            "python3",
            argv[1..].to_vec(),
            &format!("{key}:host-probe"),
            "Identify the local host and its native tools.",
            30_000,
        )?
        .json()?
    };
    let platform = observed["platform"]
        .as_str()
        .context("Host probe omitted the operating system")?;
    if !matches!(platform, "macos" | "linux") {
        bail!("Host must run macOS or Linux");
    }
    let home = observed["home"]
        .as_str()
        .context("Host probe omitted its home directory")?;
    spec["root"] = json!(options
        .directory
        .unwrap_or_else(|| format!("{home}/.local/share/workenv")));
    spec["platform"] = json!(platform);
    hosts::validate_host_spec(name, &spec)?;
    let mut fleet = ctx.fleet.clone();
    if fleet.get("hosts").is_none() {
        fleet["hosts"] = json!({});
    }
    let entries = fleet["hosts"]
        .as_object_mut()
        .context("fleet.hosts must be an object")?;
    if let Some(existing) = entries.get(name) {
        if existing != &spec {
            bail!("Host {name} already has different settings; use a new host name");
        }
        return Ok(
            json!({"ok":true,"status":"host_already_registered","host":name,"configuration":spec,"observed":observed}),
        );
    }
    entries.insert(name.into(), spec.clone());
    hosts::validate_fleet(&fleet)?;
    write_json(&ctx.root.join("fleet.json"), &fleet)?;
    Ok(
        json!({"ok":true,"status":"host_registered","host":name,"configuration":spec,"observed":observed}),
    )
}

pub fn add_worker(
    ctx: &Context,
    name: &str,
    options: WorkerRegistration,
    _key: &str,
) -> Result<Value> {
    if !valid_name(name) {
        bail!("Worker name must use lowercase letters, digits, and hyphens");
    }
    if !valid_name(&options.class) {
        bail!("Worker class must use lowercase letters, digits, and hyphens");
    }
    let _lock = fleet_lock(ctx)?;
    if ctx
        .fleet
        .get("hosts")
        .and_then(|hosts| hosts.get(&options.host))
        .is_none()
    {
        bail!(
            "Host {} is not registered; use workenv host add first",
            options.host
        );
    }
    let mut spec = json!({"name":name,"host":options.host,"class":options.class,"cpus":options.cpus,"memory_gb":options.memory_gb,"disk_gb":options.disk_gb,"lifetime":options.lifetime});
    if let Some(directory) = options.directory {
        spec["root"] = json!(directory);
    }
    let mut fleet = ctx.fleet.clone();
    let entries = fleet["workers"]
        .as_array_mut()
        .context("fleet.workers must be an array")?;
    if let Some(existing) = entries.iter().find(|entry| entry["name"] == name) {
        if existing != &spec {
            bail!("Worker {name} already has different settings; use a new worker name");
        }
        return Ok(
            json!({"ok":true,"status":"worker_already_registered","worker":name,"configuration":spec}),
        );
    }
    entries.push(spec.clone());
    hosts::validate_fleet(&fleet)?;
    write_json(&ctx.root.join("fleet.json"), &fleet)?;
    Ok(
        json!({"ok":true,"status":"worker_registered","worker":name,"configuration":spec,"next":format!("workenv up {name}")}),
    )
}
