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

pub(crate) fn controller_command(extension: &Extension) -> (String, Vec<String>) {
    let executable = extension.executable.to_string_lossy().into_owned();
    if extension.executable.starts_with("/nix/store") && !runs_unwrapped(extension) {
        (
            "devenv".to_owned(),
            vec!["shell".to_owned(), "--".to_owned(), executable],
        )
    } else {
        (executable, Vec::new())
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
pub(crate) fn runs_unwrapped(extension: &Extension) -> bool {
    let Some(inputs) = &extension.runtime_inputs else {
        return false;
    };
    // The shell was also realising the derivation as a side effect of being
    // entered. Found by running it: after a source change the manifest names a
    // store path that does not exist yet, and running it directly failed with
    // exit 126 "cannot execute: No such file or directory". So the adapter has
    // to be on disk before the shell can be skipped -- otherwise the first call
    // after any rebuild breaks instead of paying for one shell and building it.
    if !extension.executable.is_file() {
        return false;
    }
    inputs.iter().all(|name| resolves_on_path(name))
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
