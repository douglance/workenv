import json
import os
import subprocess
from pathlib import Path

from scripts import workenv_controller as controller


FLEET = {
    "tailnet_suffix": "tail.example.ts.net",
    "herdr_session": "workenv",
    "remote_user": "exedev",
    "workers": [{"name": "workenv-01", "class": "linux"}],
}

DIGEST = "a" * 64
MISSING = object()


def completed(stdout="", stderr="", returncode=0):
    return {"status": "completed", "returncode": returncode, "stdout": stdout, "stderr": stderr}


def herdr_payload(**overrides):
    payload = {
        "running": True,
        "compatible": True,
        "version": "0.9.0",
        "protocol_version": 22,
        "restart_needed": False,
        "server_binary_stale": False,
        "capabilities": {"detached_server_daemon": True},
    }
    payload.update(overrides)
    return payload


def write_executable(path, text):
    path.write_text(text)
    path.chmod(path.stat().st_mode | 0o111)


def run_bootstrap(
    tmp_path,
    *,
    profile=None,
    digest=None,
    persisted=MISSING,
    herdr_status=None,
    apoc_list=None,
    apoc_get=None,
):
    root = tmp_path / "root"
    bin_dir = tmp_path / "bin"
    home = tmp_path / "home"
    root.joinpath("remote").mkdir(parents=True)
    root.joinpath("remote/profile.py").write_text("")
    bin_dir.mkdir()
    home.mkdir()
    captured = tmp_path / "apoc-argv.json"
    write_executable(
        bin_dir / "herdr",
        "#!/usr/bin/env python3\n"
        "import json, sys\n"
        f"print({json.dumps(json.dumps(herdr_status or herdr_payload(running=False)))})\n",
    )
    write_executable(
        bin_dir / "apoc",
        "#!/usr/bin/env python3\n"
        "import json, os, pathlib, sys\n"
        f"path = pathlib.Path({str(captured)!r})\n"
        f"list_payload = {json.dumps(apoc_list or {'executions': []})}\n"
        f"get_payloads = {json.dumps(apoc_get or {})}\n"
        "argv = sys.argv[1:]\n"
        "if argv[:2] == ['execution', 'list']:\n"
        "    print(json.dumps(list_payload))\n"
        "    raise SystemExit(0)\n"
        "if argv[:2] == ['execution', 'get']:\n"
        "    print(json.dumps(get_payloads.get(argv[2], {})))\n"
        "    raise SystemExit(0)\n"
        "path.write_text(json.dumps(argv))\n",
    )
    write_executable(
        bin_dir / "sudo",
        "#!/usr/bin/env python3\n"
        "import sys\n"
        "raise SystemExit(0)\n",
    )
    if persisted is not MISSING:
        state = root / ".state"
        state.mkdir()
        (state / "identity-profile.json").write_text(json.dumps(persisted))
    env = {
        **os.environ,
        "PATH": f"{bin_dir}:{os.environ.get('PATH', '')}",
        "WORKENV_BOOTSTRAP_SHELL": "1",
        "WORKENV_ROOT": str(root),
        "WORKENV_HERDR_SESSION": "workenv",
        "WORKENV_BOOT_ID": "boot-id",
        "APOC_BIN": "apoc",
        "HERDR_BIN": "herdr",
        "HOME": str(home),
    }
    if profile is not None:
        env["WORKENV_IDENTITY_PROFILE"] = profile
    if digest is not None:
        env["WORKENV_IDENTITY_DIGEST"] = digest
    script = Path(__file__).resolve().parents[1] / "bootstrap" / "workenv-herdr-bootstrap.sh"
    result = subprocess.run([str(script)], env=env, text=True, capture_output=True, check=False)
    argv = json.loads(captured.read_text()) if captured.exists() else None
    return result, argv


def test_bootstrap_wraps_profile_env_inside_apoc_child_argv(tmp_path):
    result, argv = run_bootstrap(tmp_path, profile="personal", digest=DIGEST)

    assert result.returncode == 0, result.stderr
    assert argv[:3] == ["execution", "start", "python3"]
    assert "--label" in argv
    assert "workenv.profile=personal" in argv
    delimiter = argv.index("--")
    child = argv[delimiter + 1 :]
    assert child[:7] == [
        str(tmp_path / "root" / "remote/profile.py"),
        "--root",
        str(tmp_path / "root"),
        "exec",
        "--name",
        "personal",
        "--digest",
    ]
    assert "herdr" in child
    assert "server" in child


def test_bootstrap_loads_persisted_profile_when_env_is_absent(tmp_path):
    result, argv = run_bootstrap(
        tmp_path,
        persisted={"name": "personal", "digest": DIGEST},
    )

    assert result.returncode == 0, result.stderr
    assert "workenv.profile=personal" in argv
    assert f"workenv.profile.digest={DIGEST}" in argv


def test_bootstrap_accepts_persisted_null_profile_as_unprofiled(tmp_path):
    result, argv = run_bootstrap(tmp_path, persisted=None)

    assert result.returncode == 0, result.stderr
    assert argv[:3] == ["execution", "start", "herdr"]
    assert "workenv.profile=personal" not in argv


def test_bootstrap_fails_closed_for_malformed_persisted_profile(tmp_path):
    root = tmp_path / "root"
    state = root / ".state"
    state.mkdir(parents=True)
    (state / "identity-profile.json").write_text("{not-json")

    result, argv = run_bootstrap(tmp_path)

    assert result.returncode != 0
    assert argv is None
    assert "invalid workenv identity profile descriptor" in result.stderr


def test_bootstrap_fails_closed_for_partial_persisted_profile(tmp_path):
    result, argv = run_bootstrap(tmp_path, persisted={"name": "personal"})

    assert result.returncode != 0
    assert argv is None
    assert "invalid workenv identity profile descriptor" in result.stderr


def test_bootstrap_rejects_invalid_env_profile_pair_before_start(tmp_path):
    result, argv = run_bootstrap(tmp_path, profile="Personal", digest=DIGEST)

    assert result.returncode != 0
    assert argv is None
    assert "WORKENV_IDENTITY_PROFILE and WORKENV_IDENTITY_DIGEST" in result.stderr


def test_bootstrap_unprofiled_server_keeps_plain_herdr_child_argv(tmp_path):
    result, argv = run_bootstrap(tmp_path)

    assert result.returncode == 0, result.stderr
    assert argv[:3] == ["execution", "start", "herdr"]
    assert "workenv.profile=personal" not in argv
    delimiter = argv.index("--")
    assert argv[delimiter + 1 :] == ["--session", "workenv", "server"]


def test_bootstrap_matches_healthy_profile_from_execution_get_not_list_summary(tmp_path):
    result, argv = run_bootstrap(
        tmp_path,
        profile="personal",
        digest=DIGEST,
        herdr_status=herdr_payload(),
        apoc_list={
            "executions": [
                {
                    "id": "exec-1",
                    "name": "workenv-herdr-server-workenv",
                    "status": "running",
                    "program": "python3",
                    "cwd": str(tmp_path / "root"),
                }
            ]
        },
        apoc_get={
            "exec-1": {
                "execution": {
                    "status": "running",
                    "spec": {
                        "labels": [
                            "workenv.component=herdr-server",
                            "herdr.session=workenv",
                            "workenv.profile=personal",
                            f"workenv.profile.digest={DIGEST}",
                        ]
                    },
                }
            }
        },
    )

    assert result.returncode == 0, result.stderr
    assert argv is None
    assert "workenv Herdr server is already healthy" in result.stdout


def test_bootstrap_refuses_healthy_server_when_execution_get_profile_mismatches(tmp_path):
    result, argv = run_bootstrap(
        tmp_path,
        profile="personal",
        digest=DIGEST,
        herdr_status=herdr_payload(),
        apoc_list={
            "executions": [
                {
                    "id": "exec-1",
                    "name": "workenv-herdr-server-workenv",
                    "status": "running",
                    "program": "python3",
                    "cwd": str(tmp_path / "root"),
                }
            ]
        },
        apoc_get={
            "exec-1": {
                "execution": {
                    "status": "running",
                    "spec": {
                        "labels": {
                            "workenv.component": "herdr-server",
                            "herdr.session": "workenv",
                            "workenv.profile": "other",
                            "workenv.profile.digest": DIGEST,
                        }
                    },
                }
            }
        },
    )

    assert result.returncode != 0
    assert argv is None
    assert "without the expected APoC profile labels" in result.stderr


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
