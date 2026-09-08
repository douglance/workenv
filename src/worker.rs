use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context as AnyhowContext, Result};
use base64::Engine;
use flate2::write::GzEncoder;
use flate2::Compression;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use tar::Builder;
use uuid::Uuid;

use crate::process::{read_json, shell_quote, write_json};
use crate::{CommandSpec, Context};

const REMOTE_ROOT: &str = "/home/exedev/workenv";
const HERDR_VERSION: &str = "0.9.0";
const HERDR_PROTOCOL_VERSION: i64 = 22;
const TERMINAL_TASK_STATUSES: &[&str] = &["released"];
const LIVE_EXECUTION_STATUSES: &[&str] = &["queued", "running", "stalled", "interrupted"];

pub fn status(ctx: &Context, selector: Option<&str>) -> Result<Value> {
    let inventory = provider_inventory(ctx, &observation_key("worker-status-provider"))
        .unwrap_or_else(
            |error| json!({"status":"unknown","ok":false,"error":error.to_string(),"vms":[]}),
        );
    let workers = selected_worker_names(ctx, selector)?;
    let mut rows = Vec::new();
    for worker in workers {
        rows.push(worker_status(ctx, &worker, &inventory));
    }
    let aggregate = if rows.iter().any(row_has_unknown) {
        "partial"
    } else if rows
        .iter()
        .any(|row| row.get("status") == Some(&json!("busy")))
    {
        "busy"
    } else if rows
        .iter()
        .any(|row| row.get("status") == Some(&json!("needs_setup")))
    {
        "needs_setup"
    } else if rows
        .iter()
        .any(|row| row.get("status") == Some(&json!("available")))
    {
        "available"
    } else {
        "partial"
    };
    Ok(ok_field(
        aggregate,
        aggregate == "available",
        json!({"workers": rows}),
    ))
}

pub fn up(ctx: &Context, selector: Option<&str>, key: &str) -> Result<Value> {
    if key.is_empty() {
        bail!("key is required");
    }
    let workers = selected_worker_names(ctx, selector)?;
    let mut rows = Vec::new();
    for worker in workers {
        rows.push(up_worker(ctx, &worker, key)?);
    }
    let ready = rows
        .iter()
        .all(|row| row.get("status") == Some(&json!("ready")));
    let status = if ready { "ready" } else { "partial" };
    Ok(ok_field(status, ready, json!({"workers": rows})))
}

pub fn connection(ctx: &Context, selector: &str) -> Result<Value> {
    let worker = ctx.worker_name(selector)?;
    let session = herdr_session(ctx);
    let target = connection_target(ctx, &worker);
    let machines = herdr_machine_list(ctx)
        .unwrap_or_else(|error| json!({"status":"unknown","ok":false,"error":error.to_string()}));
    let machine = find_herdr_machine(&machines, &worker, &target, &session);
    Ok(ok(
        "connection",
        json!({
            "worker": worker,
            "session": session,
            "target": target,
            "machine": machine,
            "attach_argv": ["herdr", "--remote", target, "--session", herdr_session(ctx)],
            "ssh_argv": ["ssh", target],
            "detach": {"shortcut": "ctrl+b q", "note": "default Herdr prefix plus q detaches the local client; worker runtime stays up"},
        }),
    ))
}

pub fn out(_ctx: &Context, selector: Option<&str>, key: &str) -> Result<Value> {
    if key.is_empty() {
        bail!("key is required");
    }
    Ok(ok(
        "detach_instructions",
        json!({
            "worker": selector,
            "runtime": "preserved",
            "detach_shortcut": "ctrl+b q",
            "message": "Use the default Herdr prefix plus q to detach the local client; worker runtime stays up."
        }),
    ))
}

pub fn down(ctx: &Context, selector: &str, key: &str) -> Result<Value> {
    if key.is_empty() {
        bail!("key is required");
    }
    let worker = ctx.worker_name(selector)?;
    let open_tasks = open_task_records(ctx, &worker)?;
    if !open_tasks.is_empty() {
        return Ok(failed(
            "worker_has_open_task",
            "worker has central task records that are not released",
            json!({"worker": worker, "tasks": open_tasks}),
        ));
    }
    let reservation = reserve_worker_maintenance(ctx, &worker, key)?;
    if reservation["ok"] != true {
        return Ok(reservation);
    }
    let stopped = stop_owned_herdr_runtime(ctx, &worker, key)?;
    if stopped["status"] == "pending" {
        return Ok(stopped);
    }
    release_worker_maintenance(ctx, &worker, key)?;
    if stopped.get("ok") == Some(&json!(false)) {
        return Ok(stopped);
    }
    Ok(ok(
        "down",
        json!({"worker": worker, "vm": "preserved", "disk": "preserved", "stopped": stopped}),
    ))
}

pub fn doctor(ctx: &Context) -> Result<Value> {
    let inventory = provider_inventory(ctx, &observation_key("worker-doctor-provider"))
        .unwrap_or_else(
            |error| json!({"status":"unknown","ok":false,"error":error.to_string(),"vms":[]}),
        );
    let workers = selected_worker_names(ctx, None)?;
    let mut checks = Vec::new();
    for worker in workers {
        checks.push(json!({
            "worker": worker,
            "provider": provider_inspect(ctx, &worker, Some(&inventory)).unwrap_or_else(|error| json!({"status":"unknown","ok":false,"error":error.to_string()})),
            "status": worker_status(ctx, &worker, &inventory),
        }));
    }
    Ok(ok("doctor", json!({"workers": checks})))
}

fn up_worker(ctx: &Context, worker: &str, key: &str) -> Result<Value> {
    let open_tasks = open_task_records(ctx, worker)?;
    if !open_tasks.is_empty() {
        return Ok(failed(
            "worker_has_open_task",
            "worker has central task records that are not released",
            json!({"worker": worker, "tasks": open_tasks}),
        ));
    }
    let reservation = reserve_worker_maintenance(ctx, worker, key)?;
    if reservation.get("ok") != Some(&json!(true)) {
        return Ok(reservation);
    }
    let provider = ensure_provider_worker(ctx, worker, key)?;
    if provider.get("status") != Some(&json!("present")) {
        release_worker_maintenance(ctx, worker, key)?;
        return Ok(provider);
    }
    let remote_status = workspace_status(ctx, worker).unwrap_or_else(|error| {
        failed(
            "worker_state_unknown",
            "could not verify remote workspace is idle before worker up",
            json!({"worker": worker, "error": error.to_string()}),
        )
    });
    let preflight = if remote_status.get("status") == Some(&json!("available")) {
        inspect_worker_runtime(ctx, worker, Some(remote_status.clone()))?
    } else {
        // A first installation has no workspace helper yet. Only an absent or
        // empty managed root is sufficient evidence to initialize it.
        let root = remote_root(ctx);
        let script = "import json,pathlib,sys; p=pathlib.Path(sys.argv[1]); empty=not p.exists() or (p.is_dir() and not any(p.iterdir())); print(json.dumps({'ok':empty,'status':'uninitialized' if empty else 'worker_state_unknown'}))";
        ctx.run(
            "ssh",
            recovery_ssh_args(
                ctx,
                worker,
                vec!["python3".into(), "-c".into(), script.into(), root],
            )?,
            &observation_key("initial-worker-state"),
            "Inspect managed worker root before first installation.",
            60000,
        )?
        .json()?
    };
    if preflight["ok"] != true {
        release_worker_maintenance(ctx, worker, key)?;
        return Ok(merge(
            json!({"worker":worker,"provider":provider,"remote_status":remote_status}),
            preflight,
        ));
    }

    let source_sync = sync_worker_sources(ctx, worker, key)?;
    if source_sync.get("ok") != Some(&json!(true)) {
        release_worker_maintenance(ctx, worker, key)?;
        return Ok(source_sync);
    }

    let bootstrap = bootstrap_worker(ctx, worker, key)?;
    if bootstrap.get("ok") != Some(&json!(true)) {
        release_worker_maintenance(ctx, worker, key)?;
        return Ok(bootstrap);
    }

    let health = bootstrap_health(ctx, worker, key)?;
    if health.get("ok") == Some(&json!(false)) {
        release_worker_maintenance(ctx, worker, key)?;
        return Ok(health);
    }

    let tools = worker_tool_status(ctx, worker, key)?;
    let herdr = if tools.get("tools_ready") == Some(&json!(true)) {
        start_worker_herdr(ctx, worker, key)?
    } else {
        failed(
            "tools_unknown",
            "shared devenv tool verification did not pass",
            json!({"worker": worker, "tools": tools}),
        )
    };
    if herdr.get("ok") != Some(&json!(true)) {
        release_worker_maintenance(ctx, worker, key)?;
        return Ok(merge(
            json!({"provider": provider, "source_sync": source_sync, "bootstrap": bootstrap, "health": health, "tools": tools}),
            herdr,
        ));
    }

    let registration = register_herdr_machine(ctx, worker, key)?;
    let mut enrollment = json!({"status": "skipped"});
    let mut tailscale = tailscale_status(ctx, worker, key)?;
    if tailscale.get("ok") != Some(&json!(true)) {
        enrollment = enroll_worker_if_credentials_available(ctx, worker, key)?;
        if enrollment.get("ok") != Some(&json!(true)) {
            release_worker_maintenance(ctx, worker, key)?;
            return Ok(ok_field(
                "auth_required",
                false,
                json!({"worker": worker, "provider": provider, "source_sync": source_sync, "bootstrap": bootstrap, "health": health, "tools": tools, "herdr": herdr, "registration": registration, "tailscale": tailscale, "enrollment": enrollment, "remote_status": remote_status, "reservation": reservation}),
            ));
        }
        tailscale = tailscale_status(ctx, worker, key)?;
    }

    let auth = match worker_auth_status(ctx, worker, key) {
        Ok(auth) => auth,
        Err(error) => {
            release_worker_maintenance(ctx, worker, key)?;
            return Err(error);
        }
    };
    let missing = health
        .get("missing_prerequisites")
        .or_else(|| health.get("missing_tools"))
        .cloned()
        .unwrap_or_else(|| json!([]));
    let ready = missing
        .as_array()
        .map(|items| items.is_empty())
        .unwrap_or(false)
        && tools.get("tools_ready") == Some(&json!(true))
        && tools.pointer("/nib_auth/authenticated") == Some(&json!(true))
        && herdr.get("herdr_ready") == Some(&json!(true))
        && registration.get("ok").unwrap_or(&json!(true)) == &json!(true)
        && auth.get("ready") == Some(&json!(true));
    let result = ok_field(
        if ready { "ready" } else { "partial" },
        ready,
        json!({
            "worker": worker,
            "provider": provider,
            "remote_status": remote_status,
            "reservation": reservation,
            "source_sync": source_sync,
            "bootstrap": bootstrap,
            "health": health,
            "tools": tools,
            "herdr": herdr,
            "tailscale": tailscale,
            "registration": registration,
            "auth": auth,
            "enrollment": enrollment,
            "missing_tools": missing,
        }),
    );
    release_worker_maintenance(ctx, worker, key)?;
    Ok(result)
}

fn ensure_provider_worker(ctx: &Context, worker: &str, key: &str) -> Result<Value> {
    let inventory = provider_inventory(ctx, &observation_key(&format!("provider-list:{worker}")))?;
    let inspected = provider_inspect(ctx, worker, Some(&inventory))?;
    match inspected.get("status").and_then(Value::as_str) {
        Some("present") => return Ok(inspected),
        Some("drift") | Some("unknown") | Some("not_ready") => return Ok(inspected),
        Some("missing") => {}
        _ => {
            return Ok(failed(
                "provider_unknown",
                "provider inventory was not usable",
                json!({"worker": worker, "provider": inspected}),
            ))
        }
    }

    let spec = worker_spec(ctx, worker)?.clone();
    let plan_output = ctx.run(
        "ssh",
        vec![
            "exe.dev".into(),
            "billing".into(),
            "plan".into(),
            "--json".into(),
        ],
        &observation_key(&format!("provider-billing:{worker}")),
        &format!("Check exe.dev capacity before creating {worker}."),
        180_000,
    )?;
    let plan = plan_output.json()?;
    if let Some(blocker) = provider_capacity_blocker(&inventory, &plan, &spec)? {
        return Ok(blocker);
    }
    let output = ctx.run(
        "ssh",
        vec![
            "exe.dev".into(),
            "new".into(),
            "--name".into(),
            worker.into(),
            "--cpu".into(),
            integer_field(&spec, "cpus")?.to_string(),
            "--memory".into(),
            format!("{}GB", integer_field(&spec, "memory_gb")?),
            "--disk".into(),
            format!("{}GB", integer_field(&spec, "disk_gb")?),
            "--tag".into(),
            "workenv".into(),
            "--no-email".into(),
            "--json".into(),
        ],
        &format!("{key}:provider:create:{worker}"),
        &format!("Create missing configured workenv worker {worker}."),
        600_000,
    )?;
    let created = output
        .json()
        .unwrap_or_else(|_| json!({"status":"unknown","ok":false}));
    let observed = provider_inspect(ctx, worker, None)
        .unwrap_or_else(|error| json!({"status":"unknown","ok":false,"error":error.to_string()}));
    Ok(merge(json!({"creation": created}), observed))
}

fn provider_inventory(ctx: &Context, key: &str) -> Result<Value> {
    let output = ctx.run(
        "ssh",
        vec!["exe.dev".into(), "ls".into(), "--json".into()],
        key,
        "Inspect exe.dev workenv worker inventory.",
        180_000,
    )?;
    let value = output.json()?;
    if !value.get("vms").map(Value::is_array).unwrap_or(false) {
        bail!("provider inventory is incomplete");
    }
    Ok(value)
}

fn provider_inspect(ctx: &Context, worker: &str, inventory: Option<&Value>) -> Result<Value> {
    let inventory = match inventory {
        Some(value) => value.clone(),
        None => provider_inventory(
            ctx,
            &observation_key(&format!("worker-provider-inspect:{worker}")),
        )?,
    };
    let spec = worker_spec(ctx, worker)?;
    let rows = inventory
        .get("vms")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("provider inventory is incomplete"))?;
    let matches: Vec<&Value> = rows
        .iter()
        .filter(|item| item.get("vm_name") == Some(&json!(worker)))
        .collect();
    if matches.is_empty() {
        return Ok(ok_field("missing", false, json!({"worker": worker})));
    }
    if matches.len() != 1 {
        return Ok(failed(
            "unknown",
            "ambiguous provider inventory",
            json!({"worker": worker}),
        ));
    }
    let actual = matches[0];
    let expected = json!({
        "cpus": integer_field(spec, "cpus")?,
        "memory_bytes": integer_field(spec, "memory_gb")? * 1024 * 1024 * 1024,
        "disk_bytes": integer_field(spec, "disk_gb")? * 1024 * 1024 * 1024,
        "region": ctx.fleet.get("region").cloned().unwrap_or(Value::Null),
        "private_preview": true,
        "workenv_tag": true,
    });
    let observed = json!({
        "cpus": actual.get("allocated_cpus").cloned().unwrap_or(Value::Null),
        "memory_bytes": actual.get("memory_capacity_bytes").cloned().unwrap_or(Value::Null),
        "disk_bytes": actual.get("disk_capacity_bytes").cloned().unwrap_or(Value::Null),
        "region": actual.get("region").cloned().unwrap_or(Value::Null),
        "private_preview": actual.get("proxy_share") == Some(&json!("private")),
        "workenv_tag": actual.get("tags").and_then(Value::as_array).map(|tags| tags.iter().any(|tag| tag == "workenv")).unwrap_or(false),
    });
    let differences = diff_object(&expected, &observed);
    let vm_status = actual
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let status = if !differences.as_object().unwrap().is_empty() {
        "drift"
    } else if vm_status == "running" {
        "present"
    } else {
        "not_ready"
    };
    Ok(ok_field(
        status,
        status == "present",
        json!({"worker": worker, "vm": actual, "differences": differences}),
    ))
}

fn provider_capacity_blocker(
    inventory: &Value,
    plan: &Value,
    spec: &Value,
) -> Result<Option<Value>> {
    let rows = inventory
        .get("vms")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("provider inventory is incomplete"))?;
    let used_cpus: i64 = rows
        .iter()
        .map(|item| {
            item.get("allocated_cpus")
                .and_then(Value::as_i64)
                .unwrap_or(0)
        })
        .sum();
    let used_memory_bytes: i64 = rows
        .iter()
        .map(|item| {
            item.get("memory_capacity_bytes")
                .and_then(Value::as_i64)
                .unwrap_or(0)
        })
        .sum();
    let required_cpus = used_cpus + integer_field(spec, "cpus")?;
    let required_memory_bytes =
        used_memory_bytes + integer_field(spec, "memory_gb")? * 1024 * 1024 * 1024;
    let required_vms = rows.len() as i64 + 1;
    let max_cpus = integer_field(plan, "max_cpus")?;
    let max_memory_bytes = integer_field(plan, "max_memory_gb")? * 1024 * 1024 * 1024;
    let max_vms = integer_field(plan, "max_vms")?;
    if required_cpus > max_cpus
        || required_memory_bytes > max_memory_bytes
        || required_vms > max_vms
    {
        return Ok(Some(failed(
            "capacity_required",
            "provider plan cannot fit the configured worker",
            json!({
                "current_plan": plan,
                "required_cpus": required_cpus,
                "required_memory_gb": required_memory_bytes as f64 / 1024_f64.powi(3),
                "required_vms": required_vms,
            }),
        )));
    }
    Ok(None)
}

fn sync_worker_sources(ctx: &Context, worker: &str, key: &str) -> Result<Value> {
    let archive = source_archive(&ctx.root)?;
    let remote_script = "import pathlib,sys,tarfile; root=pathlib.Path(sys.argv[1]); root.mkdir(parents=True,exist_ok=True); archive=tarfile.open(fileobj=sys.stdin.buffer,mode='r|gz'); archive.extractall(path=root,filter='data'); assert (root/'devenv.nix').is_file(); assert (root/'remote/tool_health.py').is_file(); print('workenv-source-sync-v1')";
    let output = ctx.runtime.run(CommandSpec {
        executable: "ssh".into(),
        args: recovery_ssh_args(
            ctx,
            worker,
            vec![
                "python3".into(),
                "-c".into(),
                remote_script.into(),
                remote_root(ctx),
            ],
        )?,
        cwd: Some(ctx.root.clone()),
        stdin: Some(archive),
        timeout_ms: 180_000,
        key: format!("{key}:sync:{worker}"),
        purpose: format!("Sync workenv bootstrap and shared devenv sources to {worker}."),
    })?;
    if output.exit_code != Some(0) {
        return Ok(failed(
            "source_sync_failed",
            &String::from_utf8_lossy(&output.stderr),
            json!({"worker": worker, "execution_id": output.execution_id}),
        ));
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    if stdout.trim() != "workenv-source-sync-v1" {
        return Ok(failed(
            "source_sync_unverified",
            "worker did not acknowledge extracted environment files",
            json!({"worker": worker, "execution_id": output.execution_id}),
        ));
    }
    Ok(ok(
        "synced",
        json!({"worker": worker, "tools_dir": format!("{}/tool-builds", remote_root(ctx)), "execution_id": output.execution_id}),
    ))
}

fn bootstrap_worker(ctx: &Context, worker: &str, key: &str) -> Result<Value> {
    remote_shell(
        ctx,
        worker,
        &format!("{}/bootstrap/bootstrap.sh", remote_root(ctx)),
        &format!("{key}:bootstrap:{worker}"),
        &format!("Bootstrap workenv prerequisites on {worker}."),
        900_000,
    )
}

fn bootstrap_health(ctx: &Context, worker: &str, _key: &str) -> Result<Value> {
    let command = format!(
        "{} --health-only --json",
        shell_quote(&format!("{}/bootstrap/bootstrap.sh", remote_root(ctx)))
    );
    let value = remote_json_shell(
        ctx,
        worker,
        &command,
        &observation_key(&format!("bootstrap-health:{worker}")),
        &format!("Verify bootstrap health on {worker}."),
        120_000,
    )?;
    Ok(merge(
        json!({"status":"bootstrap_health","ok": true}),
        value,
    ))
}

fn worker_tool_status(ctx: &Context, worker: &str, _key: &str) -> Result<Value> {
    let command = format!(
        "cd {} && /usr/local/bin/devenv shell -- python3 remote/tool_health.py --require-nix",
        shell_quote(&remote_root(ctx))
    );
    remote_json_shell(
        ctx,
        worker,
        &command,
        &observation_key(&format!("tool-health:{worker}")),
        &format!("Verify shared devenv tools on {worker}."),
        900_000,
    )
}

pub fn start_worker_herdr(ctx: &Context, worker: &str, key: &str) -> Result<Value> {
    let session = herdr_session(ctx);
    let command = format!(
        "WORKENV_ROOT={} WORKENV_HERDR_SESSION={} WORKENV_BOOT_ID={} /opt/workenv/bin/workenv-herdr-bootstrap",
        shell_quote(&remote_root(ctx)),
        shell_quote(&session),
        shell_quote(key),
    );
    let boot = remote_shell(
        ctx,
        worker,
        &command,
        &format!("{key}:herdr-boot:{worker}"),
        &format!("Start workenv Herdr server on {worker}."),
        120_000,
    )?;
    let herdr = worker_herdr_status(ctx, worker, key)?;
    if herdr.get("ok") != Some(&json!(true)) {
        return Ok(merge(json!({"boot": boot}), herdr));
    }
    Ok(ok(
        "herdr_ready",
        json!({"worker": worker, "boot": boot, "herdr_status": herdr, "herdr_ready": true}),
    ))
}

pub fn worker_herdr_status(ctx: &Context, worker: &str, _key: &str) -> Result<Value> {
    let session = herdr_session(ctx);
    let command = format!(
        "cd {} && /usr/local/bin/devenv shell -- herdr --session {} status server --json",
        shell_quote(&remote_root(ctx)),
        shell_quote(&session)
    );
    let payload = remote_json_shell(
        ctx,
        worker,
        &command,
        &observation_key(&format!("herdr-status:{worker}")),
        &format!("Inspect workenv Herdr server on {worker}."),
        60_000,
    )?;
    let ready = payload.get("running") == Some(&json!(true))
        && payload.get("compatible") == Some(&json!(true))
        && payload.get("version") == Some(&json!(HERDR_VERSION))
        && payload.get("protocol_version") == Some(&json!(HERDR_PROTOCOL_VERSION))
        && payload.get("server_binary_stale") == Some(&json!(false))
        && payload.pointer("/capabilities/detached_server_daemon") == Some(&json!(true));
    if ready {
        Ok(ok(
            "herdr_ready",
            json!({"worker": worker, "herdr_ready": true, "server": payload}),
        ))
    } else if payload.get("server_binary_stale") == Some(&json!(true)) {
        Ok(failed(
            "herdr_stale",
            "worker Herdr server binary is stale",
            json!({"worker": worker, "herdr_ready": false, "server": payload}),
        ))
    } else {
        Ok(failed(
            "herdr_not_ready",
            "worker Herdr server is not healthy",
            json!({"worker": worker, "herdr_ready": false, "server": payload}),
        ))
    }
}

fn tailscale_status(ctx: &Context, worker: &str, key: &str) -> Result<Value> {
    let payload = remote_json(
        ctx,
        worker,
        vec!["tailscale".into(), "status".into(), "--json".into()],
        &observation_key(&format!("tailscale-status:{worker}")),
        &format!("Inspect Tailscale status on {worker}."),
        60_000,
    )?;
    let expected_dns = format!("{}.{}", worker, tailnet_suffix(ctx)?);
    let dns_name = payload
        .pointer("/Self/DNSName")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim_end_matches('.')
        .to_string();
    let tailnet = payload
        .pointer("/CurrentTailnet/MagicDNSSuffix")
        .or_else(|| payload.pointer("/CurrentTailnet/Name"))
        .and_then(Value::as_str)
        .unwrap_or("");
    let tags = payload
        .pointer("/Self/Tags")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let required_tag = ctx
        .fleet
        .get("tailscale_tag")
        .and_then(Value::as_str)
        .unwrap_or("tag:workenv");
    if payload.get("BackendState") != Some(&json!("Running")) {
        return Ok(failed(
            "tailscale_not_ready",
            "worker Tailscale backend is not running",
            json!({"worker": worker, "tailscale": payload}),
        ));
    }
    if dns_name != expected_dns || tailnet != tailnet_suffix(ctx)? {
        return Ok(failed(
            "tailscale_mismatch",
            "worker Tailscale identity does not match fleet",
            json!({"worker": worker, "dns_name": dns_name, "expected_dns": expected_dns, "tailnet": tailnet}),
        ));
    }
    if !tags.iter().any(|tag| tag == required_tag) {
        return Ok(failed(
            "tailscale_configuration_blocked",
            "worker is missing required Tailscale tag",
            json!({"worker": worker, "required_tag": required_tag, "tags": tags}),
        ));
    }
    let prefs = tailscale_prefs(ctx, worker, key)?;
    if prefs.get("WantRunning") != Some(&json!(true)) || prefs.get("RunSSH") != Some(&json!(true)) {
        return Ok(failed(
            "tailscale_configuration_blocked",
            "worker Tailscale SSH/running prefs are disabled",
            json!({"worker": worker, "prefs": prefs}),
        ));
    }
    Ok(ok(
        "tailscale_ready",
        json!({"worker": worker, "dns_name": dns_name, "tailnet": tailnet, "prefs": prefs}),
    ))
}

fn tailscale_prefs(ctx: &Context, worker: &str, _key: &str) -> Result<Value> {
    let payload = remote_json(
        ctx,
        worker,
        vec!["tailscale".into(), "debug".into(), "prefs".into()],
        &observation_key(&format!("tailscale-prefs:{worker}")),
        &format!("Inspect Tailscale SSH prefs on {worker}."),
        60_000,
    )?;
    Ok(json!({
        "WantRunning": payload.get("WantRunning").cloned().unwrap_or(Value::Null),
        "RunSSH": payload.get("RunSSH").cloned().unwrap_or(Value::Null),
    }))
}

fn worker_auth_status(ctx: &Context, worker: &str, _key: &str) -> Result<Value> {
    let command = format!(
        "cd {} && /usr/local/bin/devenv shell -- python3 remote/health.py",
        shell_quote(&remote_root(ctx))
    );
    remote_json_shell(
        ctx,
        worker,
        &command,
        &observation_key(&format!("auth-health:{worker}")),
        &format!("Inspect subscription authentication on {worker}."),
        120_000,
    )
}

fn enroll_worker_if_credentials_available(ctx: &Context, worker: &str, key: &str) -> Result<Value> {
    let root_state = ctx.root.join(".state");
    let credentials = root_state.join("tailscale-oauth.local.json");
    if !credentials.is_file() {
        return Ok(failed(
            "auth_required",
            "Tailscale OAuth credentials are required for worker enrollment",
            json!({"worker": worker, "credentials_file": credentials}),
        ));
    }
    let request = json!({
        "operation": "enroll",
        "request_id": format!("{key}-enroll"),
        "worker": worker,
    });
    let encoded = base64::engine::general_purpose::STANDARD.encode(serde_json::to_vec(&request)?);
    let output = ctx.run(
        "python3",
        vec![
            "-m".into(),
            "enrollment.enroll".into(),
            "--fleet".into(),
            ctx.root.join("fleet.json").display().to_string(),
            "--state-dir".into(),
            root_state.join("enrollment").display().to_string(),
            "--credentials-file".into(),
            credentials.display().to_string(),
            "--request-base64".into(),
            encoded,
        ],
        &format!("{key}:tailscale-enroll:{worker}"),
        &format!("Enroll {worker} in Tailscale through the native worker flow."),
        120_000,
    )?;
    if output.exit_code != Some(0) {
        return Ok(failed(
            "enrollment_failed",
            &String::from_utf8_lossy(&output.stderr),
            json!({"worker": worker, "execution_id": output.execution_id}),
        ));
    }
    output.json()
}

fn register_herdr_machine(ctx: &Context, worker: &str, key: &str) -> Result<Value> {
    let session = herdr_session(ctx);
    let target = provider_worker_host(ctx, worker);
    let machines = herdr_machine_list(ctx)?;
    if let Some(machine) = find_herdr_machine(&machines, worker, &target, &session) {
        if machine.get("target") == Some(&json!(target))
            && machine.get("session") == Some(&json!(session.clone()))
            && machine.get("enabled") == Some(&json!(true))
        {
            return Ok(ok(
                "already_registered",
                json!({"worker": worker, "machine": machine}),
            ));
        }
        return Ok(failed(
            "herdr_profile_mismatch",
            "existing worker profile has a different target, session, or enabled state",
            json!({"worker": worker, "machine": machine}),
        ));
    }
    let output = ctx.run(
        "herdr",
        vec![
            "machine".into(),
            "add".into(),
            target.clone(),
            "--label".into(),
            worker.into(),
            "--remote-session".into(),
            session.clone(),
        ],
        &format!("{key}:herdr-register:{worker}"),
        &format!("Register {worker} in the workenv Herdr sidebar."),
        60_000,
    )?;
    if output.exit_code != Some(0) {
        return Ok(failed(
            "herdr_registration_failed",
            &String::from_utf8_lossy(&output.stderr),
            json!({"worker": worker, "target": target, "session": session, "execution_id": output.execution_id}),
        ));
    }
    Ok(ok(
        "registered",
        json!({"worker": worker, "target": target, "session": session, "execution_id": output.execution_id}),
    ))
}

fn herdr_machine_list(ctx: &Context) -> Result<Value> {
    let output = ctx.run(
        "herdr",
        vec!["machine".into(), "list".into(), "--json".into()],
        &observation_key("herdr-machine-list"),
        "List local Herdr worker machine registrations.",
        60_000,
    )?;
    let value = output.json()?;
    Ok(value)
}

fn inspect_remote_herdr_activity(ctx: &Context, worker: &str) -> Result<Value> {
    let session = herdr_session(ctx);
    let panes = remote_json(
        ctx,
        worker,
        vec![
            "herdr".into(),
            "--session".into(),
            session.clone(),
            "pane".into(),
            "list".into(),
        ],
        &observation_key(&format!("herdr-pane-list:{worker}")),
        &format!("Inspect remote Herdr panes on {worker}."),
        60_000,
    )
    .unwrap_or_else(|error| {
        failed(
            "herdr_activity_unknown",
            "could not inspect remote Herdr panes before worker down",
            json!({"worker": worker, "error": error.to_string()}),
        )
    });
    if panes.get("ok") == Some(&json!(false)) {
        let server = remote_json(
            ctx,
            worker,
            vec![
                "herdr".into(),
                "--session".into(),
                session.clone(),
                "status".into(),
                "server".into(),
                "--json".into(),
            ],
            &observation_key("herdr-stopped-check"),
            "Verify whether the worker Herdr server is stopped.",
            60000,
        )?;
        if server["running"] == false {
            return Ok(ok(
                "herdr_activity_clear",
                json!({"worker":worker,"server_running":false,"panes":[],"agents":[]}),
            ));
        }
        return Ok(panes);
    }
    let agents = remote_json(
        ctx,
        worker,
        vec![
            "herdr".into(),
            "--session".into(),
            session,
            "agent".into(),
            "list".into(),
        ],
        &observation_key(&format!("herdr-agent-list:{worker}")),
        &format!("Inspect remote Herdr agents on {worker}."),
        60_000,
    )
    .unwrap_or_else(|error| {
        failed(
            "herdr_activity_unknown",
            "could not inspect remote Herdr agents before worker down",
            json!({"worker": worker, "error": error.to_string()}),
        )
    });
    if agents.get("ok") == Some(&json!(false)) {
        return Ok(agents);
    }
    let live_panes = json_items(&panes).context("Unknown Herdr pane inventory shape")?;
    let live_agents = json_items(&agents).context("Unknown Herdr agent inventory shape")?;
    if !live_panes.is_empty() || !live_agents.is_empty() {
        return Ok(failed(
            "worker_busy",
            "worker has live Herdr panes or agents",
            json!({"worker": worker, "panes": live_panes, "agents": live_agents}),
        ));
    }
    Ok(ok(
        "herdr_activity_clear",
        json!({"worker": worker, "panes": live_panes, "agents": live_agents}),
    ))
}

fn inspect_worker_runtime(
    ctx: &Context,
    worker: &str,
    known_workspace: Option<Value>,
) -> Result<Value> {
    let session = herdr_session(ctx);
    let remote_status = known_workspace.unwrap_or_else(|| {
        workspace_status(ctx, worker).unwrap_or_else(|error| {
            failed(
                "worker_state_unknown",
                "could not verify remote workspace is idle before worker down",
                json!({"worker": worker, "error": error.to_string()}),
            )
        })
    });
    if remote_status.get("status") != Some(&json!("available")) {
        return Ok(failed(
            "worker_busy",
            "worker is not idle for worker down",
            json!({"worker": worker, "remote_status": remote_status}),
        ));
    }
    let herdr_activity = inspect_remote_herdr_activity(ctx, worker)?;
    if herdr_activity.get("ok") != Some(&json!(true)) {
        return Ok(herdr_activity);
    }

    let list = list_worker_executions(ctx, worker)?;
    if list.get("next_cursor").is_some() && list.get("next_cursor") != Some(&Value::Null) {
        return Ok(failed(
            "runtime_unknown",
            "remote APoC execution list is truncated",
            json!({"worker": worker}),
        ));
    }
    let mut inspected = Vec::new();
    let mut candidates = Vec::new();
    for summary in list
        .get("executions")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
    {
        if matches!(
            summary["status"].as_str(),
            Some("completed" | "failed" | "canceled" | "cancelled" | "skipped")
        ) {
            continue;
        }
        let execution_id = summary["id"]
            .as_str()
            .context("Remote execution summary has no ID")?;
        let execution = remote_json(
            ctx,
            worker,
            vec![
                "apoc".into(),
                "execution".into(),
                "get".into(),
                execution_id.into(),
                "--purpose".into(),
                "Inspect running workenv runtime before worker down.".into(),
                "--verbosity".into(),
                "trace".into(),
                "--format".into(),
                "json".into(),
            ],
            &observation_key(&format!("runtime-get:{worker}:{execution_id}")),
            &format!("Inspect running workenv runtime {execution_id} on {worker}."),
            60_000,
        )?;
        let execution = execution.get("data").cloned().unwrap_or(execution);
        let labels = execution_labels(&execution);
        if matches!(
            execution["status"].as_str(),
            Some("completed" | "failed" | "canceled" | "cancelled" | "skipped")
        ) {
            continue;
        }
        if !is_live_execution(&execution) {
            return Ok(failed(
                "runtime_unknown",
                "A remote execution has no known lifecycle state",
                json!({"worker":worker,"execution_id":execution_id}),
            ));
        }
        if labels.contains_key("workenv.task_id") {
            return Ok(failed(
                "worker_busy",
                "worker has live task execution",
                json!({"worker": worker, "execution_id": execution_id, "labels": labels}),
            ));
        }
        if labels.get("workenv.component") == Some(&"herdr-server".to_string())
            && labels.get("herdr.session") == Some(&session)
            && is_live_execution(&execution)
        {
            inspected.push(json!({"execution_id": execution_id, "labels": labels}));
            candidates.push(execution_id.to_string());
            continue;
        }
        if is_live_execution(&execution) {
            return Ok(failed(
                "runtime_unknown",
                "worker has a live runtime execution that is not scoped to the idle Herdr server",
                json!({"worker": worker, "execution_id": execution_id, "labels": labels}),
            ));
        }
        inspected.push(json!({"execution_id": execution_id, "labels": labels}));
    }
    Ok(ok(
        "runtime_idle",
        json!({"remote_status":remote_status,"herdr_activity":herdr_activity,"inspected":inspected,"candidates":candidates}),
    ))
}

fn list_worker_executions(ctx: &Context, worker: &str) -> Result<Value> {
    let mut rows = Vec::new();
    let mut cursor: Option<String> = None;
    let mut seen = std::collections::BTreeSet::new();
    loop {
        let mut argv = vec![
            "apoc".into(),
            "execution".into(),
            "list".into(),
            "--limit".into(),
            "100".into(),
            "--purpose".into(),
            "Inspect worker runtime occupancy.".into(),
            "--format".into(),
            "json".into(),
        ];
        if let Some(value) = &cursor {
            argv.extend(["--cursor".into(), value.clone()]);
        }
        let page = remote_json(
            ctx,
            worker,
            argv,
            &observation_key("runtime-inventory"),
            "Inspect worker runtime occupancy.",
            60000,
        )?;
        let items = page["executions"]
            .as_array()
            .context("Remote runtime inventory has no executions")?;
        rows.extend(items.iter().cloned());
        cursor = page["next_cursor"].as_str().map(str::to_owned);
        let Some(value) = &cursor else {
            break;
        };
        if rows.len() > 10000 || !seen.insert(value.clone()) {
            bail!("Remote runtime inventory could not be completely inspected");
        }
    }
    Ok(json!({"executions":rows}))
}

fn stop_owned_herdr_runtime(ctx: &Context, worker: &str, key: &str) -> Result<Value> {
    let preflight = inspect_worker_runtime(ctx, worker, None)?;
    if preflight["ok"] != true {
        return Ok(preflight);
    }
    let candidates = preflight["candidates"]
        .as_array()
        .context("Runtime preflight omitted candidates")?
        .iter()
        .map(|id| {
            id.as_str()
                .map(str::to_owned)
                .context("Invalid runtime candidate ID")
        })
        .collect::<Result<Vec<_>>>()?;
    let mut stopped = Vec::new();
    for execution_id in candidates {
        let cancel = remote_json(
            ctx,
            worker,
            vec![
                "apoc".into(),
                "execution".into(),
                "cancel".into(),
                execution_id.clone(),
                "--idempotency-key".into(),
                format!("{key}:runtime-stop:{worker}:{execution_id}"),
                "--timeout-ms".into(),
                "5000".into(),
                "--purpose".into(),
                "Stop owned idle workenv Herdr runtime before worker down.".into(),
                "--format".into(),
                "json".into(),
            ],
            &format!("{key}:runtime-cancel:{worker}:{execution_id}"),
            &format!("Stop owned idle workenv Herdr runtime {execution_id} on {worker}."),
            60_000,
        )?;
        if cancel.get("ok") == Some(&json!(false)) {
            return Ok(cancel);
        }
        if cancel["outcome"] == "pending" || is_live_execution(&cancel) {
            return Ok(failed(
                "pending",
                "Herdr shutdown is still pending",
                json!({"worker":worker,"execution_id":execution_id,"cancel":cancel}),
            ));
        }
        if cancel["outcome"] != "passed"
            && !matches!(
                cancel["status"].as_str(),
                Some("completed" | "canceled" | "cancelled")
            )
        {
            return Ok(failed(
                "runtime_unknown",
                "Herdr shutdown was not confirmed",
                json!({"worker":worker,"execution_id":execution_id,"cancel":cancel}),
            ));
        }
        stopped.push(json!({"execution_id": execution_id, "cancel": cancel}));
    }
    Ok(ok(
        "runtime_stopped",
        json!({"preflight":preflight,"stopped":stopped}),
    ))
}

fn worker_status(ctx: &Context, worker: &str, inventory: &Value) -> Value {
    let provider = provider_inspect(ctx, worker, Some(inventory))
        .unwrap_or_else(|error| json!({"status":"unknown","ok":false,"error":error.to_string()}));
    let workspace = workspace_status(ctx, worker)
        .unwrap_or_else(|error| json!({"status":"unknown","ok":false,"error":error.to_string()}));
    let tools = worker_tool_status(ctx, worker, "worker-status").unwrap_or_else(
        |error| json!({"status":"tools_unknown","ok":false,"error":error.to_string()}),
    );
    let herdr = worker_herdr_status(ctx, worker, "worker-status").unwrap_or_else(
        |error| json!({"status":"herdr_unknown","ok":false,"error":error.to_string()}),
    );
    let auth = worker_auth_status(ctx, worker, "worker-status").unwrap_or_else(
        |error| json!({"status":"auth_unknown","ok":false,"error":error.to_string()}),
    );
    let tailscale = tailscale_status(ctx, worker, "worker-status").unwrap_or_else(
        |error| json!({"status":"tailscale_unknown","ok":false,"error":error.to_string()}),
    );
    let tasks = open_task_records(ctx, worker).unwrap_or_else(|error| {
        vec![json!({"task_id":"unreadable-record","status":"unknown","error":error.to_string()})]
    });
    let mut blockers = Vec::new();
    if provider.get("ok") != Some(&json!(true)) {
        blockers.push(json!({"kind":"provider", "status": provider.get("status")}));
    }
    if workspace.get("status") != Some(&json!("available")) {
        blockers.push(json!({"kind":"workspace", "status": workspace.get("status")}));
    }
    if !tasks.is_empty() {
        blockers.push(json!({"kind":"ownership", "status":if tasks.iter().any(|task| task["status"] == "unknown") {"unknown"} else {"assigned"}, "tasks": tasks}));
    }
    if tools.get("tools_ready") != Some(&json!(true)) {
        blockers.push(json!({"kind":"tools", "status": tools.get("status")}));
    }
    if herdr.get("herdr_ready") != Some(&json!(true)) {
        blockers.push(json!({"kind":"herdr", "status": herdr.get("status")}));
    }
    if tailscale.get("ok") != Some(&json!(true)) {
        blockers.push(json!({"kind":"tailscale", "status": tailscale.get("status")}));
    }
    if auth.get("ready") != Some(&json!(true)) {
        blockers.push(json!({"kind":"auth", "status": auth.get("status")}));
    }
    let available = blockers.is_empty();
    let status = if available {
        "available"
    } else if !tasks.is_empty() {
        "busy"
    } else if row_blockers_have_unknown(&blockers) {
        "partial"
    } else {
        "needs_setup"
    };
    ok_field(
        status,
        available,
        json!({
            "worker": worker,
            "capacity": worker_capacity(ctx, worker).unwrap_or_else(|_| json!({})),
            "provider": provider,
            "agent_state": workspace,
            "tools": tools,
            "herdr": herdr,
            "tailscale": tailscale,
            "auth": auth,
            "ownership": {"open_tasks": tasks},
            "blockers": blockers,
        }),
    )
}

fn row_has_unknown(row: &Value) -> bool {
    row.get("blockers")
        .and_then(Value::as_array)
        .map(|blockers| row_blockers_have_unknown(blockers))
        .unwrap_or(false)
}

fn row_blockers_have_unknown(blockers: &[Value]) -> bool {
    blockers.iter().any(|blocker| {
        blocker
            .get("status")
            .and_then(Value::as_str)
            .map(|status| status.contains("unknown"))
            .unwrap_or(false)
    })
}

fn workspace_status(ctx: &Context, worker: &str) -> Result<Value> {
    workspace_request(
        ctx,
        worker,
        json!({"operation":"status", "request_id": format!("native-status-{worker}")}),
        &observation_key(&format!("workspace-status:{worker}")),
        "Inspect remote workspace state.",
        30_000,
    )
}

fn workspace_request(
    ctx: &Context,
    worker: &str,
    request: Value,
    key: &str,
    purpose: &str,
    timeout_ms: u64,
) -> Result<Value> {
    let encoded = base64::engine::general_purpose::STANDARD.encode(serde_json::to_vec(&request)?);
    remote_json(
        ctx,
        worker,
        vec![
            "python3".into(),
            format!("{}/remote/workspace.py", remote_root(ctx)),
            "--root".into(),
            remote_root(ctx),
            "--request-base64".into(),
            encoded,
        ],
        key,
        purpose,
        timeout_ms,
    )
}

fn remote_json(
    ctx: &Context,
    worker: &str,
    argv: Vec<String>,
    key: &str,
    purpose: &str,
    timeout_ms: u64,
) -> Result<Value> {
    let output = ctx.ssh(worker, argv, key, purpose, timeout_ms)?;
    if output.exit_code != Some(0) {
        bail!(
            "remote command failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    output.json()
}

fn remote_json_shell(
    ctx: &Context,
    worker: &str,
    command: &str,
    key: &str,
    purpose: &str,
    timeout_ms: u64,
) -> Result<Value> {
    remote_json(
        ctx,
        worker,
        vec!["bash".into(), "-lc".into(), command.into()],
        key,
        purpose,
        timeout_ms,
    )
}

fn remote_shell(
    ctx: &Context,
    worker: &str,
    command: &str,
    key: &str,
    purpose: &str,
    timeout_ms: u64,
) -> Result<Value> {
    let output = ctx.ssh(
        worker,
        vec!["bash".into(), "-lc".into(), command.into()],
        key,
        purpose,
        timeout_ms,
    )?;
    Ok(ok_field(
        if output.exit_code == Some(0) {
            "completed"
        } else {
            "remote_failed"
        },
        output.exit_code == Some(0),
        json!({"worker": worker, "execution_id": output.execution_id, "stdout": String::from_utf8_lossy(&output.stdout).trim(), "stderr": String::from_utf8_lossy(&output.stderr).trim()}),
    ))
}

fn source_archive(root: &Path) -> Result<Vec<u8>> {
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    {
        let mut tar = Builder::new(&mut encoder);
        for dir in ["bootstrap", "remote", "devenv"] {
            add_dir(&mut tar, root, &root.join(dir))?;
        }
        for name in [
            "AGENTS.md",
            "devenv.nix",
            "devenv.yaml",
            "devenv.lock",
            "tools.json",
        ] {
            let path = root.join(name);
            if path.is_file() {
                tar.append_path_with_name(&path, name)?;
            }
        }
        add_manifest_tool_archives(&mut tar, root)?;
        tar.finish()?;
    }
    encoder.flush()?;
    Ok(encoder.finish()?)
}

fn add_manifest_tool_archives<W: Write>(tar: &mut Builder<W>, root: &Path) -> Result<()> {
    let manifest = read_json(&root.join("tools.json"))?;
    if let Some(artifacts) = manifest.get("local_artifacts").and_then(Value::as_object) {
        for (_name, artifact) in artifacts {
            let Some(archive) = artifact.get("archive").and_then(Value::as_str) else {
                continue;
            };
            let path = root.join(".state/tool-builds").join(archive);
            verify_manifest_archive(&path, artifact)?;
            tar.append_path_with_name(&path, Path::new("tool-builds").join(archive))?;
            if let Some(sidecar) = artifact.get("sha256_sidecar").and_then(Value::as_str) {
                let sidecar_path = root.join(".state/tool-builds").join(sidecar);
                if sidecar_path.is_file() {
                    tar.append_path_with_name(
                        &sidecar_path,
                        Path::new("tool-builds").join(sidecar),
                    )?;
                }
            }
        }
    }
    if let Some(tools) = manifest.get("native_tools").and_then(Value::as_object) {
        for (_name, tool) in tools {
            let Some(archive_path) = tool.get("archive_path").and_then(Value::as_str) else {
                continue;
            };
            let relative = safe_relative_path(archive_path)?;
            let path = root.join(&relative);
            verify_manifest_archive(&path, tool)?;
            tar.append_path_with_name(&path, &relative)?;
        }
    }
    Ok(())
}

fn verify_manifest_archive(path: &Path, metadata: &Value) -> Result<()> {
    if !path.is_file() {
        bail!(
            "required pinned tool artifact is missing: {}",
            path.display()
        );
    }
    let Some(expected) = metadata
        .get("archive_sha256")
        .or_else(|| metadata.get("sha256"))
        .and_then(Value::as_str)
    else {
        bail!(
            "tool artifact {} has no sha256 in tools.json",
            path.display()
        );
    };
    let mut hasher = Sha256::new();
    hasher.update(fs::read(path)?);
    let actual = format!("{:x}", hasher.finalize());
    if actual != expected {
        bail!(
            "tool artifact {} hash mismatch: expected {expected}, got {actual}",
            path.display()
        );
    }
    Ok(())
}

fn safe_relative_path(value: &str) -> Result<PathBuf> {
    let path = PathBuf::from(value);
    if path.is_absolute()
        || path
            .components()
            .any(|component| matches!(component, std::path::Component::ParentDir))
    {
        bail!("tool archive path must be relative and stay under workenv root: {value}");
    }
    Ok(path)
}

fn add_dir<W: Write>(tar: &mut Builder<W>, root: &Path, dir: &Path) -> Result<()> {
    if !dir.exists() {
        return Ok(());
    }
    for entry in walk(dir)? {
        if entry.is_file()
            && !entry
                .components()
                .any(|part| part.as_os_str() == "__pycache__")
            && entry.extension().and_then(|value| value.to_str()) != Some("pyc")
        {
            let relative = entry.strip_prefix(root)?.to_path_buf();
            tar.append_path_with_name(&entry, relative)?;
        }
    }
    Ok(())
}

fn walk(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut paths = Vec::new();
    for entry in fs::read_dir(dir).with_context(|| format!("read {}", dir.display()))? {
        let path = entry?.path();
        if path.is_dir() {
            paths.extend(walk(&path)?);
        } else {
            paths.push(path);
        }
    }
    paths.sort();
    Ok(paths)
}

fn recovery_ssh_args(ctx: &Context, worker: &str, remote_argv: Vec<String>) -> Result<Vec<String>> {
    let remote_user = ctx
        .fleet
        .get("remote_user")
        .and_then(Value::as_str)
        .unwrap_or("exedev");
    let command = remote_argv
        .iter()
        .map(|part| shell_quote(part))
        .collect::<Vec<_>>()
        .join(" ");
    Ok(vec![
        "-o".into(),
        "BatchMode=yes".into(),
        "-o".into(),
        "StrictHostKeyChecking=accept-new".into(),
        "-o".into(),
        "ConnectTimeout=15".into(),
        format!("{remote_user}@{worker}.exe.xyz"),
        command,
    ])
}

fn open_task_records(ctx: &Context, worker: &str) -> Result<Vec<Value>> {
    let tasks_dir = ctx.state.join("tasks");
    if !tasks_dir.exists() {
        return Ok(Vec::new());
    }
    let mut records = Vec::new();
    for entry in fs::read_dir(tasks_dir)? {
        let path = entry?.path();
        if path.extension().and_then(|value| value.to_str()) != Some("json") {
            continue;
        }
        let value = read_json(&path).with_context(|| {
            format!(
                "Task ownership is unknown because {} is unreadable",
                path.display()
            )
        })?;
        let owner = value["worker"]
            .as_str()
            .context("Task ownership record has no worker")?;
        let _status = value["status"]
            .as_str()
            .context("Task ownership record has no status")?;
        ctx.worker_name(owner)
            .context("Task record belongs to an unknown worker")?;
        if owner == worker
            && !TERMINAL_TASK_STATUSES.contains(
                &value
                    .get("status")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown"),
            )
        {
            records.push(task_summary(&value));
        }
    }
    Ok(records)
}

fn reserve_worker_maintenance(ctx: &Context, worker: &str, key: &str) -> Result<Value> {
    let session = ctx.apoc(
        "session_open",
        json!({
            "actor":"workenv-controller","label":["operation=worker-maintenance"],
            "ttl_ms":86400000u64,"idempotency_key":format!("{key}:maintenance-session"),
            "purpose":format!("Own maintenance of workenv worker {worker}.")
        }),
    )?;
    let session_id = session["id"]
        .as_str()
        .context("APoC maintenance session has no ID")?;
    let reservation = ctx.apoc(
        "reservation_acquire",
        json!({
            "kind":"custom","key":format!("workenv/worker/{worker}"),
            "lease":format!("session/{session_id}"),"ttl_ms":86400000u64,
            "idempotency_key":format!("{key}:maintenance-reserve:{worker}"),
            "purpose":format!("Reserve workenv worker {worker} for maintenance.")
        }),
    )?;
    let id = reservation["id"]
        .as_str()
        .context("APoC maintenance reservation has no ID")?;
    let record = json!({"worker":worker,"key":key,"reservation_id":id,"session_id":session_id});
    write_json(
        &ctx.state
            .join("worker-maintenance")
            .join(format!("{}.json", safe_name(worker))),
        &record,
    )?;
    Ok(ok("reserved", record))
}

fn release_worker_maintenance(ctx: &Context, worker: &str, key: &str) -> Result<()> {
    let path = ctx
        .state
        .join("worker-maintenance")
        .join(format!("{}.json", safe_name(worker)));
    let existing = read_json(&path)?;
    if existing["key"].as_str() != Some(key) {
        bail!("Maintenance receipt is owned by another request");
    }
    let id = existing["reservation_id"]
        .as_str()
        .context("Maintenance receipt has no reservation ID")?;
    ctx.apoc(
        "reservation_release",
        json!({
            "id":id,"idempotency_key":format!("{key}:maintenance-release:{worker}"),
            "purpose":format!("Release maintenance reservation for workenv worker {worker}.")
        }),
    )?;
    fs::remove_file(path)?;
    Ok(())
}

fn selected_worker_names(ctx: &Context, selector: Option<&str>) -> Result<Vec<String>> {
    if let Some(selector) = selector {
        return Ok(vec![ctx.worker_name(selector)?]);
    }
    let workers = ctx
        .fleet
        .get("workers")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("fleet workers must be an array"))?;
    workers
        .iter()
        .map(|worker| {
            worker
                .get("name")
                .and_then(Value::as_str)
                .map(str::to_string)
                .ok_or_else(|| anyhow!("worker name is required"))
        })
        .collect()
}

fn worker_spec<'a>(ctx: &'a Context, worker: &str) -> Result<&'a Value> {
    ctx.worker(worker)
}

fn worker_capacity(ctx: &Context, worker: &str) -> Result<Value> {
    let spec = worker_spec(ctx, worker)?;
    Ok(json!({
        "class": spec.get("class").cloned().unwrap_or(Value::Null),
        "cpus": spec.get("cpus").cloned().unwrap_or(Value::Null),
        "memory_gb": spec.get("memory_gb").cloned().unwrap_or(Value::Null),
        "disk_gb": spec.get("disk_gb").cloned().unwrap_or(Value::Null),
    }))
}

fn integer_field(value: &Value, key: &str) -> Result<i64> {
    value
        .get(key)
        .and_then(Value::as_i64)
        .ok_or_else(|| anyhow!("{key} must be an integer"))
}

fn tailnet_suffix(ctx: &Context) -> Result<&str> {
    ctx.fleet
        .get("tailnet_suffix")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("fleet tailnet_suffix is required"))
}

fn provider_worker_host(ctx: &Context, worker: &str) -> String {
    let remote_user = ctx
        .fleet
        .get("remote_user")
        .and_then(Value::as_str)
        .unwrap_or("exedev");
    format!("{remote_user}@{worker}.exe.xyz")
}

fn connection_target(ctx: &Context, worker: &str) -> String {
    provider_worker_host(ctx, worker)
}

fn remote_root(ctx: &Context) -> String {
    ctx.fleet
        .get("remote_root")
        .and_then(Value::as_str)
        .unwrap_or(REMOTE_ROOT)
        .to_string()
}

fn herdr_session(ctx: &Context) -> String {
    ctx.fleet
        .get("herdr_session")
        .and_then(Value::as_str)
        .unwrap_or("workenv")
        .to_string()
}

fn find_herdr_machine(
    machines: &Value,
    worker: &str,
    target: &str,
    session: &str,
) -> Option<Value> {
    let rows = machines
        .as_array()
        .or_else(|| machines.get("machines").and_then(Value::as_array))?;
    rows.iter()
        .find(|machine| {
            machine.get("target") == Some(&json!(target))
                || (machine.get("label") == Some(&json!(worker))
                    && machine.get("session") == Some(&json!(session)))
        })
        .cloned()
}

fn execution_labels(execution: &Value) -> BTreeMap<String, String> {
    let mut labels = BTreeMap::new();
    let Some(spec_labels) = execution.pointer("/spec/labels") else {
        return labels;
    };
    if let Some(object) = spec_labels.as_object() {
        for (key, value) in object {
            if let Some(value) = value.as_str() {
                labels.insert(key.clone(), value.to_string());
            }
        }
    }
    labels
}

fn json_items(value: &Value) -> Option<Vec<Value>> {
    if let Some(items) = value.as_array() {
        return Some(items.clone());
    }
    for key in ["items", "panes", "agents", "machines", "executions"] {
        if let Some(items) = value.get(key).and_then(Value::as_array) {
            return Some(items.clone());
        }
    }
    if let Some(result) = value.get("result") {
        return json_items(result);
    }
    None
}

fn is_live_execution(execution: &Value) -> bool {
    execution.get("outcome") == Some(&json!("pending"))
        || execution
            .get("status")
            .and_then(Value::as_str)
            .map(|status| LIVE_EXECUTION_STATUSES.contains(&status))
            .unwrap_or(false)
}

fn task_summary(record: &Value) -> Value {
    json!({
        "task_id": record.get("task_id").cloned().unwrap_or(Value::Null),
        "project": record.get("project").cloned().unwrap_or(Value::Null),
        "revision": record.get("revision").cloned().unwrap_or(Value::Null),
        "status": record.get("status").cloned().unwrap_or(Value::Null),
        "reservation_id": record.get("reservation_id").cloned().unwrap_or(Value::Null),
        "session_id": record.get("session_id").cloned().unwrap_or(Value::Null),
    })
}

fn diff_object(expected: &Value, observed: &Value) -> Value {
    let mut differences = Map::new();
    let Some(expected) = expected.as_object() else {
        return Value::Object(differences);
    };
    let Some(observed) = observed.as_object() else {
        return Value::Object(differences);
    };
    for (key, expected_value) in expected {
        let observed_value = observed.get(key).cloned().unwrap_or(Value::Null);
        if &observed_value != expected_value {
            differences.insert(
                key.clone(),
                json!({"expected": expected_value, "actual": observed_value}),
            );
        }
    }
    Value::Object(differences)
}

fn merge(left: Value, right: Value) -> Value {
    match (left, right) {
        (Value::Object(mut left), Value::Object(right)) => {
            for (key, value) in right {
                left.insert(key, value);
            }
            Value::Object(left)
        }
        (_, right) => right,
    }
}

fn ok(status: &str, fields: Value) -> Value {
    ok_field(status, true, fields)
}

fn ok_field(status: &str, ok: bool, fields: Value) -> Value {
    merge(json!({"status": status, "ok": ok}), fields)
}

fn failed(status: &str, error: &str, fields: Value) -> Value {
    merge(
        json!({"status": status, "ok": false, "error": error}),
        fields,
    )
}

fn observation_key(scope: &str) -> String {
    format!("read:{}:{}", safe_name(scope), Uuid::new_v4())
}

fn safe_name(value: &str) -> String {
    value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-') {
                ch
            } else {
                '_'
            }
        })
        .collect()
}
