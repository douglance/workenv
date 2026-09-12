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

/// Directories never worth hashing, and ruinous to walk.
const SKIPPED: &[&str] = &[".git", "target", ".devenv", "result", ".direnv"];

/// Environment variable that disables reuse.
const DISABLE: &str = "WORKENV_MANIFEST_CACHE";

/// Read a manifest that is still valid for this tree, if there is one.
pub(crate) fn read(root: &Path) -> Option<Manifest> {
    let fingerprint = fingerprint(root)?;
    let stored = std::fs::read_to_string(cache_path(root))
        .ok()?
        .parse::<Entry>()
        .ok()?;
    if stored.fingerprint != fingerprint {
        return None;
    }
    let manifest: Manifest = serde_json::from_str(&stored.manifest).ok()?;
    // The fingerprint says the inputs are unchanged; this says the outputs are
    // still there. A store path can be collected after the cache is written,
    // and a manifest naming a path that no longer exists is worse than no
    // manifest at all.
    manifest
        .extensions
        .values()
        .all(|extension| extension.executable.exists())
        .then_some(manifest)
}

/// Record this manifest as the answer for this tree.
///
/// Failure to write is not an error: the cache is an optimisation, and a
/// read-only or full disk should slow the tool down, not break it.
pub(crate) fn write(root: &Path, manifest_json: &str) {
    let Some(fingerprint) = fingerprint(root) else {
        return;
    };
    let path = cache_path(root);
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

/// Where the cache for this root lives; `.devenv` is already git-ignored.
fn cache_path(root: &Path) -> PathBuf {
    root.join(".devenv/workenv-manifest.cache")
}

/// A hash of everything that can change this root's manifest.
///
/// `None` means "do not cache": either reuse is switched off, or the root is
/// not inside a git working tree and the import closure cannot be bounded.
fn fingerprint(root: &Path) -> Option<String> {
    if std::env::var(DISABLE).as_deref() == Ok("0") {
        return None;
    }
    let repository = repository_of(root)?;
    let mut entries = Vec::new();
    collect(&repository, &repository, &mut entries)?;
    entries.sort();
    let mut digest = Sha256::new();
    for (path, hash) in &entries {
        digest.update(path.as_bytes());
        digest.update([0]);
        digest.update(hash);
    }
    // The root itself is part of the key: two roots in one repository evaluate
    // to different manifests.
    digest.update(root.to_string_lossy().as_bytes());
    Some(format!("{:x}", digest.finalize()))
}

/// The nearest enclosing directory holding a `.git`.
fn repository_of(root: &Path) -> Option<PathBuf> {
    root.ancestors()
        .find(|candidate| candidate.join(".git").exists())
        .map(Path::to_path_buf)
}

/// Hash every regular file under `dir`, relative to `base`.
///
/// Symlinks are recorded by their target text rather than followed: `target` is
/// a symlink into a build cache here, and following it would hash gigabytes.
fn collect(base: &Path, dir: &Path, out: &mut Vec<(String, Vec<u8>)>) -> Option<()> {
    for entry in std::fs::read_dir(dir).ok()? {
        let entry = entry.ok()?;
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        if SKIPPED.contains(&name.as_str()) {
            continue;
        }
        let kind = entry.file_type().ok()?;
        if kind.is_symlink() {
            let target = std::fs::read_link(&path).ok()?;
            out.push((
                relative(base, &path),
                hash_bytes(target.as_os_str().as_encoded_bytes()),
            ));
        } else if kind.is_dir() {
            collect(base, &path, out)?;
        } else if kind.is_file() {
            let bytes = std::fs::read(&path).ok()?;
            out.push((relative(base, &path), hash_bytes(&bytes)));
        }
    }
    Some(())
}

/// A path relative to the repository, as a stable string.
fn relative(base: &Path, path: &Path) -> String {
    path.strip_prefix(base)
        .unwrap_or(path)
        .to_string_lossy()
        .into_owned()
}

/// One content hash.
fn hash_bytes(bytes: &[u8]) -> Vec<u8> {
    Sha256::digest(bytes).to_vec()
}
