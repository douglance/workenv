use serde_json::{json, Value};
use std::sync::{Arc, Mutex};
use workenv::cli::build;
use workenv::{profiles, CommandOutput, CommandSpec, Context, Runtime};

async fn invoke(root: &std::path::Path, args: &[&str]) -> (Option<i32>, String) {
    let mut argv = vec![
        "--root".into(),
        root.to_string_lossy().into_owned(),
        "--format".into(),
        "json".into(),
    ];
    argv.extend(args.iter().map(|arg| (*arg).to_owned()));
    let mut output = Vec::new();
    let exit = build().serve_to(argv, &mut output, false).await.unwrap();
    (exit, String::from_utf8(output).unwrap())
}

fn fixture() -> tempfile::TempDir {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(
        root.path().join("fleet.json"),
        include_str!("../fleet.json"),
    )
    .unwrap();
    root
}

#[tokio::test]
async fn create_profile_persists_only_identity_metadata_without_assigning_workers() {
    let root = fixture();
    let before = std::fs::read(root.path().join("fleet.json")).unwrap();
    let (exit, output) = invoke(
        root.path(),
        &[
            "profile",
            "create",
            "personal",
            "--github-login",
            "example-user",
            "--git-name",
            "Example User",
            "--git-email",
            "example@example.invalid",
            "--idempotency-key",
            "profile-create-example",
        ],
    )
    .await;
    assert_eq!(exit, Some(0), "{output}");
    let spec: Value =
        serde_json::from_slice(&std::fs::read(root.path().join("profiles/personal.json")).unwrap())
            .unwrap();
    assert_eq!(
        spec,
        json!({"schema_version":1,"name":"personal","github_login":"example-user","git_name":"Example User","git_email":"example@example.invalid"})
    );
    assert_eq!(
        std::fs::read(root.path().join("fleet.json")).unwrap(),
        before
    );
    assert!(!root.path().join("profiles/personal/.gh").exists());
}

#[tokio::test]
async fn profile_create_replays_without_overwriting_another_definition() {
    let root = fixture();
    let args = [
        "profile",
        "create",
        "personal",
        "--github-login",
        "first-user",
        "--idempotency-key",
        "create-first",
    ];
    let (exit, output) = invoke(root.path(), &args).await;
    assert_eq!(exit, Some(0), "{output}");
    let (exit, output) = invoke(root.path(), &args).await;
    assert_eq!(exit, Some(0), "{output}");
    assert!(output.contains("replayed"), "{output}");
    let (exit, _) = invoke(
        root.path(),
        &[
            "profile",
            "create",
            "personal",
            "--github-login",
            "other-user",
            "--idempotency-key",
            "create-other",
        ],
    )
    .await;
    assert_ne!(exit, Some(0));
    let value: Value =
        serde_json::from_slice(&std::fs::read(root.path().join("profiles/personal.json")).unwrap())
            .unwrap();
    assert_eq!(value["github_login"], "first-user");
}

#[tokio::test]
async fn profile_creation_rejects_path_traversal_and_missing_mutation_key() {
    let root = fixture();
    for name in ["../escape", "two/profiles", "UPPER", "with space"] {
        let (exit, output) = invoke(
            root.path(),
            &["profile", "create", name, "--idempotency-key", name],
        )
        .await;
        assert_ne!(exit, Some(0), "{output}");
    }
    assert!(!root.path().join("profiles").exists());
    let (exit, output) = invoke(root.path(), &["profile", "create", "personal"]).await;
    assert_ne!(exit, Some(0), "{output}");
    assert!(output.contains("idempotency"), "{output}");
}

#[tokio::test]
async fn profile_list_shows_worker_assignment_without_network_calls() {
    let root = fixture();
    std::fs::create_dir(root.path().join("profiles")).unwrap();
    std::fs::write(
        root.path().join("profiles/personal.json"),
        r#"{"schema_version":1,"name":"personal","github_login":"example-user"}"#,
    )
    .unwrap();
    let mut fleet: Value =
        serde_json::from_slice(&std::fs::read(root.path().join("fleet.json")).unwrap()).unwrap();
    fleet["workers"][1]["profile"] = json!("personal");
    std::fs::write(
        root.path().join("fleet.json"),
        serde_json::to_vec(&fleet).unwrap(),
    )
    .unwrap();
    let (exit, output) = invoke(root.path(), &["profile", "list"]).await;
    assert_eq!(exit, Some(0), "{output}");
    assert!(
        output.contains("personal") && output.contains("workenv-02"),
        "{output}"
    );
}

struct NoRuntime;
impl Runtime for NoRuntime {
    fn run(&self, _: CommandSpec) -> anyhow::Result<CommandOutput> {
        panic!("profile validation unexpectedly launched a command")
    }
    fn apoc(&self, _: &str, _: Value) -> anyhow::Result<Value> {
        panic!("profile validation unexpectedly mutated runtime state")
    }
}

#[derive(Default)]
struct SeqRuntime {
    outputs: Mutex<Vec<CommandOutput>>,
}

impl SeqRuntime {
    fn push(&self, stdout: Value) {
        self.outputs.lock().unwrap().push(CommandOutput {
            stdout: serde_json::to_vec(&stdout).unwrap(),
            stderr: Vec::new(),
            exit_code: Some(0),
            execution_id: "fake-exec".into(),
        });
    }
}

impl Runtime for SeqRuntime {
    fn run(&self, _: CommandSpec) -> anyhow::Result<CommandOutput> {
        Ok(self.outputs.lock().unwrap().remove(0))
    }
    fn apoc(&self, _: &str, _: Value) -> anyhow::Result<Value> {
        panic!("profile already-assigned shortcut should reach open-task guard before mutation")
    }
}

fn context(root: &std::path::Path) -> Context {
    let mut ctx = Context::load(Some(root.to_str().unwrap())).unwrap();
    ctx.runtime = std::sync::Arc::new(NoRuntime);
    ctx
}

#[test]
fn active_task_prevents_assignment_without_touching_worker_or_fleet() {
    let root = fixture();
    let ctx = context(root.path());
    profiles::create(&ctx, "personal", json!({"github_login":"example-user"})).unwrap();
    std::fs::create_dir_all(ctx.state.join("tasks")).unwrap();
    std::fs::write(
        ctx.state.join("tasks/current.json"),
        r#"{"task_id":"current","worker":"workenv-01","status":"claimed"}"#,
    )
    .unwrap();
    let before = std::fs::read(root.path().join("fleet.json")).unwrap();
    let result = profiles::assign(&ctx, "1", "personal", "busy-worker-test").unwrap();
    assert_eq!(result["ok"], false, "{result}");
    assert_eq!(result["status"], "worker_has_open_task", "{result}");
    assert_eq!(
        std::fs::read(root.path().join("fleet.json")).unwrap(),
        before
    );
}

#[test]
fn same_profile_assignment_does_not_shortcut_when_herdr_runtime_profile_mismatches() {
    let root = fixture();
    let runtime = Arc::new(SeqRuntime::default());
    let mut ctx = Context::load(Some(root.path().to_str().unwrap())).unwrap();
    ctx.runtime = runtime.clone();
    profiles::create(&ctx, "personal", json!({"github_login":"example-user"})).unwrap();
    ctx.fleet["workers"][0]["profile"] = json!("personal");
    std::fs::write(
        root.path().join("fleet.json"),
        serde_json::to_vec_pretty(&ctx.fleet).unwrap(),
    )
    .unwrap();
    std::fs::create_dir_all(ctx.state.join("tasks")).unwrap();
    std::fs::write(
        ctx.state.join("tasks/current.json"),
        r#"{"task_id":"current","worker":"workenv-01","status":"claimed"}"#,
    )
    .unwrap();
    runtime.push(json!({"ok":true,"status":"profile_ready","prepared":true}));
    runtime.push(json!({
        "running": true,
        "compatible": true,
        "version": "0.9.0",
        "protocol_version": 22,
        "server_binary_stale": false,
        "capabilities": {"detached_server_daemon": true}
    }));
    runtime.push(json!({"status":"completed","result":{"executions": [{"id":"herdr-exec", "status":"running"}]}}));
    runtime.push(json!({"data": {"id":"herdr-exec", "status":"running", "outcome":"pending", "spec": {"labels": {
        "workenv.component":"herdr-server",
        "herdr.session":"workenv",
        "workenv.profile":"other",
        "workenv.profile.digest":"b"
    }}}}));

    let result = profiles::assign(&ctx, "1", "personal", "same-profile-test").unwrap();

    assert_eq!(result["status"], "worker_has_open_task", "{result}");
    assert_eq!(result["ok"], false);
}

#[test]
fn task_binding_rejects_profile_definition_drift_and_legacy_tasks_on_profiled_workers() {
    let root = fixture();
    let mut ctx = context(root.path());
    profiles::create(&ctx, "personal", json!({"github_login":"first-user"})).unwrap();
    ctx.fleet["workers"][0]["profile"] = json!("personal");
    let record =
        json!({"worker":"workenv-01","worker_profile":profiles::binding(&ctx,"1").unwrap()});
    profiles::validate_task_binding(&ctx, &record).unwrap();
    assert!(profiles::validate_task_binding(&ctx, &json!({"worker":"workenv-01"})).is_err());
    std::fs::write(
        root.path().join("profiles/personal.json"),
        r#"{"schema_version":1,"name":"personal","github_login":"different-user"}"#,
    )
    .unwrap();
    assert!(profiles::validate_task_binding(&ctx, &record).is_err());
}

#[test]
fn profile_wrapper_preserves_argument_boundaries_and_requires_exact_digest() {
    let root = fixture();
    let mut ctx = context(root.path());
    profiles::create(&ctx, "personal", json!({})).unwrap();
    let argv = vec!["printf".into(), "%s".into(), "argument with spaces".into()];
    assert_eq!(profiles::wrap(&ctx, "1", argv.clone(), true).unwrap(), argv);
    ctx.fleet["workers"][0]["profile"] = json!("personal");
    let wrapped = profiles::wrap(&ctx, "1", argv.clone(), true).unwrap();
    assert_eq!(&wrapped[wrapped.len() - 3..], &argv);
    assert!(wrapped.contains(&"--check-github".into()));
    assert!(wrapped.contains(&profiles::resolve(&ctx, "1").unwrap().unwrap().digest));
}

#[test]
fn native_profile_wrapper_uses_environment_helper_and_worker_root_directly() {
    let root = fixture();
    let mut ctx = context(root.path());
    profiles::create(&ctx, "personal", json!({})).unwrap();
    ctx.fleet["hosts"] = json!({
        "local": {
            "transport": "local",
            "root": "/tmp/workenv",
            "tools": "native"
        }
    });
    ctx.fleet["workers"][0]["name"] = json!("local-a");
    ctx.fleet["workers"][0]["host"] = json!("local");
    ctx.fleet["workers"][0]["profile"] = json!("personal");

    let wrapped =
        profiles::wrap(&ctx, "local-a", vec!["cargo".into(), "test".into()], true).unwrap();

    assert_eq!(wrapped[0], "python3");
    assert_eq!(wrapped[1], "/tmp/workenv/remote/profile.py");
    assert!(wrapped
        .windows(2)
        .any(|pair| pair == ["--root", "/tmp/workenv/workers/local-a"]));
    assert!(wrapped.contains(&"--check-github".into()));
    assert!(!wrapped.contains(&"/usr/local/bin/devenv".into()));
}

#[test]
fn credential_fields_and_symlinked_definitions_are_rejected() {
    let root = fixture();
    let ctx = context(root.path());
    let error = profiles::create(
        &ctx,
        "personal",
        json!({"github_token":"secret-not-for-output"}),
    )
    .unwrap_err();
    assert!(!format!("{error:#}").contains("secret-not-for-output"));
    assert!(!root.path().join("profiles").exists());
    #[cfg(unix)]
    {
        std::fs::create_dir(root.path().join("profiles")).unwrap();
        std::os::unix::fs::symlink(
            root.path().join("fleet.json"),
            root.path().join("profiles/personal.json"),
        )
        .unwrap();
        assert!(profiles::definition(&ctx, "personal").is_err());
    }
}

#[tokio::test]
async fn profile_mcp_catalog_exposes_the_same_commands_and_enforces_mutation_keys() {
    use incurs::tool::ToolCallOptions;
    use std::collections::BTreeMap;
    let root = fixture();
    let catalog = build().tool_catalog();
    for (name, read_only) in [
        ("profile_create", false),
        ("profile_assign", false),
        ("profile_login", false),
        ("profile_list", true),
        ("profile_status", true),
    ] {
        let command = catalog
            .get(name)
            .unwrap_or_else(|| panic!("missing {name}"));
        assert_eq!(
            command.annotations.as_ref().unwrap().read_only_hint,
            Some(read_only)
        );
    }
    let options = || ToolCallOptions {
        globals: Some(json!({"root":root.path().to_string_lossy()})),
        ..ToolCallOptions::isolated()
    };
    let result = catalog
        .call(
            "profile_create",
            BTreeMap::from([("name".into(), json!("personal"))]),
            options(),
        )
        .await;
    assert!(serde_json::to_string(&result)
        .unwrap()
        .contains("idempotency_key is required"));
    assert!(!root.path().join("profiles").exists());
    let schema = &catalog.get("profile_create").unwrap().input_schema["properties"];
    let key = schema
        .as_object()
        .unwrap()
        .keys()
        .find(|name| name.replace('-', "_") == "idempotency_key")
        .unwrap()
        .clone();
    let result = catalog
        .call(
            "profile_create",
            BTreeMap::from([
                ("name".into(), json!("personal")),
                (key, json!("mcp-profile-create")),
            ]),
            options(),
        )
        .await;
    assert!(
        root.path().join("profiles/personal.json").is_file(),
        "{}",
        serde_json::to_string(&result).unwrap()
    );
}
