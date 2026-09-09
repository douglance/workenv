import json
import time
from collections import namedtuple
from pathlib import Path

from remote import worker_health


DiskUsage = namedtuple("usage", "total used free")


def ready_herdr():
    return {
        "running": True,
        "compatible": True,
        "version": worker_health.HERDR_VERSION,
        "protocol": worker_health.HERDR_PROTOCOL_VERSION,
        "server_binary_stale": False,
        "capabilities": {"detached_server_daemon": True},
    }


def ready_tailscale():
    return {
        "BackendState": "Running",
        "Self": {"DNSName": "worker.tail.example.ts.net.", "TailscaleIPs": ["100.64.0.1"], "Tags": ["tag:workenv"]},
        "CurrentTailnet": {"MagicDNSSuffix": "tail.example.ts.net", "Name": "tail.example.ts.net"},
    }


def ready_auth_commands():
    return {
        ("codex", "login", "status"): worker_health.CommandResult(0, "Logged in using ChatGPT\n", ""),
        ("claude", "auth", "status"): worker_health.CommandResult(
            0,
            json.dumps({"loggedIn": True, "authMethod": "claude.ai", "apiProvider": "firstParty", "email": "secret@example.com"}),
            "",
        ),
    }


def core_paths(name):
    paths = {
        "git": "/usr/bin/git",
        "tailscale": "/usr/bin/tailscale",
        "herdr": "/usr/bin/herdr",
        "apoc": "/usr/bin/apoc",
    }
    return paths.get(name)


def ready_runner(extra=None):
    fixtures = {
        ("/usr/bin/git", "--version"): worker_health.CommandResult(0, "git version 2.0\n", ""),
        ("/usr/bin/tailscale", "version"): worker_health.CommandResult(0, "1.0\n", ""),
        ("/usr/bin/herdr", "--version"): worker_health.CommandResult(0, "herdr 0.9.0\n", ""),
        (
            "/usr/bin/apoc",
            "version",
            "--purpose",
            "Inspect the shared workenv APoC version.",
            "--format",
            "json",
        ): worker_health.CommandResult(0, json.dumps({"version": "0.6.0"}), ""),
        ("herdr", "--session", "workenv", "status", "server", "--json"): worker_health.CommandResult(0, json.dumps(ready_herdr()), ""),
        ("tailscale", "status", "--json"): worker_health.CommandResult(0, json.dumps(ready_tailscale()), ""),
        ("tailscale", "debug", "prefs"): worker_health.CommandResult(0, json.dumps({"WantRunning": True, "RunSSH": True}), ""),
        **ready_auth_commands(),
    }
    if extra:
        fixtures.update(extra)

    def run(argv, timeout):
        del timeout
        return fixtures.get(tuple(argv), worker_health.CommandResult(127, "", "", missing=True))

    return run


def make_workspace_ready(root: Path):
    (root / "state").mkdir(parents=True)


def test_one_hanging_probe_does_not_suppress_other_sections(tmp_path):
    make_workspace_ready(tmp_path)

    def run(argv, timeout):
        if tuple(argv) == ("herdr", "--session", "workenv", "status", "server", "--json"):
            time.sleep(0.05)
        return ready_runner()(argv, timeout)

    report = worker_health.evaluate_worker_health(
        root=tmp_path,
        session="workenv",
        tools="native",
        command_runner=run,
        path_lookup=core_paths,
        disk_usage=lambda path: DiskUsage(100, 40, 60),
        overall_timeout=0.01,
    )

    assert report["workspace"]["status"] == "available"
    assert report["tools"]["tools_ready"] is True
    assert report["herdr"]["status"] == "probe_timeout"
    assert report["herdr"]["herdr_ready"] is False
    assert report["probes"]["herdr"]["elapsed_ms"] >= 10


def test_probe_timings_are_present_for_every_section(tmp_path):
    make_workspace_ready(tmp_path)
    report = worker_health.evaluate_worker_health(
        root=tmp_path,
        session="workenv",
        tools="native",
        command_runner=ready_runner(),
        path_lookup=core_paths,
        disk_usage=lambda path: DiskUsage(100, 40, 60),
    )

    for name in ["workspace", "tools", "herdr", "auth", "tailscale"]:
        assert isinstance(report[name]["elapsed_ms"], int)
        assert isinstance(report["probes"][name]["elapsed_ms"], int)


def test_missing_optional_tools_do_not_block_core_tool_readiness(tmp_path):
    make_workspace_ready(tmp_path)
    report = worker_health.evaluate_worker_health(
        root=tmp_path,
        session="workenv",
        tools="native",
        command_runner=ready_runner(),
        path_lookup=core_paths,
        disk_usage=lambda path: DiskUsage(100, 40, 60),
    )

    assert report["tools"]["tools_ready"] is True
    assert report["tools"]["required_core_ready"] is True
    assert report["tools"]["tools"]["codex"]["error"] == "missing_command"
    assert report["status"] == "available"


def test_native_host_without_tailscale_keeps_core_tools_ready(tmp_path):
    make_workspace_ready(tmp_path)

    def paths(name):
        if name == "tailscale":
            return None
        return core_paths(name)

    def run(argv, timeout):
        if tuple(argv) == ("tailscale", "status", "--json"):
            return worker_health.CommandResult(127, "", "", missing=True)
        return ready_runner()(argv, timeout)

    report = worker_health.evaluate_worker_health(
        root=tmp_path,
        session="workenv",
        tools="native",
        command_runner=run,
        path_lookup=paths,
        disk_usage=lambda path: DiskUsage(100, 40, 60),
    )

    assert report["tools"]["required_core"] == ["git", "herdr", "apoc"]
    assert "tailscale" in report["tools"]["optional"]
    assert report["tools"]["tools_ready"] is True
    assert report["tools"]["tools"]["tailscale"]["available"] is False
    assert report["tailscale"]["status"] == "tailscale_not_ready"
    assert report["tailscale"]["ok"] is False
    assert report["herdr"]["herdr_ready"] is True
    assert report["ready"] is True
    assert report["metadata"]["cli_install"]["workenv"]["status"] == "cli_missing"


def test_hanging_tool_version_does_not_suppress_other_tool_results():
    def run(argv, timeout):
        if tuple(argv) == ("/usr/bin/git", "--version"):
            time.sleep(0.05)
        return ready_runner()(argv, timeout)

    report = worker_health.probe_tools(
        "native",
        False,
        run,
        core_paths,
        0.01,
    )

    assert report["tools"]["git"]["status"] == "timeout"
    assert report["tools"]["herdr"]["ok"] is True
    assert report["tools"]["apoc"]["ok"] is True
    assert report["tools_ready"] is False


def test_unknown_probe_state_is_not_ready(tmp_path):
    make_workspace_ready(tmp_path)
    report = worker_health.evaluate_worker_health(
        root=tmp_path,
        session="workenv",
        tools="native",
        command_runner=ready_runner({("herdr", "--session", "workenv", "status", "server", "--json"): worker_health.CommandResult(0, "not-json", "")}),
        path_lookup=core_paths,
        disk_usage=lambda path: DiskUsage(100, 40, 60),
    )

    assert report["ready"] is False
    assert report["status"] == "partial"
    assert report["herdr"]["status"] == "herdr_unknown"


def test_disk_bytes_come_from_shutil_disk_usage(tmp_path):
    make_workspace_ready(tmp_path)
    report = worker_health.evaluate_worker_health(
        root=tmp_path,
        session="workenv",
        tools="native",
        command_runner=ready_runner(),
        path_lookup=core_paths,
        disk_usage=lambda path: DiskUsage(123, 45, 78),
    )

    assert report["metadata"]["disk"]["total_bytes"] == 123
    assert report["metadata"]["disk"]["used_bytes"] == 45
    assert report["metadata"]["disk"]["free_bytes"] == 78


def test_workenv_cli_metadata_reports_availability_and_hash(tmp_path):
    make_workspace_ready(tmp_path)
    cli = tmp_path / "workenv"
    cli.write_bytes(b"workenv-test-binary")

    def paths(name):
        if name == "workenv":
            return str(cli)
        return core_paths(name)

    report = worker_health.evaluate_worker_health(
        root=tmp_path,
        session="workenv",
        tools="native",
        command_runner=ready_runner(),
        path_lookup=paths,
        disk_usage=lambda path: DiskUsage(100, 40, 60),
    )

    workenv = report["metadata"]["cli_install"]["workenv"]
    assert workenv["available"] is True
    assert workenv["status"] == "available"
    assert workenv["path"] == str(cli)
    assert len(workenv["sha256"]) == 64


def test_absent_runtime_files_are_normal_needs_setup(tmp_path):
    root = tmp_path / "missing-worker-root"
    report = worker_health.evaluate_worker_health(
        root=root,
        session="workenv",
        tools="native",
        command_runner=ready_runner(),
        path_lookup=core_paths,
        disk_usage=lambda path: DiskUsage(100, 40, 60),
    )

    assert report["workspace"]["status"] == "needs_setup"
    assert report["workspace"]["ok"] is False
    assert report["status"] == "needs_setup"


def test_details_runs_optional_tool_versions(tmp_path):
    make_workspace_ready(tmp_path)
    seen = []

    def paths(name):
        if name == "codex":
            return "/usr/bin/codex"
        return core_paths(name)

    def run(argv, timeout):
        seen.append(tuple(argv))
        if tuple(argv) == ("/usr/bin/codex", "--version"):
            return worker_health.CommandResult(0, "codex 1.0\n", "")
        return ready_runner()(argv, timeout)

    report = worker_health.evaluate_worker_health(
        root=tmp_path,
        session="workenv",
        tools="native",
        details=True,
        command_runner=run,
        path_lookup=paths,
        disk_usage=lambda path: DiskUsage(100, 40, 60),
    )

    assert ("/usr/bin/codex", "--version") in seen
    assert report["tools"]["tools"]["codex"]["ok"] is True
