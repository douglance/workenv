use std::{path::Path, process::Command};

use anyhow::{Context as _, Result, bail};
use serde_json::{Value, json};

use super::{ExecutionOutput, ExecutionSpec};

const ARTIFACT_BYTES: u64 = 16_777_216;

pub(super) fn run_execution(root: &Path, spec: &ExecutionSpec) -> Result<ExecutionOutput> {
    let code = start_code_for(root, spec)?;
    let value = run_code(root, &code_argv(spec, &code))?;
    output_from_code_result(&value)
}

pub(super) fn observe_execution(
    root: &Path,
    execution_id: &str,
    purpose: &str,
    timeout_ms: u64,
) -> Result<ExecutionOutput> {
    let code = observe_code_for(execution_id, purpose, timeout_ms)?;
    output_from_code_result(&run_code(
        root,
        &observe_code_argv(execution_id, purpose, timeout_ms, &code),
    )?)
}

fn run_code(root: &Path, argv: &[String]) -> Result<Value> {
    let apoc = crate::execution::resolve_executable("apoc")?;
    let output = Command::new(apoc)
        .args(argv)
        .current_dir(root)
        .output()
        .context("run APoC Code Mode")?;
    let value: Value = serde_json::from_slice(&output.stdout)
        .with_context(|| format!("APoC returned invalid JSON: {}", stderr(&output)))?;
    if !output.status.success() {
        bail!(
            "APoC Code Mode failed: {}",
            failure_message(&value, &output)
        );
    }
    Ok(value)
}

/// Everything `APoC` said about a failure, rather than one field that is often
/// absent.
///
/// `value["error"]` alone renders as the literal `null` whenever `APoC` exits
/// non-zero without that key -- which is what it does when the daemon rejects or
/// times out a call. That message named neither the cause nor where to look, so
/// this falls through `error`, `message`, `code` and `status`, and appends
/// stderr when the JSON carries nothing at all.
fn failure_message(value: &Value, output: &std::process::Output) -> String {
    let described = ["error", "message", "code", "status"]
        .into_iter()
        .filter_map(|key| match value.get(key) {
            Some(found) if !found.is_null() => Some(match found.as_str() {
                Some(text) => format!("{key}: {text}"),
                None => format!("{key}: {found}"),
            }),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("; ");
    let trailer = stderr(output).trim().to_owned();
    match (described.is_empty(), trailer.is_empty()) {
        (true, true) => format!("no diagnostic; apoc exited {}", output.status),
        (true, false) => trailer,
        (false, true) => described,
        (false, false) => format!("{described} ({trailer})"),
    }
}

fn code_argv(spec: &ExecutionSpec, code: &str) -> Vec<String> {
    code_mode_argv(
        code,
        spec.timeout_ms.saturating_add(10_000),
        format!(
            "workenv-platform:{}:observe:{}",
            spec.idempotency_key,
            uuid::Uuid::new_v4()
        ),
        spec.purpose.clone(),
    )
}

fn observe_code_argv(
    execution_id: &str,
    purpose: &str,
    timeout_ms: u64,
    code: &str,
) -> Vec<String> {
    code_mode_argv(
        code,
        timeout_ms.saturating_add(10_000),
        format!(
            "workenv-platform:{execution_id}:observe:{}",
            uuid::Uuid::new_v4()
        ),
        purpose.to_owned(),
    )
}

fn code_mode_argv(
    code: &str,
    timeout_ms: u64,
    idempotency_key: String,
    purpose: String,
) -> Vec<String> {
    vec![
        "code".to_owned(),
        "run".to_owned(),
        code.to_owned(),
        "--timeout-ms".to_owned(),
        timeout_ms.to_string(),
        "--idempotency-key".to_owned(),
        idempotency_key,
        "--purpose".to_owned(),
        purpose,
        "--filter-output".to_owned(),
        "id,status,result,error".to_owned(),
        "--format".to_owned(),
        "json".to_owned(),
    ]
}

pub(super) fn start_code_for(root: &Path, spec: &ExecutionSpec) -> Result<String> {
    let cwd = spec.cwd.clone().unwrap_or_else(|| root.to_path_buf());
    let payload = serde_json::to_string(&json!({
        "executable": spec.executable,
        "arg": spec.arg,
        "cwd": cwd,
        "timeout_ms": spec.timeout_ms,
        "idempotency_key": spec.idempotency_key,
        "purpose": spec.purpose,
        "artifact_bytes": ARTIFACT_BYTES,
    }))?;
    Ok(format!(
        "const input = {payload};\
         const started = await apoc.execution_start({{\
         executable: input.executable,arg: input.arg,cwd: input.cwd,\
         timeout_ms: input.timeout_ms,artifact_bytes: input.artifact_bytes,\
         expect_exit_code: [0],verbosity: \"trace\",\
         idempotency_key: input.idempotency_key,purpose: input.purpose}});\
         const execution = started.execution || started;\
         const id = execution.id || started.id;\
         let waited = {{outcome: \"pending\"}};\
         try {{ waited = await apoc.execution_wait({{\
         id,timeout_ms: Math.min(input.timeout_ms, 30000),verbosity: \"trace\",\
         purpose: input.purpose}}); }}\
         catch (error) {{ waited = {{outcome: \"pending\", error: String(error)}}; }}\
         const logs = await apoc.execution_logs({{\
         id,tail_bytes: input.artifact_bytes,purpose: input.purpose}});\
         return {{id,outcome: waited.outcome || started.outcome,waited,logs}};"
    ))
}

fn observe_code_for(execution_id: &str, purpose: &str, timeout_ms: u64) -> Result<String> {
    let payload = serde_json::to_string(&json!({
        "id": execution_id,
        "timeout_ms": timeout_ms,
        "purpose": purpose,
        "artifact_bytes": ARTIFACT_BYTES,
    }))?;
    Ok(format!(
        "const input = {payload};\
         let waited = {{outcome: \"pending\"}};\
         try {{ waited = await apoc.execution_wait({{\
         id: input.id,timeout_ms: Math.min(input.timeout_ms, 30000),verbosity: \"trace\",\
         purpose: input.purpose}}); }}\
         catch (error) {{ waited = {{outcome: \"pending\", error: String(error)}}; }}\
         const logs = await apoc.execution_logs({{\
         id: input.id,tail_bytes: input.artifact_bytes,purpose: input.purpose}});\
         return {{id: input.id,outcome: waited.outcome,waited,logs}};"
    ))
}

fn output_from_code_result(value: &Value) -> Result<ExecutionOutput> {
    let result = value
        .get("result")
        .context("APoC Code Mode returned no result")?;
    let logs = result
        .get("logs")
        .context("APoC execution returned no logs")?;
    if logs["stdout_truncated"] == true || logs["stderr_truncated"] == true {
        bail!("APoC execution output was truncated");
    }
    Ok(ExecutionOutput {
        stdout: logs["stdout"].as_str().unwrap_or_default().to_owned(),
        stderr: logs["stderr"].as_str().unwrap_or_default().to_owned(),
        exit_code: exit_code(result),
        execution_id: result["id"].as_str().unwrap_or_default().to_owned(),
    })
}

fn exit_code(value: &Value) -> Option<i32> {
    value
        .pointer("/waited/result/exit_code")
        .or_else(|| value.pointer("/waited/execution/result/exit_code"))
        .or_else(|| value.pointer("/waited/execution/exit_code"))
        .or_else(|| value.get("exit_code"))
        .and_then(Value::as_i64)
        .and_then(|code| i32::try_from(code).ok())
        .or_else(|| (value["outcome"] == "passed").then_some(0))
}

fn stderr(output: &std::process::Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[cfg(test)]
mod tests {
    use std::os::unix::process::ExitStatusExt as _;

    use serde_json::json;

    use super::failure_message;

    fn output(stderr: &str) -> std::process::Output {
        std::process::Output {
            status: std::process::ExitStatus::from_raw(256),
            stdout: Vec::new(),
            stderr: stderr.as_bytes().to_vec(),
        }
    }

    #[test]
    fn failure_without_an_error_key_still_names_the_cause() {
        let value = json!({"code":"EXECUTION_TIMEOUT","status":"error"});
        let message = failure_message(&value, &output(""));
        assert!(message.contains("EXECUTION_TIMEOUT"), "{message}");
        assert!(message.contains("error"), "{message}");
    }

    #[test]
    fn failure_carrying_nothing_falls_back_to_stderr() {
        let message = failure_message(&json!({}), &output("daemon refused the call"));
        assert_eq!(message, "daemon refused the call");
    }

    #[test]
    fn failure_carrying_nothing_at_all_still_reports_the_exit() {
        let message = failure_message(&json!({}), &output(""));
        assert!(message.contains("no diagnostic"), "{message}");
    }

    #[test]
    fn an_error_key_is_preferred_and_stderr_is_kept_beside_it() {
        let value = json!({"error":"boom"});
        let message = failure_message(&value, &output("context"));
        assert_eq!(message, "error: boom (context)");
    }
}
