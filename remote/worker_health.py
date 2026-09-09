from __future__ import annotations

import argparse
import concurrent.futures
import json
import os
import platform
import shutil
import subprocess
import sys
import time
from collections.abc import Callable, Mapping, Sequence
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Optional

try:
    from . import health as auth_health
    from . import tool_health
except ImportError:  # pragma: no cover - supports python3 remote/worker_health.py
    import health as auth_health  # type: ignore
    import tool_health  # type: ignore


HERDR_VERSION = "0.9.0"
HERDR_PROTOCOL_VERSION = 22
PROBE_TIMEOUT_SECONDS = 10.0
OVERALL_TIMEOUT_SECONDS = 12.0
REQUIRED_CORE_TOOLS = ("git", "herdr", "apoc")
OPTIONAL_TOOLS = tuple(name for name in tool_health.TOOLS if name not in REQUIRED_CORE_TOOLS)


@dataclass(frozen=True)
class CommandResult:
    returncode: int
    stdout: str
    stderr: str
    timed_out: bool = False
    missing: bool = False


CommandRunner = Callable[[Sequence[str], float], CommandResult]
PathLookup = Callable[[str], Optional[str]]
DiskUsage = Callable[[str], shutil._ntuple_diskusage]


def evaluate_worker_health(
    *,
    root: str | Path,
    session: str,
    tools: str,
    details: bool = False,
    environ: Mapping[str, str] | None = None,
    command_runner: CommandRunner | None = None,
    path_lookup: PathLookup | None = None,
    disk_usage: DiskUsage | None = None,
    probe_timeout: float = PROBE_TIMEOUT_SECONDS,
    overall_timeout: float = OVERALL_TIMEOUT_SECONDS,
) -> dict[str, Any]:
    root_path = Path(root).expanduser()
    runner = default_command_runner if command_runner is None else command_runner
    lookup = shutil.which if path_lookup is None else path_lookup
    usage = shutil.disk_usage if disk_usage is None else disk_usage
    env = os.environ if environ is None else environ
    started = time.monotonic()

    probe_specs: dict[str, Callable[[], dict[str, Any]]] = {
        "workspace": lambda: probe_workspace(root_path),
        "tools": lambda: probe_tools(tools, details, runner, lookup, probe_timeout),
        "herdr": lambda: probe_herdr(session, runner, probe_timeout),
        "auth": lambda: probe_auth(env, runner, probe_timeout),
        "tailscale": lambda: probe_tailscale(runner, probe_timeout),
    }
    sections, timings = _run_probes(probe_specs, overall_timeout)

    metadata = {
        "disk": probe_disk(root_path, usage),
        "os": {
            "system": platform.system(),
            "release": platform.release(),
            "machine": platform.machine(),
            "python_version": platform.python_version(),
        },
        "cli_install": {
            "root": str(root_path),
            "root_exists": root_path.exists(),
            "session": session,
            "tools": tools,
            "details": details,
            "python": sys.executable,
            "workenv": probe_workenv_cli(lookup),
        },
    }
    blockers = _blockers(sections)
    ready = not blockers
    status = _status(ready, blockers)
    return {
        "schema": 1,
        "status": status,
        "ok": ready,
        "ready": ready,
        "root": str(root_path),
        "session": session,
        "tools_mode": tools,
        "details": details,
        "workspace": sections["workspace"],
        "tools": sections["tools"],
        "herdr": sections["herdr"],
        "auth": sections["auth"],
        "tailscale": sections["tailscale"],
        "metadata": metadata,
        "probes": timings,
        "elapsed_ms": _elapsed_ms(started),
        "blockers": blockers,
    }


def _run_probes(
    probe_specs: Mapping[str, Callable[[], dict[str, Any]]],
    overall_timeout: float,
) -> tuple[dict[str, dict[str, Any]], dict[str, dict[str, Any]]]:
    executor = concurrent.futures.ThreadPoolExecutor(max_workers=len(probe_specs))
    futures = {executor.submit(_timed_probe, name, probe): name for name, probe in probe_specs.items()}
    done, pending = concurrent.futures.wait(futures, timeout=overall_timeout)
    sections: dict[str, dict[str, Any]] = {}
    timings: dict[str, dict[str, Any]] = {}
    for future in done:
        name = futures[future]
        try:
            section, elapsed = future.result()
        except Exception as exc:  # noqa: BLE001
            section = _probe_failed(name, "probe_exception", str(exc))
            elapsed = None
        sections[name] = section
        timings[name] = {"elapsed_ms": elapsed if elapsed is not None else section.get("elapsed_ms"), "status": section.get("status")}
    for future in pending:
        name = futures[future]
        section = _probe_failed(name, "probe_timeout", f"{name} probe exceeded overall timeout")
        section["elapsed_ms"] = int(overall_timeout * 1000)
        sections[name] = section
        timings[name] = {"elapsed_ms": section["elapsed_ms"], "status": section["status"]}
    executor.shutdown(wait=False, cancel_futures=True)
    return sections, timings


def _timed_probe(name: str, probe: Callable[[], dict[str, Any]]) -> tuple[dict[str, Any], int]:
    started = time.monotonic()
    section = probe()
    elapsed = _elapsed_ms(started)
    section.setdefault("status", f"{name}_unknown")
    section["elapsed_ms"] = elapsed
    return section, elapsed


def probe_workspace(root: Path) -> dict[str, Any]:
    state_dir = root / "state"
    active_path = state_dir / "active.json"
    if not state_dir.exists():
        return {"status": "needs_setup", "ok": False, "active": None, "error": "workspace_state_missing"}
    if not active_path.exists():
        return {"status": "available", "ok": True, "active": None}
    try:
        active = json.loads(active_path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as exc:
        return {"status": "unknown", "ok": False, "error": str(exc)}
    if not isinstance(active, dict):
        return {"status": "unknown", "ok": False, "error": "active workspace state is not an object"}
    status = active.get("status", "busy")
    if status == "released":
        return {"status": "available", "ok": True, "active": _redact_active(active)}
    return {"status": status if isinstance(status, str) else "busy", "ok": False, "active": _redact_active(active)}


def probe_tools(
    tools_mode: str,
    details: bool,
    command_runner: CommandRunner,
    path_lookup: PathLookup,
    timeout: float,
) -> dict[str, Any]:
    run_versions = {name: True for name in REQUIRED_CORE_TOOLS}
    run_versions.update({name: details for name in OPTIONAL_TOOLS})
    observed = _run_tool_checks(tools_mode, run_versions, command_runner, path_lookup, timeout)

    required_ready = all(observed[name]["ok"] is True for name in REQUIRED_CORE_TOOLS)
    nib_auth = {"authenticated": False, "status": "unavailable"}
    if details and observed.get("nib", {}).get("ok") is True:
        nib_auth = _nib_auth(command_runner, timeout)
    return {
        "schema": 1,
        "status": "tools_ready" if required_ready else "tools_missing",
        "ok": required_ready,
        "tools_ready": required_ready,
        "required_core_ready": required_ready,
        "required_core": list(REQUIRED_CORE_TOOLS),
        "optional": list(OPTIONAL_TOOLS),
        "tools": observed,
        "nib_auth": nib_auth,
    }


def _run_tool_checks(
    tools_mode: str,
    run_versions: Mapping[str, bool],
    command_runner: CommandRunner,
    path_lookup: PathLookup,
    timeout: float,
) -> dict[str, Any]:
    observed: dict[str, dict[str, Any]] = {}
    executor = concurrent.futures.ThreadPoolExecutor(max_workers=len(run_versions))
    futures = {}
    for name, run_version in run_versions.items():
        path = path_lookup(name)
        if not path:
            observed[name] = {"ok": False, "available": False, "error": "missing_command"}
            continue
        if tools_mode == "devenv" and not path.startswith("/nix/store/"):
            observed[name] = {"ok": False, "available": True, "path": path, "error": "command_is_outside_devenv"}
            continue
        if not run_version:
            observed[name] = {"ok": True, "available": True, "path": path, "status": "available"}
            continue
        future = executor.submit(_tool_version_status, path, tool_health.TOOLS[name], command_runner, timeout)
        futures[future] = (name, path)

    done, pending = concurrent.futures.wait(futures, timeout=timeout)
    for future in done:
        name, _path = futures[future]
        try:
            observed[name] = future.result()
        except Exception as exc:  # noqa: BLE001
            observed[name] = {"ok": False, "available": True, "status": "error", "error": str(exc)}
    for future in pending:
        name, path = futures[future]
        observed[name] = {"ok": False, "available": True, "path": path, "status": "timeout", "error": "version_timeout"}
    executor.shutdown(wait=False, cancel_futures=True)
    return observed


def _tool_version_status(
    path: str,
    arguments: Sequence[str],
    command_runner: CommandRunner,
    timeout: float,
) -> dict[str, Any]:
    result = command_runner((path, *arguments), timeout)
    if result.timed_out:
        return {"ok": False, "available": True, "path": path, "status": "timeout", "error": "version_timeout"}
    if result.missing:
        return {"ok": False, "available": False, "error": "missing_command"}
    item = {
        "ok": result.returncode == 0,
        "available": True,
        "path": path,
        "status": "ok" if result.returncode == 0 else "error",
        "version": _trim(result.stdout),
    }
    if result.returncode != 0:
        item["error"] = _trim(result.stderr)
    return item


def _nib_auth(command_runner: CommandRunner, timeout: float) -> dict[str, Any]:
    result = command_runner(("nib", "auth", "status", "--format", "json"), timeout)
    if result.returncode != 0 or result.timed_out or result.missing:
        return {"authenticated": False, "status": "auth_required" if not result.timed_out else "unknown"}
    try:
        payload = json.loads(result.stdout)
    except json.JSONDecodeError:
        return {"authenticated": False, "status": "unknown"}
    if not isinstance(payload, dict):
        return {"authenticated": False, "status": "unknown"}
    return {"authenticated": payload.get("authenticated") is True, "source": payload.get("source"), "status": "checked"}


def probe_herdr(session: str, command_runner: CommandRunner, timeout: float) -> dict[str, Any]:
    result = command_runner(("herdr", "--session", session, "status", "server", "--json"), timeout)
    if result.timed_out:
        return {"status": "herdr_unknown", "ok": False, "herdr_ready": False, "error": "timeout"}
    if result.missing:
        return {"status": "herdr_not_ready", "ok": False, "herdr_ready": False, "error": "missing_command"}
    if result.returncode != 0:
        return {"status": "herdr_not_ready", "ok": False, "herdr_ready": False, "error": _trim(result.stderr)}
    try:
        payload = json.loads(result.stdout)
    except json.JSONDecodeError:
        return {"status": "herdr_unknown", "ok": False, "herdr_ready": False, "error": "invalid_json"}
    if not isinstance(payload, dict):
        return {"status": "herdr_unknown", "ok": False, "herdr_ready": False, "error": "invalid_json"}
    ready = (
        payload.get("running") is True
        and payload.get("compatible") is True
        and payload.get("version") == HERDR_VERSION
        and payload.get("protocol", payload.get("protocol_version")) == HERDR_PROTOCOL_VERSION
        and payload.get("server_binary_stale") is False
        and _pointer(payload, ("capabilities", "detached_server_daemon")) is True
    )
    if ready:
        return {"status": "herdr_ready", "ok": True, "herdr_ready": True, "server": payload}
    if payload.get("server_binary_stale") is True:
        return {"status": "herdr_stale", "ok": False, "herdr_ready": False, "server": payload}
    return {"status": "herdr_not_ready", "ok": False, "herdr_ready": False, "server": payload}


def probe_auth(environ: Mapping[str, str], command_runner: CommandRunner, timeout: float) -> dict[str, Any]:
    auth_timeout = max(0.1, timeout / 2)

    def run(argv: Sequence[str], command_timeout: float) -> auth_health.CommandResult:
        result = command_runner(argv, min(command_timeout, auth_timeout))
        return auth_health.CommandResult(
            returncode=result.returncode,
            stdout=result.stdout,
            stderr=result.stderr,
            timed_out=result.timed_out,
            missing=result.missing,
        )

    return auth_health.evaluate_health(environ=environ, command_runner=run, timeout=auth_timeout)


def probe_tailscale(command_runner: CommandRunner, timeout: float) -> dict[str, Any]:
    command_timeout = max(0.1, timeout / 2)
    status = command_runner(("tailscale", "status", "--json"), command_timeout)
    if status.timed_out:
        return {"status": "tailscale_unknown", "ok": False, "error": "timeout"}
    if status.missing:
        return {"status": "tailscale_not_ready", "ok": False, "error": "missing_command"}
    if status.returncode != 0:
        return {"status": "tailscale_not_ready", "ok": False, "error": _trim(status.stderr)}
    try:
        payload = json.loads(status.stdout)
    except json.JSONDecodeError:
        return {"status": "tailscale_unknown", "ok": False, "error": "invalid_json"}
    if not isinstance(payload, dict):
        return {"status": "tailscale_unknown", "ok": False, "error": "invalid_json"}
    prefs_result = command_runner(("tailscale", "debug", "prefs"), command_timeout)
    if prefs_result.returncode != 0 or prefs_result.timed_out or prefs_result.missing:
        prefs = {}
    else:
        try:
            prefs_payload = json.loads(prefs_result.stdout)
            prefs = prefs_payload if isinstance(prefs_payload, dict) else {}
        except json.JSONDecodeError:
            prefs = {}
    subset = _tailscale_subset(payload)
    if payload.get("BackendState") != "Running":
        return {"status": "tailscale_not_ready", "ok": False, "tailscale": subset}
    if prefs.get("WantRunning") is not True or prefs.get("RunSSH") is not True:
        return {"status": "tailscale_configuration_blocked", "ok": False, "tailscale": subset, "prefs": _prefs_subset(prefs)}
    return {"status": "tailscale_ready", "ok": True, "tailscale": subset, "prefs": _prefs_subset(prefs)}


def probe_disk(root: Path, disk_usage: DiskUsage) -> dict[str, Any]:
    target = root if root.exists() else _nearest_existing_parent(root)
    usage = disk_usage(str(target))
    return {
        "path": str(target),
        "total_bytes": usage.total,
        "used_bytes": usage.used,
        "free_bytes": usage.free,
    }


def probe_workenv_cli(path_lookup: PathLookup) -> dict[str, Any]:
    path = path_lookup("workenv")
    if not path:
        return {"available": False, "status": "cli_missing"}
    try:
        digest = _sha256_file(Path(path))
    except OSError as exc:
        return {"available": True, "path": path, "status": "hash_unavailable", "error": str(exc)}
    return {"available": True, "path": path, "status": "available", "sha256": digest}


def default_command_runner(argv: Sequence[str], timeout: float) -> CommandResult:
    executable = argv[0]
    if "/" not in executable and shutil.which(executable) is None:
        return CommandResult(returncode=127, stdout="", stderr="", missing=True)
    try:
        completed = subprocess.run(
            list(argv),
            check=False,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            timeout=timeout,
        )
    except subprocess.TimeoutExpired:
        return CommandResult(returncode=124, stdout="", stderr="", timed_out=True)
    return CommandResult(returncode=completed.returncode, stdout=completed.stdout, stderr=completed.stderr)


def _blockers(sections: Mapping[str, dict[str, Any]]) -> list[dict[str, Any]]:
    blockers = []
    workspace = sections["workspace"]
    if workspace.get("status") != "available":
        blockers.append({"kind": "workspace", "status": workspace.get("status")})
    if sections["tools"].get("tools_ready") is not True:
        blockers.append({"kind": "tools", "status": sections["tools"].get("status")})
    if sections["herdr"].get("herdr_ready") is not True:
        blockers.append({"kind": "herdr", "status": sections["herdr"].get("status")})
    if sections["auth"].get("ready") is not True:
        blockers.append({"kind": "auth", "status": sections["auth"].get("status")})
    return blockers


def _status(ready: bool, blockers: Sequence[Mapping[str, Any]]) -> str:
    if ready:
        return "available"
    if any(blocker.get("kind") == "workspace" and blocker.get("status") not in {"available", "needs_setup", "unknown"} for blocker in blockers):
        return "busy"
    if any("unknown" in str(blocker.get("status", "")) or blocker.get("status") == "probe_timeout" for blocker in blockers):
        return "partial"
    return "needs_setup"


def _probe_failed(name: str, status: str, error: str) -> dict[str, Any]:
    section = {"status": status, "ok": False, "error": error}
    if name == "tools":
        section["tools_ready"] = False
    if name == "herdr":
        section["herdr_ready"] = False
    if name == "auth":
        section["ready"] = False
    return section


def _redact_active(active: Mapping[str, Any]) -> dict[str, Any]:
    return {
        "status": active.get("status"),
        "task_id": active.get("task_id"),
        "repo": active.get("repo"),
        "revision": active.get("revision"),
        "branch": active.get("branch"),
        "worktree": active.get("worktree"),
        "service_state_dir": active.get("service_state_dir"),
        "runtime": active.get("runtime") if isinstance(active.get("runtime"), dict) else {"herdr": {}, "apoc_execution_ids": []},
        "last_collection": active.get("last_collection"),
    }


def _tailscale_subset(payload: Mapping[str, Any]) -> dict[str, Any]:
    self_node = payload.get("Self") if isinstance(payload.get("Self"), dict) else {}
    tailnet = payload.get("CurrentTailnet") if isinstance(payload.get("CurrentTailnet"), dict) else {}
    return {
        "BackendState": payload.get("BackendState"),
        "Self": {
            "DNSName": self_node.get("DNSName"),
            "TailscaleIPs": self_node.get("TailscaleIPs"),
            "Tags": self_node.get("Tags"),
        },
        "CurrentTailnet": {
            "MagicDNSSuffix": tailnet.get("MagicDNSSuffix"),
            "Name": tailnet.get("Name"),
        },
    }


def _prefs_subset(payload: Mapping[str, Any]) -> dict[str, Any]:
    return {"WantRunning": payload.get("WantRunning"), "RunSSH": payload.get("RunSSH")}


def _pointer(payload: Mapping[str, Any], path: Sequence[str]) -> Any:
    current: Any = payload
    for part in path:
        if not isinstance(current, dict):
            return None
        current = current.get(part)
    return current


def _nearest_existing_parent(path: Path) -> Path:
    current = path
    while not current.exists() and current.parent != current:
        current = current.parent
    return current


def _trim(value: str, limit: int = 500) -> str:
    return value.strip()[:limit]


def _sha256_file(path: Path) -> str:
    import hashlib

    digest = hashlib.sha256()
    with path.open("rb") as file:
        for chunk in iter(lambda: file.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def _elapsed_ms(started: float) -> int:
    return int((time.monotonic() - started) * 1000)


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description="Report bounded worker health in one remote invocation.")
    parser.add_argument("--root", default=str(Path.home() / "workenv"))
    parser.add_argument("--session", default="workenv")
    parser.add_argument("--tools", choices=("native", "devenv"), default="native")
    parser.add_argument("--details", action="store_true")
    parser.add_argument("--probe-timeout", type=float, default=PROBE_TIMEOUT_SECONDS)
    parser.add_argument("--overall-timeout", type=float, default=OVERALL_TIMEOUT_SECONDS)
    args = parser.parse_args(argv)
    report = evaluate_worker_health(
        root=args.root,
        session=args.session,
        tools=args.tools,
        details=args.details,
        probe_timeout=args.probe_timeout,
        overall_timeout=args.overall_timeout,
    )
    print(json.dumps(report, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
