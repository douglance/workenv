//! End-to-end policy fixtures for the xtask checker.

use std::{
    error::Error,
    fmt::Write as _,
    fs,
    path::{Path, PathBuf},
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};
use xtask::{CheckInput, check};

fn temp_root(name: &str) -> Result<PathBuf, Box<dyn Error>> {
    let suffix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos());
    let path = std::env::temp_dir().join(format!("workenv-policy-{name}-{suffix}"));
    fs::create_dir_all(&path)?;
    Ok(path)
}

fn write_manifest(root: &Path, members: &[&str]) -> Result<(), Box<dyn Error>> {
    let members = members
        .iter()
        .map(|member| format!("\"{member}\""))
        .collect::<Vec<_>>()
        .join(", ");
    fs::write(
        root.join("Cargo.toml"),
        format!(
            "[workspace]\nmembers = [{members}]\n\n[workspace.lints.rust]\nwarnings = \"deny\"\n"
        ),
    )?;
    Ok(())
}

fn write_crate(root: &Path, name: &str, extra: &str) -> Result<PathBuf, Box<dyn Error>> {
    let dir = root.join(name);
    fs::create_dir_all(dir.join("src"))?;
    fs::write(
        dir.join("Cargo.toml"),
        format!(
            "[package]\nname = \"{name}\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[lints]\nworkspace = true\n{extra}\n"
        ),
    )?;
    Ok(dir)
}

fn init_git(root: &Path) -> Result<(), Box<dyn Error>> {
    let status = Command::new("git")
        .arg("-C")
        .arg(root)
        .arg("init")
        .arg("-q")
        .status()?;
    assert!(status.success(), "git init failed with {status}");
    Ok(())
}

#[test]
fn rejects_oversized_closure() -> Result<(), Box<dyn Error>> {
    let root = temp_root("closure")?;
    write_manifest(&root, &["app"])?;
    let dir = write_crate(&root, "app", "")?;
    let mut source = String::from("pub fn run() {\n    let _work = || {\n");
    for index in 0..61 {
        writeln!(source, "        let _value_{index} = {index};")?;
    }
    source.push_str("    };\n}\n");
    let file = dir.join("src/lib.rs");
    fs::write(&file, source)?;
    let violations = check(CheckInput::new(&root).with_rust_files(vec![file]))?;
    assert!(
        violations
            .iter()
            .any(|violation| violation.to_string().contains("closure has"))
    );
    Ok(())
}

#[test]
fn rejects_untracked_workspace_rust_file() -> Result<(), Box<dyn Error>> {
    let root = temp_root("untracked")?;
    write_manifest(&root, &["app"])?;
    let dir = write_crate(&root, "app", "")?;
    let file = dir.join("src/lib.rs");
    let mut source = String::new();
    for _ in 0..301 {
        source.push_str("pub fn item() {}\n");
    }
    fs::write(&file, source)?;
    init_git(&root)?;
    let violations = check(CheckInput::new(&root))?;
    assert!(
        violations
            .iter()
            .any(|violation| violation.to_string().contains("physical lines"))
    );
    Ok(())
}

#[test]
fn rejects_dependency_cycle() -> Result<(), Box<dyn Error>> {
    let root = temp_root("cycle")?;
    write_manifest(&root, &["a", "b"])?;
    write_crate(&root, "a", "\n[dependencies]\nb = { path = \"../b\" }\n")?;
    write_crate(&root, "b", "\n[dependencies]\na = { path = \"../a\" }\n")?;
    fs::write(root.join("a/src/lib.rs"), "pub fn a() {}\n")?;
    fs::write(root.join("b/src/lib.rs"), "pub fn b() {}\n")?;
    let violations = check(
        CheckInput::new(&root)
            .with_rust_files(vec![root.join("a/src/lib.rs"), root.join("b/src/lib.rs")]),
    )?;
    assert!(
        violations
            .iter()
            .any(|violation| violation.to_string().contains("dependency cycle"))
    );
    Ok(())
}

#[test]
fn rejects_broad_allow() -> Result<(), Box<dyn Error>> {
    let root = temp_root("allow")?;
    write_manifest(&root, &["app"])?;
    let dir = write_crate(&root, "app", "")?;
    let file = dir.join("src/lib.rs");
    fs::write(&file, "#![allow(warnings)]\npub fn run() {}\n")?;
    let violations = check(CheckInput::new(&root).with_rust_files(vec![file]))?;
    assert!(
        violations
            .iter()
            .any(|violation| violation.to_string().contains("lint suppression"))
    );
    Ok(())
}

#[test]
fn core_provider_field_is_allowed_but_specific_vendor_is_rejected() -> Result<(), Box<dyn Error>> {
    let root = temp_root("provider-vendor")?;
    write_manifest(&root, &["workenv-core"])?;
    let dir = write_crate(&root, "workenv-core", "")?;
    let file = dir.join("src/lib.rs");
    fs::write(
        &file,
        "pub struct Host {\n    pub provider: Option<String>,\n}\n",
    )?;
    let violations = check(CheckInput::new(&root).with_rust_files(vec![file.clone()]))?;
    assert!(violations.is_empty(), "{violations:?}");

    fs::write(&file, "pub fn branch() {\n    let _name = \"OpenAI\";\n}\n")?;
    let violations = check(CheckInput::new(&root).with_rust_files(vec![file]))?;
    assert!(
        violations
            .iter()
            .any(|violation| violation.to_string().contains("openai"))
    );
    Ok(())
}

#[test]
fn external_example_source_is_checked_for_size() -> Result<(), Box<dyn Error>> {
    let root = temp_root("external")?;
    write_manifest(&root, &["app"])?;
    write_crate(&root, "app", "")?;
    let example = root.join("examples/external-extension");
    fs::create_dir_all(example.join("src"))?;
    fs::write(
        example.join("Cargo.toml"),
        "[package]\nname = \"external\"\nversion = \"1.0.0\"\nedition = \"2024\"\n",
    )?;
    let file = example.join("src/main.rs");
    let mut source = String::new();
    for _ in 0..301 {
        source.push_str("fn item() {}\n");
    }
    fs::write(&file, source)?;
    let violations = check(CheckInput::new(&root).with_rust_files(vec![file]))?;
    assert!(
        violations
            .iter()
            .any(|violation| violation.to_string().contains("physical lines"))
    );
    Ok(())
}

#[test]
fn rejects_zero_inspected_rust_files() -> Result<(), Box<dyn Error>> {
    let root = temp_root("zero")?;
    write_manifest(&root, &["app"])?;
    write_crate(&root, "app", "")?;
    let Err(error) = check(CheckInput::new(&root).with_rust_files(Vec::new())) else {
        panic!("empty inspection must fail");
    };
    assert!(error.to_string().contains("zero Rust files"));
    Ok(())
}
