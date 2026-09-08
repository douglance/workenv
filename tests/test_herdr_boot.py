import json

from scripts import workenv_controller as controller


FLEET = {
    "tailnet_suffix": "tail.example.ts.net",
    "herdr_session": "workenv",
    "remote_user": "exedev",
    "workers": [{"name": "workenv-01", "class": "linux"}],
}


def completed(stdout="", stderr="", returncode=0):
    return {"status": "completed", "returncode": returncode, "stdout": stdout, "stderr": stderr}


def herdr_payload(**overrides):
    payload = {
        "running": True,
        "compatible": True,
        "version": "0.9.0",
        "protocol_version": 22,
        "server_binary_stale": False,
        "capabilities": {"detached_server_daemon": True},
    }
    payload.update(overrides)
    return payload


def test_worker_herdr_status_requires_running_compatible_protocol_and_detached_capability(monkeypatch):
    observed = {}

    def run(args, *, timeout, text):
        observed["args"] = args
        observed["timeout"] = timeout
        return completed(json.dumps(herdr_payload()))

    monkeypatch.setattr(controller, "run", run)

    result = controller.worker_herdr_status(FLEET, "workenv-01", remote_root="/home/exedev/workenv", recovery=True)

    assert result["status"] == "herdr_ready"
    assert result["herdr_ready"] is True
    assert observed["timeout"] == 60
    assert observed["args"][-2] == "exedev@workenv-01.exe.xyz"
    assert "/usr/local/bin/devenv shell -- herdr --session workenv status server --json" in observed["args"][-1]


def test_worker_herdr_status_refuses_stale_or_missing_detached_capability(monkeypatch):
    statuses = [
        herdr_payload(server_binary_stale=True),
        herdr_payload(capabilities={"detached_server_daemon": False}),
    ]

    def run(args, *, timeout, text):
        return completed(json.dumps(statuses.pop(0)))

    monkeypatch.setattr(controller, "run", run)

    stale = controller.worker_herdr_status(FLEET, "workenv-01")
    missing_capability = controller.worker_herdr_status(FLEET, "workenv-01")

    assert stale["status"] == "herdr_stale"
    assert stale["ok"] is False
    assert missing_capability["status"] == "herdr_not_ready"
    assert missing_capability["ok"] is False


def test_worker_herdr_status_refuses_unknown_json(monkeypatch):
    monkeypatch.setattr(controller, "run", lambda *args, **kwargs: completed("not-json"))

    result = controller.worker_herdr_status(FLEET, "workenv-01")

    assert result["status"] == "herdr_unknown"


def test_start_worker_herdr_runs_boot_helper_then_verifies_status(monkeypatch):
    calls = []

    def run(args, *, timeout, text):
        calls.append(args[-1])
        return completed("started\n")

    def worker_herdr_status(fleet, worker, *, remote_root, recovery):
        calls.append("status")
        assert remote_root == "/remote/workenv"
        assert recovery is True
        return {"status": "herdr_ready", "ok": True, "herdr_ready": True}

    monkeypatch.setattr(controller, "run", run)
    monkeypatch.setattr(controller, "worker_herdr_status", worker_herdr_status)

    result = controller.start_worker_herdr(FLEET, "workenv-01", "ensure-1", remote_root="/remote/workenv", recovery=True)

    assert result["status"] == "herdr_ready"
    assert result["herdr_ready"] is True
    assert "WORKENV_ROOT=/remote/workenv WORKENV_HERDR_SESSION=workenv /opt/workenv/bin/workenv-herdr-bootstrap" in calls[0]
    assert calls[1:] == ["status"]


def test_start_worker_herdr_fails_when_boot_helper_is_missing(monkeypatch):
    monkeypatch.setattr(controller, "run", lambda *args, **kwargs: completed("", "not found", 127))

    result = controller.start_worker_herdr(FLEET, "workenv-01", "ensure-1")

    assert result["status"] == "herdr_boot_failed"
    assert result["ok"] is False


def test_ensure_runs_tool_status_and_herdr_boot_before_tailscale_auth_required(monkeypatch, tmp_path):
    events = []

    monkeypatch.setattr(controller, "ensure_provider_worker", lambda *args, **kwargs: {"status": "present", "ok": True})
    monkeypatch.setattr(controller, "tailscale_status", lambda *args, **kwargs: {"status": "tailscale_not_enrolled", "ok": False})
    monkeypatch.setattr(controller, "sync_worker_sources", lambda *args, **kwargs: events.append("sync") or {"status": "synced", "ok": True})
    monkeypatch.setattr(controller, "run_workspace_request", lambda *args, **kwargs: events.append("idle") or {"status": "available", "ok": True})

    def run(args, *, timeout, text):
        if "--health-only" in args[-1]:
            events.append("health")
            return completed(json.dumps({"missing_tools": []}))
        events.append("bootstrap")
        return completed()

    def worker_tool_status(*args, **kwargs):
        events.append("tools")
        return {"schema": 1, "tools_ready": True, "nib_auth": {"authenticated": True}}

    def start_worker_herdr(*args, **kwargs):
        events.append("herdr")
        return {"status": "herdr_ready", "ok": True, "herdr_ready": True}

    def enroll_worker_if_credentials_available(*args, **kwargs):
        events.append("enroll")
        return {"status": "auth_required", "ok": False, "error": "missing credentials"}

    monkeypatch.setattr(controller, "run", run)
    monkeypatch.setattr(controller, "worker_tool_status", worker_tool_status)
    monkeypatch.setattr(controller, "start_worker_herdr", start_worker_herdr)
    monkeypatch.setattr(controller, "enroll_worker_if_credentials_available", enroll_worker_if_credentials_available)

    result = controller.ensure(FLEET, "workenv-01", "ensure-1", reconcile=True, local_state=tmp_path / ".state" / "controller")

    assert result["status"] == "auth_required"
    assert result["tools"]["tools_ready"] is True
    assert result["herdr"]["herdr_ready"] is True
    assert events == ["sync", "idle", "bootstrap", "health", "tools", "herdr", "enroll"]


def test_ensure_refuses_ready_when_herdr_boot_reports_stale_server(monkeypatch, tmp_path):
    events = []

    monkeypatch.setattr(controller, "ensure_provider_worker", lambda *args, **kwargs: {"status": "present", "ok": True})
    monkeypatch.setattr(controller, "tailscale_status", lambda *args, **kwargs: {"status": "tailscale_ready", "ok": True})
    monkeypatch.setattr(controller, "sync_worker_sources", lambda *args, **kwargs: {"status": "synced", "ok": True})
    monkeypatch.setattr(controller, "run_workspace_request", lambda *args, **kwargs: {"status": "available", "ok": True})
    monkeypatch.setattr(controller, "run", lambda *args, **kwargs: completed(json.dumps({"missing_tools": []})) if "--health-only" in args[0][-1] else completed())
    monkeypatch.setattr(controller, "worker_tool_status", lambda *args, **kwargs: {"schema": 1, "tools_ready": True, "nib_auth": {"authenticated": True}})
    monkeypatch.setattr(controller, "start_worker_herdr", lambda *args, **kwargs: events.append("herdr") or {"status": "herdr_stale", "ok": False, "herdr_ready": False})
    monkeypatch.setattr(controller, "worker_auth_status", lambda *args, **kwargs: (_ for _ in ()).throw(AssertionError("stale Herdr must block before auth readiness")))

    result = controller.ensure(FLEET, "workenv-01", "ensure-2", reconcile=True, local_state=tmp_path / ".state" / "controller")

    assert result["status"] == "herdr_stale"
    assert result["tools"]["tools_ready"] is True
    assert events == ["herdr"]
