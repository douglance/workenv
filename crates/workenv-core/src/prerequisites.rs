//! What the controller needs on the host before it can read anything.
//!
//! This is deliberately answerable without the manifest. `doctor` used to build
//! a controller first, which evaluates the configuration through devenv, so on a
//! machine with no devenv the one command whose job is to say what is missing
//! failed with `executable devenv was not found in PATH` and nothing else --
//! no list, no indication that `bootstrap` is the way in, and no report of the
//! prerequisite that *was* present.
use serde_json::{Value, json};
use workenv_platform::locate_executable;

/// The executables the controller shells out through.
///
/// `devenv` evaluates the configuration; `apoc` runs every adapter invocation.
/// Neither has a fallback, which is why their absence is a prerequisite failure
/// rather than a degraded mode.
const REQUIRED: [&str; 2] = ["devenv", "apoc"];

/// One entry per required executable, with where it resolved or that it did not.
#[must_use]
pub fn report() -> Value {
    let found: Vec<Value> = REQUIRED
        .iter()
        .map(|name| match locate_executable(name) {
            Some(path) => json!({ "executable": name, "present": true, "path": path }),
            None => json!({ "executable": name, "present": false }),
        })
        .collect();
    let missing: Vec<&Value> = found
        .iter()
        .filter(|entry| entry["present"] == json!(false))
        .collect();
    json!({
        "ok": missing.is_empty(),
        "required": found,
        "missing": missing.iter().map(|entry| &entry["executable"]).collect::<Vec<_>>(),
    })
}

/// Whether every required executable resolved.
#[must_use]
pub fn satisfied(report: &Value) -> bool {
    report["ok"] == json!(true)
}

#[cfg(test)]
#[path = "prerequisites_tests.rs"]
mod tests;
