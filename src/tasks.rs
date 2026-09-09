use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Cursor;
use std::path::{Component, Path, PathBuf};

use anyhow::{anyhow, bail, Context as AnyhowContext, Result};
use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine as _;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::hosts::{self, Provider, RemoteCommandSpec, ResolvedTools, ResolvedTransport};
use crate::process::{read_json, shell_quote, write_json};
use crate::profiles;
use crate::Context;

const LIVE_EXECUTION_STATUSES: &[&str] = &["queued", "running", "stalled", "interrupted"];
const TERMINAL_EXECUTION_STATUSES: &[&str] =
    &["completed", "failed", "canceled", "cancelled", "skipped"];
const TERMINAL_EXECUTION_OUTCOMES: &[&str] = &["passed", "failed", "error", "skipped"];
const AMBIGUOUS_REMOTE_STATUSES: &[&str] = &[
    "pending",
    "auth_failed",
    "remote_failed",
    "remote_invalid_json",
];
const RUNNABLE_TASK_STATUSES: &[&str] = &["claimed", "runtime_recorded"];
const TERMINAL_TASK_STATUSES: &[&str] = &["released"];

pub fn resolve_claim_revision(ctx: &Context, request: &mut Value, key: &str) -> Result<()> {
    if request.get("revision").and_then(Value::as_str).is_some() {
        let revision = required_str(request, "revision")?;
        if !is_full_sha(revision) {
            bail!("revision must be an exact full lowercase SHA");
        }
        return Ok(());
    }
    if request
        .get("source_bundle")
        .and_then(Value::as_str)
        .is_some()
    {
        return Ok(());
    }
    let project = required_str(request, "project")?;
    let task_id = required_str(request, "task_id")?;
    validate_task_id(task_id)?;
    let project_spec = project_config(&ctx.fleet, project)?;
    let requested_profile = task_requested_profile(request, &project_spec)?;
    let worker = match request.get("worker").and_then(Value::as_str) {
        Some(selector) => {
            let worker = ctx.worker_name(selector)?;
            ensure_worker_matches_profile(ctx, &worker, requested_profile.as_deref())?;
            ensure_worker_matches_class(ctx, &worker, project_spec.class.as_deref())?;
            worker
        }
        None => select_available_worker(
            ctx,
            project_spec.class.as_deref(),
            requested_profile.as_deref(),
        )?,
    };
    if !open_task_records_for_worker(&ctx.state, &worker)?.is_empty() {
        bail!("worker {worker} already has an open task");
    }
    let argv = profiles::wrap(
        ctx,
        &worker,
        vec![
            "git".to_string(),
            "ls-remote".to_string(),
            "--".to_string(),
            project_spec.remote_url.clone(),
            "HEAD".to_string(),
        ],
        true,
    )?;
    let output = ctx.remote(
        &worker,
        RemoteCommandSpec {
            argv,
            stdin: None,
            cwd: Some(ctx.worker_environment_root(&worker)?),
            tools_env: false,
            key: format!("{key}:resolve-head:{worker}"),
            purpose: "Resolve the exact task source revision on the selected worker profile."
                .into(),
            timeout_ms: 60_000,
        },
    )?;
    output.success()?;
    let stdout = String::from_utf8(output.stdout).context("git ls-remote output was not UTF-8")?;
    let revision = stdout
        .split_whitespace()
        .next()
        .ok_or_else(|| anyhow!("Repository has no HEAD; supply revision with an exact commit"))?;
    if !is_full_sha(revision) {
        bail!("Repository returned an invalid HEAD revision");
    }
    request["revision"] = json!(revision);
    request["worker"] = json!(worker);
    Ok(())
}

pub fn claim(ctx: &Context, request: Value, key: &str) -> Result<Value> {
    let project = required_str(&request, "project")?;
    let task_id = required_str(&request, "task_id")?;
    let revision = required_str(&request, "revision")?;
    if !is_full_sha(revision) {
        bail!("revision must be an exact full lowercase SHA");
    }
    validate_task_id(task_id)?;
    let project_spec = project_config(&ctx.fleet, project)?;
    let requested_profile = task_requested_profile(&request, &project_spec)?;
    let requested_worker = request
        .get("worker")
        .and_then(Value::as_str)
        .map(|selector| ctx.worker_name(selector))
        .transpose()?;
    let source = if request
        .get("source_bundle")
        .and_then(Value::as_str)
        .is_some()
    {
        "bundle"
    } else {
        "remote"
    };
    if let Some(existing) = read_task_record(&ctx.state, task_id)? {
        let status = existing.get("status").and_then(Value::as_str).unwrap_or("");
        let matches_request = existing.get("project").and_then(Value::as_str) == Some(project)
            && existing.get("revision").and_then(Value::as_str) == Some(revision)
            && existing.get("source").and_then(Value::as_str) == Some(source)
            && requested_worker.as_deref().is_none_or(|worker| {
                existing.get("worker").and_then(Value::as_str) == Some(worker)
            })
            && requested_profile
                .as_deref()
                .is_none_or(|profile| record_profile_name(&existing) == Some(profile));
        if matches_request && RUNNABLE_TASK_STATUSES.contains(&status) {
            let worker = existing
                .get("worker")
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow!("central task record missing worker"))?;
            profiles::validate_task_binding(ctx, &existing)?;
            validate_task_host_binding(ctx, &existing)?;
            let reservation_id = required_record_str(&existing, "reservation_id")?;
            let reservation = validate_reservation(ctx, worker, reservation_id)?;
            if !truthy(&reservation, "ok") {
                return Ok(reservation);
            }
            return Ok(ok(
                "claimed",
                json!({
                    "worker": worker,
                    "task_id": task_id,
                    "reservation_id": existing.get("reservation_id"),
                    "session_id": existing.get("session_id"),
                    "worktree": existing.get("worktree"),
                    "reservation": reservation,
                    "task_record": { "status": "recorded", "ok": true, "task": existing },
                    "worker_profile": existing.get("worker_profile").cloned().unwrap_or(Value::Null),
                    "worker_host": existing.get("worker_host").cloned().unwrap_or(Value::Null),
                    "replayed": true
                }),
            ));
        }
        if matches_request && AMBIGUOUS_REMOTE_STATUSES.contains(&status) {
            let worker = existing
                .get("worker")
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow!("central task record missing worker"))?;
            profiles::validate_task_binding(ctx, &existing)?;
            validate_task_host_binding(ctx, &existing)?;
            let worker_profile = existing
                .get("worker_profile")
                .cloned()
                .unwrap_or(Value::Null);
            let reservation_id = required_record_str(&existing, "reservation_id")?;
            let reservation = validate_reservation(ctx, worker, reservation_id)?;
            if !truthy(&reservation, "ok") {
                return Ok(reservation);
            }
            let staged_source_bundle = match request.get("source_bundle").and_then(Value::as_str) {
                Some(path) => Some(stage_source_bundle(ctx, worker, path, key)?),
                None => None,
            };
            let mut remote_request = json!({
                "operation": "claim",
                "request_id": format!("{key}-{worker}"),
                "task_id": task_id,
                "repo": project_spec.repo,
                "revision": revision,
                "branch": format!("workenv/{task_id}")
            });
            if let Some(source_bundle) = staged_source_bundle.as_deref() {
                remote_request["source_bundle"] = json!(source_bundle);
            } else {
                remote_request["remote_url"] = json!(project_spec.remote_url);
            }
            let mut remote = workspace_request(
                ctx,
                worker,
                remote_request,
                &format!("{key}:claim:{worker}"),
                true,
                180_000,
            )?;
            remote["worker"] = json!(worker);
            remote["worker_profile"] = worker_profile.clone();
            remote["worker_host"] = resolved_worker_record(ctx, worker)?;
            remote["reservation"] = reservation;
            remote["replayed"] = json!(true);
            if should_record_claim(remote.get("status").and_then(Value::as_str)) {
                let worktree = remote
                    .get("worktree")
                    .and_then(Value::as_str)
                    .map(str::to_owned);
                let record = upsert_task_record(
                    &ctx.state,
                    TaskRecordUpdate {
                        task_id,
                        worker,
                        project,
                        repo: project_spec.repo.clone(),
                        revision,
                        reservation_id,
                        session_id: existing.get("session_id").and_then(Value::as_str),
                        source,
                        status: remote
                            .get("status")
                            .and_then(Value::as_str)
                            .unwrap_or("unknown"),
                        worktree: worktree.as_deref(),
                        worker_profile,
                        worker_host: resolved_worker_record(ctx, worker)?,
                        allocation_id: remote.get("allocation_id").and_then(Value::as_str),
                        allocation_root: remote.get("allocation_root").and_then(Value::as_str),
                        worker_lifetime: remote.get("worker_lifetime").and_then(Value::as_str),
                        controller_execution_id: None,
                    },
                )?;
                remote["reservation_id"] = json!(reservation_id);
                remote["session_id"] = existing.get("session_id").cloned().unwrap_or(Value::Null);
                remote["task_record"] = ok("recorded", json!({ "task": record }));
            }
            return Ok(remote);
        }
    }
    let worker = match requested_worker {
        Some(worker) => {
            ensure_worker_matches_profile(ctx, &worker, requested_profile.as_deref())?;
            worker
        }
        None => select_available_worker(
            ctx,
            project_spec.class.as_deref(),
            requested_profile.as_deref(),
        )?,
    };
    let worker_spec = ctx.worker(&worker)?;
    if let Some(class) = project_spec.class.as_deref() {
        if worker_spec.get("class").and_then(Value::as_str) != Some(class) {
            return Ok(failed(
                "conflict",
                format!("worker {worker} is not class {class}"),
                json!({ "worker": worker }),
            ));
        }
    }
    let open = open_task_records_for_worker(&ctx.state, &worker)?;
    if !open.is_empty() {
        return Ok(failed(
            "worker_has_open_task",
            "worker has a central task record that is not released",
            json!({ "worker": worker, "tasks": summaries(&open) }),
        ));
    }
    let worker_profile = profile_binding_for_worker(ctx, &worker)?;

    let staged_source_bundle = match request.get("source_bundle").and_then(Value::as_str) {
        Some(path) => Some(stage_source_bundle(ctx, &worker, path, key)?),
        None => None,
    };

    let session = ctx.apoc(
        "session_open",
        json!({
            "actor": "workenv-controller",
            "label": ["operation=claim"],
            "ttl_ms": 86_400_000u64,
            "idempotency_key": format!("{key}:claim:session"),
            "purpose": "Open the workenv claim controller session."
        }),
    )?;
    let session_id =
        string_field(&session, "id").ok_or_else(|| anyhow!("APoC session_open returned no id"))?;
    let reservation = ctx.apoc(
        "reservation_acquire",
        json!({
            "kind": "custom",
            "key": format!("workenv/worker/{worker}"),
            "lease": format!("session/{session_id}"),
            "ttl_ms": 86_400_000u64,
            "idempotency_key": format!("{key}:reserve:claim:{worker}"),
            "purpose": format!("Reserve workenv worker {worker}.")
        }),
    )?;
    let reservation_id = string_field(&reservation, "id")
        .ok_or_else(|| anyhow!("APoC reservation_acquire returned no id"))?;

    let mut remote_request = json!({
        "operation": "claim",
        "request_id": format!("{key}-{worker}"),
        "task_id": task_id,
        "repo": project_spec.repo,
        "revision": revision,
        "branch": format!("workenv/{task_id}")
    });
    if let Some(source_bundle) = staged_source_bundle.as_deref() {
        remote_request["source_bundle"] = json!(source_bundle);
    } else {
        remote_request["remote_url"] = json!(project_spec.remote_url);
    }

    let mut remote = workspace_request(
        ctx,
        &worker,
        remote_request,
        &format!("{key}:claim:{worker}"),
        true,
        180_000,
    )?;
    remote["worker"] = json!(worker);
    remote["worker_profile"] = worker_profile.clone();
    remote["worker_host"] = resolved_worker_record(ctx, &worker)?;
    if should_record_claim(remote.get("status").and_then(Value::as_str)) {
        let worktree = remote
            .get("worktree")
            .and_then(Value::as_str)
            .map(str::to_owned);
        let record = upsert_task_record(
            &ctx.state,
            TaskRecordUpdate {
                task_id,
                worker: &worker,
                project,
                repo: project_spec.repo.clone(),
                revision,
                reservation_id: &reservation_id,
                session_id: Some(&session_id),
                source,
                status: remote
                    .get("status")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown"),
                worktree: worktree.as_deref(),
                worker_profile,
                worker_host: resolved_worker_record(ctx, &worker)?,
                allocation_id: remote.get("allocation_id").and_then(Value::as_str),
                allocation_root: remote.get("allocation_root").and_then(Value::as_str),
                worker_lifetime: remote.get("worker_lifetime").and_then(Value::as_str),
                controller_execution_id: None,
            },
        )?;
        remote["reservation_id"] = json!(reservation_id);
        remote["session_id"] = json!(session_id);
        remote["task_record"] = ok("recorded", json!({ "task": record }));
        return Ok(remote);
    }

    let _ = ctx.apoc(
        "reservation_release",
        json!({
            "id": reservation_id,
            "idempotency_key": format!("{key}:release-reservation:claim:{worker}"),
            "purpose": format!("Release workenv reservation for failed claim on {worker}.")
        }),
    );
    Ok(remote)
}

pub fn run(ctx: &Context, task: &str, argv: Vec<String>, key: &str) -> Result<Value> {
    run_with_kind(ctx, task, argv, key, "task", false)
}

pub fn run_profiled(ctx: &Context, task: &str, argv: Vec<String>, key: &str) -> Result<Value> {
    run_with_kind(ctx, task, argv, key, "task", true)
}

pub fn services(ctx: &Context, task: &str, key: &str) -> Result<Value> {
    run_with_kind(
        ctx,
        task,
        vec!["devenv".to_string(), "up".to_string()],
        key,
        "service",
        false,
    )
}

pub fn down(ctx: &Context, task: &str, key: &str) -> Result<Value> {
    let record = read_task_record(&ctx.state, task)?
        .ok_or_else(|| anyhow!("central task record is missing"))?;
    require_worker_bound(ctx, &record)?;
    profiles::validate_task_binding(ctx, &record)?;
    let worker = required_record_str(&record, "worker")?;
    let reservation_id = required_record_str(&record, "reservation_id")?;
    let reservation = validate_reservation(ctx, worker, reservation_id)?;
    if !truthy(&reservation, "ok") {
        return Ok(reservation);
    }
    let stopped = stop_recorded_task_services(ctx, worker, task, key, &record)?;
    if !truthy(&stopped, "ok") {
        return Ok(stopped);
    }
    let collection = collect(ctx, task, &format!("{key}:collect"))?;
    if !truthy(&collection, "ok") {
        return Ok(collection);
    }
    let release = release(ctx, task, &format!("{key}:release"))?;
    if !truthy(&release, "ok") {
        return Ok(release);
    }
    Ok(ok(
        "down",
        json!({
            "task_id": task,
            "worker": worker,
            "stopped": stopped,
            "collection": collection,
            "release": release
        }),
    ))
}

fn run_with_kind(
    ctx: &Context,
    task: &str,
    argv: Vec<String>,
    key: &str,
    kind: &str,
    telemetry: bool,
) -> Result<Value> {
    if !matches!(kind, "task" | "service") {
        bail!("unsupported task execution kind");
    }
    if argv.is_empty() {
        return Ok(failed(
            "conflict",
            "task run requires a command",
            json!({ "task_id": task }),
        ));
    }
    let record = read_task_record(&ctx.state, task)?
        .ok_or_else(|| anyhow!("central task record is missing"))?;
    require_worker_bound(ctx, &record)?;
    profiles::validate_task_binding(ctx, &record)?;
    require_runnable_task(&record)?;
    let worker = required_record_str(&record, "worker")?;
    let reservation_id = required_record_str(&record, "reservation_id")?;
    let reservation = validate_reservation(ctx, worker, reservation_id)?;
    if !truthy(&reservation, "ok") {
        return Ok(reservation);
    }
    let revision = required_record_str(&record, "revision")?;
    let worktree = required_record_str(&record, "worktree")?;
    let profile = record.get("worker_profile").cloned().unwrap_or(Value::Null);
    let child_argv = profiles::wrap(ctx, worker, argv, true)?;
    if child_argv.is_empty() {
        bail!("profile wrapper returned no command");
    }
    let mut args = vec![
        "execution".to_string(),
        "start".to_string(),
        child_argv[0].clone(),
        "--cwd".to_string(),
        worktree.to_string(),
        "--idempotency-key".to_string(),
        key.to_string(),
        "--purpose".to_string(),
        format!("Run workenv task {task} {kind} command."),
        "--label".to_string(),
        format!("workenv.task_id={task}"),
        "--label".to_string(),
        format!("workenv.worker={worker}"),
        "--label".to_string(),
        format!("workenv.kind={kind}"),
    ];
    if let Some(name) = profile.get("name").and_then(Value::as_str) {
        args.extend([
            "--label".to_string(),
            format!("workenv.profile={name}"),
            "--label".to_string(),
            format!(
                "workenv.profile.digest={}",
                profile.get("digest").and_then(Value::as_str).unwrap_or("")
            ),
        ]);
    }
    if let Some(allocation_id) = record.get("allocation_id").and_then(Value::as_str) {
        args.extend([
            "--label".to_string(),
            format!("workenv.allocation_id={allocation_id}"),
        ]);
    }
    if let Some(lifetime) = record.get("worker_lifetime").and_then(Value::as_str) {
        args.extend([
            "--label".to_string(),
            format!("workenv.lifetime={lifetime}"),
        ]);
    }
    if telemetry {
        args.push("--telemetry".into());
    }
    args.extend(["--format".to_string(), "json".to_string(), "--".to_string()]);
    args.extend(child_argv.into_iter().skip(1));
    let remote = ssh_json(
        ctx,
        worker,
        vec!["apoc".to_string()].into_iter().chain(args).collect(),
        &format!("{key}:run"),
        60_000,
    )?;
    if remote.get("ok") == Some(&Value::Bool(false)) {
        return Ok(remote);
    }
    let execution = remote.get("execution").unwrap_or(&remote);
    let execution_id = execution
        .get("id")
        .or_else(|| remote.get("id"))
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("remote APoC execution start returned no execution id"))?;
    record_remote_execution(&ctx.state, task, execution_id)?;
    let runtime_recorded = record_runtime(
        ctx,
        worker,
        task,
        revision,
        &format!("{key}:runtime"),
        json!({ "apoc_execution_ids": [execution_id] }),
    )?;
    Ok(ok(
        "started",
        json!({
            "worker": worker,
            "task_id": task,
            "execution_id": execution_id,
            "kind": kind,
            "telemetry": telemetry,
            "worker_profile": profile,
            "remote": remote,
            "runtime_recorded": runtime_recorded
        }),
    ))
}

pub fn collect(ctx: &Context, task: &str, key: &str) -> Result<Value> {
    let record = read_task_record(&ctx.state, task)?
        .ok_or_else(|| anyhow!("central task record is missing"))?;
    require_worker_bound(ctx, &record)?;
    profiles::validate_task_binding(ctx, &record)?;
    require_runnable_task(&record)?;
    let worker = required_record_str(&record, "worker")?;
    let reservation_id = required_record_str(&record, "reservation_id")?;
    let reservation = validate_reservation(ctx, worker, reservation_id)?;
    if !truthy(&reservation, "ok") {
        return Ok(reservation);
    }
    let activity = inspect_task_activity(ctx, worker, task, &record)?;
    if !truthy(&activity, "ok") {
        return Ok(activity);
    }
    let remote = workspace_request(
        ctx,
        worker,
        json!({
            "operation": "collect",
            "request_id": key,
            "task_id": task,
            "evidence_paths": []
        }),
        &format!("{key}:collect"),
        false,
        180_000,
    )?;
    if remote.get("status").and_then(Value::as_str) != Some("collected") {
        return Ok(remote);
    }
    let local_dir = retrieve_collection(ctx, worker, task, &remote, key)?;
    let digest = required_remote_str(&remote, "collection_digest")?;
    let verified =
        verify_local_collection(&local_dir, digest, Some(task), None, None, None, false)?;
    if !truthy(&verified, "ok") {
        return Ok(verified);
    }
    let mut updated = record.clone();
    updated["status"] = json!("collected");
    updated["last_collection"] = json!({
        "digest": digest,
        "local_collection_dir": local_dir,
        "remote": remote,
        "verified": verified,
        "updated_at": now_ms()
    });
    updated["updated_at"] = json!(now_ms());
    write_task_record(&ctx.state, task, &updated)?;
    let mut result = remote;
    result["local_collection_dir"] = json!(local_dir);
    result["reservation"] = reservation;
    Ok(result)
}

pub fn release(ctx: &Context, task: &str, key: &str) -> Result<Value> {
    let record = read_task_record(&ctx.state, task)?
        .ok_or_else(|| anyhow!("central task record is missing"))?;
    require_worker_bound(ctx, &record)?;
    profiles::validate_task_binding(ctx, &record)?;
    let worker = required_record_str(&record, "worker")?;
    let reservation_id = required_record_str(&record, "reservation_id")?;
    let reservation = validate_reservation(ctx, worker, reservation_id)?;
    if !truthy(&reservation, "ok") {
        return Ok(reservation);
    }
    let collection = record
        .get("last_collection")
        .and_then(Value::as_object)
        .ok_or_else(|| anyhow!("task has no verified local collection"))?;
    let digest = collection
        .get("digest")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("task collection record has no digest"))?;
    let local_dir = collection
        .get("local_collection_dir")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("task collection record has no local_collection_dir"))?;
    let verified = verify_local_collection(
        Path::new(local_dir),
        digest,
        Some(task),
        record.get("repo"),
        record.get("source").and_then(Value::as_str),
        record.get("revision").and_then(Value::as_str),
        false,
    )?;
    if !truthy(&verified, "ok") {
        return Ok(verified);
    }
    let stopped = stop_recorded_task_services(ctx, worker, task, key, &record)?;
    if !truthy(&stopped, "ok") {
        return Ok(stopped);
    }
    let activity = inspect_task_activity(ctx, worker, task, &record)?;
    if !truthy(&activity, "ok") {
        return Ok(activity);
    }
    let mut remote = workspace_request(
        ctx,
        worker,
        json!({
            "operation": "release",
            "request_id": key,
            "task_id": task,
            "runtime_quiescent": true,
            "collection_digest": digest
        }),
        &format!("{key}:release"),
        false,
        120_000,
    )?;
    if remote.get("status").and_then(Value::as_str) == Some("released") {
        let mut updated = record.clone();
        updated["status"] = json!("released");
        updated["updated_at"] = json!(now_ms());
        write_task_record(&ctx.state, task, &updated)?;
        let released = ctx.apoc(
            "reservation_release",
            json!({
                "id": reservation_id,
                "idempotency_key": format!("{key}:release-reservation:{reservation_id}"),
                "purpose": format!("Release workenv reservation {reservation_id}.")
            }),
        )?;
        remote["reservation_released"] = released;
        remote["local_collection_dir"] = json!(local_dir);
    }
    Ok(remote)
}

pub fn status(ctx: &Context, task: Option<&str>) -> Result<Value> {
    if let Some(task_id) = task {
        let Some(record) = read_task_record(&ctx.state, task_id)? else {
            return Ok(failed(
                "untracked_task",
                "central task record is missing",
                json!({ "task_id": task_id }),
            ));
        };
        let worker = record.get("worker").and_then(Value::as_str).unwrap_or("");
        let activity = if !worker.is_empty()
            && !TERMINAL_TASK_STATUSES
                .contains(&record.get("status").and_then(Value::as_str).unwrap_or(""))
        {
            inspect_task_activity(ctx, worker, task_id, &record)?
        } else {
            ok("terminal", json!({}))
        };
        return Ok(ok(
            "task_status",
            json!({ "task": record, "activity": activity }),
        ));
    }

    let reservations = ctx
        .apoc(
            "reservation_list",
            json!({ "active": true, "purpose": "List active workenv worker reservations." }),
        )
        .unwrap_or_else(|err| failed("reservations_unknown", err.to_string(), json!({})));
    let mut tasks = Vec::new();
    for record in open_task_records(&ctx.state)? {
        let worker = record.get("worker").and_then(Value::as_str).unwrap_or("");
        let task_id = record.get("task_id").and_then(Value::as_str).unwrap_or("");
        let activity = if !worker.is_empty() && !task_id.is_empty() {
            inspect_task_activity(ctx, worker, task_id, &record)?
        } else {
            failed(
                "task_activity_unknown",
                "central task record is incomplete",
                json!({}),
            )
        };
        tasks.push(json!({ "task": summary(&record), "activity": activity }));
    }
    Ok(ok(
        "status",
        json!({ "tasks": tasks, "reservations": reservations }),
    ))
}

fn record_runtime(
    ctx: &Context,
    worker: &str,
    task: &str,
    revision: &str,
    key: &str,
    runtime: Value,
) -> Result<Value> {
    let mut record = read_task_record(&ctx.state, task)?
        .ok_or_else(|| anyhow!("central task record is missing"))?;
    profiles::validate_task_binding(ctx, &record)?;
    if required_record_str(&record, "worker")? != worker {
        return Ok(failed(
            "task_worker_mismatch",
            "central task record is bound to a different worker",
            json!({ "task_id": task }),
        ));
    }
    if required_record_str(&record, "revision")? != revision {
        return Ok(failed(
            "conflict",
            "runtime revision does not match central task record",
            json!({ "task_id": task }),
        ));
    }
    require_runnable_task(&record)?;
    let remote = workspace_request(
        ctx,
        worker,
        json!({
            "operation": "record-runtime",
            "request_id": key,
            "task_id": task,
            "revision": revision,
            "runtime": runtime
        }),
        key,
        false,
        120_000,
    )?;
    if remote.get("status").and_then(Value::as_str) == Some("runtime_recorded") {
        record["runtime"] = remote
            .get("runtime")
            .cloned()
            .unwrap_or_else(|| json!({ "herdr": {}, "apoc_execution_ids": [] }));
        record["status"] = json!("runtime_recorded");
        record["updated_at"] = json!(now_ms());
        write_task_record(&ctx.state, task, &record)?;
    }
    Ok(remote)
}

fn workspace_request(
    ctx: &Context,
    worker: &str,
    request: Value,
    key: &str,
    check_github: bool,
    timeout_ms: u64,
) -> Result<Value> {
    let encoded = BASE64.encode(canonical_json(&request).as_bytes());
    let resolved = hosts::resolve(ctx, worker)?;
    let worker_root = resolved.root.to_string_lossy().into_owned();
    let environment_root = resolved.environment_root.to_string_lossy().into_owned();
    let lifetime = worker_lifetime_value(&resolved);
    let argv = profiles::wrap(
        ctx,
        worker,
        vec![
            "python3".to_string(),
            format!("{environment_root}/remote/workspace.py"),
            "--root".to_string(),
            worker_root,
            "--worker".to_string(),
            resolved.name.clone(),
            "--lifetime".to_string(),
            lifetime.to_string(),
            "--request-base64".to_string(),
            encoded,
        ],
        check_github,
    )?;
    let output = ctx.remote(
        worker,
        RemoteCommandSpec {
            argv,
            stdin: None,
            cwd: Some(resolved.root),
            tools_env: false,
            key: key.into(),
            purpose: "Run remote workenv workspace helper.".into(),
            timeout_ms,
        },
    )?;
    command_json_result(output, worker)
}

fn ssh_json(
    ctx: &Context,
    worker: &str,
    argv: Vec<String>,
    key: &str,
    timeout_ms: u64,
) -> Result<Value> {
    let tools_env = matches!(argv.first().map(String::as_str), Some("apoc" | "herdr"));
    let output = ctx.remote(
        worker,
        RemoteCommandSpec {
            argv,
            stdin: None,
            cwd: Some(ctx.worker_root(worker)?),
            tools_env,
            key: key.into(),
            purpose: "Run remote workenv command.".into(),
            timeout_ms,
        },
    )?;
    command_json_result(output, worker)
}

fn stage_source_bundle(
    ctx: &Context,
    worker: &str,
    source_bundle: &str,
    key: &str,
) -> Result<String> {
    let local_path = Path::new(source_bundle);
    if !local_path.is_file() {
        bail!("source_bundle must be a readable local file");
    }
    let bytes = fs::read(local_path)
        .with_context(|| format!("read source bundle {}", local_path.display()))?;
    let digest = hex(&Sha256::digest(&bytes));
    let worker_root = ctx.worker_root(worker)?;
    let remote_path = format!("{}/incoming/{digest}.bundle", worker_root.to_string_lossy());
    let script = r#"
import hashlib, json, os, pathlib, sys
path = pathlib.Path(sys.argv[1])
expected = sys.argv[2]
path.parent.mkdir(parents=True, exist_ok=True)
if path.exists():
    actual = hashlib.sha256(path.read_bytes()).hexdigest()
    if actual != expected:
        print(json.dumps({"status":"conflict","ok":False,"error":"remote source bundle digest mismatch","path":str(path),"sha256":actual}))
        sys.exit(1)
else:
    tmp = path.with_suffix(path.suffix + ".tmp." + str(os.getpid()))
    digest = hashlib.sha256()
    with tmp.open("wb") as out:
        while True:
            chunk = sys.stdin.buffer.read(1024 * 1024)
            if not chunk:
                break
            digest.update(chunk)
            out.write(chunk)
    actual = digest.hexdigest()
    if actual != expected:
        tmp.unlink(missing_ok=True)
        print(json.dumps({"status":"conflict","ok":False,"error":"uploaded source bundle digest mismatch","path":str(path),"sha256":actual}))
        sys.exit(1)
    os.replace(tmp, path)
print(json.dumps({"status":"staged","ok":True,"path":str(path),"sha256":expected}))
"#;
    let output = ctx.remote(
        worker,
        RemoteCommandSpec {
            argv: vec![
                "python3".to_string(),
                "-c".to_string(),
                script.to_string(),
                remote_path.clone(),
                digest.clone(),
            ],
            stdin: Some(bytes),
            cwd: Some(worker_root),
            tools_env: false,
            key: format!("{key}:stage-source-bundle:{digest}"),
            purpose: format!("Stage workenv source bundle for task claim on {worker}."),
            timeout_ms: 300_000,
        },
    )?;
    let result = command_json_result(output, worker)?;
    if result.get("status").and_then(Value::as_str) != Some("staged") {
        return Err(anyhow!(
            "source bundle staging failed: {}",
            result
                .get("error")
                .and_then(Value::as_str)
                .unwrap_or("unknown error")
        ));
    }
    if result.get("sha256").and_then(Value::as_str) != Some(digest.as_str())
        || result.get("path").and_then(Value::as_str) != Some(remote_path.as_str())
    {
        bail!("source bundle staging response did not match verified upload");
    }
    Ok(remote_path)
}

fn command_json_result(output: crate::CommandOutput, worker: &str) -> Result<Value> {
    if output.exit_code == Some(255)
        || String::from_utf8_lossy(&output.stderr).contains("Permission denied")
    {
        return Ok(failed(
            "auth_failed",
            String::from_utf8_lossy(&output.stderr).trim(),
            json!({ "worker": worker }),
        ));
    }
    if output.exit_code.is_none() {
        return Ok(failed(
            "pending",
            "remote command did not reach a terminal state; inspect the retained execution before retrying",
            json!({ "worker": worker, "execution_id": output.execution_id }),
        ));
    }
    if output.exit_code != Some(0) {
        return Ok(failed(
            "remote_failed",
            nonempty_or(
                String::from_utf8_lossy(&output.stderr).trim(),
                "remote command failed",
            ),
            json!({ "worker": worker, "execution_id": output.execution_id }),
        ));
    }
    let mut value: Value =
        serde_json::from_slice(&output.stdout).context("remote command did not return JSON")?;
    if let Some(object) = value.as_object_mut() {
        object
            .entry("execution_id".to_string())
            .or_insert(json!(output.execution_id));
    }
    Ok(value)
}

fn retrieve_collection(
    ctx: &Context,
    worker: &str,
    task: &str,
    remote: &Value,
    key: &str,
) -> Result<PathBuf> {
    let metadata_path = required_remote_str(remote, "metadata_path")?;
    let remote_dir = Path::new(metadata_path)
        .parent()
        .ok_or_else(|| anyhow!("remote metadata_path has no parent"))?
        .to_string_lossy()
        .to_string();
    let expected_prefix = remote_task_collection_root(ctx, worker, task)?;
    if !(remote_dir == expected_prefix || remote_dir.starts_with(&format!("{expected_prefix}/"))) {
        bail!("remote collection path is outside the worker collection root");
    }
    let digest = required_remote_str(remote, "collection_digest")?;
    if !is_sha256_hex(digest) {
        bail!("remote collection digest is not a lowercase SHA-256 hex digest");
    }
    let local_dir = ctx
        .state
        .join("collections")
        .join(worker)
        .join(task)
        .join(digest);
    if local_dir.exists() {
        return Ok(local_dir);
    }
    fs::create_dir_all(
        local_dir
            .parent()
            .ok_or_else(|| anyhow!("collection path has no parent"))?,
    )?;
    let temp = local_dir
        .parent()
        .unwrap()
        .join(format!(".collection.{}", Uuid::new_v4()));
    let command = format!(
        "tar -C {} -cf - . | base64 | tr -d '\\n'",
        shell_quote(&remote_dir)
    );
    let output = ctx.remote(
        worker,
        RemoteCommandSpec {
            argv: vec!["bash".to_string(), "-lc".to_string(), command],
            stdin: None,
            cwd: Some(PathBuf::from(&remote_dir)),
            tools_env: false,
            key: format!("{key}:fetch-collection"),
            purpose: "Fetch remote workenv collection as base64 tar.".into(),
            timeout_ms: 180_000,
        },
    )?;
    if output.exit_code.unwrap_or(1) != 0 {
        bail!(
            "remote collection tar failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    let raw = BASE64
        .decode(strip_ascii_whitespace(&output.stdout))
        .context("remote collection tar was not valid base64")?;
    extract_tar_safely(&raw, &temp)?;
    verify_local_collection(&temp, digest, Some(task), None, None, None, true)?;
    fs::rename(&temp, &local_dir).or_else(|_| {
        copy_dir_all(&temp, &local_dir)?;
        fs::remove_dir_all(&temp)?;
        Ok::<(), anyhow::Error>(())
    })?;
    Ok(local_dir)
}

fn extract_tar_safely(raw: &[u8], destination: &Path) -> Result<()> {
    fs::create_dir_all(destination)?;
    let mut archive = tar::Archive::new(Cursor::new(raw));
    let dest = destination
        .canonicalize()
        .unwrap_or_else(|_| destination.to_path_buf());
    for entry in archive.entries()? {
        let mut entry = entry?;
        let kind = entry.header().entry_type();
        if kind.is_symlink() || kind.is_hard_link() {
            bail!("collection archive contains a link");
        }
        let path = entry.path()?.into_owned();
        reject_archive_path(&path)?;
        let target = dest.join(&path);
        if !target.starts_with(&dest) {
            bail!("collection archive path escapes destination");
        }
        entry.unpack_in(destination)?;
    }
    Ok(())
}

fn verify_local_collection(
    local_dir: &Path,
    expected_digest: &str,
    task_id: Option<&str>,
    repo: Option<&Value>,
    source: Option<&str>,
    revision: Option<&str>,
    require_ok: bool,
) -> Result<Value> {
    let failed_result = |message: &str| {
        failed(
            "uncollected",
            message,
            json!({ "local_collection_dir": local_dir }),
        )
    };
    if !is_sha256_hex(expected_digest) {
        let result = failed_result("collection digest is not a lowercase SHA-256 hex digest");
        if require_ok {
            bail!(
                "{}",
                result["error"]
                    .as_str()
                    .unwrap_or("collection verification failed")
            );
        }
        return Ok(result);
    }
    let metadata_path = local_dir.join("metadata.json");
    if !metadata_path.is_file() {
        let result = failed_result("local collection metadata is missing");
        if require_ok {
            bail!(
                "{}",
                result["error"]
                    .as_str()
                    .unwrap_or("collection verification failed")
            );
        }
        return Ok(result);
    }
    let metadata = read_json(&metadata_path)?;
    if let Some(task_id) = task_id {
        if metadata.get("task_id").and_then(Value::as_str) != Some(task_id) {
            let result = failed_result("local collection task_id does not match request");
            if require_ok {
                bail!(
                    "{}",
                    result["error"]
                        .as_str()
                        .unwrap_or("collection verification failed")
                );
            }
            return Ok(result);
        }
    }
    if let Some(repo) = repo {
        if metadata.get("repo") != Some(repo) {
            let result = failed_result("local collection repo does not match task record");
            if require_ok {
                bail!(
                    "{}",
                    result["error"]
                        .as_str()
                        .unwrap_or("collection verification failed")
                );
            }
            return Ok(result);
        }
    }
    if let Some(source) = source {
        if metadata.get("source").and_then(Value::as_str) != Some(source) {
            let result = failed_result("local collection source does not match task record");
            if require_ok {
                bail!(
                    "{}",
                    result["error"]
                        .as_str()
                        .unwrap_or("collection verification failed")
                );
            }
            return Ok(result);
        }
    }
    if let Some(revision) = revision {
        if metadata.get("base").and_then(Value::as_str) != Some(revision) {
            let result = failed_result("local collection base revision does not match task record");
            if require_ok {
                bail!(
                    "{}",
                    result["error"]
                        .as_str()
                        .unwrap_or("collection verification failed")
                );
            }
            return Ok(result);
        }
    }
    let Some(archives) = metadata.get("archives").and_then(Value::as_object) else {
        let result = failed_result("local collection metadata has no archives");
        if require_ok {
            bail!(
                "{}",
                result["error"]
                    .as_str()
                    .unwrap_or("collection verification failed")
            );
        }
        return Ok(result);
    };
    if archives.is_empty() {
        let result = failed_result("local collection metadata has no archives");
        if require_ok {
            bail!(
                "{}",
                result["error"]
                    .as_str()
                    .unwrap_or("collection verification failed")
            );
        }
        return Ok(result);
    }
    let mut archive_paths = Vec::new();
    for name in archives.values() {
        let Some(name) = name.as_str() else {
            let result = failed_result("local collection archive name is not a string");
            if require_ok {
                bail!(
                    "{}",
                    result["error"]
                        .as_str()
                        .unwrap_or("collection verification failed")
                );
            }
            return Ok(result);
        };
        if !safe_archive_name(name) {
            let result = failed_result(&format!("local collection archive name is unsafe: {name}"));
            if require_ok {
                bail!(
                    "{}",
                    result["error"]
                        .as_str()
                        .unwrap_or("collection verification failed")
                );
            }
            return Ok(result);
        }
        let path = local_dir.join(name);
        if !path.is_file() {
            let result = failed_result(&format!("local collection archive is missing: {name}"));
            if require_ok {
                bail!(
                    "{}",
                    result["error"]
                        .as_str()
                        .unwrap_or("collection verification failed")
                );
            }
            return Ok(result);
        }
        archive_paths.push(path);
    }
    let mut metadata_without_digest = metadata.clone();
    if let Some(object) = metadata_without_digest.as_object_mut() {
        object.remove("collection_digest");
    }
    let actual = collection_digest(&metadata_without_digest, &archive_paths)?;
    if metadata.get("collection_digest").and_then(Value::as_str) != Some(expected_digest)
        || actual != expected_digest
    {
        let result = failed_result("local collection digest does not match archive bytes");
        if require_ok {
            bail!(
                "{}",
                result["error"]
                    .as_str()
                    .unwrap_or("collection verification failed")
            );
        }
        return Ok(result);
    }
    Ok(ok(
        "verified",
        json!({ "local_collection_dir": local_dir, "metadata": metadata }),
    ))
}

fn stop_recorded_task_services(
    ctx: &Context,
    worker: &str,
    task: &str,
    key: &str,
    record: &Value,
) -> Result<Value> {
    let mut inspected = Vec::new();
    let mut stopped = Vec::new();
    for execution_id in recorded_apoc_execution_ids(record) {
        let inspected_exec = get_remote_apoc_execution(ctx, worker, task, &execution_id)?;
        if !truthy(&inspected_exec, "ok") {
            return Ok(inspected_exec);
        }
        let execution = inspected_exec.get("execution").unwrap_or(&Value::Null);
        inspected.push(json!(execution_id));
        match apoc_execution_state(execution) {
            ExecutionState::Terminal => continue,
            ExecutionState::Unknown => {
                return Ok(failed(
                    "task_activity_unknown",
                    "recorded remote APoC execution liveness is unknown",
                    json!({ "execution_id": execution_id }),
                ));
            }
            ExecutionState::Live => {}
        }
        let labels = apoc_execution_labels(execution);
        if labels.get("workenv.task_id").and_then(Value::as_str) != Some(task)
            || labels.get("workenv.worker").and_then(Value::as_str) != Some(worker)
        {
            return Ok(failed(
                "unmatched_execution",
                "recorded remote APoC execution labels do not match this task and worker",
                json!({ "execution_id": execution_id, "labels": labels }),
            ));
        }
        if labels.get("workenv.kind").and_then(Value::as_str) != Some("service") {
            return Ok(failed(
                "live_task_activity",
                "recorded non-service APoC execution is still live",
                json!({ "live": [{ "kind": "apoc_execution", "id": execution_id, "labels": labels }] }),
            ));
        }
        let cancel = ssh_json(
            ctx,
            worker,
            vec![
                "apoc".to_string(),
                "execution".to_string(),
                "cancel".to_string(),
                execution_id.clone(),
                "--idempotency-key".to_string(),
                format!("{key}:service:{execution_id}"),
                "--timeout-ms".to_string(),
                "5000".to_string(),
                "--purpose".to_string(),
                format!("Stop workenv service execution {execution_id} for task {task}."),
                "--format".to_string(),
                "json".to_string(),
            ],
            &format!("{key}:cancel:{execution_id}"),
            60_000,
        )?;
        if cancel.get("ok") == Some(&Value::Bool(false)) {
            return Ok(cancel);
        }
        stopped.push(json!({ "id": execution_id, "cancel": cancel }));
    }
    Ok(ok(
        "services_stopped",
        json!({ "inspected_execution_ids": inspected, "stopped": stopped }),
    ))
}

fn inspect_task_activity(ctx: &Context, worker: &str, task: &str, record: &Value) -> Result<Value> {
    let apoc = inspect_remote_apoc_activity(ctx, worker, task, record)?;
    if !truthy(&apoc, "ok") {
        return Ok(apoc);
    }
    let herdr = inspect_remote_herdr_activity(ctx, worker, task, record)?;
    if !truthy(&herdr, "ok") {
        return Ok(herdr);
    }
    let live_apoc = apoc
        .get("live_executions")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let live_herdr = herdr
        .get("live_panes")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if !live_apoc.is_empty() || !live_herdr.is_empty() {
        let mut live = live_apoc;
        live.extend(live_herdr);
        return Ok(failed(
            "live_task_activity",
            "task still has live remote APoC executions or Herdr panes",
            json!({ "live": live }),
        ));
    }
    Ok(ok("clear", json!({ "apoc": apoc, "herdr": herdr })))
}

fn inspect_remote_apoc_activity(
    ctx: &Context,
    worker: &str,
    task: &str,
    record: &Value,
) -> Result<Value> {
    let mut live = Vec::new();
    let recorded = recorded_apoc_execution_ids(record);
    for execution_id in &recorded {
        let inspected = get_remote_apoc_execution(ctx, worker, task, execution_id)?;
        if !truthy(&inspected, "ok") {
            return Ok(inspected);
        }
        let execution = inspected.get("execution").unwrap_or(&Value::Null);
        match apoc_execution_state(execution) {
            ExecutionState::Live => live.push(json!({
                "kind": "apoc_execution",
                "id": execution_id,
                "status": execution.get("status"),
                "outcome": execution.get("outcome")
            })),
            ExecutionState::Unknown => {
                return Ok(failed(
                    "task_activity_unknown",
                    "remote APoC execution liveness is unknown",
                    json!({ "execution_id": execution_id, "execution": execution }),
                ));
            }
            ExecutionState::Terminal => {}
        }
    }
    let listed = crate::worker::list_worker_executions(ctx, worker)?;
    if listed.get("next_cursor").is_some() && !listed.get("next_cursor").unwrap().is_null() {
        return Ok(failed(
            "task_activity_unknown",
            "remote APoC execution list is truncated",
            json!({}),
        ));
    }
    let executions = listed
        .get("executions")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("remote APoC execution list did not return executions"))?;
    let worktree = record.get("worktree").and_then(Value::as_str);
    for execution in executions {
        if execution.get("cwd_truncated").and_then(Value::as_bool) == Some(true) {
            return Ok(failed(
                "task_activity_unknown",
                "remote APoC execution cwd is truncated",
                json!({ "execution_id": execution.get("id") }),
            ));
        }
        let status = execution
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or("");
        if !LIVE_EXECUTION_STATUSES.contains(&status) {
            continue;
        }
        let cwd = execution.get("cwd").and_then(Value::as_str).unwrap_or("");
        if let Some(worktree) = worktree {
            if cwd == worktree || cwd.starts_with(&format!("{worktree}/")) {
                live.push(json!({ "kind": "apoc_execution", "id": execution.get("id"), "status": status, "cwd": cwd }));
            }
        }
    }
    Ok(ok(
        "apoc_clear",
        json!({ "live_executions": live, "inspected_execution_ids": recorded }),
    ))
}

fn inspect_remote_herdr_activity(
    ctx: &Context,
    worker: &str,
    task: &str,
    record: &Value,
) -> Result<Value> {
    let mut live = Vec::new();
    let panes = runtime_herdr_ids(record, "pane_ids");
    let session = ctx.worker_session(worker)?;
    let worktree = required_record_str(record, "worktree")?;
    for kind in ["pane", "agent"] {
        let output = ctx.remote(
            worker,
            RemoteCommandSpec {
                argv: vec![
                    "herdr".into(),
                    "--session".into(),
                    session.clone(),
                    kind.into(),
                    "list".into(),
                ],
                stdin: None,
                cwd: Some(ctx.worker_root(worker)?),
                tools_env: true,
                key: fresh_read_key(&format!("{task}:herdr:{kind}:inventory")),
                purpose: "Inspect all remote Herdr activity before collecting or releasing a task."
                    .into(),
                timeout_ms: 60_000,
            },
        )?;
        if output.exit_code.unwrap_or(1) != 0 {
            return Ok(failed(
                "task_activity_unknown",
                "could not inventory remote Herdr activity",
                json!({"kind": kind}),
            ));
        }
        let payload: Value = serde_json::from_slice(&output.stdout)
            .context("remote Herdr inventory did not return JSON")?;
        let payload = payload.get("result").unwrap_or(&payload);
        let items = payload
            .as_array()
            .or_else(|| payload.get(format!("{kind}s")).and_then(Value::as_array));
        let Some(items) = items else {
            return Ok(failed(
                "task_activity_unknown",
                "remote Herdr inventory has an unknown shape",
                json!({"kind": kind, "herdr": payload}),
            ));
        };
        for item in items {
            let paths: Vec<_> = ["cwd", "foreground_cwd"]
                .iter()
                .filter_map(|field| item.get(field).and_then(Value::as_str))
                .filter(|path| path.starts_with('/'))
                .collect();
            let recorded = item
                .get("pane_id")
                .and_then(Value::as_str)
                .map(|id| panes.iter().any(|pane| pane == id))
                .unwrap_or(false);
            // A retained pane still owns a shell even when its agent is done.
            // Missing directory provenance cannot prove that it is unrelated.
            if recorded
                || paths.is_empty()
                || paths
                    .iter()
                    .any(|path| *path == worktree || path.starts_with(&format!("{worktree}/")))
            {
                live.push(json!({"kind": format!("herdr_{kind}"), "activity": item}));
            }
        }
    }
    Ok(ok(
        "herdr_clear",
        json!({ "live_panes": live, "inspected_pane_ids": panes }),
    ))
}

fn get_remote_apoc_execution(
    ctx: &Context,
    worker: &str,
    task: &str,
    execution_id: &str,
) -> Result<Value> {
    let payload = ssh_json(
        ctx,
        worker,
        vec![
            "apoc".to_string(),
            "execution".to_string(),
            "get".to_string(),
            execution_id.to_string(),
            "--purpose".to_string(),
            format!(
                "Inspect recorded workenv task {task} execution {execution_id} before release."
            ),
            "--verbosity".to_string(),
            "trace".to_string(),
            "--format".to_string(),
            "json".to_string(),
        ],
        &fresh_read_key(&format!("{task}:execution:{execution_id}")),
        60_000,
    )?;
    if payload.get("ok") == Some(&Value::Bool(false)) {
        return Ok(failed(
            "task_activity_unknown",
            "could not inspect recorded remote APoC execution",
            json!({ "execution_id": execution_id, "remote": payload }),
        ));
    }
    let execution = payload.get("data").cloned().unwrap_or(payload);
    if !execution.is_object() {
        return Ok(failed(
            "task_activity_unknown",
            "remote APoC execution get did not return an object",
            json!({ "execution_id": execution_id }),
        ));
    }
    Ok(ok(
        "inspected",
        json!({ "execution_id": execution_id, "execution": execution }),
    ))
}

fn validate_reservation(ctx: &Context, worker: &str, reservation_id: &str) -> Result<Value> {
    let reservation = ctx.apoc("reservation_get", json!({ "id": reservation_id, "purpose": format!("Read workenv reservation {reservation_id}.") }))?;
    if truthy(&reservation, "released")
        || reservation.get("key").and_then(Value::as_str)
            != Some(&format!("workenv/worker/{worker}"))
    {
        return Ok(failed(
            "reservation_mismatch",
            "reservation does not belong to this worker",
            json!({ "reservation": reservation }),
        ));
    }
    if expired(&reservation) {
        return Ok(failed(
            "reservation_expired",
            "reservation has expired",
            json!({ "reservation": reservation }),
        ));
    }
    let lease = reservation
        .get("lease_id")
        .or_else(|| reservation.get("lease"))
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("reservation has no lease"))?;
    let session_id = lease
        .strip_prefix("session/")
        .ok_or_else(|| anyhow!("reservation lease is not a session"))?;
    let session = ctx.apoc("session_get", json!({ "id": session_id, "purpose": format!("Read workenv reservation {reservation_id} owning session.") }))?;
    if truthy(&session, "closed") || truthy(&session, "closing") {
        return Ok(failed(
            "reservation_session_closed",
            "reservation owning session is closed",
            json!({ "reservation": reservation, "session": session }),
        ));
    }
    if expired(&session) {
        return Ok(failed(
            "reservation_session_expired",
            "reservation owning session has expired",
            json!({ "reservation": reservation, "session": session }),
        ));
    }
    Ok(ok(
        "verified",
        json!({ "reservation": reservation, "session": session }),
    ))
}

fn expired(value: &Value) -> bool {
    let now = now_ms();
    value
        .get("expires_at")
        .and_then(Value::as_i64)
        .is_some_and(|expires| expires <= now)
        || value
            .get("expiresAt")
            .and_then(Value::as_i64)
            .is_some_and(|expires| expires <= now)
}

fn require_worker_bound(ctx: &Context, record: &Value) -> Result<()> {
    let worker = required_record_str(record, "worker")?;
    let _ = ctx.worker(worker)?;
    validate_task_host_binding(ctx, record)?;
    Ok(())
}

fn require_runnable_task(record: &Value) -> Result<()> {
    let status = record.get("status").and_then(Value::as_str).unwrap_or("");
    if !RUNNABLE_TASK_STATUSES.contains(&status) {
        bail!("central task record is not in a runnable state");
    }
    Ok(())
}

fn task_requested_profile(request: &Value, project_spec: &ProjectSpec) -> Result<Option<String>> {
    let requested = request.get("profile").and_then(Value::as_str);
    if let Some(name) = requested {
        profiles::validate_name(name)?;
        if let Some(project_profile) = project_spec.worker_profile.as_deref() {
            if project_profile != name {
                bail!("task profile request conflicts with the project worker_profile");
            }
        }
        return Ok(Some(name.to_string()));
    }
    Ok(project_spec.worker_profile.clone())
}

fn profile_binding_for_worker(ctx: &Context, worker: &str) -> Result<Value> {
    profiles::binding(ctx, worker)
}

fn resolved_worker_record(ctx: &Context, worker: &str) -> Result<Value> {
    let resolved = hosts::resolve(ctx, worker)?;
    Ok(json!({
        "name": resolved.name,
        "host_id": resolved.host_id,
        "transport": match resolved.transport {
            ResolvedTransport::Local => json!({"kind": "local"}),
            ResolvedTransport::Ssh { ref target } => json!({"kind": "ssh", "target": target}),
        },
        "root": resolved.root,
        "environment_root": resolved.environment_root,
        "session": resolved.session,
        "tools": match resolved.tools {
            ResolvedTools::Native => json!({"kind": "native"}),
            ResolvedTools::Devenv { ref executable } => {
                json!({"kind": "devenv", "executable": executable})
            }
        },
        "provider": match resolved.provider {
            Provider::ExeDev => "exe.dev",
            Provider::Existing => "existing",
        },
        "lifetime": worker_lifetime_value(&resolved),
    }))
}

fn validate_task_host_binding(ctx: &Context, record: &Value) -> Result<()> {
    let worker = required_record_str(record, "worker")?;
    let current = resolved_worker_record(ctx, worker)?;
    if let Some(recorded) = record.get("worker_host") {
        if recorded != &current {
            bail!("Task worker host binding changed; restore its recorded host/root/session before continuing");
        }
    } else {
        let worktree = required_record_str(record, "worktree")?;
        let root = ctx.worker_root(worker)?.to_string_lossy().into_owned();
        if !remote_path_contains(&root, worktree) {
            bail!("Legacy task record worktree is outside the currently configured worker root");
        }
    }
    if let Some(worktree) = record.get("worktree").and_then(Value::as_str) {
        let root = record
            .get("allocation_root")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .unwrap_or_else(|| {
                ctx.worker_root(worker)
                    .map(|p| p.to_string_lossy().into_owned())
                    .unwrap_or_default()
            });
        if root.is_empty() || !remote_path_contains(&root, worktree) {
            bail!("Task worktree is outside its recorded allocation or worker root");
        }
    }
    Ok(())
}

fn remote_task_collection_root(
    ctx: &Context,
    worker: &str,
    task: &str,
) -> Result<String> {
    let root = ctx.worker_root(worker)?.to_string_lossy().into_owned();
    Ok(format!("{root}/collections/{task}"))
}

fn worker_lifetime_value(worker: &hosts::ResolvedWorker) -> &'static str {
    match worker.lifetime {
        hosts::Lifetime::Static => "static",
        hosts::Lifetime::Ephemeral => "ephemeral",
    }
}

fn remote_path_contains(parent: &str, child: &str) -> bool {
    child == parent || child.starts_with(&format!("{}/", parent.trim_end_matches('/')))
}

fn record_profile_name(record: &Value) -> Option<&str> {
    record
        .pointer("/worker_profile/name")
        .and_then(Value::as_str)
}

fn worker_matches_profile(ctx: &Context, worker: &str, profile: Option<&str>) -> Result<bool> {
    match (profile, profiles::resolve(ctx, worker)?) {
        (None, _) => Ok(true),
        (Some(expected), Some(actual)) => Ok(actual.name == expected),
        (Some(_), None) => Ok(false),
    }
}

fn ensure_worker_matches_profile(ctx: &Context, worker: &str, profile: Option<&str>) -> Result<()> {
    if !worker_matches_profile(ctx, worker, profile)? {
        let expected = profile.unwrap_or("");
        bail!("worker {worker} is not assigned to profile {expected}");
    }
    Ok(())
}

fn ensure_worker_matches_class(ctx: &Context, worker: &str, class: Option<&str>) -> Result<()> {
    if let Some(class) = class {
        if ctx.worker(worker)?.get("class").and_then(Value::as_str) != Some(class) {
            bail!("worker {worker} is not class {class}");
        }
    }
    Ok(())
}

fn select_available_worker(
    ctx: &Context,
    class: Option<&str>,
    profile: Option<&str>,
) -> Result<String> {
    let workers = ctx
        .fleet
        .get("workers")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("fleet has no workers"))?;
    for worker in workers {
        if class.is_some() && worker.get("class").and_then(Value::as_str) != class {
            continue;
        }
        let name = worker
            .get("name")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("worker has no name"))?;
        if !worker_matches_profile(ctx, name, profile)? {
            continue;
        }
        if open_task_records_for_worker(&ctx.state, name)?.is_empty() {
            return Ok(name.to_string());
        }
    }
    bail!("no available worker")
}

struct ProjectSpec {
    repo: Value,
    remote_url: String,
    class: Option<String>,
    worker_profile: Option<String>,
}

fn project_config(fleet: &Value, project: &str) -> Result<ProjectSpec> {
    let spec = fleet
        .get("projects")
        .and_then(Value::as_object)
        .and_then(|projects| projects.get(project))
        .ok_or_else(|| anyhow!("unknown project: {project}"))?;
    let repository = spec
        .get("repository")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("project has no repository"))?;
    let (owner, name) = repository
        .split_once('/')
        .ok_or_else(|| anyhow!("invalid repository: {repository}"))?;
    if owner.is_empty() || name.is_empty() || name.contains('/') {
        bail!("invalid repository: {repository}");
    }
    Ok(ProjectSpec {
        repo: json!({ "owner": owner, "name": name }),
        remote_url: format!("https://github.com/{owner}/{name}.git"),
        class: spec.get("class").and_then(Value::as_str).map(str::to_owned),
        worker_profile: spec
            .get("worker_profile")
            .and_then(Value::as_str)
            .map(str::to_owned),
    })
}

struct TaskRecordUpdate<'a> {
    task_id: &'a str,
    worker: &'a str,
    project: &'a str,
    repo: Value,
    revision: &'a str,
    reservation_id: &'a str,
    session_id: Option<&'a str>,
    source: &'a str,
    status: &'a str,
    worktree: Option<&'a str>,
    worker_profile: Value,
    worker_host: Value,
    allocation_id: Option<&'a str>,
    allocation_root: Option<&'a str>,
    worker_lifetime: Option<&'a str>,
    controller_execution_id: Option<&'a str>,
}

fn upsert_task_record(state: &Path, update: TaskRecordUpdate<'_>) -> Result<Value> {
    let mut existing = read_task_record(state, update.task_id)?.unwrap_or_else(|| json!({}));
    let mut controller_ids = existing
        .get("controller_execution_ids")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if let Some(id) = update.controller_execution_id {
        if !controller_ids
            .iter()
            .any(|value| value.as_str() == Some(id))
        {
            controller_ids.push(json!(id));
        }
    }
    let remote_ids = existing
        .get("remote_execution_ids")
        .cloned()
        .unwrap_or_else(|| json!([]));
    existing["task_id"] = json!(update.task_id);
    existing["worker"] = json!(update.worker);
    existing["project"] = json!(update.project);
    existing["repo"] = update.repo;
    existing["revision"] = json!(update.revision);
    existing["reservation_id"] = json!(update.reservation_id);
    existing["session_id"] = update.session_id.map_or(Value::Null, |value| json!(value));
    existing["source"] = json!(update.source);
    existing["status"] = json!(update.status);
    if let Some(worktree) = update.worktree {
        existing["worktree"] = json!(worktree);
    }
    existing["worker_profile"] = update.worker_profile;
    existing["worker_host"] = update.worker_host;
    if let Some(allocation_id) = update.allocation_id {
        existing["allocation_id"] = json!(allocation_id);
    }
    if let Some(allocation_root) = update.allocation_root {
        existing["allocation_root"] = json!(allocation_root);
    }
    if let Some(worker_lifetime) = update.worker_lifetime {
        existing["worker_lifetime"] = json!(worker_lifetime);
    }
    existing["controller_execution_ids"] = Value::Array(controller_ids);
    existing["remote_execution_ids"] = remote_ids;
    existing["updated_at"] = json!(now_ms());
    write_task_record(state, update.task_id, &existing)?;
    Ok(existing)
}

fn record_remote_execution(state: &Path, task: &str, execution_id: &str) -> Result<()> {
    let mut record =
        read_task_record(state, task)?.ok_or_else(|| anyhow!("central task record is missing"))?;
    require_runnable_task(&record)?;
    append_unique(&mut record, "remote_execution_ids", execution_id);
    let mut runtime = normalize_runtime(record.get("runtime"));
    append_unique(&mut runtime, "apoc_execution_ids", execution_id);
    record["runtime"] = runtime;
    record["updated_at"] = json!(now_ms());
    write_task_record(state, task, &record)
}

fn append_unique(record: &mut Value, key: &str, value: &str) {
    let array = record
        .as_object_mut()
        .unwrap()
        .entry(key.to_string())
        .or_insert_with(|| json!([]));
    if !array
        .as_array()
        .is_some_and(|items| items.iter().any(|item| item.as_str() == Some(value)))
    {
        array.as_array_mut().unwrap().push(json!(value));
    }
}

fn normalize_runtime(raw: Option<&Value>) -> Value {
    let mut runtime = raw.cloned().unwrap_or_else(|| json!({}));
    if !runtime.get("herdr").is_some_and(Value::is_object) {
        runtime["herdr"] = json!({});
    }
    if !runtime
        .get("apoc_execution_ids")
        .is_some_and(Value::is_array)
    {
        runtime["apoc_execution_ids"] = json!([]);
    }
    runtime
}

fn read_task_record(state: &Path, task: &str) -> Result<Option<Value>> {
    let path = task_record_path(state, task);
    if !path.exists() {
        return Ok(None);
    }
    Ok(Some(read_json(&path)?))
}

fn write_task_record(state: &Path, task: &str, value: &Value) -> Result<()> {
    write_json(&task_record_path(state, task), value)
}

fn open_task_records_for_worker(state: &Path, worker: &str) -> Result<Vec<Value>> {
    Ok(open_task_records(state)?
        .into_iter()
        .filter(|record| record.get("worker").and_then(Value::as_str) == Some(worker))
        .collect())
}

fn open_task_records(state: &Path) -> Result<Vec<Value>> {
    let dir = state.join("tasks");
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut records = Vec::new();
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        if entry.path().extension().and_then(|value| value.to_str()) != Some("json") {
            continue;
        }
        let record = read_json(&entry.path())
            .with_context(|| format!("read task record {}", entry.path().display()))?;
        if !TERMINAL_TASK_STATUSES
            .contains(&record.get("status").and_then(Value::as_str).unwrap_or(""))
        {
            records.push(record);
        }
    }
    Ok(records)
}

fn task_record_path(state: &Path, task: &str) -> PathBuf {
    let safe = safe_name(task);
    let mut hasher = Sha256::new();
    hasher.update(task.as_bytes());
    state.join("tasks").join(format!(
        "{}.{}.json",
        &safe[..safe.len().min(80)],
        hex(&hasher.finalize())
    ))
}

fn summaries(records: &[Value]) -> Value {
    Value::Array(records.iter().map(summary).collect())
}

fn summary(record: &Value) -> Value {
    json!({
        "task_id": record.get("task_id"),
        "project": record.get("project"),
        "revision": record.get("revision"),
        "status": record.get("status"),
        "reservation_id": record.get("reservation_id"),
        "session_id": record.get("session_id"),
        "worker_profile": record.get("worker_profile").cloned().unwrap_or(Value::Null)
    })
}

fn recorded_apoc_execution_ids(record: &Value) -> Vec<String> {
    let mut ids = BTreeSet::new();
    if let Some(values) = record.get("remote_execution_ids").and_then(Value::as_array) {
        for value in values {
            if let Some(id) = value.as_str() {
                ids.insert(id.to_string());
            }
        }
    }
    if let Some(values) = record
        .pointer("/runtime/apoc_execution_ids")
        .and_then(Value::as_array)
    {
        for value in values {
            if let Some(id) = value.as_str() {
                ids.insert(id.to_string());
            }
        }
    }
    ids.into_iter().collect()
}

fn runtime_herdr_ids(record: &Value, key: &str) -> Vec<String> {
    record
        .pointer(&format!("/runtime/herdr/{key}"))
        .and_then(Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
enum ExecutionState {
    Live,
    Terminal,
    Unknown,
}

fn apoc_execution_state(execution: &Value) -> ExecutionState {
    let outcome = execution
        .get("outcome")
        .and_then(Value::as_str)
        .unwrap_or("");
    if outcome == "pending" {
        return ExecutionState::Live;
    }
    if TERMINAL_EXECUTION_OUTCOMES.contains(&outcome) {
        return ExecutionState::Terminal;
    }
    let status = execution
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("");
    if LIVE_EXECUTION_STATUSES.contains(&status) {
        ExecutionState::Live
    } else if TERMINAL_EXECUTION_STATUSES.contains(&status) {
        ExecutionState::Terminal
    } else {
        ExecutionState::Unknown
    }
}

fn apoc_execution_labels(execution: &Value) -> Value {
    let Some(spec) = execution.get("spec").and_then(Value::as_object) else {
        return json!({});
    };
    let Some(labels) = spec.get("labels") else {
        return json!({});
    };
    if let Some(object) = labels.as_object() {
        let mut parsed = Map::new();
        for (key, value) in object {
            if let Some(value) = value.as_str() {
                parsed.insert(key.clone(), json!(value));
            }
        }
        return Value::Object(parsed);
    }
    if let Some(array) = labels.as_array() {
        let mut parsed = Map::new();
        for value in array {
            if let Some(label) = value.as_str() {
                if let Some((key, value)) = label.split_once('=') {
                    parsed.insert(key.to_string(), json!(value));
                }
            }
        }
        return Value::Object(parsed);
    }
    json!({})
}

fn collection_digest(metadata_without_digest: &Value, archive_paths: &[PathBuf]) -> Result<String> {
    let mut digest = Sha256::new();
    digest.update(canonical_json(metadata_without_digest).as_bytes());
    let mut paths = archive_paths.to_vec();
    paths.sort_by_key(|path| path.file_name().map(|name| name.to_os_string()));
    for path in paths {
        let name = path
            .file_name()
            .and_then(|value| value.to_str())
            .ok_or_else(|| anyhow!("archive path has no file name"))?;
        digest.update(name.as_bytes());
        digest.update(b"\0");
        digest.update(fs::read(&path)?);
        digest.update(b"\0");
    }
    Ok(hex(&digest.finalize()))
}

fn reject_archive_path(path: &Path) -> Result<()> {
    if path.is_absolute() {
        bail!("collection archive path is absolute");
    }
    for component in path.components() {
        if matches!(component, Component::ParentDir | Component::Prefix(_)) {
            bail!("collection archive path escapes destination");
        }
    }
    Ok(())
}

fn safe_archive_name(value: &str) -> bool {
    let path = Path::new(value);
    !value.is_empty()
        && !path.is_absolute()
        && path
            .components()
            .all(|component| !matches!(component, Component::ParentDir | Component::Prefix(_)))
        && path.file_name().and_then(|name| name.to_str()) == Some(value)
}

fn should_record_claim(status: Option<&str>) -> bool {
    matches!(status, Some("claimed"))
        || status.is_some_and(|status| AMBIGUOUS_REMOTE_STATUSES.contains(&status))
}

fn required_str<'a>(value: &'a Value, key: &str) -> Result<&'a str> {
    value
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow!("{key} is required"))
}

fn required_remote_str<'a>(value: &'a Value, key: &str) -> Result<&'a str> {
    value
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow!("remote result missing {key}"))
}

fn required_record_str<'a>(value: &'a Value, key: &str) -> Result<&'a str> {
    value
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow!("central task record missing {key}"))
}

fn string_field(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(Value::as_str).map(str::to_owned)
}

fn truthy(value: &Value, key: &str) -> bool {
    value.get(key).and_then(Value::as_bool) == Some(true)
}

fn ok(status: &str, fields: Value) -> Value {
    let mut result = Map::new();
    result.insert("status".to_string(), json!(status));
    result.insert("ok".to_string(), json!(true));
    merge_fields(&mut result, fields);
    Value::Object(result)
}

fn failed(status: &str, error: impl Into<String>, fields: Value) -> Value {
    let mut result = Map::new();
    result.insert("status".to_string(), json!(status));
    result.insert("ok".to_string(), json!(false));
    result.insert("error".to_string(), json!(error.into()));
    merge_fields(&mut result, fields);
    Value::Object(result)
}

fn merge_fields(result: &mut Map<String, Value>, fields: Value) {
    if let Some(fields) = fields.as_object() {
        for (key, value) in fields {
            result.insert(key.clone(), value.clone());
        }
    }
}

fn fresh_read_key(prefix: &str) -> String {
    format!("{prefix}:read:{}", Uuid::new_v4())
}

fn validate_task_id(value: &str) -> Result<()> {
    let mut chars = value.chars();
    let Some(first) = chars.next() else {
        bail!("task_id is required");
    };
    if value.len() > 128
        || !first.is_ascii_alphanumeric()
        || !chars.all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-'))
    {
        bail!("task_id is not safe for remote workspace use");
    }
    Ok(())
}

fn is_full_sha(value: &str) -> bool {
    value.len() == 40
        && value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

fn is_sha256_hex(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

fn safe_name(value: &str) -> String {
    let safe: String = value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-') {
                ch
            } else {
                '_'
            }
        })
        .collect();
    if safe.is_empty() {
        "_".to_string()
    } else {
        safe
    }
}

fn strip_ascii_whitespace(bytes: &[u8]) -> Vec<u8> {
    bytes
        .iter()
        .copied()
        .filter(|byte| !byte.is_ascii_whitespace())
        .collect()
}

fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

fn nonempty_or(value: &str, fallback: &str) -> String {
    if value.is_empty() {
        fallback.to_string()
    } else {
        value.to_string()
    }
}

fn copy_dir_all(src: &Path, dst: &Path) -> Result<()> {
    fs::create_dir_all(dst)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let ty = entry.file_type()?;
        let target = dst.join(entry.file_name());
        if ty.is_dir() {
            copy_dir_all(&entry.path(), &target)?;
        } else if ty.is_file() {
            fs::copy(entry.path(), target)?;
        } else {
            bail!("refusing to copy non-file collection entry");
        }
    }
    Ok(())
}

fn canonical_json(value: &Value) -> String {
    match value {
        Value::Null => "null".to_string(),
        Value::Bool(true) => "true".to_string(),
        Value::Bool(false) => "false".to_string(),
        Value::Number(number) => number.to_string(),
        Value::String(text) => canonical_string(text),
        Value::Array(values) => format!(
            "[{}]",
            values
                .iter()
                .map(canonical_json)
                .collect::<Vec<_>>()
                .join(",")
        ),
        Value::Object(object) => {
            let mut items = BTreeMap::new();
            for (key, value) in object {
                items.insert(key, value);
            }
            format!(
                "{{{}}}",
                items
                    .into_iter()
                    .map(|(key, value)| format!(
                        "{}:{}",
                        canonical_string(key),
                        canonical_json(value)
                    ))
                    .collect::<Vec<_>>()
                    .join(",")
            )
        }
    }
}

fn canonical_string(value: &str) -> String {
    let mut out = String::from("\"");
    for ch in value.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0c}' => out.push_str("\\f"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            ch if ch <= '\u{1f}' => out.push_str(&format!("\\u{:04x}", ch as u32)),
            ch if ch.is_ascii() => out.push(ch),
            ch => {
                let code = ch as u32;
                if code <= 0xffff {
                    out.push_str(&format!("\\u{code:04x}"));
                } else {
                    let code = code - 0x1_0000;
                    let high = 0xd800 + ((code >> 10) & 0x3ff);
                    let low = 0xdc00 + (code & 0x3ff);
                    out.push_str(&format!("\\u{high:04x}\\u{low:04x}"));
                }
            }
        }
    }
    out.push('"');
    out
}
