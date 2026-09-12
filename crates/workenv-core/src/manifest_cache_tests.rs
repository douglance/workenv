//! When a cached manifest is reused, and when it must not be.
use std::path::{Path, PathBuf};

use super::manifest_cache::{read, write};

/// A throwaway git working tree with a root directory inside it.
struct Tree {
    repository: PathBuf,
}

impl Tree {
    fn new(name: &str) -> Self {
        let repository =
            std::env::temp_dir().join(format!("workenv-mc-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&repository);
        std::fs::create_dir_all(repository.join("root")).expect("a root directory");
        std::fs::write(repository.join("modules.nix"), "original").expect("a module file");
        // A real repository, not a bare `.git` directory: the fingerprint asks
        // git rather than walking the tree, so a fake one answers nothing and
        // every test would pass for the wrong reason.
        for args in [
            vec!["init", "-q"],
            vec!["config", "user.email", "t@example.com"],
            vec!["config", "user.name", "t"],
            vec!["add", "-A"],
            vec!["commit", "-qm", "initial"],
        ] {
            let status = std::process::Command::new("git")
                .arg("-C")
                .arg(&repository)
                .args(&args)
                .output()
                .expect("running git");
            assert!(status.status.success(), "git {args:?} failed");
        }
        Self { repository }
    }

    fn root(&self) -> PathBuf {
        self.repository.join("root")
    }

    fn touch(&self, contents: &str) {
        std::fs::write(self.repository.join("modules.nix"), contents)
            .expect("rewriting the module");
    }
}

impl Drop for Tree {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.repository);
    }
}

/// A manifest naming one extension whose executable is `executable`.
fn manifest_json(executable: &Path) -> String {
    format!(
        r#"{{"schema_version":1,"hosts":{{}},"environments":{{}},"extensions":{{"e":{{
           "version":"0.2.0","protocol_version":1,
           "executable":"{}","location":"controller","operations":{{}}}}}}}}"#,
        executable.to_string_lossy()
    )
}

/// A real file OUTSIDE the hashed tree, so the existence check is what is
/// being tested.
///
/// An earlier version put this inside the fake repository, which made
/// `a_vanished_executable_invalidates_it...` pass for the wrong reason:
/// deleting the file also changed the fingerprint, so the test stayed green
/// with the existence check removed entirely. Found by removing it.
fn existing_executable(tree: &Tree) -> PathBuf {
    let path = std::env::temp_dir().join(format!(
        "workenv-mc-adapter-{}-{}",
        tree.repository
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default(),
        std::process::id()
    ));
    std::fs::write(&path, "x").expect("an adapter file");
    path
}

#[test]
fn an_unchanged_tree_reuses_the_manifest() {
    // The whole point: 68 s of devenv evaluation not paid again.
    let tree = Tree::new("unchanged");
    let adapter = existing_executable(&tree);
    write(&tree.root(), &manifest_json(&adapter));
    assert!(read(&tree.root()).is_some());
}

#[test]
fn editing_any_file_in_the_tree_invalidates_it() {
    // Not just .nix files. The manifest embeds /nix/store paths derived from the
    // Rust sources, so a .rs edit changes the manifest too; a fingerprint over
    // only the Nix files would serve a manifest naming a store path that the
    // edit has already replaced.
    let tree = Tree::new("edited");
    let adapter = existing_executable(&tree);
    write(&tree.root(), &manifest_json(&adapter));
    tree.touch("changed");
    assert!(read(&tree.root()).is_none());
}

#[test]
fn an_unbuilt_executable_does_not_invalidate_it() {
    // This test asserted the OPPOSITE until measurement showed the requirement
    // was wrong. `devenv eval` returns a derivation's output path without
    // building it, so a correct manifest routinely names store paths that do not
    // exist yet -- all nine of them, here -- and requiring them made the cache
    // miss every time. The missing-adapter case is handled downstream, where
    // adapter_invocation falls back to a devenv shell that builds it.
    //
    // Worth remembering: the old version was green AND its mutation was caught.
    // Both say the code matches the test; neither says the test is right.
    let tree = Tree::new("unbuilt");
    let absent = PathBuf::from("/nix/store/does-not-exist-workenv/bin/adapter");
    write(&tree.root(), &manifest_json(&absent));
    assert!(read(&tree.root()).is_some());
}

#[test]
fn a_root_outside_a_git_tree_is_never_cached() {
    // The import closure cannot be bounded there, so a .nix file outside the
    // hashed set could change without invalidating anything.
    let loose = std::env::temp_dir().join(format!("workenv-mc-loose-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&loose);
    std::fs::create_dir_all(&loose).expect("a loose directory");
    write(&loose, &manifest_json(Path::new("/bin/sh")));
    assert!(read(&loose).is_none());
    let _ = std::fs::remove_dir_all(&loose);
}

#[test]
fn a_manifest_spanning_lines_survives_the_round_trip() {
    // The entry format is "fingerprint newline manifest", so splitting on every
    // newline instead of the first would truncate a pretty-printed manifest.
    let tree = Tree::new("multiline");
    let adapter = existing_executable(&tree);
    let pretty = manifest_json(&adapter).replace(',', ",\n");
    write(&tree.root(), &pretty);
    assert!(read(&tree.root()).is_some());
}

#[test]
fn editing_one_file_twice_invalidates_it() {
    // The existing invalidation test writes, commits, then edits once, which flips
    // `git status` from empty to " M modules.nix" -- and the status string alone is
    // enough to change the fingerprint. So deleting the whole per-file
    // content-hashing loop left it green. The case the loop actually exists for is
    // the one named in the code comment at manifest_cache.rs: editing the same file
    // a second time leaves the status line identical while changing what the
    // manifest evaluates to.
    let tree = Tree::new("twice");
    let executable = existing_executable(&tree);
    tree.touch("first edit");
    write(&tree.root(), &manifest_json(&executable));
    assert!(
        read(&tree.root()).is_some(),
        "the manifest just written was not reused"
    );
    // Same " M modules.nix" status line, different contents.
    tree.touch("second edit");
    assert!(
        read(&tree.root()).is_none(),
        "a second edit to an already-dirty file served a stale manifest"
    );
}

#[test]
fn deleting_a_tracked_file_invalidates_it() {
    // Deletion invalidation is real and this covers it. What it does NOT cover is
    // the `status` bytes in the digest: measured, dropping them leaves this green,
    // because the loop below still hashes the changed *path name* and a deletion
    // puts the path into `git status`. Nor is that a hole -- the only thing the
    // status bytes add beyond the path set is the status code itself, and a file
    // moving between staged and unstaged does not change its contents, so it cannot
    // change the manifest. They are a redundant input, not a guard.
    let tree = Tree::new("deleted");
    let executable = existing_executable(&tree);
    write(&tree.root(), &manifest_json(&executable));
    assert!(read(&tree.root()).is_some(), "baseline was not reused");
    std::fs::remove_file(tree.repository.join("modules.nix")).expect("removing the module");
    assert!(
        read(&tree.root()).is_none(),
        "a deleted tracked file served a stale manifest"
    );
}
