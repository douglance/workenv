//! What the controller needs on the host before it can read anything.
//!
//! This is deliberately answerable without the manifest. `doctor` used to build
//! a controller first, which evaluates the configuration through devenv, so on a
//! machine with no devenv the one command whose job is to say what is missing
//! failed with `executable devenv was not found in PATH` and nothing else --
//! no list, no indication that `bootstrap` is the way in, and no report of the
//! prerequisite that *was* present.
use serde_json::{Value, json};
use workenv_platform::{Dangling, dangling_executable, locate_executable};

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
            None => match dangling_executable(name) {
                Some(dangling) => missing_behind_a_link(name, &dangling),
                None => json!({ "executable": name, "present": false }),
            },
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

/// A required executable that is there as a link to something that is not.
///
/// Reported separately from plain absence because the remedy is the opposite:
/// the executable is installed, and what it lives on is missing. Found on this
/// repository's own controller, where `doctor` said devenv was missing and the
/// truth was that the encrypted volume holding the Nix store had not unlocked at
/// boot -- everything was still there, and a reinstall would have been the wrong
/// fix.
fn missing_behind_a_link(name: &str, dangling: &Dangling) -> Value {
    let missing = dangling.missing.display().to_string();
    let hint = if missing == "/nix" || missing.starts_with("/nix/") {
        format!(
            "{name} is installed, but the Nix store it lives in is not there: {missing} does not \
             exist. Nix is likely not mounted rather than removed; on macOS its store is an \
             encrypted volume unlocked at boot."
        )
    } else {
        format!(
            "{name} is a link into {missing}, which does not exist. Restore what holds it rather \
             than reinstalling."
        )
    };
    json!({
        "executable": name,
        "present": false,
        "dangling": { "link": dangling.link, "missing": dangling.missing },
        "hint": hint,
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
