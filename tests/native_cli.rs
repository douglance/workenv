use serde_json::Value;
use workenv::cli::build;

async fn invoke(args: Vec<String>) -> (Option<i32>, String) {
    let mut output = Vec::new();
    let code = build().serve_to(args, &mut output, false).await.unwrap();
    (code, String::from_utf8(output).unwrap())
}

#[tokio::test]
async fn manifest_exposes_the_short_lifecycle_commands() {
    let (_, output) = invoke(vec!["--llms-full".into(), "--format".into(), "json".into()]).await;
    let manifest: Value = serde_json::from_str(&output).unwrap();
    let text = manifest.to_string();
    for command in [
        "status", "up", "in", "out", "down", "claim", "run", "services", "collect", "release",
    ] {
        assert!(
            text.contains(&format!("\"{command}\"")),
            "missing {command}"
        );
    }
    assert!(text.contains("idempotency-key"), "{text}");
}

#[tokio::test]
async fn command_flags_after_delimiter_remain_task_arguments() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(
        root.path().join("fleet.json"),
        include_str!("../fleet.json"),
    )
    .unwrap();
    let (code, output) = invoke(vec![
        "--root".into(),
        root.path().to_string_lossy().into_owned(),
        "--json".into(),
        "run".into(),
        "untracked-task".into(),
        "--idempotency-key".into(),
        "cli-argv-test".into(),
        "--".into(),
        "printf".into(),
        "--help".into(),
    ])
    .await;
    assert_ne!(code, Some(0));
    assert!(
        output.contains("central task record is missing"),
        "{output}"
    );
    let files: Vec<_> = std::fs::read_dir(root.path().join(".state/operations"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
        .collect();
    let receipt: Value = serde_json::from_slice(&std::fs::read(&files[0]).unwrap()).unwrap();
    assert_eq!(
        receipt["request"]["input"]["argv"],
        serde_json::json!(["printf", "--help"])
    );
}
