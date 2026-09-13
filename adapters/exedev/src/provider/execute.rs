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

/// Milliseconds allowed for one command before the transport gives up.
const DEFAULT_TIMEOUT_MS: u64 = 300_000;

/// Marks the one stdout line carrying the child's result.
///
/// The relay prints an unsolicited "Tip: relaying through exe.dev adds a hop to
/// every command" on stdout, so the stream is not the child's alone. Selecting a
/// line by prefix is what keeps that banner out of the caller's `stdout`.
pub(super) const SENTINEL: &str = "__WORKENV_EXEDEV_RESULT__";

/// Exit statuses the wrapper reserves for its own failures.
///
/// Chosen above any status a child realistically returns, so a wrapper fault is
/// never mistaken for the command's own result.
const WRAPPER_CHDIR_FAILED: i64 = 122;

/// Build the remote script that runs one command and reports its own result.
///
/// The script is responsible for the whole answer: it captures the child's
/// streams and status itself and prints one sentinel line. Nothing downstream
/// reads the relay's own exit code, because it does not reliably carry the
/// child's.
pub(super) fn script(argv: &[String], cwd: &str, stdin: &str) -> String {
    let command = argv
        .iter()
        .map(|part| shell_quote(part))
        .collect::<Vec<_>>()
        .join(" ");
    let stdin_b64 = BASE64.encode(stdin.as_bytes());
    format!(
        r#"set -u
out=$(mktemp); err=$(mktemp); inp=$(mktemp)
printf %s {stdin} | base64 -d > "$inp" 2>/dev/null || : > "$inp"
cd {cwd} 2>/dev/null || {{
  printf '%s{{"exit_code":{chdir},"stdout":"","stderr":"cannot change directory"}}\n' '{sentinel}'
  exit 0
}}
{command} < "$inp" > "$out" 2> "$err"
status=$?
printf '%s{{"exit_code":%s,"stdout":"%s","stderr":"%s"}}\n' \
  '{sentinel}' "$status" "$(base64 < "$out" | tr -d '\n')" "$(base64 < "$err" | tr -d '\n')"
rm -f "$out" "$err" "$inp"
"#,
        stdin = shell_quote(&stdin_b64),
        cwd = shell_quote(cwd),
        chdir = WRAPPER_CHDIR_FAILED,
        sentinel = SENTINEL,
        command = command,
    )
}

/// The child's result, recovered from the one line that carries it.
///
/// Returns `None` when no sentinel line is present, which means the command
/// never ran -- a relay refusal, a missing VM, a dropped connection -- rather
/// than a command that failed. Those are different answers and the caller has to
/// be able to tell them apart.
pub(super) fn parse(stdout: &str) -> Option<Value> {
    let line = stdout
        .lines()
        .find_map(|line| line.strip_prefix(SENTINEL))?;
    let value: Value = serde_json::from_str(line.trim()).ok()?;
    // Written out rather than chained: the combinator form failed to infer a
    // sized type through `decode`'s `AsRef<[u8]>` argument. Each step also says
    // what a missing or malformed field means -- an empty stream, not an error,
    // because a command that legitimately wrote nothing is the common case.
    let decode = |field: &str| -> String {
        let Some(text) = value.get(field).and_then(Value::as_str) else {
            return String::new();
        };
        let Ok(bytes) = BASE64.decode(text) else {
            return String::new();
        };
        String::from_utf8_lossy(&bytes).into_owned()
    };
    Some(json!({
        "exit_code": value.get("exit_code").and_then(Value::as_i64).unwrap_or(-1),
        "stdout": decode("stdout"),
        "stderr": decode("stderr"),
    }))
}

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

fn observe<R: Runner>(runner: &mut R, args: &[String]) -> ProviderResult<Value> {
    let raw = runner.observe_raw(args, DEFAULT_TIMEOUT_MS)?;
    parse(&raw).ok_or_else(|| {
        let excerpt: String = raw.chars().take(400).collect();
        format!("no result line in relay output: {excerpt}")
    })
}

/// Quote one argument for a POSIX shell.
///
/// Single quotes with the standard `'\''` escape, so a value containing spaces,
/// `$`, or a quote of its own cannot break out of the command being built.
fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', r"'\''"))
}

#[cfg(test)]
mod tests {
    use anyhow::{Result, anyhow};

    use super::*;

    #[test]
    fn result_line_is_selected_out_of_a_polluted_stream() -> Result<()> {
        // The relay's banner is real, arrives on stdout, and is what a naive
        // `serde_json::from_str(&stdout)` would choke on.
        let payload = format!(
            "Tip: relaying through exe.dev adds a hop to every command\n\
             {SENTINEL}{{\"exit_code\":7,\"stdout\":\"aGk=\",\"stderr\":\"\"}}\n"
        );
        let parsed = parse(&payload).ok_or_else(|| anyhow!("no sentinel line found"))?;
        assert_eq!(parsed["exit_code"], 7);
        assert_eq!(parsed["stdout"], "hi");
        Ok(())
    }

    #[test]
    fn a_command_that_never_ran_is_not_a_command_that_failed() {
        // Without this the caller cannot tell "the VM refused us" from "the
        // program exited non-zero", and would retry the wrong one.
        assert!(parse("slot wkv-1.exe.xyz has no ssh.config on the VM host\n").is_none());
        assert!(parse("").is_none());
    }

    #[test]
    fn a_nonzero_exit_is_reported_as_itself() -> Result<()> {
        // The relay does not propagate the child's status, so if this regressed
        // to reading the process result every failure would surface as 1.
        let line = format!("{SENTINEL}{{\"exit_code\":101,\"stdout\":\"\",\"stderr\":\"YmFk\"}}");
        let parsed = parse(&line).ok_or_else(|| anyhow!("no sentinel line found"))?;
        assert_eq!(parsed["exit_code"], 101);
        assert_eq!(parsed["stderr"], "bad");
        Ok(())
    }

    #[test]
    fn quoting_contains_an_argument_that_tries_to_escape() {
        let built = script(
            &["sh".into(), "-c".into(), "echo '; rm -rf /".into()],
            "/tmp",
            "",
        );
        assert!(built.contains(r"'echo '\''; rm -rf /'"));
    }

    #[test]
    fn the_script_reports_a_failed_chdir_rather_than_running_anywhere() {
        // Running the command in an unexpected directory is worse than not
        // running it, so the script answers with its own reserved status.
        let built = script(&["true".into()], "/missing", "");
        assert!(built.contains(&format!("\"exit_code\":{WRAPPER_CHDIR_FAILED}")));
    }
}
