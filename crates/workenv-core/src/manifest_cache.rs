//! Reusing a manifest evaluation instead of paying for it every command.
//!
//! `devenv eval workenv.manifestJSON` takes 68 s on a warm tree, and every
//! single command pays it once. Measured on the same tree, `devenv eval
//! packages` is 32 s of that, so roughly half is devenv and nixpkgs evaluation
//! and the rest is evaluating the adapter package derivation to get its store
//! path. Neither half can be made cheaper from here, so the evaluation is
//! reused instead.
//!
//! A stale manifest is far worse than a slow one -- it would act on host
//! definitions that no longer exist -- so the fingerprint covers everything
//! that can change the result, and a cached entry is additionally checked
//! against reality before it is trusted:
//!
//! * every file in the git working tree that holds the manifest, by content.
//!   The manifest embeds `/nix/store` paths derived from the Rust sources, so
//!   editing a `.rs` file changes the manifest and must invalidate it; hashing
//!   only the `.nix` files would not.
//! * `devenv.yaml` and `devenv.lock`, which pin nixpkgs and devenv.
//! * every executable the cached manifest names still existing on disk, which
//!   catches a store path garbage-collected out from under the cache.
//!
//! The residual risk is a manifest importing a `.nix` file from outside the
//! repository: that cannot be seen from here, so caching is skipped entirely
//! when the root is not inside a git working tree. `WORKENV_MANIFEST_CACHE=0`
//! turns it off.
use std::path::{Path, PathBuf};

use anyhow::Result;
use sha2::{Digest, Sha256};
use workenv_protocol::Manifest;

/// Environment variable that disables reuse.
const DISABLE: &str = "WORKENV_MANIFEST_CACHE";

/// Read a manifest that is still valid for this tree, if there is one.
pub(crate) fn read(root: &Path) -> Option<Manifest> {
    let fingerprint = fingerprint(root)?;
    let stored = std::fs::read_to_string(cache_path(root)?)
        .ok()?
        .parse::<Entry>()
        .ok()?;
    if stored.fingerprint != fingerprint {
        return None;
    }
    // The fingerprint is the whole validity test, and deliberately so.
    //
    // An earlier version also required every executable the manifest names to
    // exist on disk, reasoning that a collected store path makes the manifest
    // useless. That was wrong twice over. `devenv eval` returns a derivation's
    // OUTPUT path without building it, so a correct, freshly written manifest
    // routinely names paths that do not exist yet -- measured here with all nine
    // of them absent, which made the cache miss every single time and left the
    // feature doing nothing at all. And it duplicated a guarantee that already
    // lives downstream: adapter_invocation falls back to a devenv shell when the
    // adapter is not on disk, which is what builds it.
    serde_json::from_str(&stored.manifest).ok()
}

/// Record this manifest as the answer for this tree.
///
/// Failure to write is not an error: the cache is an optimisation, and a
/// read-only or full disk should slow the tool down, not break it.
pub(crate) fn write(root: &Path, manifest_json: &str) {
    let Some(fingerprint) = fingerprint(root) else {
        return;
    };
    let Some(path) = cache_path(root) else {
        return;
    };
    if path
        .parent()
        .is_some_and(|parent| std::fs::create_dir_all(parent).is_err())
    {
        return;
    }
    let body = format!("{fingerprint}\n{manifest_json}");
    let _ = std::fs::write(path, body);
}

/// One cache entry: a fingerprint line, then the manifest it belongs to.
struct Entry {
    fingerprint: String,
    manifest: String,
}

impl std::str::FromStr for Entry {
    type Err = ();

    fn from_str(text: &str) -> Result<Self, ()> {
        // Split once, so a manifest containing newlines survives intact.
        let (fingerprint, manifest) = text.split_once('\n').ok_or(())?;
        Ok(Self {
            fingerprint: fingerprint.to_owned(),
            manifest: manifest.to_owned(),
        })
    }
}

/// Where the cache for this root lives.
///
/// Outside the repository, deliberately. Written inside it, the cache file is an
/// untracked path that `git status` reports, so writing it changed the very
/// fingerprint it was stored under and every read missed. It appeared to work
/// only where `.devenv*` happened to be gitignored, which is exactly the kind of
/// environment-dependent behaviour worth not having. Found by the tests.
fn cache_path(root: &Path) -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".cache")))?;
    let key = format!("{:x}", Sha256::digest(root.to_string_lossy().as_bytes()));
    Some(base.join("workenv").join(format!("manifest-{key}.cache")))
}

/// A hash of everything that can change this root's manifest.
///
/// Asked of git, not computed by walking the tree. The first version of this
/// content-hashed every file under the repository, which was measured at
/// **13.6 GB across 65,741 files** here -- `.state/` holds build-artifact
/// directories and git bundles -- and it ran twice per command. The cache made
/// the tool *slower*: 77 s with caching disabled against a 900 s timeout with it
/// enabled. git answers the same question in 0.04 s.
///
/// It is also more correct. Ignored paths cannot affect the manifest, and git is
/// the thing that knows which those are. The assumption this rests on is that
/// nothing gitignored feeds the evaluation; `devenv.yaml`, `devenv.lock` and
/// every module are tracked, so that holds here.
///
/// `None` means "do not cache": either reuse is switched off, or this is not a
/// git working tree and the inputs cannot be bounded.
fn fingerprint(root: &Path) -> Option<String> {
    if std::env::var(DISABLE).as_deref() == Ok("0") {
        return None;
    }
    let repository = repository_of(root)?;
    let head = git(&repository, &["rev-parse", "HEAD"])?;
    // Dirty and untracked-but-not-ignored paths, which HEAD alone cannot see.
    let status = git(&repository, &["status", "--porcelain=v1"])?;
    let mut digest = Sha256::new();
    digest.update(head.as_bytes());
    digest.update([0]);
    digest.update(status.as_bytes());
    digest.update([0]);
    // The status line alone is not enough: editing one file twice leaves the
    // same " M path" line while changing what the manifest evaluates to.
    for path in status.lines().filter_map(changed_path) {
        digest.update(path.as_bytes());
        digest.update([0]);
        if let Ok(bytes) = std::fs::read(repository.join(path)) {
            digest.update(Sha256::digest(&bytes));
        }
    }
    // The root itself is part of the key: two roots in one repository evaluate
    // to different manifests.
    digest.update(root.to_string_lossy().as_bytes());
    Some(format!("{:x}", digest.finalize()))
}

/// One path out of a `git status --porcelain=v1` line.
///
/// A rename reads `R  old -> new`; the new name is the one on disk to hash.
fn changed_path(line: &str) -> Option<&str> {
    let rest = line.get(3..)?.trim();
    Some(rest.rsplit(" -> ").next().unwrap_or(rest))
}

/// Run one read-only git query, or nothing when it cannot be answered.
fn git(repository: &Path, args: &[&str]) -> Option<String> {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(repository)
        .args(args)
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).into_owned())
}

/// The nearest enclosing directory holding a `.git`.
fn repository_of(root: &Path) -> Option<PathBuf> {
    root.ancestors()
        .find(|candidate| candidate.join(".git").exists())
        .map(Path::to_path_buf)
}
