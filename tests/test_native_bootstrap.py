import json
import os
import subprocess
import sys
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
NATIVE_HOST = ROOT / "bootstrap" / "native-host.py"
DIGEST = "a" * 64


def write_executable(path, text):
    path.write_text(text)
    path.chmod(path.stat().st_mode | 0o111)


def fake_required_tools(tmp_path, *, omit=(), herdr_status=None, apoc_list=None, apoc_get=None):
    bin_dir = tmp_path / "fake-bin"
    bin_dir.mkdir()
    captured = tmp_path / "apoc-argv.json"
    omit = set(omit)
    for name in ["git", "cargo", "direnv"]:
        if name not in omit:
            write_executable(bin_dir / name, "#!/usr/bin/env sh\nexit 0\n")
    if "python3" not in omit:
        (bin_dir / "python3").symlink_to(Path(sys.executable))
    if "herdr" not in omit:
        payload = herdr_status if herdr_status is not None else {"running": False}
        write_executable(
            bin_dir / "herdr",
            "#!/usr/bin/env python3\n"
            "import json\n"
            f"print({json.dumps(json.dumps(payload))})\n",
        )
    if "apoc" not in omit:
        write_executable(
            bin_dir / "apoc",
            "#!/usr/bin/env python3\n"
            "import json, pathlib, sys\n"
            f"captured = pathlib.Path({str(captured)!r})\n"
            f"apoc_list = {json.dumps(apoc_list or {'executions': []})}\n"
            f"apoc_get = {json.dumps(apoc_get or {})}\n"
            "argv = sys.argv[1:]\n"
            "if argv[:2] == ['execution', 'list']:\n"
            "    print(json.dumps(apoc_list))\n"
            "    raise SystemExit(0)\n"
            "if argv[:2] == ['execution', 'get']:\n"
            "    print(json.dumps(apoc_get.get(argv[2], {})))\n"
            "    raise SystemExit(0)\n"
            "captured.write_text(json.dumps(argv))\n"
            "print(json.dumps({'id': 'exec-started'}))\n",
        )
    return bin_dir, captured


def run_native(tmp_path, args, *, bin_dir):
    env = {
        **os.environ,
        "PATH": str(bin_dir),
        "HOME": str(tmp_path / "home"),
    }
    (tmp_path / "home").mkdir(exist_ok=True)
    return subprocess.run(
        [sys.executable, str(NATIVE_HOST), *args],
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        env=env,
        check=False,
    )


def test_native_host_prepares_runtime_and_reports_capabilities(tmp_path):
    bin_dir, _captured = fake_required_tools(tmp_path)
    root = tmp_path / "runtime"

    result = run_native(tmp_path, ["--root", str(root), "--json"], bin_dir=bin_dir)

    assert result.returncode == 0, result.stderr
    report = json.loads(result.stdout)
    assert report["ready"] is True
    assert report["status"] == "ready"
    assert report["os"]["system"] in {"darwin", "linux"}
    assert report["arch"]
    assert report["home"] == str(tmp_path / "home")
    assert report["bin_path"] == str(root / "bin")
    assert report["capabilities"]["core_tools"] is True
    assert "codex" in report["optional_missing"]
    assert "codex" not in [item["name"] for item in report["needs_setup"]]
    snapshot = Path(report["runtime"]["environment_snapshot"])
    assert snapshot.exists()
    snapshot_json = json.loads(snapshot.read_text())
    assert snapshot_json["bin_path"] == str(root / "bin")
    assert Path(report["runtime"]["tool_links"]["herdr"]).resolve() == (bin_dir / "herdr").resolve()


def test_native_host_reports_needs_setup_for_missing_required_tool(tmp_path):
    bin_dir, _captured = fake_required_tools(tmp_path, omit={"cargo"})
    root = tmp_path / "runtime"

    result = run_native(tmp_path, ["--root", str(root), "--json"], bin_dir=bin_dir)

    assert result.returncode == 0, result.stderr
    report = json.loads(result.stdout)
    assert report["ready"] is False
    assert report["status"] == "needs_setup"
    assert {"name": "cargo", "reason": "not found on PATH"} in report["needs_setup"]
    assert report["capabilities"]["herdr_session_start"] is False


def test_native_host_profiled_runtime_requires_profile_helper_not_direnv(tmp_path):
    bin_dir, _captured = fake_required_tools(tmp_path, omit={"direnv"})
    root = tmp_path / "runtime"

    plain = run_native(tmp_path, ["--root", str(root), "--json"], bin_dir=bin_dir)
    profiled = run_native(
        tmp_path,
        ["--root", str(root), "--json", "--profile", "personal", "--digest", DIGEST],
        bin_dir=bin_dir,
    )

    assert json.loads(plain.stdout)["ready"] is True
    profile_report = json.loads(profiled.stdout)
    assert profile_report["ready"] is False
    assert {"name": "remote/profile.py", "reason": "required for profiled Herdr launch"} in profile_report["needs_setup"]


def test_native_host_rejects_symlink_root_and_non_symlink_tool_path(tmp_path):
    bin_dir, _captured = fake_required_tools(tmp_path)
    actual = tmp_path / "actual"
    actual.mkdir()
    linked = tmp_path / "linked"
    linked.symlink_to(actual)

    symlinked = run_native(tmp_path, ["--root", str(linked), "--json"], bin_dir=bin_dir)

    assert symlinked.returncode == 2
    assert json.loads(symlinked.stdout)["status"] == "invalid_request"

    root = tmp_path / "runtime"
    (root / "bin").mkdir(parents=True)
    (root / "bin" / "herdr").write_text("occupied")
    occupied = run_native(tmp_path, ["--root", str(root), "--json"], bin_dir=bin_dir)

    assert occupied.returncode == 2
    assert json.loads(occupied.stdout)["status"] == "path_guard_failed"


def test_native_host_rejects_symlink_root_ancestor(tmp_path):
    bin_dir, _captured = fake_required_tools(tmp_path)
    actual_parent = tmp_path / "actual-parent"
    actual_parent.mkdir()
    linked_parent = tmp_path / "linked-parent"
    linked_parent.symlink_to(actual_parent)

    result = run_native(tmp_path, ["--root", str(linked_parent / "runtime"), "--json"], bin_dir=bin_dir)

    assert result.returncode == 2
    assert "ancestor" in json.loads(result.stdout)["error"]


def test_native_host_start_session_uses_apoc_pty_with_owned_labels(tmp_path):
    bin_dir, captured = fake_required_tools(tmp_path, herdr_status={"running": False})
    root = tmp_path / "runtime"

    result = run_native(
        tmp_path,
        [
            "--root",
            str(root),
            "--json",
            "--start-session",
            "workenv",
            "--worker",
            "native-a",
            "--idempotency-key",
            "caller-key-1",
        ],
        bin_dir=bin_dir,
    )

    assert result.returncode == 0, result.stderr
    report = json.loads(result.stdout)
    assert report["start_session"]["status"] == "start_requested"
    argv = json.loads(captured.read_text())
    assert argv[:3] == ["execution", "start", str((bin_dir / "herdr").resolve())]
    assert "--pty" in argv
    assert "workenv.component=herdr-server" in argv
    assert "herdr.session=workenv" in argv
    assert "workenv.worker=native-a" in argv
    assert "--env" not in argv  # Host APoC profile owns the execution environment.
    assert argv[argv.index("--idempotency-key") + 1] == "caller-key-1"
    assert argv[argv.index("--format") + 1] == "json"
    delimiter = argv.index("--")
    assert argv[delimiter + 1 :] == ["--session", "workenv", "server"]
    assert report["start_session"]["execution_id"] == "exec-started"


def test_native_host_start_session_requires_caller_idempotency_key(tmp_path):
    bin_dir, captured = fake_required_tools(tmp_path, herdr_status={"running": False})
    root = tmp_path / "runtime"

    result = run_native(
        tmp_path,
        ["--root", str(root), "--json", "--start-session", "workenv", "--worker", "native-a"],
        bin_dir=bin_dir,
    )

    assert result.returncode == 2
    assert "--idempotency-key is required" in json.loads(result.stdout)["error"]
    assert not captured.exists()


def test_native_host_profile_start_uses_remote_profile_wrapper(tmp_path):
    bin_dir, captured = fake_required_tools(tmp_path, herdr_status={"running": False})
    root = tmp_path / "runtime"
    (root / "remote").mkdir(parents=True)
    (root / "remote" / "profile.py").write_text("")

    result = run_native(
        tmp_path,
        [
            "--root",
            str(root),
            "--json",
            "--start-session",
            "workenv",
            "--worker",
            "native-a",
            "--profile",
            "personal",
            "--digest",
            DIGEST,
            "--idempotency-key",
            "profile-key-1",
        ],
        bin_dir=bin_dir,
    )

    assert result.returncode == 0, result.stderr
    argv = json.loads(captured.read_text())
    assert argv[:3] == ["execution", "start", str(Path(sys.executable).resolve())]
    assert "workenv.profile=personal" in argv
    assert f"workenv.profile.digest={DIGEST}" in argv
    delimiter = argv.index("--")
    assert argv[delimiter + 1 :] == [
        str(root / "remote" / "profile.py"),
        "--root",
        str(root),
        "exec",
        "--name",
        "personal",
        "--digest",
        DIGEST,
        "--",
        str((bin_dir / "herdr").resolve()),
        "--session",
        "workenv",
        "server",
    ]


def test_native_host_start_session_uses_caller_key_for_each_restart_attempt(tmp_path):
    bin_dir, captured = fake_required_tools(tmp_path, herdr_status={"running": False})
    root = tmp_path / "runtime"

    first = run_native(
        tmp_path,
        [
            "--root",
            str(root),
            "--json",
            "--start-session",
            "workenv",
            "--worker",
            "native-a",
            "--idempotency-key",
            "restart-key-1",
        ],
        bin_dir=bin_dir,
    )
    first_argv = json.loads(captured.read_text())
    second = run_native(
        tmp_path,
        [
            "--root",
            str(root),
            "--json",
            "--start-session",
            "workenv",
            "--worker",
            "native-a",
            "--idempotency-key",
            "restart-key-2",
        ],
        bin_dir=bin_dir,
    )
    second_argv = json.loads(captured.read_text())

    assert first.returncode == 0, first.stderr
    assert second.returncode == 0, second.stderr
    assert first_argv[first_argv.index("--idempotency-key") + 1] == "restart-key-1"
    assert second_argv[second_argv.index("--idempotency-key") + 1] == "restart-key-2"


def test_native_host_start_session_refuses_compatible_unowned_herdr(tmp_path):
    herdr_status = {
        "running": True,
        "compatible": True,
        "restart_needed": False,
        "server_binary_stale": False,
        "capabilities": {"detached_server_daemon": True},
    }
    bin_dir, captured = fake_required_tools(tmp_path, herdr_status=herdr_status)
    root = tmp_path / "runtime"

    result = run_native(
        tmp_path,
        [
            "--root",
            str(root),
            "--json",
            "--start-session",
            "workenv",
            "--worker",
            "native-a",
            "--idempotency-key",
            "caller-key-1",
        ],
        bin_dir=bin_dir,
    )

    assert result.returncode == 1
    report = json.loads(result.stdout)
    assert report["start_session"]["status"] == "herdr_running_unowned"
    assert not captured.exists()


def test_native_host_start_session_refuses_running_unhealthy_herdr(tmp_path):
    herdr_status = {
        "running": True,
        "compatible": True,
        "restart_needed": True,
        "server_binary_stale": False,
        "capabilities": {"detached_server_daemon": True},
    }
    bin_dir, captured = fake_required_tools(tmp_path, herdr_status=herdr_status)
    root = tmp_path / "runtime"

    result = run_native(
        tmp_path,
        [
            "--root",
            str(root),
            "--json",
            "--start-session",
            "workenv",
            "--worker",
            "native-a",
            "--idempotency-key",
            "caller-key-1",
        ],
        bin_dir=bin_dir,
    )

    assert result.returncode == 1
    report = json.loads(result.stdout)
    assert report["start_session"]["status"] == "herdr_running_unhealthy"
    assert not captured.exists()


def test_native_host_missing_restart_needed_still_counts_as_healthy_when_owned(tmp_path):
    herdr_status = {
        "running": True,
        "compatible": True,
        "server_binary_stale": False,
        "capabilities": {"detached_server_daemon": True},
    }
    bin_dir, captured = fake_required_tools(
        tmp_path,
        herdr_status=herdr_status,
        apoc_list={"executions": [{"id": "exec-1", "name": "workenv-native-herdr-workenv", "status": "running"}]},
        apoc_get={
            "exec-1": {
                "execution": {
                    "status": "running",
                    "spec": {
                        "labels": [
                            "workenv.component=herdr-server",
                            "herdr.session=workenv",
                            "workenv.worker=native-a",
                        ]
                    },
                }
            }
        },
    )
    root = tmp_path / "runtime"

    result = run_native(
        tmp_path,
        [
            "--root",
            str(root),
            "--json",
            "--start-session",
            "workenv",
            "--worker",
            "native-a",
            "--idempotency-key",
            "caller-key-1",
        ],
        bin_dir=bin_dir,
    )

    assert result.returncode == 0, result.stderr
    assert json.loads(result.stdout)["start_session"]["status"] == "herdr_ready"
    assert not captured.exists()


def test_native_host_start_session_reuses_matching_owned_herdr(tmp_path):
    herdr_status = {
        "running": True,
        "compatible": True,
        "restart_needed": False,
        "server_binary_stale": False,
        "capabilities": {"detached_server_daemon": True},
    }
    bin_dir, captured = fake_required_tools(
        tmp_path,
        herdr_status=herdr_status,
        apoc_list={"executions": [{"id": "exec-1", "name": "workenv-native-herdr-workenv", "status": "running"}]},
        apoc_get={
            "exec-1": {
                "execution": {
                    "status": "running",
                    "spec": {
                        "labels": [
                            "workenv.component=herdr-server",
                            "herdr.session=workenv",
                            "workenv.worker=native-a",
                        ]
                    },
                }
            }
        },
    )
    root = tmp_path / "runtime"

    result = run_native(
        tmp_path,
        [
            "--root",
            str(root),
            "--json",
            "--start-session",
            "workenv",
            "--worker",
            "native-a",
            "--idempotency-key",
            "caller-key-1",
        ],
        bin_dir=bin_dir,
    )

    assert result.returncode == 0, result.stderr
    report = json.loads(result.stdout)
    assert report["start_session"] == {"status": "herdr_ready", "execution_id": "exec-1", "started": False}
    assert not captured.exists()
