import io
import json
import shlex
import subprocess
import tarfile
from pathlib import Path

from scripts import workenv_controller as controller


FLEET = {
    "tailnet_suffix": "tail.example.ts.net",
    "tailscale_tag": "tag:workenv",
    "remote_user": "exedev",
    "workers": [{"name": "workenv-01", "class": "linux", "cpus": 4, "memory_gb": 16, "disk_gb": 80}],
}


def completed(stdout="", stderr="", returncode=0):
    return {"status": "completed", "returncode": returncode, "stdout": stdout, "stderr": stderr}


def tailscale_payload(*, backend="Running", dns="workenv-01.tail.example.ts.net.", tailnet="tail.example.ts.net", tags=None):
    return {
        "BackendState": backend,
        "Self": {"DNSName": dns, "Tags": tags if tags is not None else ["tag:workenv"]},
        "CurrentTailnet": {"MagicDNSSuffix": tailnet},
    }


def test_tailscale_status_requires_running_identity_tag_and_ssh_prefs(monkeypatch):
    calls = []

    def run(args, *, timeout, text):
        calls.append(args)
        command = args[-1]
        if command == "tailscale status --json":
            return completed(json.dumps(tailscale_payload()))
        if command == "tailscale debug prefs":
            return completed(json.dumps({"WantRunning": True, "RunSSH": True}))
        raise AssertionError(args)

    monkeypatch.setattr(controller, "run", run)

    result = controller.tailscale_status(FLEET, "workenv-01")

    assert result["status"] == "tailscale_ready"
    assert result["ok"] is True
    assert result["dns_name"] == "workenv-01.tail.example.ts.net"
    assert calls[0][-1] == "tailscale status --json"
    assert calls[1][-1] == "tailscale debug prefs"


def test_tailscale_status_rejects_disabled_runssh(monkeypatch):
    def run(args, *, timeout, text):
        if args[-1] == "tailscale status --json":
            return completed(json.dumps(tailscale_payload()))
        if args[-1] == "tailscale debug prefs":
            return completed(json.dumps({"WantRunning": True, "RunSSH": False}))
        raise AssertionError(args)

    monkeypatch.setattr(controller, "run", run)

    result = controller.tailscale_status(FLEET, "workenv-01")

    assert result["status"] == "tailscale_configuration_blocked"
    assert result["ok"] is False


def test_tailscale_status_rejects_wrong_tailnet(monkeypatch):
    def run(args, *, timeout, text):
        if args[-1] == "tailscale status --json":
            return completed(json.dumps(tailscale_payload(tailnet="other.ts.net")))
        raise AssertionError("prefs should not be read after identity mismatch")

    monkeypatch.setattr(controller, "run", run)

    result = controller.tailscale_status(FLEET, "workenv-01")

    assert result["status"] == "tailscale_mismatch"
    assert result["expected_tailnet"] == "tail.example.ts.net"


def test_source_sync_requires_and_transfers_pinned_apoc_artifacts(monkeypatch, tmp_path):
    tool_builds = tmp_path / "tool-builds"
    tool_builds.mkdir()
    (tool_builds / controller.APOC_TOOL_ARCHIVE).write_bytes(b"archive")
    (tool_builds / controller.APOC_TOOL_SHA).write_text("hash  apoc-linux-x86_64.tar.gz\n", encoding="utf-8")
    observed = {}

    def run_with_input(args, *, timeout, text, input_data):
        observed["args"] = args
        observed["input"] = input_data
        result = subprocess.run(shlex.split(args[-1]), input=input_data, capture_output=True)
        return completed(result.stdout.decode(), result.stderr.decode(), result.returncode)

    monkeypatch.setattr(controller, "TOOL_BUILDS_DIR", tool_builds)
    monkeypatch.setattr(controller, "run_with_input", run_with_input)

    remote_root = tmp_path / "worker"
    result = controller.sync_worker_sources(FLEET, "workenv-01", remote_root=str(remote_root), recovery=True)

    assert result["status"] == "synced"
    assert result["tools_dir"] == f"{remote_root}/tool-builds"
    assert observed["args"][-2] == "exedev@workenv-01.exe.xyz"
    assert (remote_root / "devenv.nix").read_bytes() == (controller.DEFAULT_ROOT / "devenv.nix").read_bytes()
    assert (remote_root / "remote/tool_health.py").is_file()
    with tarfile.open(fileobj=io.BytesIO(observed["input"]), mode="r:gz") as archive:
        names = set(archive.getnames())
    assert "tool-builds/apoc-linux-x86_64.tar.gz" in names
    assert "tool-builds/apoc-linux-x86_64.tar.gz.sha256" in names
    assert any(name.startswith("bootstrap/") for name in names)
    assert any(name.startswith("remote/") for name in names)


def test_source_sync_reports_missing_pinned_artifact(monkeypatch, tmp_path):
    monkeypatch.setattr(controller, "TOOL_BUILDS_DIR", tmp_path)

    result = controller.sync_worker_sources(FLEET, "workenv-01", remote_root="/remote/workenv", recovery=False)

    assert result["status"] == "missing_tool_artifact"
    assert any(path.endswith(controller.APOC_TOOL_ARCHIVE) for path in result["missing"])
    assert any(path.endswith(controller.APOC_TOOL_SHA) for path in result["missing"])


def test_reconcile_with_healthy_tailnet_checks_idle_before_sync_and_does_not_mutate_busy_worker(monkeypatch, tmp_path):
    events = []

    monkeypatch.setattr(controller, "ensure_provider_worker", lambda *args, **kwargs: {"status": "present", "ok": True})
    monkeypatch.setattr(controller, "tailscale_status", lambda *args, **kwargs: {"status": "tailscale_ready", "ok": True})

    def run_workspace_request(*args, **kwargs):
        events.append("status")
        return {"status": "busy", "ok": False}

    def sync_worker_sources(*args, **kwargs):
        events.append("sync")
        raise AssertionError("busy worker must not be mutated")

    monkeypatch.setattr(controller, "run_workspace_request", run_workspace_request)
    monkeypatch.setattr(controller, "sync_worker_sources", sync_worker_sources)

    result = controller.ensure(FLEET, "workenv-01", "ensure-1", reconcile=True, local_state=tmp_path / ".state" / "controller")

    assert result["status"] == "worker_busy"
    assert events == ["status"]


def test_reconcile_with_healthy_tailnet_syncs_after_idle_and_installs_prerequisites(monkeypatch, tmp_path):
    events = []
    commands = []

    monkeypatch.setattr(controller, "ensure_provider_worker", lambda *args, **kwargs: {"status": "present", "ok": True})
    monkeypatch.setattr(controller, "tailscale_status", lambda *args, **kwargs: {"status": "tailscale_ready", "ok": True})
    monkeypatch.setattr(controller, "worker_auth_status", lambda *args, **kwargs: {"schema": 1, "ready": True})
    monkeypatch.setattr(controller, "worker_tool_status", lambda *args, **kwargs: {"schema": 1, "tools_ready": True, "nib_auth": {"authenticated": True}})
    monkeypatch.setattr(controller, "start_worker_herdr", lambda *args, **kwargs: {"status": "herdr_ready", "ok": True, "herdr_ready": True})

    def run_workspace_request(*args, **kwargs):
        events.append("status")
        return {"status": "available", "ok": True}

    def sync_worker_sources(*args, **kwargs):
        events.append("sync")
        return {"status": "synced", "ok": True, "tools_dir": "/remote/workenv/tool-builds"}

    def run(args, *, timeout, text):
        commands.append(args[-1])
        if "--health-only" in args[-1]:
            return completed(json.dumps({"missing_tools": []}))
        return completed()

    monkeypatch.setattr(controller, "run_workspace_request", run_workspace_request)
    monkeypatch.setattr(controller, "sync_worker_sources", sync_worker_sources)
    monkeypatch.setattr(controller, "run", run)

    result = controller.ensure(
        FLEET,
        "workenv-01",
        "ensure-2",
        reconcile=True,
        remote_root="/remote/workenv",
        local_state=tmp_path / ".state" / "controller",
    )

    assert result["status"] == "ready"
    assert events == ["status", "sync"]
    assert any("/remote/workenv/bootstrap/bootstrap.sh" in command for command in commands)
    assert all("--tools-dir" not in command for command in commands)


def test_recovery_reconcile_syncs_before_idle_and_returns_auth_required_without_credentials(monkeypatch, tmp_path):
    events = []

    monkeypatch.setattr(controller, "ensure_provider_worker", lambda *args, **kwargs: {"status": "present", "ok": True})
    monkeypatch.setattr(controller, "tailscale_status", lambda *args, **kwargs: {"status": "tailscale_not_enrolled", "ok": False})
    monkeypatch.setattr(controller, "EnrollmentDriver", lambda *args, **kwargs: (_ for _ in ()).throw(AssertionError("no credentials should not enroll")))

    def sync_worker_sources(*args, **kwargs):
        events.append("sync")
        assert kwargs["recovery"] is True
        return {"status": "synced", "ok": True}

    def run_workspace_request(*args, **kwargs):
        events.append("status")
        assert kwargs["recovery"] is True
        return {"status": "available", "ok": True}

    def run(args, *, timeout, text):
        if "--health-only" in args[-1]:
            return completed(json.dumps({"missing_tools": []}))
        return completed()

    monkeypatch.setattr(controller, "sync_worker_sources", sync_worker_sources)
    monkeypatch.setattr(controller, "run_workspace_request", run_workspace_request)
    monkeypatch.setattr(controller, "run", run)

    result = controller.ensure(FLEET, "workenv-01", "ensure-3", reconcile=True, local_state=tmp_path / ".state" / "controller")

    assert result["status"] == "auth_required"
    assert result["health"] == {"missing_tools": []}
    assert events == ["sync", "status"]


def test_reconcile_uses_enrollment_driver_when_controller_credentials_exist(monkeypatch, tmp_path):
    local_state = tmp_path / ".state" / "controller"
    credentials = local_state.parent / "tailscale-oauth.local.json"
    credentials.parent.mkdir(parents=True)
    credentials.write_text(json.dumps({"client_id": "id", "client_secret": "secret"}), encoding="utf-8")
    observed = {}
    tailscale_results = iter([
        {"status": "tailscale_not_enrolled", "ok": False},
        {"status": "tailscale_not_enrolled", "ok": False},
        {"status": "tailscale_ready", "ok": True},
    ])

    class FakeEnrollmentDriver:
        def __init__(self, *, fleet_path, state_dir, credentials_file, timeout):
            observed["fleet_path"] = Path(fleet_path)
            observed["state_dir"] = Path(state_dir)
            observed["credentials_file"] = Path(credentials_file)
            observed["timeout"] = timeout

        def handle(self, request):
            observed["request"] = request
            observed["fleet"] = json.loads(observed["fleet_path"].read_text(encoding="utf-8"))
            return {"status": "enrolled", "ok": True}

    monkeypatch.setattr(controller, "ensure_provider_worker", lambda *args, **kwargs: {"status": "present", "ok": True})
    monkeypatch.setattr(controller, "tailscale_status", lambda *args, **kwargs: next(tailscale_results))
    monkeypatch.setattr(controller, "sync_worker_sources", lambda *args, **kwargs: {"status": "synced", "ok": True})
    monkeypatch.setattr(controller, "run_workspace_request", lambda *args, **kwargs: {"status": "available", "ok": True})
    monkeypatch.setattr(controller, "run", lambda *args, **kwargs: completed(json.dumps({"missing_tools": []})) if "--health-only" in args[0][-1] else completed())
    monkeypatch.setattr(controller, "worker_auth_status", lambda *args, **kwargs: {"schema": 1, "ready": True})
    monkeypatch.setattr(controller, "EnrollmentDriver", FakeEnrollmentDriver)
    monkeypatch.setattr(controller, "worker_tool_status", lambda *args, **kwargs: {"schema": 1, "tools_ready": True, "nib_auth": {"authenticated": True}})
    monkeypatch.setattr(controller, "start_worker_herdr", lambda *args, **kwargs: {"status": "herdr_ready", "ok": True, "herdr_ready": True})

    result = controller.ensure(FLEET, "workenv-01", "ensure-4", reconcile=True, local_state=local_state)

    assert result["status"] == "ready"
    assert result["enrollment"]["status"] == "enrolled"
    assert observed["credentials_file"] == credentials
    assert observed["state_dir"] == local_state.parent / "enrollment"
    assert observed["timeout"] == 20
    assert observed["request"] == {"operation": "enroll", "request_id": "ensure-4-enroll", "worker": "workenv-01"}
    assert observed["fleet"] == FLEET


def test_task_run_and_record_runtime_reject_released_central_task(monkeypatch, tmp_path):
    controller.record_task(
        tmp_path,
        task_id="task-a",
        worker="workenv-01",
        project="apoc",
        repo={"owner": "operator", "name": "apoc"},
        revision="a" * 40,
        reservation_id="reservation-a",
        session_id="session-a",
        source="remote",
        status="released",
        worktree="/remote/worktree",
    )

    def no_remote(*args, **kwargs):
        raise AssertionError("released task must not contact the worker")

    monkeypatch.setattr(controller, "run", no_remote)
    monkeypatch.setattr(controller, "run_workspace_request", no_remote)

    started = controller.task_run(FLEET, "workenv-01", "task-a", "run-1", "test", ["true"], local_state=tmp_path)
    recorded = controller.record_runtime(
        FLEET,
        "workenv-01",
        "task-a",
        "a" * 40,
        "runtime-1",
        {"apoc_execution_ids": ["exec-a"]},
        local_state=tmp_path,
    )

    assert started["status"] == "task_not_running"
    assert recorded["status"] == "task_not_running"



def test_status_reports_capacity_connectivity_auth_ownership_runtime_and_activity(monkeypatch, tmp_path):
    controller.record_task(
        tmp_path,
        task_id="task-a",
        worker="workenv-01",
        project="apoc",
        repo={"owner": "operator", "name": "apoc"},
        revision="a" * 40,
        reservation_id="reservation-a",
        session_id="session-a",
        source="remote",
        status="claimed",
        worktree="/remote/worktree",
    )
    record_path = controller.task_record_path(tmp_path, "task-a")
    record = json.loads(record_path.read_text(encoding="utf-8"))
    record["runtime"] = {"apoc_execution_ids": ["exec-a"], "herdr": {"pane_ids": ["pane-a"]}}
    controller.write_json_atomic(record_path, record)
    observed = {}

    def run_workspace_request(*args, **kwargs):
        observed["timeout"] = kwargs["timeout"]
        return {"status": "available", "ok": True, "detail": "idle"}

    def inspect_task_activity(fleet, worker, task_id, task_record):
        observed["activity"] = {"worker": worker, "task_id": task_id, "runtime": task_record["runtime"]}
        return {"status": "clear", "ok": True}

    monkeypatch.setattr(controller, "run_workspace_request", run_workspace_request)
    monkeypatch.setattr(controller, "worker_auth_status", lambda *args, **kwargs: {"schema": 1, "status": "ready", "ready": True})
    monkeypatch.setattr(controller, "inspect_task_activity", inspect_task_activity)

    result = controller.status(FLEET, "status-1", worker="workenv-01", local_state=tmp_path)

    worker = result["workers"][0]
    assert result["status"] == "busy"
    assert worker["status"] == "blocked"
    assert worker["capacity"] == {"class": "linux", "cpus": 4, "memory_gb": 16, "disk_gb": 80}
    assert worker["connectivity"] == {"status": "available", "ok": True, "detail": "idle"}
    assert worker["agent_state"] == worker["connectivity"]
    assert worker["auth"]["ready"] is True
    assert worker["ownership"]["open_tasks"][0]["runtime"] == {"apoc_execution_ids": ["exec-a"], "herdr": {"pane_ids": ["pane-a"]}}
    assert worker["executions"] == ["exec-a"]
    assert worker["blockers"][0]["kind"] == "ownership"
    assert observed["timeout"] == 30
    assert observed["activity"]["task_id"] == "task-a"


def test_status_marks_auth_blocker_without_claiming_worker_ready(monkeypatch, tmp_path):
    monkeypatch.setattr(controller, "run_workspace_request", lambda *args, **kwargs: {"status": "available", "ok": True})
    monkeypatch.setattr(controller, "worker_auth_status", lambda *args, **kwargs: {"status": "auth_unknown", "ok": False, "error": "probe failed"})
    monkeypatch.setattr(controller, "inspect_task_activity", lambda *args, **kwargs: (_ for _ in ()).throw(AssertionError("no active tasks")))

    result = controller.status(FLEET, "status-2", worker="workenv-01", local_state=tmp_path)

    worker = result["workers"][0]
    assert result["status"] == "busy"
    assert worker["status"] == "blocked"
    assert worker["blockers"] == [{"kind": "auth", "status": "auth_unknown", "error": "probe failed"}]
