use crate::{
    process::{read_json, write_json},
    profiles, tasks, worker, Context,
};
use anyhow::{bail, Context as _, Result};
use fs2::FileExt;
use incurs::{
    cli::Cli,
    command::{CommandDef, McpAnnotations, McpCommandOptions, TypedContext, TypedResult},
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, fs, io::IsTerminal};
use uuid::Uuid;

#[derive(Deserialize, incurs::Options)]
pub struct Globals {
    /// Folder containing fleet.json. Defaults to WORKENV_ROOT or saved configuration.
    pub root: Option<String>,
}
#[derive(Deserialize, incurs::Options)]
struct MutationOptions {
    /// Stable request key. Required for MCP and noninteractive calls; terminal calls generate one.
    idempotency_key: Option<String>,
}
#[derive(Deserialize, incurs::Options)]
struct StatusOptions {
    /// Include full probe evidence.
    #[serde(default)]
    details: bool,
}
#[derive(Deserialize, incurs::Args)]
struct WorkerArgs {
    /// Worker name or number, such as 1 or workenv-01. Omit for the whole fleet.
    worker: Option<String>,
}
#[derive(Deserialize, incurs::Args)]
struct TargetArgs {
    /// Worker name, worker number, or active task ID.
    target: String,
}
#[derive(Deserialize, incurs::Args)]
struct TaskArgs {
    /// Exact task ID returned by claim.
    task: String,
}
#[derive(Deserialize, incurs::Args)]
struct ClaimArgs {
    /// Project name from fleet.json.
    project: String,
    /// Your unique task ID.
    task: String,
}
#[derive(Deserialize, incurs::Options)]
struct ClaimOptions {
    /// Stable request key. Required for MCP and noninteractive calls; terminal calls generate one.
    idempotency_key: Option<String>,
    /// Exact full Git commit SHA. Omit to resolve the repository's current HEAD.
    revision: Option<String>,
    /// Preferred worker name or number. Otherwise select an available worker of the project's class.
    worker: Option<String>,
    /// Local Git bundle containing the exact revision, for unpublished source.
    source_bundle: Option<String>,
    /// Require a worker assigned to this named identity profile.
    profile: Option<String>,
}
#[derive(Deserialize, incurs::Args)]
struct ProfileNameArgs {
    /// Profile name: lowercase letters, digits, and hyphens.
    name: String,
}
#[derive(Deserialize, incurs::Options)]
struct ProfileCreateOptions {
    /// Stable request key. Required for MCP and noninteractive calls.
    idempotency_key: Option<String>,
    /// Expected GitHub username. Task commands refuse a different login.
    github_login: Option<String>,
    /// Git commit author name for this profile.
    git_name: Option<String>,
    /// Git commit author email for this profile.
    git_email: Option<String>,
    /// Named profile for the Anthropic CLI, when used.
    anthropic_profile: Option<String>,
}
#[derive(Deserialize, incurs::Args)]
struct ProfileAssignArgs {
    /// Worker name or number. Existing work must be stopped first.
    worker: String,
    /// Existing profile name.
    name: String,
}
#[derive(Deserialize, incurs::Args)]
struct ProfileWorkerArgs {
    /// Worker name or number.
    worker: String,
}
#[derive(Deserialize, incurs::Args)]
struct ProfileLoginArgs {
    /// Worker name or number with an assigned profile.
    worker: String,
    /// Login service: github (default), codex, or claude.
    service: Option<String>,
}
#[derive(Deserialize, incurs::Args)]
struct RunArgs {
    /// Exact task ID.
    task: String,
    /// Program and exact arguments. Put command flags after --.
    argv: Vec<String>,
}
#[derive(Deserialize, incurs::Options)]
struct InOptions {
    /// Return connection details without opening the interactive Herdr client.
    #[serde(default)]
    print: bool,
}

#[derive(Serialize, JsonSchema)]
pub struct Report {
    pub ok: bool,
    pub status: String,
    #[serde(flatten)]
    pub details: BTreeMap<String, Value>,
}

fn report(result: Result<Value>) -> TypedResult<Report> {
    match result {
        Ok(Value::Object(mut object)) => {
            let Some(ok) = object.remove("ok").and_then(|v| v.as_bool()) else {
                return TypedResult::error(
                    "invalid_result",
                    "Workenv operation omitted its success flag",
                );
            };
            let Some(status) = object
                .remove("status")
                .and_then(|v| v.as_str().map(str::to_owned))
            else {
                return TypedResult::error(
                    "invalid_result",
                    "Workenv operation omitted its status",
                );
            };
            TypedResult::ok_with_exit_code(
                Report {
                    ok,
                    status,
                    details: object.into_iter().collect(),
                },
                if ok { 0 } else { 1 },
            )
        }
        Ok(_) => TypedResult::error(
            "invalid_result",
            "Workenv operation did not return an object",
        ),
        Err(error) => TypedResult::error("workenv_error", format!("{error:#}")),
    }
}
fn context(globals: &Value) -> Result<Context> {
    Context::load(globals["root"].as_str())
}

fn mutation_globals(globals: &Value, key: Option<&str>) -> Value {
    let mut merged = globals.clone();
    if !merged.is_object() {
        merged = json!({});
    }
    merged["idempotency_key"] = key.map(Value::from).unwrap_or(Value::Null);
    merged
}

fn mcp_options(read_only: bool, destructive: bool) -> McpCommandOptions {
    McpCommandOptions {
        annotations: Some(McpAnnotations {
            read_only_hint: Some(read_only),
            destructive_hint: Some(destructive),
            idempotent_hint: Some(true),
            open_world_hint: Some(true),
            ..Default::default()
        }),
        destructive,
        ..Default::default()
    }
}

fn summarize_status(mut value: Value) -> Value {
    if let Some(workers) = value.get_mut("workers").and_then(Value::as_array_mut) {
        for row in workers {
            let blockers = row["blockers"]
                .as_array()
                .map(|items| {
                    items
                        .iter()
                        .map(|item| json!({"kind":item["kind"],"status":item["status"]}))
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            let tasks = row
                .pointer("/ownership/open_tasks")
                .and_then(Value::as_array)
                .map(|items| {
                    items
                        .iter()
                        .filter_map(|task| task["task_id"].as_str())
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            *row = json!({
                "worker":row["worker"],"status":row["status"],"capacity":row["capacity"],
                "workspace":row.pointer("/agent_state/status"),"tasks":tasks,
                "tools":row.pointer("/tools/tools_ready"),"herdr":row.pointer("/herdr/herdr_ready"),
                "nib":row.pointer("/tools/nib_auth/authenticated"),
                "codex":row.pointer("/auth/auth/codex/chatgpt_subscription"),
                "claude":row.pointer("/auth/auth/claude/subscription_auth"),
                "tailscale":row.pointer("/tailscale/ok"),"blockers":blockers,
            });
        }
    }
    value
}

fn mutation<F>(
    ctx: &Context,
    command: &str,
    input: Value,
    globals: &Value,
    remote: bool,
    operation: F,
) -> Result<Value>
where
    F: FnOnce(&str) -> Result<Value>,
{
    let key = match globals["idempotency_key"].as_str() {
        Some(key) if !key.trim().is_empty() => key.to_owned(),
        _ if remote => bail!("idempotency_key is required for MCP and noninteractive mutations; reuse it to retrieve the same request"),
        _ => format!("{}-{}", command, Uuid::new_v4()),
    };
    if !remote && std::io::stderr().is_terminal() {
        eprintln!("Request: {key}");
    }
    let fingerprint = json!({"command":command,"input":input});
    let dir = ctx.root.join(".state/operations");
    fs::create_dir_all(&dir)?;
    let digest = format!("{:x}", Sha256::digest(key.as_bytes()));
    let lock = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(dir.join(format!("{digest}.lock")))?;
    lock.try_lock_exclusive()
        .context("This request is already running; inspect workenv status before retrying")?;
    let path = dir.join(format!("{digest}.json"));
    if path.exists() {
        let previous = read_json(&path)?;
        if previous["request"] != fingerprint {
            bail!("Idempotency key is already bound to a different request");
        }
        if previous.get("result").is_some() {
            let mut result = previous["result"].clone();
            result["replayed"] = true.into();
            return Ok(result);
        }
        return Ok(json!({"ok":false,"status":"unknown","idempotency_key":key,
            "error":"The previous controller stopped before saving its result. Inspect worker and task state before issuing a new request; this request will not be repeated."}));
    }
    write_json(
        &path,
        &json!({"key":key,"request":fingerprint,"phase":"running"}),
    )?;
    let mut value = match operation(&key) {
        Ok(value) => value,
        Err(error) => json!({"ok":false,"status":"needs_inspection","error":format!("{error:#}"),
            "next":"Inspect worker and task state. Reusing this key returns the saved result without repeating the operation."}),
    };
    value
        .as_object_mut()
        .context("Mutation returned no object")?
        .insert("idempotency_key".into(), key.clone().into());
    let pending = value["ok"] != true
        || matches!(
            value["status"].as_str(),
            Some("pending" | "unknown" | "running")
        );
    write_json(
        &path,
        &json!({"key":key,"request":fingerprint,"phase":if pending {"running"} else {"completed"},"result":value}),
    )?;
    Ok(value)
}

fn target_worker(ctx: &Context, target: &str) -> Result<(String, Option<Value>)> {
    if let Ok(worker) = ctx.worker_name(target) {
        return Ok((worker, None));
    }
    let dir = ctx.state.join("tasks");
    if dir.exists() {
        for entry in fs::read_dir(dir)? {
            let path = entry?.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            let task = read_json(&path)?;
            if task["task_id"] == target {
                if matches!(task["status"].as_str(), Some("released" | "closed")) {
                    bail!("Task {target} has already been released");
                }
                return Ok((
                    ctx.worker_name(task["worker"].as_str().context("Task has no worker")?)?,
                    Some(task),
                ));
            }
        }
    }
    bail!("Unknown worker or active task {target:?}")
}

fn status_command() -> CommandDef {
    CommandDef::typed::<WorkerArgs, StatusOptions, (), Report, _, _>(
        "status",
        |input: TypedContext<WorkerArgs, StatusOptions, ()>| async move {
            report(
                context(&input.globals)
                    .and_then(|ctx| match input.args.worker.as_deref() {
                        Some(target) if ctx.worker_name(target).is_err() => {
                            tasks::status(&ctx, Some(target))
                        }
                        selector => worker::status(&ctx, selector),
                    })
                    .map(|value| {
                        if input.options.details {
                            value
                        } else {
                            summarize_status(value)
                        }
                    }),
            )
        },
    )
    .description("See live worker, tool, authentication, and task readiness.")
    .mcp(mcp_options(true, false))
    .command_aliases(vec!["list".into(), "ls".into()])
    .done()
}

fn profile_commands() -> Cli {
    let create = CommandDef::typed::<ProfileNameArgs, ProfileCreateOptions, (), Report, _, _>(
        "create",
        |input: TypedContext<ProfileNameArgs, ProfileCreateOptions, ()>| async move {
            report(context(&input.globals).and_then(|ctx| {
                let metadata = json!({
                    "github_login":input.options.github_login,
                    "git_name":input.options.git_name,
                    "git_email":input.options.git_email,
                    "anthropic_profile":input.options.anthropic_profile,
                });
                mutation(
                    &ctx,
                    "profile create",
                    json!({"name":input.args.name,"metadata":metadata}),
                    &mutation_globals(&input.globals, input.options.idempotency_key.as_deref()),
                    input.agent || input.request.is_some(),
                    |_| profiles::create(&ctx, &input.args.name, metadata),
                )
            }))
        },
    )
    .description(
        "Create a reusable identity profile without copying credentials or changing workers.",
    )
    .mcp(mcp_options(false, false))
    .done();
    let list = CommandDef::typed::<(), (), (), Report, _, _>(
        "list",
        |input: TypedContext<(), (), ()>| async move {
            report(context(&input.globals).and_then(|ctx| profiles::list(&ctx)))
        },
    )
    .description(
        "List profile definitions and their worker assignments without contacting workers.",
    )
    .mcp(mcp_options(true, false))
    .done();
    let status = CommandDef::typed::<ProfileWorkerArgs, (), (), Report, _, _>(
        "status",|input:TypedContext<ProfileWorkerArgs,(),()>| async move {
            report(context(&input.globals).and_then(|ctx| profiles::status(&ctx,&input.args.worker)))
        },
    ).description("Verify a worker's profile directories and expected GitHub login without exposing credentials.")
        .mcp(mcp_options(true,false)).done();
    let assign = CommandDef::typed::<ProfileAssignArgs, MutationOptions, (), Report, _, _>(
        "assign",|input:TypedContext<ProfileAssignArgs,MutationOptions,()>| async move {
            report(context(&input.globals).and_then(|ctx| {
                mutation(&ctx,"profile assign",json!({"worker":input.args.worker,"name":input.args.name}),
                    &mutation_globals(&input.globals,input.options.idempotency_key.as_deref()),
                    input.agent || input.request.is_some(),
                    |key| profiles::assign(&ctx,&input.args.worker,&input.args.name,key))
            }))
        },
    ).description("Assign a profile to an idle worker and restart its empty Herdr runtime; active work blocks the change.")
        .mcp(mcp_options(false,false)).done();
    let login = CommandDef::typed::<ProfileLoginArgs, MutationOptions, (), Report, _, _>(
        "login",
        |input: TypedContext<ProfileLoginArgs, MutationOptions, ()>| async move {
            report(context(&input.globals).and_then(|ctx| {
                let service = input.args.service.as_deref().unwrap_or("github");
                mutation(
                    &ctx,
                    "profile login",
                    json!({"worker":input.args.worker,"service":service}),
                    &mutation_globals(&input.globals, input.options.idempotency_key.as_deref()),
                    input.agent || input.request.is_some(),
                    |key| profiles::login(&ctx, &input.args.worker, service, key),
                )
            }))
        },
    )
    .description(
        "Open GitHub, Codex, or Claude login in a Herdr workspace scoped to the worker's profile.",
    )
    .mcp(mcp_options(false, false))
    .done();
    Cli::create("profile")
        .description("Named worker identities with directory-scoped credentials.")
        .command("create", create)
        .command("list", list)
        .command("assign", assign)
        .command("status", status)
        .command("login", login)
}

pub fn build() -> Cli {
    let up = CommandDef::typed::<WorkerArgs, MutationOptions, (), Report, _, _>(
        "up",
        |input: TypedContext<WorkerArgs, MutationOptions, ()>| async move {
            report(context(&input.globals).and_then(|ctx| {
                mutation(
                    &ctx,
                    "up",
                    json!({"worker":input.args.worker}),
                    &mutation_globals(&input.globals, input.options.idempotency_key.as_deref()),
                    input.agent || input.request.is_some(),
                    |key| worker::up(&ctx, input.args.worker.as_deref(), key),
                )
            }))
        },
    )
    .description("Create or prepare workers, start Herdr, and add them to your sidebar.")
    .mcp(mcp_options(false, false))
    .done();
    let enter = CommandDef::typed::<TargetArgs, InOptions, (), Report, _, _>(
        "in",
        |input: TypedContext<TargetArgs, InOptions, ()>| async move {
            report((|| {
                let ctx = context(&input.globals)?;
                let (name, task) = target_worker(&ctx, &input.args.target)?;
                let mut value = worker::connection(&ctx, &name)?;
                if let Some(task) = task {
                    value["task"] = task;
                }
                if !input.options.print
                    && !input.agent
                    && input.request.is_none()
                    && std::io::stdin().is_terminal()
                    && std::io::stdout().is_terminal()
                    && value["ok"] != false
                {
                    // Foreground terminal UI; worker processes retain their remote owners.
                    let target = format!(
                        "{}@{}.exe.xyz",
                        ctx.fleet["remote_user"].as_str().unwrap_or("exedev"),
                        name
                    );
                    let session = ctx.fleet["herdr_session"].as_str().unwrap_or("workenv");
                    let result = std::process::Command::new("herdr")
                        .args(["--remote", &target, "--session", session])
                        .status()?;
                    value["ok"] = result.success().into();
                    value["status"] = if result.success() {
                        "detached"
                    } else {
                        "client_failed"
                    }
                    .into();
                }
                Ok(value)
            })())
        },
    )
    .description("Jump into a worker's Herdr session. MCP returns scoped connection details.")
    .mcp(mcp_options(true, false))
    .done();
    let out = CommandDef::typed::<WorkerArgs, (), (), Report, _, _>(
        "out",
        |input: TypedContext<WorkerArgs, (), ()>| async move {
            report(context(&input.globals).and_then(|ctx| {
                worker::out(&ctx, input.args.worker.as_deref(), "detach-instructions")
            }))
        },
    )
    .description("Show the shortcut to leave Herdr while remote work keeps running.")
    .mcp(mcp_options(true, false))
    .done();
    let down = CommandDef::typed::<TargetArgs, MutationOptions, (), Report, _, _>(
        "down",
        |input: TypedContext<TargetArgs, MutationOptions, ()>| async move {
            report(context(&input.globals).and_then(|ctx| {
                mutation(
                    &ctx,
                    "down",
                    json!({"target":input.args.target}),
                    &mutation_globals(&input.globals, input.options.idempotency_key.as_deref()),
                    input.agent || input.request.is_some(),
                    |key| {
                        if let Ok(name) = ctx.worker_name(&input.args.target) {
                            worker::down(&ctx, &name, key)
                        } else {
                            tasks::down(&ctx, &input.args.target, key)
                        }
                    },
                )
            }))
        },
    )
    .description(
        "Collect and release a finished task, or stop an idle worker's runtime while preserving its disk.",
    )
    .mcp(mcp_options(false, true))
    .done();
    let doctor = CommandDef::typed::<(), (), (), Report, _, _>(
        "doctor",
        |input: TypedContext<(), (), ()>| async move {
            report(context(&input.globals).and_then(|ctx| worker::doctor(&ctx)))
        },
    )
    .description("Check controller prerequisites and give concrete repair steps.")
    .mcp(mcp_options(true, false))
    .done();
    let claim = CommandDef::typed::<ClaimArgs, ClaimOptions, (), Report, _, _>("claim", |input: TypedContext<ClaimArgs, ClaimOptions, ()>| async move {
        report(context(&input.globals).and_then(|ctx| {
            let request = json!({"project":input.args.project,"task_id":input.args.task,"revision":input.options.revision,"worker":input.options.worker,"source_bundle":input.options.source_bundle,"profile":input.options.profile});
            mutation(&ctx, "claim", request.clone(), &mutation_globals(&input.globals, input.options.idempotency_key.as_deref()), input.agent || input.request.is_some(), |key| {
                let mut request = request;
                if request["revision"].is_null() {
                    if let Some(bundle) = request["source_bundle"].as_str() {
                        let source = std::path::Path::new(bundle).canonicalize()?.to_string_lossy().into_owned();
                        let output = ctx.run("git", vec!["ls-remote".into(), "--".into(), source, "HEAD".into()], &format!("{key}:resolve-head"), "Resolve the exact task source revision from a local bundle.", 60000)?.text()?;
                        let revision = output.split_whitespace().next().context("Source bundle has no HEAD; supply --revision with an exact commit")?;
                        if !matches!(revision.len(), 40 | 64) || !revision.bytes().all(|c| c.is_ascii_hexdigit()) { bail!("Source bundle returned an invalid HEAD revision"); }
                        request["revision"] = revision.into();
                    } else {
                        tasks::resolve_claim_revision(&ctx, &mut request, key)?;
                    }
                }
                tasks::claim(&ctx, request, key)
            })
        }))
    }).description("Reserve an available worker and check out an exact revision for one task.").mcp(mcp_options(false, false)).done();
    let run = CommandDef::typed::<RunArgs, MutationOptions, (), Report, _, _>(
        "run",
        |input: TypedContext<RunArgs, MutationOptions, ()>| async move {
            report(context(&input.globals).and_then(|ctx| {
                mutation(
                    &ctx,
                    "run",
                    json!({"task":input.args.task,"argv":input.args.argv}),
                    &mutation_globals(&input.globals, input.options.idempotency_key.as_deref()),
                    input.agent || input.request.is_some(),
                    |key| tasks::run(&ctx, &input.args.task, input.args.argv, key),
                )
            }))
        },
    )
    .description(
        "Run an exact command in a task worktree under remote APoC and return its execution ID.",
    )
    .mcp(mcp_options(false, true))
    .done();
    let collect = CommandDef::typed::<TaskArgs, MutationOptions, (), Report, _, _>(
        "collect",
        |input: TypedContext<TaskArgs, MutationOptions, ()>| async move {
            report(context(&input.globals).and_then(|ctx| {
                mutation(
                    &ctx,
                    "collect",
                    json!({"task":input.args.task}),
                    &mutation_globals(&input.globals, input.options.idempotency_key.as_deref()),
                    input.agent || input.request.is_some(),
                    |key| tasks::collect(&ctx, &input.args.task, key),
                )
            }))
        },
    )
    .description("Collect and verify task commits, dirty files, and evidence on this Mac.")
    .mcp(mcp_options(false, false))
    .done();
    let release = CommandDef::typed::<TaskArgs, MutationOptions, (), Report, _, _>(
        "release",
        |input: TypedContext<TaskArgs, MutationOptions, ()>| async move {
            report(context(&input.globals).and_then(|ctx| {
                mutation(
                    &ctx,
                    "release",
                    json!({"task":input.args.task}),
                    &mutation_globals(&input.globals, input.options.idempotency_key.as_deref()),
                    input.agent || input.request.is_some(),
                    |key| tasks::release(&ctx, &input.args.task, key),
                )
            }))
        },
    )
    .description("Release a stopped task only after verifying its unchanged collection.")
    .mcp(mcp_options(false, false))
    .done();
    let services = CommandDef::typed::<TaskArgs, MutationOptions, (), Report, _, _>(
        "services",
        |input: TypedContext<TaskArgs, MutationOptions, ()>| async move {
            report(context(&input.globals).and_then(|ctx| {
                mutation(
                    &ctx,
                    "services",
                    json!({"task":input.args.task}),
                    &mutation_globals(&input.globals, input.options.idempotency_key.as_deref()),
                    input.agent || input.request.is_some(),
                    |key| tasks::services(&ctx, &input.args.task, key),
                )
            }))
        },
    )
    .description(
        "Start the task's devenv services. Task down will stop the owned service execution.",
    )
    .mcp(mcp_options(false, false))
    .done();
    Cli::create("workenv")
        .version(env!("CARGO_PKG_VERSION"))
        .description("Your personal cloud development workers.")
        .globals::<Globals>()
        .root(status_command())
        .command("status", status_command())
        .command("up", up)
        .command("in", enter)
        .command("out", out)
        .command("down", down)
        .command("doctor", doctor)
        .command("claim", claim)
        .command("run", run)
        .command("services", services)
        .command("collect", collect)
        .command("release", release)
        .group(profile_commands())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::process::{CommandOutput, CommandSpec, Runtime};
    use std::sync::Arc;
    struct NoCommands;
    impl Runtime for NoCommands {
        fn run(&self, _: CommandSpec) -> Result<CommandOutput> {
            panic!("unexpected command")
        }
        fn apoc(&self, _: &str, _: Value) -> Result<Value> {
            panic!("unexpected APoC call")
        }
    }
    #[test]
    fn pending_receipt_is_returned_without_repeating_the_operation() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = Context {
            root: dir.path().into(),
            state: dir.path().join("state"),
            fleet: json!({}),
            runtime: Arc::new(NoCommands),
        };
        let globals = json!({"idempotency_key":"pending"});
        mutation(&ctx, "run", json!({}), &globals, true, |_| {
            Ok(json!({"ok":false,"status":"pending","execution_id":"durable-execution"}))
        })
        .unwrap();
        let replay = mutation(&ctx, "run", json!({}), &globals, true, |_| {
            panic!("must not repeat accepted execution")
        })
        .unwrap();
        assert_eq!(replay["execution_id"], "durable-execution");
        assert_eq!(replay["replayed"], true);
    }
    #[test]
    fn interrupted_intent_does_not_restart_an_unknown_operation() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = Context {
            root: dir.path().into(),
            state: dir.path().join("state"),
            fleet: json!({}),
            runtime: Arc::new(NoCommands),
        };
        let digest = format!("{:x}", Sha256::digest(b"interrupted"));
        write_json(
            &ctx.root.join(format!(".state/operations/{digest}.json")),
            &json!({"key":"interrupted","request":{"command":"up","input":{}},"phase":"running"}),
        )
        .unwrap();
        let result = mutation(
            &ctx,
            "up",
            json!({}),
            &json!({"idempotency_key":"interrupted"}),
            true,
            |_| panic!("must inspect unknown operation"),
        )
        .unwrap();
        assert_eq!(result["status"], "unknown");
        assert_eq!(result["ok"], false);
    }
    #[test]
    fn mutation_replays_and_rejects_different_input() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = Context {
            root: dir.path().into(),
            state: dir.path().join("state"),
            fleet: json!({}),
            runtime: Arc::new(NoCommands),
        };
        let globals = json!({"idempotency_key":"same"});
        mutation(&ctx, "up", json!({"worker":1}), &globals, true, |_| {
            Ok(json!({"ok":true,"status":"ready"}))
        })
        .unwrap();
        let replay = mutation(&ctx, "up", json!({"worker":1}), &globals, true, |_| {
            panic!("must replay")
        })
        .unwrap();
        assert_eq!(replay["status"], "ready");
        assert!(
            mutation(&ctx, "up", json!({"worker":2}), &globals, true, |_| panic!(
                "must reject"
            ))
            .is_err()
        );
        assert!(
            mutation(&ctx, "up", json!({}), &json!({}), true, |_| panic!(
                "key required"
            ))
            .is_err()
        );
    }
}
