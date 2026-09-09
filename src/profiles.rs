use std::{fs, io::Write, path::Path};

use anyhow::{bail, Context as _, Result};
use base64::{engine::general_purpose::STANDARD, Engine as _};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::hosts::{self, RemoteCommandSpec, ResolvedTools};
use crate::process::{read_json, shell_join, shell_quote, write_json};
use crate::{worker, Context};

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ProfileSpec {
    schema_version: u32,
    name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    github_login: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    git_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    git_email: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    anthropic_profile: Option<String>,
}

#[derive(Clone, Debug)]
pub struct ResolvedProfile {
    pub name: String,
    pub digest: String,
    pub spec: Value,
}

pub fn validate_name(name: &str) -> Result<()> {
    if name.is_empty()
        || name.len() > 48
        || !name.as_bytes()[0].is_ascii_lowercase()
        || !name
            .bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-')
    {
        bail!("Profile names must start with a lowercase letter and contain only lowercase letters, digits, and hyphens (48 characters maximum)");
    }
    Ok(())
}

fn parse_spec(value: Value, name: &str) -> Result<ResolvedProfile> {
    validate_name(name)?;
    let spec: ProfileSpec = serde_json::from_value(value).context(
        "Profile definitions accept identity metadata only; credential values are not allowed",
    )?;
    if spec.schema_version != 1 || spec.name != name {
        bail!("Profile schema or name does not match its definition");
    }
    for value in [
        &spec.github_login,
        &spec.git_name,
        &spec.git_email,
        &spec.anthropic_profile,
    ]
    .into_iter()
    .flatten()
    {
        if value.is_empty()
            || value.len() > 254
            || value.trim() != value
            || value.chars().any(char::is_control)
        {
            bail!("Profile identity fields must be nonempty text without control characters or surrounding whitespace");
        }
    }
    if let Some(login) = &spec.github_login {
        if login.len() > 39
            || !login.as_bytes()[0].is_ascii_alphanumeric()
            || !login
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || c == b'-')
        {
            bail!("Invalid GitHub login in profile");
        }
    }
    let spec = serde_json::to_value(spec)?;
    let digest = format!("{:x}", Sha256::digest(serde_json::to_vec(&spec)?));
    Ok(ResolvedProfile {
        name: name.into(),
        digest,
        spec,
    })
}

fn reject_symlink(path: &Path) -> Result<()> {
    if fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_symlink()) {
        bail!("Profile paths must not be symbolic links");
    }
    Ok(())
}

pub fn definition(ctx: &Context, name: &str) -> Result<ResolvedProfile> {
    validate_name(name)?;
    let directory = ctx.root.join("profiles");
    let path = directory.join(format!("{name}.json"));
    reject_symlink(&directory)?;
    reject_symlink(&path)?;
    parse_spec(
        read_json(&path).with_context(|| {
            format!("Profile {name} is not defined; use workenv profile create")
        })?,
        name,
    )
}

pub fn resolve(ctx: &Context, worker: &str) -> Result<Option<ResolvedProfile>> {
    let spec = ctx.worker(worker)?;
    match spec.get("profile") {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(name)) => definition(ctx, name).map(Some),
        Some(_) => bail!("Worker profile must be a profile name"),
    }
}

pub fn binding(ctx: &Context, worker: &str) -> Result<Value> {
    Ok(resolve(ctx, worker)?
        .map(|p| json!({"name":p.name,"digest":p.digest}))
        .unwrap_or(Value::Null))
}

pub fn validate_task_binding(ctx: &Context, record: &Value) -> Result<()> {
    let worker = record["worker"].as_str().context("Task has no worker")?;
    let expected = record.get("worker_profile").unwrap_or(&Value::Null);
    if expected != &binding(ctx, worker)? {
        bail!("Task worker profile changed; restore its recorded profile before continuing");
    }
    Ok(())
}

pub fn create(ctx: &Context, name: &str, metadata: Value) -> Result<Value> {
    let mut spec = metadata
        .as_object()
        .cloned()
        .context("Profile metadata must be an object")?;
    spec.retain(|_, value| !value.is_null());
    spec.insert("schema_version".into(), json!(1));
    spec.insert("name".into(), json!(name));
    let profile = parse_spec(Value::Object(spec), name)?;
    let directory = ctx.root.join("profiles");
    reject_symlink(&directory)?;
    fs::create_dir_all(&directory)?;
    let path = directory.join(format!("{name}.json"));
    reject_symlink(&path)?;
    if path.exists() {
        if definition(ctx, name)?.digest != profile.digest {
            bail!("Profile already has a different definition; create a new profile name to preserve existing logins and task bindings");
        }
        return Ok(
            json!({"ok":true,"status":"profile_exists","profile":profile.spec,"digest":profile.digest,"path":path}),
        );
    }
    let mut options = fs::OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&path)?;
    file.write_all(&serde_json::to_vec_pretty(&profile.spec)?)?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    Ok(
        json!({"ok":true,"status":"profile_created","profile":profile.spec,"digest":profile.digest,"path":path,"next":"Assign this profile to an idle worker with workenv profile assign WORKER NAME."}),
    )
}

pub fn list(ctx: &Context) -> Result<Value> {
    let directory = ctx.root.join("profiles");
    reject_symlink(&directory)?;
    let mut names = Vec::new();
    if directory.exists() {
        for entry in fs::read_dir(&directory)? {
            let path = entry?.path();
            if path
                .extension()
                .is_some_and(|extension| extension == "json")
            {
                names.push(
                    path.file_stem()
                        .and_then(|s| s.to_str())
                        .context("Invalid profile filename")?
                        .to_owned(),
                );
            }
        }
    }
    names.sort();
    let workers = ctx.fleet["workers"]
        .as_array()
        .context("Fleet has no workers")?;
    let mut profiles = Vec::new();
    for name in names {
        let p = definition(ctx, &name)?;
        let assigned: Vec<_> = workers
            .iter()
            .filter(|worker| worker["profile"].as_str() == Some(&name))
            .map(|worker| worker["name"].clone())
            .collect();
        profiles
            .push(json!({"name":name,"definition":p.spec,"digest":p.digest,"workers":assigned}));
    }
    for worker in workers {
        if let Some(name) = worker["name"].as_str() {
            resolve(ctx, name)?;
        }
    }
    Ok(
        json!({"ok":true,"status":"profiles","profiles":profiles,"unassigned_workers":workers.iter().filter(|worker| worker.get("profile").is_none_or(Value::is_null)).map(|worker| worker["name"].clone()).collect::<Vec<_>>()}),
    )
}

fn helper_args(ctx: &Context, worker: &str) -> Result<Vec<String>> {
    let resolved = hosts::resolve(ctx, worker)?;
    let environment_root = resolved.environment_root.to_string_lossy().into_owned();
    let worker_root = resolved.root.to_string_lossy().into_owned();
    Ok(vec![
        "python3".into(),
        format!("{environment_root}/remote/profile.py"),
        "--root".into(),
        worker_root,
    ])
}

fn shared_environment(ctx: &Context, worker: &str, argv: Vec<String>) -> Result<Vec<String>> {
    let resolved = hosts::resolve(ctx, worker)?;
    match resolved.tools {
        ResolvedTools::Native => Ok(argv),
        ResolvedTools::Devenv { executable } => Ok(vec![
            "bash".into(),
            "-lc".into(),
            format!(
                "cd {} && {} shell -- {}",
                shell_quote(&resolved.environment_root.to_string_lossy()),
                shell_quote(&executable),
                shell_join(&argv)
            ),
        ]),
    }
}

pub fn wrap(
    ctx: &Context,
    worker: &str,
    argv: Vec<String>,
    check_github: bool,
) -> Result<Vec<String>> {
    let resolved = hosts::resolve(ctx, worker)?;
    let Some(profile) = resolve(ctx, worker)? else {
        return Ok(argv);
    };
    let mut args = helper_args(ctx, worker)?;
    args.extend([
        "exec".into(),
        "--name".into(),
        profile.name,
        "--digest".into(),
        profile.digest,
    ]);
    if check_github {
        args.push("--check-github".into());
    }
    args.push("--".into());
    args.extend(argv);
    // APoC's daemon does not inherit the invoking SSH shell's environment.
    // Load tools inside the durable child, then restore the task's cwd before
    // direnv selects its identity. Exact command arguments remain separate.
    match resolved.tools {
        ResolvedTools::Native => Ok(args),
        ResolvedTools::Devenv { executable } => {
            let script = r#"workenv_command_cwd=$PWD
workenv_environment_root=$1
workenv_devenv=$2
shift 2
cd -- "$workenv_environment_root" || exit
exec "$workenv_devenv" shell -- bash -c 'cd -- "$1" || exit; shift; exec "$@"' workenv-profile "$workenv_command_cwd" "$@""#;
            let mut wrapped = vec![
                "bash".into(),
                "-c".into(),
                script.into(),
                "workenv-profile".into(),
                resolved.environment_root.to_string_lossy().into_owned(),
                executable,
            ];
            wrapped.extend(args);
            Ok(wrapped)
        }
    }
}

fn raw_remote(
    ctx: &Context,
    worker: &str,
    argv: Vec<String>,
    stdin: Option<Vec<u8>>,
    key: &str,
    purpose: &str,
) -> Result<Value> {
    let output = ctx.remote(
        worker,
        RemoteCommandSpec {
            argv,
            stdin,
            cwd: Some(ctx.worker_root(worker)?),
            tools_env: false,
            timeout_ms: 60_000,
            key: key.into(),
            purpose: purpose.into(),
        },
    )?;
    if output.exit_code != Some(0) {
        // Login helpers may mention credential paths or provider diagnostics.
        // Keep their raw output out of controller errors and durable receipts.
        bail!(
            "Profile operation failed on {worker}; inspect execution {}",
            output.execution_id
        );
    }
    output.json()
}

pub fn prepare(ctx: &Context, worker: &str, key: &str) -> Result<Value> {
    let Some(profile) = resolve(ctx, worker)? else {
        return Ok(
            json!({"ok":true,"status":"profile_unassigned","worker":ctx.worker_name(worker)?}),
        );
    };
    let source = fs::read(ctx.root.join("remote/profile.py"))
        .context("Worker profile runtime is missing from this checkout")?;
    let install = r#"import pathlib,sys,os,uuid,json
p=pathlib.Path(sys.argv[1]); data=sys.stdin.buffer.read()
for parent in [p,*p.parents]:
 if parent.is_symlink(): raise SystemExit('managed runtime path is a symlink')
p.parent.mkdir(parents=True,exist_ok=True)
if not p.exists() or p.read_bytes()!=data:
 t=p.with_name(p.name+'.tmp.'+uuid.uuid4().hex)
 with t.open('xb') as f:
  os.chmod(t,0o700); f.write(data); f.flush(); os.fsync(f.fileno())
 os.replace(t,p)
print(json.dumps({'ok':True,'status':'profile_runtime_installed'}))"#;
    let installed = raw_remote(
        ctx,
        worker,
        vec![
            "python3".into(),
            "-c".into(),
            install.into(),
            format!(
                "{}/remote/profile.py",
                ctx.worker_environment_root(worker)?.to_string_lossy()
            ),
        ],
        Some(source),
        &format!("{key}:profile-runtime"),
        "Install the worker profile runtime without transferring credentials.",
    )?;
    if installed["ok"] != true {
        return Ok(installed);
    }
    let mut args = helper_args(ctx, worker)?;
    args.extend([
        "prepare".into(),
        "--name".into(),
        profile.name,
        "--spec-base64".into(),
        STANDARD.encode(serde_json::to_vec(&profile.spec)?),
        "--digest".into(),
        profile.digest,
    ]);
    raw_remote(
        ctx,
        worker,
        shared_environment(ctx, worker, args)?,
        None,
        &format!("{key}:profile-prepare"),
        "Prepare isolated worker profile directories without transferring credentials.",
    )
}

pub fn status(ctx: &Context, worker: &str) -> Result<Value> {
    let worker = ctx.worker_name(worker)?;
    let Some(profile) = resolve(ctx, &worker)? else {
        return Ok(json!({"ok":true,"status":"profile_unassigned","worker":worker}));
    };
    let mut args = helper_args(ctx, &worker)?;
    args.extend([
        "status".into(),
        "--name".into(),
        profile.name.clone(),
        "--digest".into(),
        profile.digest.clone(),
    ]);
    let result = raw_remote(
        ctx,
        &worker,
        shared_environment(ctx, &worker, args)?,
        None,
        &format!("profile-status-{}", uuid::Uuid::new_v4()),
        "Check the selected worker profile and GitHub identity without exposing credentials.",
    );
    match result {
        Ok(mut value) => {
            value["worker"] = json!(worker);
            Ok(value)
        }
        Err(error) => Ok(
            json!({"ok":false,"status":"profile_unknown","worker":worker,"profile":profile.name,"digest":profile.digest,"error":error.to_string()}),
        ),
    }
}

pub fn assign(ctx: &Context, selector: &str, name: &str, key: &str) -> Result<Value> {
    let worker = ctx.worker_name(selector)?;
    let profile = definition(ctx, name)?;
    fs::create_dir_all(&ctx.state)?;
    let lock = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(ctx.state.join("fleet.lock"))?;
    lock.try_lock_exclusive()
        .context("Another worker profile assignment is in progress")?;
    if read_json(&ctx.root.join("fleet.json"))? != ctx.fleet {
        bail!("Fleet configuration changed; retry with a new request key");
    }
    if resolve(ctx, &worker)?.is_some_and(|current| current.digest == profile.digest) {
        let observed = status(ctx, &worker)?;
        let runtime = worker::worker_herdr_status(ctx, &worker, key)?;
        if observed["prepared"] == true && runtime["herdr_ready"] == true {
            return Ok(
                json!({"ok":true,"status":"profile_already_assigned","worker":worker,"profile":name,"observed":observed,"herdr":runtime}),
            );
        }
    }
    let guard = worker::begin_profile_change(ctx, &worker, key)?;
    if guard["ok"] != true {
        return Ok(guard);
    }
    let result = (|| {
        let synced = worker::sync_worker_sources(ctx, &worker, &format!("{key}:profile-sources"))?;
        if synced["ok"] != true {
            return Ok(synced);
        }
        let bootstrapped =
            worker::bootstrap_worker(ctx, &worker, &format!("{key}:profile-bootstrap"))?;
        if bootstrapped["ok"] != true {
            return Ok(bootstrapped);
        }
        let mut next = ctx.clone();
        let entry = next.fleet["workers"]
            .as_array_mut()
            .and_then(|rows| rows.iter_mut().find(|row| row["name"] == worker))
            .context("Worker disappeared from fleet")?;
        entry["profile"] = json!(name);
        let prepared = prepare(&next, &worker, key)?;
        if prepared["ok"] != true {
            return Ok(prepared);
        }
        if read_json(&ctx.root.join("fleet.json"))? != ctx.fleet {
            bail!("Fleet configuration changed during preparation; worker runtime is stopped and the profile was not assigned");
        }
        write_json(&ctx.root.join("fleet.json"), &next.fleet)?;
        let started = worker::start_worker_herdr(&next, &worker, &format!("{key}:profile-start"))?;
        Ok(
            json!({"ok":started["ok"]==true,"status":if started["ok"]==true {"profile_assigned"} else {"profile_assigned_start_failed"},"worker":worker,"profile":name,"digest":profile.digest,"prepared":prepared,"herdr":started,"next":format!("Use workenv profile login {worker} github to authenticate this profile.")}),
        )
    })();
    let released = worker::end_profile_change(ctx, &worker, key);
    match (result, released) {
        (Err(error), _) => Err(error),
        (Ok(_), Err(error)) => Err(error),
        (Ok(value), Ok(())) => Ok(value),
    }
}

pub fn login(ctx: &Context, selector: &str, service: &str, key: &str) -> Result<Value> {
    let worker = ctx.worker_name(selector)?;
    let profile =
        resolve(ctx, &worker)?.context("Worker has no profile; create and assign one first")?;
    let argv: Vec<String> = match service {
        "github" => vec![
            "gh",
            "auth",
            "login",
            "--hostname",
            "github.com",
            "--git-protocol",
            "https",
            "--web",
        ],
        "codex" => vec!["codex", "login", "--device-auth"],
        "claude" => vec!["claude", "auth", "login"],
        _ => bail!("Login service must be github, codex, or claude"),
    }
    .into_iter()
    .map(str::to_owned)
    .collect();
    let observed = status(ctx, &worker)?;
    if observed["prepared"] != true {
        return Ok(
            json!({"ok":false,"status":"profile_not_prepared","worker":worker,"observed":observed}),
        );
    }
    let session = ctx.worker_session(&worker)?;
    let created = ctx
        .remote(
            &worker,
            RemoteCommandSpec {
                argv: vec![
                    "herdr".into(),
                    "--session".into(),
                    session.clone(),
                    "workspace".into(),
                    "create".into(),
                    "--cwd".into(),
                    ctx.worker_root(&worker)?.to_string_lossy().into_owned(),
                    "--label".into(),
                    format!("{} {service} login", profile.name),
                    "--focus".into(),
                ],
                stdin: None,
                cwd: Some(ctx.worker_root(&worker)?),
                tools_env: true,
                key: format!("{key}:login-pane"),
                purpose: "Open a profile login workspace in the worker's Herdr session.".into(),
                timeout_ms: 60_000,
            },
        )?
        .json()?;
    let result = created.get("result").unwrap_or(&created);
    let pane = result
        .pointer("/root_pane/pane_id")
        .and_then(Value::as_str)
        .context("Herdr did not return a login pane ID")?;
    let command = shell_join(&wrap(ctx, &worker, argv, false)?);
    let sent = ctx.remote(
        &worker,
        RemoteCommandSpec {
            argv: vec![
                "herdr".into(),
                "--session".into(),
                session.clone(),
                "pane".into(),
                "run".into(),
                pane.into(),
                command,
            ],
            stdin: None,
            cwd: Some(ctx.worker_root(&worker)?),
            tools_env: true,
            key: format!("{key}:login-command"),
            purpose: "Start the requested interactive login inside its worker profile.".into(),
            timeout_ms: 60_000,
        },
    )?;
    sent.success()?;
    Ok(
        json!({"ok":true,"status":"login_started","worker":worker,"profile":profile.name,"service":service,"pane_id":pane,"session":session,"next":"Complete the login in the worker's Herdr workspace, then run workenv profile status WORKER."}),
    )
}
