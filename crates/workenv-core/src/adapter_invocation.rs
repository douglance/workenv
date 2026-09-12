//! Deciding how a controller-located adapter is launched.
//!
//! The devenv shell around an adapter exists to put its helper executables on
//! PATH, and it is extremely expensive: measured on a warm tree,
//! `devenv shell -- <adapter>` takes about 100 s against 39 ms for the same
//! adapter run directly, for an identical result. An `environment up` with two
//! integrations paid that three times over, which is why `extension call`
//! looked like it had hung while `extension inspect` returned straight away.
//! After this split the adapter call adds nothing measurable: 71 s for a call
//! against 73 s for an inspect that makes no adapter call at all.
use workenv_protocol::Extension;

/// How long a directly-run adapter is given.
pub(crate) const DIRECT_TIMEOUT_MS: u64 = 300_000;

/// How long an adapter wrapped in a devenv shell is given.
///
/// The shell builds the adapter as a side effect of being entered, so the budget
/// has to cover a compile, not just a run. Measured here: after editing any Rust
/// source the manifest names a store path that does not exist yet, the call is
/// wrapped, and the whole thing took 346 s against the 300 s direct budget -- so
/// the execution was cancelled and surfaced as "exited with 128 and wrote nothing
/// to either stream". Every first call after an edit failed that way, which is
/// exactly the loop someone developing workenv is in.
pub(crate) const WRAPPED_TIMEOUT_MS: u64 = 1_800_000;

/// A wrapped call must never be given less time than a direct one. Checked at
/// compile time rather than by a test, because a test of two constants in the same
/// file only proves the file agrees with itself.
const _: () = assert!(WRAPPED_TIMEOUT_MS > DIRECT_TIMEOUT_MS);

/// The budget this extension's next controller call should be given.
pub(crate) fn controller_timeout_ms(extension: &Extension) -> u64 {
    if extension.executable.starts_with("/nix/store") && wrapper_reason(extension).is_some() {
        WRAPPED_TIMEOUT_MS
    } else {
        DIRECT_TIMEOUT_MS
    }
}

pub(crate) fn controller_command(extension: &Extension) -> (String, Vec<String>) {
    let executable = extension.executable.to_string_lossy().into_owned();
    if !extension.executable.starts_with("/nix/store") {
        return (executable, Vec::new());
    }
    match wrapper_reason(extension) {
        None => (executable, Vec::new()),
        Some(reason) => {
            // Said out loud, because the cost is enormous and was invisible. The
            // shell adds roughly 100 s to a call that otherwise takes 39 ms, and
            // nothing reported which of the two preconditions failed -- so the
            // symptom was a command that looked hung, and finding the cause meant
            // reading this function. stdout carries the command's JSON, so stderr
            // is free for this.
            eprintln!(
                "workenv: wrapping {executable} in a devenv shell because {reason}; \
                 this adds roughly 100s to the call"
            );
            (
                "devenv".to_owned(),
                vec!["shell".to_owned(), "--".to_owned(), executable],
            )
        }
    }
}

/// Whether this adapter can be run without a devenv shell around it.
///
/// The shell exists to put an adapter's helper executables on PATH, and it is
/// extremely expensive: measured on a warm tree, `devenv shell -- <adapter>`
/// takes about 100 s against 39 ms for the same adapter run directly, for an
/// identical result. An `environment up` with two integrations pays that three
/// times over, which is why `extension call` looked like it was hanging while
/// `extension inspect` returned instantly.
///
/// The test is made against the machine, not against a flag. An extension opts
/// in by stating what it runs; the shell is skipped only when every one of those
/// already resolves here. An extension that states nothing keeps the shell, and
/// one whose helper is genuinely missing falls back to it, so this cannot
/// silently strand an adapter on a host that is set up differently.
/// Kept for the tests that read this as a yes/no question; production reads the
/// reason instead, so it can report it.
#[cfg(test)]
pub(crate) fn runs_unwrapped(extension: &Extension) -> bool {
    wrapper_reason(extension).is_none()
}

/// Why the shell cannot be skipped, or `None` when it can.
///
/// Separate from `runs_unwrapped` so the answer can be reported rather than only
/// acted on. Both preconditions below are environment-dependent, and this session
/// is the evidence for why that needs saying: `bootstrap` declared `nix`, which
/// resolves in an interactive shell and does NOT resolve in the PATH that apoc
/// hands its children -- which is the PATH this function actually reads, because
/// workenv runs under apoc. One name nobody could see cost every bootstrap call a
/// shell.
pub(crate) fn wrapper_reason(extension: &Extension) -> Option<String> {
    let Some(inputs) = &extension.runtime_inputs else {
        return Some("it declares no runtime_inputs, so nothing states what it needs".to_owned());
    };
    // The shell was also realising the derivation as a side effect of being
    // entered. Found by running it: after a source change the manifest names a
    // store path that does not exist yet, and running it directly failed with
    // exit 126 "cannot execute: No such file or directory". So the adapter has
    // to be on disk before the shell can be skipped -- otherwise the first call
    // after any rebuild breaks instead of paying for one shell and building it.
    if !extension.executable.is_file() {
        return Some(format!(
            "the adapter is not built yet at {}",
            extension.executable.display()
        ));
    }
    let unresolved: Vec<&str> = inputs
        .iter()
        .filter(|name| !resolves_on_path(name))
        .map(String::as_str)
        .collect();
    (!unresolved.is_empty())
        .then(|| format!("it declares {unresolved:?}, which do not resolve on PATH"))
}

/// Whether one executable name resolves to something runnable on PATH.
fn resolves_on_path(name: &str) -> bool {
    // An absolute path needs no search, and searching for it would fail.
    if name.contains('/') {
        return std::path::Path::new(name).is_file();
    }
    let Some(path) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&path).any(|dir| dir.join(name).is_file())
}
