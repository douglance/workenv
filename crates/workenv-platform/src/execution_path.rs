//! Finding the executables the controller shells out through.
//!
//! Its own module because `execution.rs` reached its line limit, and because the
//! rule here is worth stating on its own: when the process has no `PATH` -- which
//! is how `APoC` launches workenv -- the search falls back to a built-in list plus
//! the operator's `~/.local/bin`, and a failure has to say which of the two it
//! actually searched.
use anyhow::{Result, bail};
use std::path::Path;

/// Used when the process has no `PATH`, which is how `APoC` launches workenv.
const DEFAULT_UNIX_PATH: &str = "/usr/bin:/bin:/usr/sbin:/sbin:/opt/homebrew/bin";

/// Where an executable would be found, or nothing when it is absent. A caller
/// reporting on the host wants the absence as a value, not as an error message
/// it has to parse.
#[must_use]
pub fn locate_executable(executable: &str) -> Option<String> {
    resolve_executable(executable).ok()
}

/// An executable that is on the search path as a link to something that is not
/// there: the link, and the first piece of its chain that does not exist.
///
/// "Not found" and "a link to something missing" call for opposite remedies.
/// The first means install it; the second usually means something it lives on
/// -- a volume, a store, a checkout -- is absent, and reinstalling is the wrong
/// move. On a Mac whose Nix store volume had not unlocked at boot, every Nix
/// tool was the second kind, and was reported as the first.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Dangling {
    /// The link found on the search path.
    pub link: std::path::PathBuf,
    /// The first piece of its chain that does not exist.
    pub missing: std::path::PathBuf,
}

/// Look for `executable` as a dangling link anywhere on the search path.
#[must_use]
pub fn dangling_executable(executable: &str) -> Option<Dangling> {
    dangling_on_path(
        executable,
        std::env::var_os("PATH").filter(|value| !value.is_empty()),
    )
}

pub(crate) fn dangling_on_path(
    executable: &str,
    path_var: Option<std::ffi::OsString>,
) -> Option<Dangling> {
    let path_var = path_var.unwrap_or_else(default_search_path);
    std::env::split_paths(&path_var)
        .map(|dir| dir.join(executable))
        .find(|link| link.symlink_metadata().is_ok() && !link.exists())
        .map(|link| Dangling {
            missing: first_missing(&link),
            link,
        })
}

/// Follow a path through every link in it, component by component, and return
/// the first place that does not exist. Bounded, because links can loop.
fn first_missing(path: &Path) -> std::path::PathBuf {
    let mut remaining: std::collections::VecDeque<std::ffi::OsString> = path
        .components()
        .map(|part| part.as_os_str().to_owned())
        .collect();
    let mut current = std::path::PathBuf::new();
    let mut hops = 0;
    while let Some(part) = remaining.pop_front() {
        current.push(&part);
        match std::fs::read_link(&current) {
            Ok(target) if hops < 64 => {
                hops += 1;
                // An absolute target replaces the base; a relative one is read
                // from the link's own directory. Either way the walk restarts
                // from the resolved path with the unwalked parts after it.
                let base = current.parent().map(Path::to_path_buf).unwrap_or_default();
                prepend(&mut remaining, &base.join(target));
                current = std::path::PathBuf::new();
            }
            Ok(_) => return current,
            Err(_) if current.symlink_metadata().is_err() => return current,
            Err(_) => {}
        }
    }
    current
}

/// Put a path's components in front of the ones still to walk.
fn prepend(remaining: &mut std::collections::VecDeque<std::ffi::OsString>, path: &Path) {
    for part in path.components().rev() {
        remaining.push_front(part.as_os_str().to_owned());
    }
}

pub(crate) fn resolve_executable(executable: &str) -> Result<String> {
    resolve_on_path(
        executable,
        std::env::var_os("PATH").filter(|value| !value.is_empty()),
    )
}

fn default_search_path() -> std::ffi::OsString {
    default_search_path_for(std::env::var_os("HOME"))
}

/// The home directory is a parameter so the fallback can be exercised against a
/// directory the test builds. Reading `HOME` inside the rule left the only test
/// of it asking the host for a real `apoc` and skipping the assertion when the
/// host had none -- green on this developer's Mac, red on a runner, and proof of
/// nothing either way.
fn default_search_path_for(home: Option<std::ffi::OsString>) -> std::ffi::OsString {
    let mut path = DEFAULT_UNIX_PATH.to_owned();
    if let Some(home) = home {
        path.push(':');
        path.push_str(&Path::new(&home).join(".local/bin").to_string_lossy());
    }
    path.into()
}

pub(crate) fn resolve_on_path(
    executable: &str,
    path_var: Option<std::ffi::OsString>,
) -> Result<String> {
    let path = Path::new(executable);
    if path.is_absolute() || executable.contains(std::path::MAIN_SEPARATOR) {
        return Ok(executable.to_owned());
    }
    // Which of the two lists was searched, because they are different lists and
    // the caller cannot tell from the outside.
    let (source, path_var) = match path_var {
        Some(value) => ("PATH", value),
        None => ("the built-in search path", default_search_path()),
    };
    for dir in std::env::split_paths(&path_var) {
        let candidate = dir.join(executable);
        if candidate.is_file() {
            return Ok(candidate.to_string_lossy().into_owned());
        }
    }
    // Naming which list, not printing it: when the caller passed nothing, PATH
    // is precisely what was *not* searched, so saying "not found in PATH" sends
    // the reader to look at the wrong thing -- and printing forty entries sends
    // them nowhere at all. `doctor` is where the actionable detail lives.
    bail!(
        "executable {executable} was not found on {source}, across {} directories",
        std::env::split_paths(&path_var).count()
    );
}

#[cfg(test)]
// Test-only, and only these: a fixture that cannot unwrap says less than one
// that panics loudly when the fixture is wrong.
#[allow(clippy::expect_used, clippy::unwrap_used)]
#[path = "execution_path_tests.rs"]
mod tests;
