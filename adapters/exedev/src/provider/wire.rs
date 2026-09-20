//! The wire format the transport speaks to an exe.dev guest.
//!
//! Split from `execute` because the relay forces a format rather than a call:
//! stdin is not forwarded, stdout is shared with an unsolicited banner, and the
//! exit code does not come back. So the controller sends a self-contained script
//! and reads one sentinel-prefixed line of JSON, and that encoding -- with the
//! quoting that keeps an argument from escaping it -- is what lives here. The
//! operations that use it live in `execute`.
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use serde_json::{Value, json};

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

/// Quote one argument for a POSIX shell.
///
/// Single quotes with the standard `'\''` escape, so a value containing spaces,
/// `$`, or a quote of its own cannot break out of the command being built.
pub(super) fn shell_quote(value: &str) -> String {
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
