use anyhow::{bail, Context as _, Result};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    fs,
    path::{Path, PathBuf},
};
use uuid::Uuid;

use crate::hosts::{self, RemoteCommandSpec, ResolvedTools, ResolvedTransport};
use crate::process::{read_json, write_json};
use crate::{profiles, worker, Context};

const BUILD_TIMEOUT_MS: u64 = 600_000;
const BUILD_WAIT_SLICE_MS: u64 = 30_000;

pub fn install(ctx: &Context, selector: Option<&str>, key: &str) -> Result<Value> {
    install_from(ctx, selector, key, &ctx.root)
}

pub fn install_from(
    ctx: &Context,
    selector: Option<&str>,
    key: &str,
    source: &Path,
) -> Result<Value> {
    if key.is_empty() {
        bail!("key is required");
    }
    let source_identity = source_request_identity(source)?;
    let workers = selected_worker_names(ctx, selector)?;
    let mut rows = Vec::new();
    for worker in workers {
        let cli_key = format!("{key}:cli:{worker}");
        if let Some(cli) =
            wait_for_own_checkpointed_build(ctx, &worker, &cli_key, Some(&source_identity))?
        {
            rows.push(merge(json!({"worker":worker}), cli));
            continue;
        }
        if let Some(cli) = wait_for_environment_build_blocker(ctx, &worker, &cli_key)? {
            rows.push(merge(json!({"worker":worker}), cli));
            continue;
        }
        let source_sync = worker::sync_worker_sources_from(ctx, &worker, key, source)?;
        if source_sync.get("ok") != Some(&json!(true)) {
            rows.push(merge(
                json!({"worker":worker,"source_sync":source_sync}),
                failed("source_sync_failed", "source sync did not complete"),
            ));
            continue;
        }
        let cli = ensure_cli_after_sync(
            ctx,
            &worker,
            &format!("{key}:cli:{worker}"),
            &source_identity,
        )?;
        rows.push(merge(
            json!({"worker":worker,"source_sync":source_sync}),
            cli,
        ));
    }
    let ready = rows.iter().all(|row| row.get("ok") == Some(&json!(true)));
    let pending = rows.iter().any(is_pending_result);
    Ok(ok_field(
        if ready {
            "installed"
        } else if pending {
            "pending"
        } else {
            "partial"
        },
        ready,
        json!({"workers":rows}),
    ))
}

pub fn ensure_cli(ctx: &Context, worker: &str, key: &str) -> Result<Value> {
    if key.is_empty() {
        bail!("key is required");
    }
    let worker = ctx.worker_name(worker)?;
    ensure_cli_after_sync(ctx, &worker, key, &source_request_identity(&ctx.root)?)
}

fn ensure_cli_after_sync(
    ctx: &Context,
    worker: &str,
    key: &str,
    source_identity: &Value,
) -> Result<Value> {
    if let Some(cli) = wait_for_own_checkpointed_build(ctx, worker, key, Some(source_identity))? {
        return Ok(merge(json!({"worker":worker}), cli));
    }
    if let Some(cli) = wait_for_environment_build_blocker(ctx, worker, key)? {
        return Ok(merge(json!({"worker":worker}), cli));
    }
    let configured = configure_local_cli(ctx, worker, key)?;
    if configured.get("ok") != Some(&json!(true)) {
        return Ok(merge(
            json!({"worker":worker,"configuration":configured}),
            failed(
                "cli_config_failed",
                "local CLI configuration did not complete",
            ),
        ));
    }
    let build = ensure_cli_after_config(ctx, worker, key, source_identity)?;
    Ok(merge(
        json!({"worker":worker,"configuration":configured}),
        build,
    ))
}

fn ensure_cli_after_config(
    ctx: &Context,
    worker: &str,
    key: &str,
    source_identity: &Value,
) -> Result<Value> {
    if let Some(build) = wait_for_own_checkpointed_build(ctx, worker, key, Some(source_identity))? {
        return Ok(build);
    }
    if let Some(build) = wait_for_environment_build_blocker(ctx, worker, key)? {
        return Ok(build);
    }
    let resolved = hosts::resolve(ctx, worker)?;
    let environment_root = resolved.environment_root.clone();
    let checkpoint = checkpoint_path(ctx, worker);
    let config = config_override(ctx, &resolved)?;
    let execution_id = start_build(ctx, worker, key, &environment_root, config.as_deref())?;
    write_json(
        &checkpoint,
        &json!({
            "schema":1,
            "worker":worker,
            "key":key,
            "execution_id":execution_id,
            "status":"running",
            "environment_root":environment_root,
            "source_identity":source_identity,
        }),
    )?;
    wait_for_build(
        ctx,
        worker,
        key,
        &environment_root,
        &checkpoint,
        &execution_id,
        Some(source_identity),
    )
}

fn start_build(
    ctx: &Context,
    worker: &str,
    key: &str,
    root: &Path,
    config: Option<&Path>,
) -> Result<String> {
    let purpose = format!("Build and install native workenv CLI for {worker}.");
    let mut child = vec![
        "python3".to_string(),
        format!("{}/remote/build_workenv.py", root.to_string_lossy()),
        "--root".to_string(),
        root.to_string_lossy().into_owned(),
    ];
    if let Some(config) = config {
        child.extend([
            "--config".to_string(),
            config.to_string_lossy().into_owned(),
        ]);
    }
    let mut argv = vec![
        "apoc".to_string(),
        "execution".to_string(),
        "start".to_string(),
        "python3".to_string(),
        "--purpose".to_string(),
        purpose.clone(),
        "--idempotency-key".to_string(),
        format!("{key}:build"),
        "--cwd".to_string(),
        root.to_string_lossy().into_owned(),
        "--timeout-ms".to_string(),
        BUILD_TIMEOUT_MS.to_string(),
        "--expect-exit-code".to_string(),
        "0".to_string(),
        "--verbosity".to_string(),
        "trace".to_string(),
        "--label".to_string(),
        "workenv.component=cli-install".to_string(),
        "--label".to_string(),
        format!("workenv.worker={worker}"),
        "--label".to_string(),
        format!("workenv.worker.name={worker}"),
        "--telemetry".to_string(),
        "--format".to_string(),
        "json".to_string(),
        "--".to_string(),
    ];
    argv.extend(child.into_iter().skip(1));
    let value = remote_apoc_json(
        ctx,
        worker,
        argv,
        &format!("{key}:build:start"),
        &purpose,
        root,
        45_000,
    )?;
    value
        .pointer("/execution/id")
        .or_else(|| value.get("id"))
        .and_then(Value::as_str)
        .map(str::to_string)
        .context("APoC build start returned no execution ID")
}

fn wait_for_build(
    ctx: &Context,
    worker: &str,
    key: &str,
    root: &Path,
    checkpoint: &Path,
    execution_id: &str,
    source_identity: Option<&Value>,
) -> Result<Value> {
    let purpose = format!("Wait for native workenv CLI build on {worker}.");
    let waited = remote_apoc_json(
        ctx,
        worker,
        vec![
            "apoc".into(),
            "execution".into(),
            "wait".into(),
            execution_id.into(),
            "--timeout-ms".into(),
            BUILD_WAIT_SLICE_MS.to_string(),
            "--purpose".into(),
            purpose.clone(),
            "--format".into(),
            "json".into(),
        ],
        &fresh_read_key(&format!("{key}:build:wait")),
        &purpose,
        root,
        BUILD_WAIT_SLICE_MS + 15_000,
    )?;
    if wait_is_pending_or_unknown(&waited) {
        write_json(
            checkpoint,
            &checkpoint_payload(
                worker,
                key,
                execution_id,
                "pending",
                root,
                source_identity,
                json!({"last_wait":waited}),
            ),
        )?;
        return Ok(ok_field(
            "pending",
            false,
            json!({"execution_id":execution_id,"wait":waited}),
        ));
    }
    let logs = build_logs(ctx, worker, key, root, execution_id)?;
    if waited.get("outcome") != Some(&json!("passed")) {
        write_json(
            checkpoint,
            &checkpoint_payload(
                worker,
                key,
                execution_id,
                "failed",
                root,
                source_identity,
                json!({"wait":waited,"logs":logs}),
            ),
        )?;
        return Ok(ok_field(
            "build_failed",
            false,
            json!({"execution_id":execution_id,"wait":waited,"logs":logs}),
        ));
    }
    let stdout = logs
        .get("stdout")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim();
    let mut helper: Value = serde_json::from_str(stdout).with_context(|| {
        format!("native build helper output was not JSON for execution {execution_id}")
    })?;
    let helper_ok = helper.get("ok") == Some(&json!(true));
    let status = if helper_ok {
        "cli_ready"
    } else {
        "build_failed"
    };
    helper["execution_id"] = json!(execution_id);
    write_json(
        checkpoint,
        &checkpoint_payload(
            worker,
            key,
            execution_id,
            status,
            root,
            source_identity,
            json!({"result":helper}),
        ),
    )?;
    Ok(ok_field(
        status,
        helper_ok,
        json!({"execution_id":execution_id,"build":helper}),
    ))
}

fn build_logs(
    ctx: &Context,
    worker: &str,
    key: &str,
    root: &Path,
    execution_id: &str,
) -> Result<Value> {
    let purpose = format!("Read native workenv CLI build logs for {worker}.");
    let logs = remote_apoc_json(
        ctx,
        worker,
        vec![
            "apoc".into(),
            "execution".into(),
            "logs".into(),
            execution_id.into(),
            "--tail-bytes".into(),
            "16777216".into(),
            "--purpose".into(),
            purpose.clone(),
            "--format".into(),
            "json".into(),
        ],
        &fresh_read_key(&format!("{key}:build:logs")),
        &purpose,
        root,
        45_000,
    )?;
    if logs.get("stdout_truncated") == Some(&json!(true))
        || logs.get("stderr_truncated") == Some(&json!(true))
    {
        bail!("Build execution {execution_id} output was truncated");
    }
    Ok(logs)
}

fn remote_apoc_json(
    ctx: &Context,
    worker: &str,
    argv: Vec<String>,
    key: &str,
    purpose: &str,
    cwd: &Path,
    timeout_ms: u64,
) -> Result<Value> {
    let output = ctx.remote(
        worker,
        RemoteCommandSpec {
            argv,
            stdin: None,
            cwd: Some(cwd.to_path_buf()),
            tools_env: true,
            key: key.to_string(),
            purpose: purpose.to_string(),
            timeout_ms,
        },
    )?;
    match serde_json::from_slice(&output.stdout) {
        Ok(value) => Ok(value),
        Err(error) if output.exit_code != Some(0) => bail!(
            "remote APoC command failed: {}; stdout was not JSON: {error}",
            String::from_utf8_lossy(&output.stderr)
        ),
        Err(error) => Err(error).context("Command did not return valid JSON"),
    }
}

fn configure_local_cli(ctx: &Context, worker: &str, key: &str) -> Result<Value> {
    let resolved = hosts::resolve(ctx, worker)?;
    if resolved.transport == ResolvedTransport::Local
        && same_path(&resolved.environment_root, &ctx.root)
    {
        return Ok(ok(
            "configuration_current",
            json!({"root":resolved.environment_root}),
        ));
    }
    let payload = local_controller_payload(ctx, worker)?;
    let script = r#"import json,os,pathlib,sys
root=pathlib.Path(sys.argv[1])
payload=json.loads(sys.stdin.read())
fleet_payload=payload['fleet']
profile_payloads=payload.get('profiles',{})
def write_json_file(path,value):
    path.parent.mkdir(parents=True,exist_ok=True)
    tmp=path.with_name('.'+path.name+'.tmp')
    with tmp.open('w') as handle:
        json.dump(value,handle,sort_keys=True,separators=(',',':'))
        handle.write('\n')
        handle.flush()
        os.fsync(handle.fileno())
    os.replace(tmp,path)
root.mkdir(parents=True,exist_ok=True)
for name,value in sorted(profile_payloads.items()):
    write_json_file(root/'profiles'/(name+'.json'),value)
fleet=root/'fleet.json'
if fleet.exists():
    try:
        current=json.loads(fleet.read_text())
    except Exception:
        current=None
    if isinstance(current,dict) and current.get('local_controller') is True:
        print(json.dumps({'ok':True,'status':'configuration_preserved','path':str(fleet),'profiles':sorted(profile_payloads)}))
        raise SystemExit(0)
write_json_file(fleet,fleet_payload)
print(json.dumps({'ok':True,'status':'configured','path':str(fleet),'profiles':sorted(profile_payloads)}))"#;
    let output = ctx.remote(
        worker,
        RemoteCommandSpec {
            argv: vec![
                "python3".into(),
                "-c".into(),
                script.into(),
                resolved.environment_root.to_string_lossy().into_owned(),
            ],
            stdin: Some(serde_json::to_vec(&payload)?),
            cwd: Some(PathBuf::from("/")),
            tools_env: false,
            key: format!("{key}:configure-local-cli"),
            purpose: format!(
                "Configure native workenv CLI local controller metadata for {worker}."
            ),
            timeout_ms: 30_000,
        },
    )?;
    if output.exit_code != Some(0) {
        bail!(
            "native CLI configuration failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    output.json()
}

fn local_controller_payload(ctx: &Context, worker: &str) -> Result<Value> {
    let resolved = hosts::resolve(ctx, worker)?;
    let spec = ctx.worker(worker)?;
    let mut host = json!({
        "transport":"local",
        "root":resolved.environment_root,
        "tools":match &resolved.tools { ResolvedTools::Native => "native", ResolvedTools::Devenv { .. } => "devenv" },
    });
    if let ResolvedTools::Devenv { executable } = &resolved.tools {
        host["devenv_bin"] = json!(executable);
    }
    let mut self_worker = json!({
        "name":"self",
        "host":"self",
        "class":spec.get("class").cloned().unwrap_or(json!("general")),
        "cpus":spec.get("cpus").cloned().unwrap_or(json!(1)),
        "memory_gb":spec.get("memory_gb").cloned().unwrap_or(json!(1)),
        "disk_gb":spec.get("disk_gb").cloned().unwrap_or(json!(50)),
        "lifetime":"static",
    });
    let mut profile_definitions = serde_json::Map::new();
    if let Some(profile_name) = spec.get("profile").and_then(Value::as_str) {
        let profile = profiles::definition(ctx, profile_name)?;
        self_worker["profile"] = json!(profile.name);
        profile_definitions.insert(profile.name, profile.spec);
    }
    Ok(json!({
        "fleet":{
            "schema_version":ctx.fleet.get("schema_version").cloned().unwrap_or(json!(1)),
            "local_controller":true,
            "hosts":{"self":host},
            "workers":[self_worker],
            "projects":ctx.fleet.get("projects").cloned().unwrap_or_else(|| json!({})),
        },
        "profiles":profile_definitions,
    }))
}

fn source_request_identity(source: &Path) -> Result<Value> {
    let root = source
        .canonicalize()
        .with_context(|| format!("Source root {} does not exist", source.display()))?;
    Ok(json!({"path":root,"digest":source_digest(&root)?}))
}

fn source_digest(root: &Path) -> Result<String> {
    let mut digest = Sha256::new();
    for path in source_files(root)? {
        let relative = path
            .strip_prefix(root)?
            .to_string_lossy()
            .replace('\\', "/");
        let content = fs::read(&path)?;
        digest.update(relative.as_bytes());
        digest.update(b"\0");
        digest.update(format!("{:x}", Sha256::digest(&content)).as_bytes());
        digest.update(b"\0");
    }
    Ok(format!("sha256:{:x}", digest.finalize()))
}

fn source_files(root: &Path) -> Result<Vec<PathBuf>> {
    reject_symlink(root)?;
    let mut files = Vec::new();
    for entry in
        fs::read_dir(root).with_context(|| format!("read source root {}", root.display()))?
    {
        let path = entry?.path();
        if path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with("Cargo") && name.ends_with(".toml"))
        {
            validate_source_file(&path)?;
            files.push(path);
        }
    }
    let lock = root.join("Cargo.lock");
    validate_source_file(&lock)?;
    files.push(lock);
    let build_rs = root.join("build.rs");
    if build_rs.exists() || build_rs.is_symlink() {
        validate_source_file(&build_rs)?;
        files.push(build_rs);
    }
    let src = root.join("src");
    reject_symlink(&src)?;
    if !src.is_dir() {
        bail!("{} is not a directory", src.display());
    }
    collect_source_files(root, &src, &mut files)?;
    files.sort_by_key(|path| {
        path.strip_prefix(root)
            .map(Path::to_path_buf)
            .unwrap_or_else(|_| path.clone())
    });
    files.dedup();
    Ok(files)
}

fn collect_source_files(root: &Path, directory: &Path, files: &mut Vec<PathBuf>) -> Result<()> {
    for entry in fs::read_dir(directory)
        .with_context(|| format!("read source directory {}", directory.display()))?
    {
        let path = entry?.path();
        reject_symlink(&path)?;
        if path.is_dir() {
            collect_source_files(root, &path, files)?;
        } else if path.is_file() {
            validate_source_file(&path)?;
            files.push(path);
        }
    }
    let _ = root;
    Ok(())
}

fn validate_source_file(path: &Path) -> Result<()> {
    reject_symlink(path)?;
    if !path.is_file() {
        bail!("{} is not a regular file", path.display());
    }
    Ok(())
}

fn reject_symlink(path: &Path) -> Result<()> {
    if fs::symlink_metadata(path).is_ok_and(|metadata| metadata.file_type().is_symlink()) {
        bail!("{} is a symlink", path.display());
    }
    Ok(())
}

fn checkpoint_payload(
    worker: &str,
    key: &str,
    execution_id: &str,
    status: &str,
    environment_root: &Path,
    source_identity: Option<&Value>,
    fields: Value,
) -> Value {
    let mut payload = merge(
        json!({"schema":1,"worker":worker,"key":key,"execution_id":execution_id,"status":status,"environment_root":environment_root}),
        fields,
    );
    if let Some(source_identity) = source_identity {
        payload["source_identity"] = source_identity.clone();
    }
    payload
}

fn config_override(ctx: &Context, resolved: &hosts::ResolvedWorker) -> Result<Option<PathBuf>> {
    if resolved.transport == ResolvedTransport::Local
        && !same_path(&resolved.environment_root, &ctx.root)
    {
        return Ok(Some(
            resolved.environment_root.join(".state/cli-config.json"),
        ));
    }
    Ok(None)
}

#[derive(Clone, Debug)]
struct BuildCheckpoint {
    path: PathBuf,
    worker: String,
    execution_id: String,
    environment_root: PathBuf,
    transport: ResolvedTransport,
    source_identity: Option<Value>,
}

fn wait_for_own_checkpointed_build(
    ctx: &Context,
    worker: &str,
    key: &str,
    expected_source_identity: Option<&Value>,
) -> Result<Option<Value>> {
    let checkpoint = checkpoint_path(ctx, worker);
    let Some(record) = recover_checkpoint(ctx, &checkpoint, Some(worker))? else {
        return Ok(None);
    };
    let result = wait_for_checkpoint_record(ctx, key, &record)?;
    if expected_source_identity.is_some()
        && record.source_identity.as_ref() != expected_source_identity
    {
        if is_pending_result(&result) {
            return Ok(Some(result));
        }
        return Ok(None);
    }
    Ok(Some(result))
}

fn wait_for_environment_build_blocker(
    ctx: &Context,
    worker: &str,
    key: &str,
) -> Result<Option<Value>> {
    let resolved = hosts::resolve(ctx, worker)?;
    let Some(record) = recover_environment_checkpoint(ctx, worker, &resolved.environment_root)?
    else {
        return Ok(None);
    };
    let result = wait_for_checkpoint_record(ctx, key, &record)?;
    if !is_pending_result(&result) {
        return Ok(None);
    }
    Ok(Some(merge(
        json!({"blocked_by_worker":record.worker,"environment_root":record.environment_root}),
        result,
    )))
}

fn wait_for_checkpoint_record(ctx: &Context, key: &str, record: &BuildCheckpoint) -> Result<Value> {
    wait_for_build(
        ctx,
        &record.worker,
        key,
        &record.environment_root,
        &record.path,
        &record.execution_id,
        record.source_identity.as_ref(),
    )
}

fn checkpoint_path(ctx: &Context, worker: &str) -> PathBuf {
    ctx.state.join(format!("install-worker-{worker}.json"))
}

fn recover_environment_checkpoint(
    ctx: &Context,
    worker: &str,
    environment_root: &Path,
) -> Result<Option<BuildCheckpoint>> {
    if !ctx.state.exists() {
        return Ok(None);
    }
    let mut paths = Vec::new();
    for entry in fs::read_dir(&ctx.state)? {
        let path = entry?.path();
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if name.starts_with("install-worker-") && name.ends_with(".json") {
            paths.push(path);
        }
    }
    paths.sort();
    for path in paths {
        let Some(record) = recover_checkpoint(ctx, &path, None)? else {
            continue;
        };
        if record.worker != worker
            && same_transport_host(&record.transport, &hosts::resolve(ctx, worker)?.transport)
            && same_path(&record.environment_root, environment_root)
        {
            return Ok(Some(record));
        }
    }
    Ok(None)
}

fn recover_checkpoint(
    ctx: &Context,
    path: &Path,
    fallback_worker: Option<&str>,
) -> Result<Option<BuildCheckpoint>> {
    if !path.exists() {
        return Ok(None);
    }
    let value = read_json(path)?;
    let status = value
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    if !matches!(status, "running" | "pending" | "unknown") {
        return Ok(None);
    }
    let Some(execution_id) = value.get("execution_id").and_then(Value::as_str) else {
        return Ok(None);
    };
    let worker = value
        .get("worker")
        .and_then(Value::as_str)
        .or(fallback_worker)
        .context("Installer checkpoint has no worker")?
        .to_string();
    let resolved = hosts::resolve(ctx, &worker)?;
    let environment_root = match value.get("environment_root").and_then(Value::as_str) {
        Some(path) => PathBuf::from(path),
        None => resolved.environment_root.clone(),
    };
    Ok(Some(BuildCheckpoint {
        path: path.to_path_buf(),
        worker,
        execution_id: execution_id.to_string(),
        environment_root,
        transport: resolved.transport,
        source_identity: value.get("source_identity").cloned(),
    }))
}

fn same_transport_host(left: &ResolvedTransport, right: &ResolvedTransport) -> bool {
    match (left, right) {
        (ResolvedTransport::Local, ResolvedTransport::Local) => true,
        (ResolvedTransport::Ssh { target: left }, ResolvedTransport::Ssh { target: right }) => {
            left == right
        }
        _ => false,
    }
}

fn wait_is_pending_or_unknown(value: &Value) -> bool {
    match value.get("outcome").and_then(Value::as_str) {
        Some("passed" | "failed" | "error" | "skipped") => false,
        Some("pending" | "unknown") | None => true,
        Some(_) => true,
    }
}

fn is_pending_result(value: &Value) -> bool {
    matches!(
        value.get("status").and_then(Value::as_str),
        Some("pending" | "running" | "unknown")
    ) || value
        .get("workers")
        .and_then(Value::as_array)
        .is_some_and(|workers| workers.iter().any(is_pending_result))
}

fn fresh_read_key(prefix: &str) -> String {
    format!("{prefix}:read:{}", Uuid::new_v4())
}

fn selected_worker_names(ctx: &Context, selector: Option<&str>) -> Result<Vec<String>> {
    if let Some(selector) = selector {
        return Ok(vec![ctx.worker_name(selector)?]);
    }
    let workers = ctx.fleet["workers"]
        .as_array()
        .context("fleet.json requires workers")?;
    workers
        .iter()
        .map(|worker| {
            worker["name"]
                .as_str()
                .map(str::to_string)
                .context("Worker requires a name")
        })
        .collect()
}

fn same_path(left: &Path, right: &Path) -> bool {
    match (left.canonicalize(), right.canonicalize()) {
        (Ok(left), Ok(right)) => left == right,
        _ => left == right,
    }
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
    merge(json!({"status":status,"ok":ok}), fields)
}

fn failed(status: &str, error: &str) -> Value {
    json!({"status":status,"ok":false,"error":error})
}
