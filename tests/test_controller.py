import base64
import io
import json
import os
import subprocess
import tarfile
import tempfile
import textwrap
import uuid
from pathlib import Path

import pytest

from scripts import workenv_controller as controller
from scripts import workenv_recipes


ROOT = Path(__file__).resolve().parents[1]
FLEET = ROOT / "fleet.json"
RECIPES = ROOT / "recipes" / "workenv.json"
VALID_DIGEST = "a" * 64


def write_collection(local_dir: Path, metadata_without_digest: dict, files: dict[str, bytes]) -> str:
    local_dir.mkdir(parents=True, exist_ok=True)
    archive_paths = []
    for name, content in files.items():
        path = local_dir / name
        path.write_bytes(content)
        archive_paths.append(path)
    digest = controller.collection_digest(metadata_without_digest, archive_paths)
    metadata = dict(metadata_without_digest)
    metadata["collection_digest"] = digest
    (local_dir / "metadata.json").write_text(controller.canonical_json(metadata), encoding="utf-8")
    return digest


def test_plan_claim_uses_project_class_order():
    fleet = controller.load_fleet(FLEET)

    result = controller.plan_claim(fleet, "apoc")

    assert result["status"] == "planned"
    assert [worker["name"] for worker in result["workers"]] == ["workenv-05", "workenv-06"]
    assert result["project"]["repository"] == "operator/apoc"


def test_plan_claim_excludes_workers_with_open_local_task_records(tmp_path):
    fleet = controller.load_fleet(FLEET)
    controller.record_task(
        tmp_path,
        task_id="task-a",
        worker="workenv-05",
        project="apoc",
        repo={"owner": "operator", "name": "apoc"},
        revision="b" * 40,
        reservation_id="reservation-a",
        session_id="session-a",
        source="remote",
        status="pending",
        worktree="/remote/worktree",
    )

    result = controller.plan_claim(fleet, "apoc", local_state=tmp_path)
    selected = controller.plan_claim(fleet, "apoc", "workenv-05", local_state=tmp_path)

    assert [worker["name"] for worker in result["workers"]] == ["workenv-06"]
    assert result["blocked"][0]["worker"] == "workenv-05"
    assert selected["status"] == "worker_has_open_task"


def test_remote_workspace_command_scopes_worker_and_task_identity():
    fleet = controller.load_fleet(FLEET)
    request = {
        "operation": "claim",
        "request_id": "claim-1",
        "task_id": "ENG-123",
        "repo": {"owner": "operator", "name": "apoc"},
        "remote_url": "https://github.com/douglance/apoc.git",
        "revision": "a" * 40,
        "branch": "workenv/ENG-123",
    }

    argv = controller.workspace_ssh_argv(fleet, "workenv-05", request, connect_timeout=7)

    assert argv[:6] == [
        "ssh",
        "-o",
        "BatchMode=yes",
        "-o",
        "ConnectTimeout=7",
        "exedev@workenv-05.example.ts.net",
    ]
    remote_command = argv[-1]
    assert "remote/workspace.py" in remote_command
    encoded = remote_command.rsplit(" ", 1)[-1]
    decoded = json.loads(base64.b64decode(encoded).decode("utf-8"))
    assert decoded == request


def test_remote_json_reports_auth_failure_without_success(monkeypatch):
    def fail_auth(*args, **kwargs):
        return subprocess.CompletedProcess(
            args=args[0],
            returncode=255,
            stdout="",
            stderr="Permission denied (publickey).\n",
        )

    monkeypatch.setattr(controller.subprocess, "run", fail_auth)
    fleet = controller.load_fleet(FLEET)

    result = controller.run_workspace_request(
        fleet,
        "workenv-01",
        {"operation": "status", "request_id": "status-1"},
        timeout=1,
    )

    assert result["status"] == "auth_failed"
    assert result["ok"] is False
    assert "Permission denied" in result["error"]


def test_timeout_result_is_json_serializable_when_text_mode_returns_bytes(monkeypatch):
    def timeout(*args, **kwargs):
        raise subprocess.TimeoutExpired(args[0], timeout=1, output=b"partial out", stderr=b"partial err")

    monkeypatch.setattr(controller.subprocess, "run", timeout)

    result = controller.run(["ssh", "example"], timeout=1, text=True)

    assert result["status"] == "pending"
    assert result["stdout"] == "partial out"
    assert result["stderr"] == "partial err"
    json.dumps(result)


def test_collect_retrieves_and_verifies_remote_collection(monkeypatch, tmp_path):
    metadata_without_digest = {
        "task_id": "task-a",
        "repo": {"owner": "operator", "name": "apoc"},
        "source": "remote",
        "archives": {"tracked_diff": "tracked.diff"},
    }
    archive_file = tmp_path / "tracked.diff"
    archive_file.write_bytes(b"diff --git a/README.md b/README.md\n")
    digest = controller.collection_digest(metadata_without_digest, [archive_file])
    remote_collection = {
        "status": "collected",
        "ok": True,
        "task_id": "task-a",
        "collection_digest": digest,
        "metadata": {**metadata_without_digest, "collection_digest": digest},
        "metadata_path": "/home/exedev/workenv/collections/task-a/c1/metadata.json",
        "archives": {"tracked_diff": "/home/exedev/workenv/collections/task-a/c1/tracked.diff"},
    }
    tar_bytes = io.BytesIO()
    with tarfile.open(fileobj=tar_bytes, mode="w") as archive:
        metadata = controller.canonical_json(remote_collection["metadata"]).encode("utf-8")
        info = tarfile.TarInfo("metadata.json")
        info.size = len(metadata)
        archive.addfile(info, io.BytesIO(metadata))
        diff = archive_file.read_bytes()
        info = tarfile.TarInfo("tracked.diff")
        info.size = len(diff)
        archive.addfile(info, io.BytesIO(diff))

    calls = []

    def fake_run(args, **kwargs):
        calls.append(args)
        if "remote/workspace.py" in args[-1]:
            return subprocess.CompletedProcess(args=args, returncode=0, stdout=json.dumps(remote_collection), stderr="")
        return subprocess.CompletedProcess(args=args, returncode=0, stdout=tar_bytes.getvalue(), stderr=b"")

    monkeypatch.setattr(controller.subprocess, "run", fake_run)
    fleet = controller.load_fleet(FLEET)

    result = controller.collect(
        fleet,
        "workenv-01",
        "task-a",
        "collect-1",
        local_state=tmp_path / ".state",
    )

    assert result["status"] == "collected"
    assert result["collection_digest"] == digest
    assert Path(result["local_collection_dir"], "metadata.json").exists()
    assert len(calls) == 2


def test_verify_local_collection_rejects_tampered_archive_bytes(tmp_path):
    metadata = {
        "task_id": "task-a",
        "repo": {"owner": "operator", "name": "apoc"},
        "source": "remote",
        "archives": {"tracked_diff": "tracked.diff"},
    }
    digest = write_collection(tmp_path, metadata, {"tracked.diff": b"original"})
    (tmp_path / "tracked.diff").write_bytes(b"tampered")

    result = controller.verify_local_collection(
        tmp_path,
        digest,
        task_id="task-a",
        repo={"owner": "operator", "name": "apoc"},
        source="remote",
    )

    assert result["status"] == "uncollected"
    assert "digest" in result["error"]


def test_release_requires_central_task_record_before_collection(tmp_path):
    fleet = controller.load_fleet(FLEET)

    result = controller.release(
        fleet,
        "workenv-01",
        "task-a",
        "release-1",
        VALID_DIGEST,
        local_state=tmp_path / ".state",
    )

    assert result["status"] == "untracked_task"
    assert result["ok"] is False


def test_release_refuses_live_recorded_task_activity(monkeypatch, tmp_path):
    fleet = controller.load_fleet(FLEET)
    state = tmp_path / ".state"
    monkeypatch.setattr(controller, "verify_local_collection", lambda *args, **kwargs: controller.ok("verified"))
    controller.record_task(
        state,
        task_id="task-a",
        worker="workenv-01",
        project="apoc",
        repo={"owner": "operator", "name": "apoc"},
        revision="b" * 40,
        reservation_id="reservation-a",
        session_id="session-a",
        source="remote",
        status="claimed",
        worktree="/remote/worktree",
    )
    monkeypatch.setattr(
        controller,
        "inspect_task_activity",
        lambda *args, **kwargs: controller.failed("live_task_activity", "task still has live remote APoC executions or Herdr panes"),
    )

    result = controller.release(fleet, "workenv-01", "task-a", "release-1", VALID_DIGEST, local_state=state)

    assert result["status"] == "live_task_activity"


def test_release_updates_task_record_after_verified_release(monkeypatch, tmp_path):
    fleet = controller.load_fleet(FLEET)
    state = tmp_path / ".state"
    repo = {"owner": "operator", "name": "apoc"}
    controller.record_task(
        state,
        task_id="task-a",
        worker="workenv-01",
        project="apoc",
        repo=repo,
        revision="b" * 40,
        reservation_id="reservation-a",
        session_id="session-a",
        source="remote",
        status="claimed",
        worktree="/remote/worktree",
    )
    local_dir = state / "collections" / "workenv-01" / "task-a"
    metadata = {"task_id": "task-a", "repo": repo, "source": "remote", "base": "b" * 40, "archives": {"tracked_diff": "tracked.diff"}}
    digest = write_collection(local_dir / "pending", metadata, {"tracked.diff": b"diff"})
    (local_dir / "pending").replace(local_dir / digest)
    monkeypatch.setattr(controller, "inspect_task_activity", lambda *args, **kwargs: controller.ok("clear"))

    def fake_run_workspace_request(*args, **kwargs):
        return {"status": "released", "ok": True, "task_id": "task-a", "collection_digest": digest}

    monkeypatch.setattr(controller, "run_workspace_request", fake_run_workspace_request)

    result = controller.release(fleet, "workenv-01", "task-a", "release-1", digest, local_state=state)
    record = controller.read_task_record(state, "task-a")

    assert result["status"] == "released"
    assert record["status"] == "released"


def test_claim_rejects_worker_class_mismatch_before_remote(monkeypatch, tmp_path):
    calls = []

    def fake_run(args, **kwargs):
        calls.append(args)
        raise AssertionError("claim should not start SSH for a mismatched worker class")

    monkeypatch.setattr(controller.subprocess, "run", fake_run)
    fleet = controller.load_fleet(FLEET)

    with pytest.raises(controller.ControllerError) as excinfo:
        controller.claim(fleet, "workenv-01", "apoc", "task-a", "b" * 40, "claim-1", local_state=tmp_path)

    assert excinfo.value.status == "conflict"
    assert calls == []


def test_claim_refuses_open_local_task_record_before_remote(monkeypatch, tmp_path):
    calls = []
    monkeypatch.setattr(controller.subprocess, "run", lambda *args, **kwargs: calls.append(args))
    fleet = controller.load_fleet(FLEET)
    controller.record_task(
        tmp_path,
        task_id="task-existing",
        worker="workenv-05",
        project="apoc",
        repo={"owner": "operator", "name": "apoc"},
        revision="b" * 40,
        reservation_id="reservation-a",
        session_id="session-a",
        source="remote",
        status="auth_failed",
        worktree=None,
    )

    result = controller.claim(fleet, "workenv-05", "apoc", "task-new", "b" * 40, "claim-1", local_state=tmp_path)

    assert result["status"] == "worker_has_open_task"
    assert calls == []


def test_ensure_refuses_mismatched_registration_target_before_ssh(monkeypatch):
    calls = []

    def fake_run(args, **kwargs):
        calls.append(args)
        raise AssertionError("ensure should not start SSH for a mismatched registration target")

    monkeypatch.setattr(controller.subprocess, "run", fake_run)
    monkeypatch.setattr(controller, "ensure_provider_worker", lambda *args, **kwargs: {"status": "present", "ok": True, "worker": args[1], "vm": {}})
    fleet = controller.load_fleet(FLEET)

    result = controller.ensure(
        fleet,
        "workenv-01",
        "ensure-1",
        register_herdr=True,
        registration_target="exedev@recovery.example.ts.net",
    )

    assert result["status"] == "registration_target_mismatch"
    assert result["ok"] is False
    assert calls == []


def test_ensure_reconcile_requires_idle_remote_worker_before_bootstrap(monkeypatch, tmp_path):
    calls = []
    monkeypatch.setattr(controller, "tailscale_status", lambda *args: controller.ok("tailscale_ready"))
    monkeypatch.setattr(controller, "sync_worker_sources", lambda *args, **kwargs: controller.ok("synced"))

    def fake_run(args, **kwargs):
        calls.append(args)
        if "tailscale status --json" in args[-1]:
            return subprocess.CompletedProcess(args=args, returncode=0, stdout=json.dumps({"BackendState": "Running"}), stderr="")
        if "remote/workspace.py" in args[-1]:
            return subprocess.CompletedProcess(args=args, returncode=0, stdout=json.dumps({"status": "claimed", "ok": False}), stderr="")
        raise AssertionError("bootstrap should not run while the remote worker has an active task")

    monkeypatch.setattr(controller.subprocess, "run", fake_run)
    monkeypatch.setattr(controller, "ensure_provider_worker", lambda *args, **kwargs: {"status": "present", "ok": True, "worker": args[1], "vm": {}})
    fleet = controller.load_fleet(FLEET)

    result = controller.ensure(fleet, "workenv-01", "ensure-1", reconcile=True, local_state=tmp_path)

    assert result["status"] == "worker_busy"
    assert not any("bootstrap.sh" in call[-1] for call in calls)


def test_ensure_returns_ready_only_when_health_tailscale_and_registration_pass(monkeypatch, tmp_path):
    calls = []
    monkeypatch.setattr(controller, "tailscale_status", lambda *args: controller.ok("tailscale_ready"))
    monkeypatch.setattr(controller, "sync_worker_sources", lambda *args, **kwargs: controller.ok("synced"))

    def fake_run(args, **kwargs):
        calls.append(args)
        if "remote/health.py" in args[-1]:
            return subprocess.CompletedProcess(args=args, returncode=0, stdout=json.dumps({"schema": 1, "ready": True, "ok": True}), stderr="")
        if "remote/tool_health.py" in args[-1]:
            return subprocess.CompletedProcess(args=args, returncode=0, stdout=json.dumps({"schema": 1, "tools_ready": True, "nib_auth": {"authenticated": True}}), stderr="")
        if "workenv-herdr-bootstrap" in args[-1]:
            return subprocess.CompletedProcess(args=args, returncode=0, stdout="workenv Herdr server is already healthy\n", stderr="")
        if "herdr --session workenv status server --json" in args[-1]:
            return subprocess.CompletedProcess(args=args, returncode=0, stdout=json.dumps({"running": True, "compatible": True, "version": "0.9.0", "protocol_version": 22, "server_binary_stale": False, "capabilities": {"detached_server_daemon": True}}), stderr="")
        if "remote/workspace.py" in args[-1]:
            return subprocess.CompletedProcess(args=args, returncode=0, stdout=json.dumps({"status": "available", "ok": True}), stderr="")
        if "bootstrap.sh" in args[-1] and "--health-only" in args[-1]:
            return subprocess.CompletedProcess(args=args, returncode=0, stdout=json.dumps({"missing_tools": []}), stderr="")
        if args[:3] == ["herdr", "machine", "list"] and args[3:4] == ["--json"]:
            return subprocess.CompletedProcess(args=args, returncode=0, stdout=json.dumps([]), stderr="")
        if args[:3] == ["herdr", "machine", "add"] and args[-2:] == ["--remote-session", "workenv"]:
            return subprocess.CompletedProcess(args=args, returncode=0, stdout="added\n", stderr="")
        if "tailscale status --json" in args[-1]:
            return subprocess.CompletedProcess(args=args, returncode=0, stdout=json.dumps({"BackendState": "Running"}), stderr="")
        if "bootstrap.sh" in args[-1]:
            return subprocess.CompletedProcess(args=args, returncode=0, stdout="installed\n", stderr="")
        raise AssertionError(args)

    monkeypatch.setattr(controller.subprocess, "run", fake_run)
    monkeypatch.setattr(controller, "ensure_provider_worker", lambda *args, **kwargs: {"status": "present", "ok": True, "worker": args[1], "vm": {}})
    fleet = controller.load_fleet(FLEET)

    result = controller.ensure(fleet, "workenv-01", "ensure-1", reconcile=True, register_herdr=True, local_state=tmp_path)

    assert result["status"] == "ready"
    assert result["ok"] is True
    assert any(args[:3] == ["herdr", "machine", "list"] and args[3:4] == ["--json"] for args in calls)
    assert any(args[:3] == ["herdr", "machine", "add"] and args[-2:] == ["--remote-session", "workenv"] for args in calls)



def test_ensure_reports_provider_missing_before_ssh(monkeypatch, tmp_path):
    calls = []
    monkeypatch.setattr(controller.subprocess, "run", lambda *args, **kwargs: calls.append(args))
    monkeypatch.setattr(controller, "ensure_provider_worker", lambda *args, **kwargs: {"status": "missing", "ok": False, "worker": args[1], "capacity_available": True})
    fleet = controller.load_fleet(FLEET)

    result = controller.ensure(fleet, "workenv-01", "ensure-provider", local_state=tmp_path)

    assert result["status"] == "missing"
    assert calls == []


def test_ensure_passes_explicit_provider_create(monkeypatch, tmp_path):
    seen = []
    monkeypatch.setattr(controller.subprocess, "run", lambda *args, **kwargs: subprocess.CompletedProcess(args=args[0], returncode=1, stdout="", stderr="not ready"))

    def fake_provider(fleet, worker, request_id, *, local_state, create):
        seen.append(create)
        return {"status": "present", "ok": True, "worker": worker, "vm": {}}

    monkeypatch.setattr(controller, "ensure_provider_worker", fake_provider)
    fleet = controller.load_fleet(FLEET)

    result = controller.ensure(fleet, "workenv-01", "ensure-provider", local_state=tmp_path, provider_create=True)

    assert seen == [True]
    assert result["status"] == "needs_reconcile"


def test_inspect_remote_apoc_activity_treats_pending_outcome_as_live(monkeypatch):
    fleet = controller.load_fleet(FLEET)

    def fake_run(args, **kwargs):
        if "execution get" in args[-1]:
            return subprocess.CompletedProcess(args=args, returncode=0, stdout=json.dumps({"id": "exec-1", "outcome": "pending"}), stderr="")
        if "execution list" in args[-1]:
            return subprocess.CompletedProcess(args=args, returncode=0, stdout=json.dumps({"executions": [], "next_cursor": None}), stderr="")
        raise AssertionError(args)

    monkeypatch.setattr(controller.subprocess, "run", fake_run)

    result = controller.inspect_remote_apoc_activity(
        fleet,
        "workenv-01",
        "task-a",
        {"remote_execution_ids": ["exec-1"], "worktree": "/home/exedev/workenv/worktrees/task-a"},
    )

    assert result["status"] == "apoc_clear"
    assert result["live_executions"][0]["id"] == "exec-1"


def test_inspect_remote_apoc_activity_rejects_truncated_execution_list(monkeypatch):
    fleet = controller.load_fleet(FLEET)

    def fake_run(args, **kwargs):
        if "execution get" in args[-1]:
            return subprocess.CompletedProcess(args=args, returncode=0, stdout=json.dumps({"id": "exec-1", "outcome": "passed"}), stderr="")
        if "execution list" in args[-1]:
            return subprocess.CompletedProcess(args=args, returncode=0, stdout=json.dumps({"executions": [], "next_cursor": "cursor"}), stderr="")
        raise AssertionError(args)

    monkeypatch.setattr(controller.subprocess, "run", fake_run)

    result = controller.inspect_remote_apoc_activity(
        fleet,
        "workenv-01",
        "task-a",
        {"remote_execution_ids": ["exec-1"], "worktree": "/home/exedev/workenv/worktrees/task-a"},
    )

    assert result["status"] == "task_activity_unknown"
    assert "truncated" in result["error"]


def test_inspect_remote_herdr_activity_uses_exact_recorded_pane_ids(monkeypatch):
    fleet = controller.load_fleet(FLEET)
    calls = []

    def fake_run(args, **kwargs):
        calls.append(args)
        return subprocess.CompletedProcess(args=args, returncode=0, stdout=json.dumps({"result": {"id": "pane-1"}}), stderr="")

    monkeypatch.setattr(controller.subprocess, "run", fake_run)

    result = controller.inspect_remote_herdr_activity(
        fleet,
        "workenv-01",
        "task-a",
        {"runtime": {"herdr": {"pane_ids": ["pane-1"]}}},
    )

    assert result["live_panes"][0]["pane_id"] == "pane-1"
    assert "--session workenv pane get pane-1" in calls[0][-1]
    assert "pane list" not in calls[0][-1]


def test_record_runtime_updates_local_task_record(monkeypatch, tmp_path):
    fleet = controller.load_fleet(FLEET)
    state = tmp_path / ".state"
    controller.record_task(
        state,
        task_id="task-a",
        worker="workenv-01",
        project="apoc",
        repo={"owner": "operator", "name": "apoc"},
        revision="b" * 40,
        reservation_id="reservation-a",
        session_id="session-a",
        source="remote",
        status="claimed",
        worktree="/remote/worktree",
    )
    monkeypatch.setattr(
        controller,
        "run_workspace_request",
        lambda *args, **kwargs: {"status": "runtime_recorded", "ok": True, "runtime": {"herdr": {"pane_ids": ["pane-1"]}, "apoc_execution_ids": ["exec-1"]}},
    )

    result = controller.record_runtime(
        fleet,
        "workenv-01",
        "task-a",
        "b" * 40,
        "runtime-1",
        {"herdr": {"pane_id": "pane-1"}, "apoc_execution_ids": ["exec-1"]},
        local_state=state,
    )

    record = controller.read_task_record(state, "task-a")
    assert result["status"] == "runtime_recorded"
    assert record["runtime"]["herdr"]["pane_ids"] == ["pane-1"]
    assert record["runtime"]["apoc_execution_ids"] == ["exec-1"]


def test_recipe_bundle_is_current_and_importable(tmp_path):
    bundle = json.loads(RECIPES.read_text(encoding="utf-8"))

    assert bundle == workenv_recipes.bundle()
    assert bundle["namespace"] == "workenv"
    assert [recipe["name"] for recipe in bundle["recipes"]] == [
        "workenv.ensure",
        "workenv.claim",
        "workenv.record-runtime",
        "workenv.status",
        "workenv.collect",
        "workenv.release",
    ]

    apoc_home = Path(tempfile.mkdtemp(prefix="apoc-home-", dir="/tmp"))
    apoc_runtime = Path(tempfile.mkdtemp(prefix="apoc-run-", dir="/tmp"))
    env = os.environ.copy()
    env["APOC_HOME"] = str(apoc_home)
    env["APOC_RUNTIME_DIR"] = str(apoc_runtime)
    try:
        completed = subprocess.run(
            [
                "apoc",
                "recipe",
                "import",
                str(RECIPES),
                "--cwd",
                str(ROOT),
                "--idempotency-key",
                f"test-workenv-recipe-import-{uuid.uuid4()}",
                "--purpose",
                "Validate the workenv recipe bundle in isolated state.",
                "--format",
                "json",
            ],
            check=True,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            env=env,
        )
    finally:
        subprocess.run(
            [
                "apoc",
                "daemon",
                "stop",
                "--idempotency-key",
                f"test-workenv-recipe-import-stop-{uuid.uuid4()}",
                "--purpose",
                "Stop the isolated APoC daemon used by the recipe import test.",
                "--format",
                "json",
            ],
            check=False,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            env=env,
        )

    assert json.loads(completed.stdout)["recipes"] == 6


def test_live_apoc_reservation_uses_session_lease_and_is_released():
    run_id = uuid.uuid4().hex
    purpose = "Validate APoC session reservation ownership for workenv recipes."
    completed = subprocess.run(
        [
            "apoc",
            "code",
            "run",
            textwrap.dedent(
                f'''
                const purpose = {json.dumps(purpose)};
                const session = await apoc.session_open({{actor:"workenv-test", ttl_ms:60000, idempotency_key:"workenv-test-session-{run_id}", purpose}});
                const reservation = await apoc.reservation_acquire({{kind:"custom", key:"workenv/test/{run_id}", lease:`session/${{session.id}}`, ttl_ms:60000, idempotency_key:"workenv-test-reservation-{run_id}", purpose}});
                const readReservation = await apoc.reservation_get({{id: reservation.id, purpose}});
                const readSession = await apoc.session_get({{id: session.id, purpose}});
                const released = await apoc.reservation_release({{id: reservation.id, idempotency_key:"workenv-test-reservation-release-{run_id}", purpose}});
                const closed = await apoc.session_close({{id: session.id, timeout_ms: 2000, idempotency_key:"workenv-test-session-close-{run_id}", purpose}});
                return {{session, reservation: readReservation, readSession, released, closed}};
                '''
            ),
            "--idempotency-key",
            f"workenv-test-reservation-outer-{run_id}",
            "--purpose",
            purpose,
            "--format",
            "json",
        ],
        check=True,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )
    result = json.loads(completed.stdout)["result"]

    assert result["reservation"]["lease_id"] == f"session/{result['session']['id']}"
    assert result["reservation"]["expires_at"] >= result["reservation"]["created_at"]
    assert result["readSession"]["expires_at"] >= result["readSession"]["created_at"]


def test_claim_recipe_passes_reservation_and_retains_ambiguous_status(tmp_path):
    apoc_home = Path(tempfile.mkdtemp(prefix="apoc-home-", dir="/tmp"))
    apoc_runtime = Path(tempfile.mkdtemp(prefix="apoc-run-", dir="/tmp"))
    env = os.environ.copy()
    env["APOC_HOME"] = str(apoc_home)
    env["APOC_RUNTIME_DIR"] = str(apoc_runtime)
    calls_path = tmp_path / "calls.jsonl"
    fake_controller = tmp_path / "fake_controller.py"
    fake_fleet = tmp_path / "fleet.json"
    fake_fleet.write_text("{}", encoding="utf-8")
    fake_controller.write_text(
        textwrap.dedent(
            f'''
            import json, pathlib, sys
            calls = pathlib.Path({str(calls_path)!r})
            argv = sys.argv[1:]
            calls.parent.mkdir(parents=True, exist_ok=True)
            calls.write_text(calls.read_text() + json.dumps(argv) + "\\n" if calls.exists() else json.dumps(argv) + "\\n")
            command = argv[argv.index("--state") + 2]
            if command == "plan-claim":
                print(json.dumps({{"status":"planned","ok":True,"workers":[{{"name":"worker-a"}}]}}))
            elif command == "claim":
                assert "--reservation-id" in argv
                assert "--session-id" in argv
                print(json.dumps({{"status":"auth_failed","ok":False,"error":"no auth"}}))
            elif command == "record-task":
                assert "--controller-execution-id" in argv
                print(json.dumps({{"status":"recorded","ok":True}}))
            else:
                print(json.dumps({{"status":"unexpected","ok":False,"command":command}}))
            '''
        ),
        encoding="utf-8",
    )
    try:
        subprocess.run(
            [
                "apoc",
                "recipe",
                "import",
                str(RECIPES),
                "--cwd",
                str(ROOT),
                "--idempotency-key",
                f"test-workenv-claim-import-{uuid.uuid4()}",
                "--purpose",
                "Import the workenv recipe bundle for the claim retention test.",
                "--format",
                "json",
            ],
            check=True,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            env=env,
        )
        run_input = {
            "idempotency_key": f"claim-{uuid.uuid4().hex}",
            "project": "apoc",
            "task_id": "task-a",
            "revision": "b" * 40,
            "workenv_root": str(ROOT),
            "controller": str(fake_controller),
            "fleet": str(fake_fleet),
            "state": str(tmp_path / "state"),
            "timeout_ms": 60000,
        }
        completed = subprocess.run(
            [
                "apoc",
                "recipe",
                "run",
                "workenv.claim",
                "--input",
                json.dumps(run_input),
                "--cwd",
                str(ROOT),
                "--idempotency-key",
                f"test-workenv-claim-run-{uuid.uuid4()}",
                "--purpose",
                "Run the workenv claim recipe against a fake ambiguous controller.",
                "--format",
                "json",
            ],
            check=True,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            env=env,
        )
        payload = json.loads(completed.stdout)
    finally:
        subprocess.run(
            [
                "apoc",
                "daemon",
                "stop",
                "--idempotency-key",
                f"test-workenv-claim-stop-{uuid.uuid4()}",
                "--purpose",
                "Stop the isolated APoC daemon used by the claim recipe test.",
                "--format",
                "json",
            ],
            check=False,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            env=env,
        )

    result = payload["result"]
    calls = [json.loads(line) for line in calls_path.read_text(encoding="utf-8").splitlines()]
    claim_call = next(call for call in calls if "claim" in call)

    assert result["status"] == "auth_failed"
    assert result["reservation_id"]
    assert result["task_record"]["status"] == "recorded"
    assert "--reservation-id" in claim_call
    assert "--session-id" in claim_call
