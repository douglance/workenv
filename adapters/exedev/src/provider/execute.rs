//! Running one exact command inside an exe.dev VM, as a transport.
//!
//! `workenv-core` ships a target-located extension to its host by calling the
//! host's transport with `execute`, so a host without a transport can only run
//! controller-located extensions. Every other transport dials `target.address`,
//! and an exe.dev VM does not have a usable one: the API advertises
//! `ssh <name>.exe.xyz`, and every attempt answers `slot <name>.exe.xyz has no
//! ssh.config on the VM host` -- measured across pooled and `--no-pool`
//! placement, and again after a restart. So without this, an exe.dev-backed host
//! could be created and then could not run `identity` or `project` at all.
//!
//! The only way in is the relay, `ssh exe.dev ssh <vm> <command>`, and it is a
//! restricted shell rather than a general one. Measured against a live VM:
//!
//! | property | result |
//! |---|---|
//! | `ProxyJump` / `-W` | refused: "only session channels supported" |
//! | stdin | **not forwarded** -- `cat > file` through it wrote zero bytes |
//! | stdout | carries a relay "Tip:" banner alongside the child's bytes |
//! | exit code | not reliably propagated |
//!
//! Two consequences shape this module. The command travels base64'd through
//! argv, because stdin is not available -- and a `cat > file` that silently
//! writes nothing is the worst failure mode available, since it reports success.
//! And the child's result is read from a sentinel-prefixed line rather than from
//! the process, because a transport that trusted this stdout would hand the
//! relay's banner to the caller as program output.
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use serde_json::{Value, json};
use workenv_protocol::{AdapterRequest, AdapterResponse, ResponseStatus};

use super::response;
use super::runner::{ProviderResult, Runner};
use super::wire::{parse, script, shell_quote};

/// Milliseconds allowed for one command before the transport gives up.
const DEFAULT_TIMEOUT_MS: u64 = 300_000;

/// The VM this request is about.
///
/// Mirrors `model::spec`'s resolution -- config name, then input name, then the
/// environment -- because a transport that resolved differently would run the
/// caller's command on a different machine than `create` made, and report
/// success for it.
pub(super) fn vm_name(request: &AdapterRequest) -> String {
    request
        .config
        .get("name")
        .and_then(Value::as_str)
        .or_else(|| request.input.get("name").and_then(Value::as_str))
        .unwrap_or(&request.target.environment)
        .to_owned()
}

/// Handle the two transport operations, or decline the rest.
pub(super) fn dispatch<R: Runner>(
    request: &AdapterRequest,
    runner: &mut R,
) -> Option<AdapterResponse> {
    let vm = vm_name(request);
    match request.operation.as_str() {
        "execute" => Some(run(request, runner, &vm)),
        "connect" => Some(response(
            request,
            ResponseStatus::Ready,
            json!({
                "status": "connection",
                "guest": vm,
                "attach_argv": attach_argv(request, &vm),
                "reaches_by": "exe.dev-relay",
            }),
            None,
        )),
        _ => None,
    }
}

/// Run one argv inside the named VM and report what it did.
fn run<R: Runner>(request: &AdapterRequest, runner: &mut R, vm: &str) -> AdapterResponse {
    let input = &request.input;
    let argv: Vec<String> = input
        .get("argv")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default();
    if argv.is_empty() {
        return response(
            request,
            ResponseStatus::Failed,
            json!({}),
            Some("execute requires a non-empty argv"),
        );
    }
    let cwd = input.get("cwd").and_then(Value::as_str).unwrap_or("/");
    let stdin = input.get("stdin").and_then(Value::as_str).unwrap_or("");
    let encoded = BASE64.encode(script(&argv, cwd, stdin).as_bytes());
    // `printf %s` rather than `echo`: a base64 payload can begin with `-`, which
    // echo would read as a flag and silently drop.
    let remote = format!("printf %s {} | base64 -d | bash", shell_quote(&encoded));
    let relay_args = vec!["ssh".to_owned(), vm.to_owned(), remote];
    match observe(runner, &relay_args) {
        Ok(value) => response(request, ResponseStatus::Ready, value, None),
        Err(message) => response(
            request,
            ResponseStatus::Failed,
            json!({}),
            Some(&format!("exe.dev relay did not report a result: {message}")),
        ),
    }
}

/// Argv that opens a shell on this VM, for `connect`.
///
/// No cluster read at all, deliberately: this answers even when the relay is
/// briefly unreachable, and the caller finds out at attach time either way.
///
/// `request` may carry the argv core wants run, because an environment binding
/// no `connection` still reaches here through the host's transport, and core
/// then supplies the devenv shell argv and the directory to run it in. Those are
/// folded into a single trailing argument: the relay takes two positionals, so
/// spreading a command across several silently loses all but the first.
fn attach_argv(request: &AdapterRequest, vm: &str) -> Vec<String> {
    let mut argv = vec![
        "ssh".to_owned(),
        "-o".to_owned(),
        "BatchMode=yes".to_owned(),
        "exe.dev".to_owned(),
        "ssh".to_owned(),
        vm.to_owned(),
    ];
    let inner: Vec<String> = request
        .input
        .get("argv")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(shell_quote)
                .collect()
        })
        .unwrap_or_default();
    if !inner.is_empty() {
        let cwd = request.input.get("cwd").and_then(Value::as_str);
        let command = inner.join(" ");
        argv.push(cwd.map_or(command.clone(), |dir| {
            format!("cd {} && {command}", shell_quote(dir))
        }));
    }
    argv
}

/// Exit status of one argv inside the named VM, or `None` when it never ran.
///
/// Shares `run`'s encoding rather than repeating it. The relay's quoting is the
/// part most easily got subtly wrong, and got wrong twice it would be wrong in
/// two different ways.
pub(super) fn probe_status<R: Runner>(runner: &mut R, vm: &str, argv: &[String]) -> Option<i64> {
    let encoded = BASE64.encode(script(argv, "/", "").as_bytes());
    let remote = format!("printf %s {} | base64 -d | bash", shell_quote(&encoded));
    observe(runner, &["ssh".to_owned(), vm.to_owned(), remote])
        .ok()
        .and_then(|value| value["exit_code"].as_i64())
}

fn observe<R: Runner>(runner: &mut R, args: &[String]) -> ProviderResult<Value> {
    let raw = runner.observe_raw(args, DEFAULT_TIMEOUT_MS)?;
    parse(&raw).ok_or_else(|| {
        let excerpt: String = raw.chars().take(400).collect();
        format!("no result line in relay output: {excerpt}")
    })
}
