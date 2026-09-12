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
        std::fs::create_dir_all(repository.join(".git")).expect("a fake repository");
        std::fs::create_dir_all(repository.join("root")).expect("a root directory");
        std::fs::write(repository.join("modules.nix"), "original").expect("a module file");
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
fn a_vanished_executable_invalidates_it_even_though_the_inputs_match() {
    // The fingerprint says the inputs are unchanged; this says the outputs are
    // gone. A store path can be garbage-collected after the cache is written,
    // and a manifest naming a path that no longer exists is worse than none.
    let tree = Tree::new("collected");
    let adapter = existing_executable(&tree);
    write(&tree.root(), &manifest_json(&adapter));
    std::fs::remove_file(&adapter).expect("removing the adapter");
    assert!(read(&tree.root()).is_none());
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
