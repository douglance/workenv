#!/usr/bin/env python3
import argparse
import json
import os
import platform
import re
import shutil
import subprocess
import sys
import time
from pathlib import Path


REQUIRED_TOOLS = ("apoc", "herdr", "git", "python3", "cargo")
OPTIONAL_TOOLS = ("codex", "claude", "gemini")
PROFILE_RE = re.compile(r"[a-z][a-z0-9-]{0,47}")
DIGEST_RE = re.compile(r"[0-9a-f]{64}")
TERMINAL_STATUSES = {"completed", "failed", "canceled", "cancelled", "skipped"}
COMMAND_TIMEOUT_SECONDS = 10


def parse_args():
    parser = argparse.ArgumentParser(
        description="Prepare an existing Mac/Linux host for workenv native runtime use."
    )
    parser.add_argument("--root", required=True, help="Absolute workenv runtime root to prepare.")
    parser.add_argument("--json", action="store_true", help="Emit JSON. This is the default output.")
    parser.add_argument("--start-session", metavar="NAME", help="Start or reuse a Herdr server session through APoC.")
    parser.add_argument("--worker", help="Worker identifier label for --start-session.")
    parser.add_argument("--profile", help="Optional workenv identity/profile label.")
    parser.add_argument("--digest", help="Lowercase sha256 digest for --profile.")
    parser.add_argument("--idempotency-key", help="Caller-supplied APoC idempotency key for --start-session.")
    return parser.parse_args()


def json_exit(payload, code=0):
    print(json.dumps(payload, sort_keys=True))
    raise SystemExit(code)


def reject(message, *, status="invalid_request", code=2):
    json_exit({"ready": False, "status": status, "error": message}, code)


def safe_root(raw_root):
    root = Path(raw_root).expanduser()
    if not root.is_absolute():
        reject("--root must be an absolute path")
    probe = Path(root.anchor)
    for part in root.parts[1:-1]:
        probe = probe / part
        if probe.is_symlink():
            reject(f"--root ancestor must not be a symlink: {probe}")
    parent = root.parent
    if not parent.exists():
        reject(f"--root parent does not exist: {parent}")
    if not parent.is_dir():
        reject(f"--root parent is not a directory: {parent}")
    if root.exists() or root.is_symlink():
        if root.is_symlink():
            reject(f"--root must not be a symlink: {root}")
        if not root.is_dir():
            reject(f"--root exists and is not a directory: {root}")
    return root


def ensure_dir(path):
    if path.is_symlink():
        reject(f"refusing symlinked runtime path: {path}", status="path_guard_failed")
    if path.exists() and not path.is_dir():
        reject(f"runtime path exists and is not a directory: {path}", status="path_guard_failed")
    path.mkdir(mode=0o700, parents=True, exist_ok=True)


def resolve_tool(name):
    found = shutil.which(name)
    if not found:
        return {"ok": False, "name": name, "path": None, "realpath": None}
    path = Path(found)
    try:
        realpath = path.resolve(strict=True)
    except OSError:
        return {"ok": False, "name": name, "path": str(path), "realpath": None}
    return {"ok": realpath.is_file() and os.access(realpath, os.X_OK), "name": name, "path": str(path), "realpath": str(realpath)}


def link_tool(bin_dir, name, realpath):
    link = bin_dir / name
    target = Path(realpath)
    if link.exists() or link.is_symlink():
        if not link.is_symlink():
            reject(f"refusing to overwrite non-symlink tool path: {link}", status="path_guard_failed")
        current = link.resolve(strict=False)
        if current == target:
            return str(link)
        link.unlink()
    link.symlink_to(target)
    return str(link)


def validate_profile(profile, digest):
    if bool(profile) != bool(digest):
        reject("--profile and --digest must be provided together")
    if profile and (not PROFILE_RE.fullmatch(profile) or not DIGEST_RE.fullmatch(digest)):
        reject("--profile must be lowercase kebab-case and --digest must be a lowercase sha256")


def prepare_runtime(root, profile=None, digest=None):
    ensure_dir(root)
    dirs = {
        "bin": root / "bin",
        "state": root / ".state",
        "workers": root / "workers",
        "sessions": root / "sessions",
        "logs": root / "logs",
    }
    for directory in dirs.values():
        ensure_dir(directory)

    tools = {name: resolve_tool(name) for name in (*REQUIRED_TOOLS, *OPTIONAL_TOOLS)}

    required_names = list(REQUIRED_TOOLS)
    required_missing = [
        {"name": name, "reason": "not found on PATH" if not tools[name]["path"] else "not executable or realpath unavailable"}
        for name in required_names
        if not tools[name]["ok"]
    ]
    profile_helper = root / "remote" / "profile.py"
    if profile and (not profile_helper.is_file() or not os.access(profile_helper, os.R_OK)):
        required_missing.append({"name": "remote/profile.py", "reason": "required for profiled Herdr launch"})

    tool_links = {}
    if not required_missing:
        for name, detail in tools.items():
            if detail["ok"]:
                tool_links[name] = link_tool(dirs["bin"], name, detail["realpath"])

    env_snapshot = {
        "schema": "workenv.native-host.env.v1",
        "generated_at": int(time.time()),
        "home": str(Path.home()),
        "path": os.environ.get("PATH", ""),
        "bin_path": str(dirs["bin"]),
        "tools": {
            name: {"realpath": detail["realpath"], "link": tool_links.get(name)}
            for name, detail in tools.items()
            if detail["ok"]
        },
        "profile": {"name": profile, "digest": digest} if profile else None,
    }
    snapshot_path = dirs["state"] / "native-host-env.json"
    snapshot_path.write_text(json.dumps(env_snapshot, sort_keys=True, indent=2) + "\n")

    system = platform.system().lower()
    arch = platform.machine()
    ready = not required_missing and system in {"darwin", "linux"}
    return {
        "ready": ready,
        "status": "ready" if ready else "needs_setup",
        "needs_setup": required_missing,
        "os": {
            "system": system,
            "release": platform.release(),
            "platform": platform.platform(),
        },
        "arch": arch,
        "home": str(Path.home()),
        "root": str(root),
        "bin_path": str(dirs["bin"]),
        "capabilities": {
            "core_tools": ready,
            "profile_support": bool(profile) and profile_helper.is_file(),
            "herdr_session_start": ready,
        },
        "tools": tools,
        "optional_missing": [name for name in OPTIONAL_TOOLS if not tools[name]["ok"]],
        "runtime": {
            "directories": {name: str(path) for name, path in dirs.items()},
            "environment_snapshot": str(snapshot_path),
            "tool_links": tool_links,
        },
    }


def run_json(argv):
    try:
        result = subprocess.run(
            argv,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            check=False,
            timeout=COMMAND_TIMEOUT_SECONDS,
        )
    except subprocess.TimeoutExpired:
        return {}
    if result.returncode != 0 or not result.stdout.strip():
        return {}
    try:
        return json.loads(result.stdout)
    except json.JSONDecodeError:
        return {}


def herdr_healthy(status):
    return (
        bool(status.get("running"))
        and status.get("compatible") is True
        and status.get("restart_needed") is not True
        and status.get("server_binary_stale") is not True
        and status.get("capabilities", {}).get("detached_server_daemon") is True
    )


def labels_from_execution(value):
    if isinstance(value, dict) and isinstance(value.get("data"), dict):
        value = value["data"]
    if isinstance(value, dict) and isinstance(value.get("execution"), dict):
        value = value["execution"]
    if not isinstance(value, dict) or value.get("status") in TERMINAL_STATUSES:
        return {}
    labels = value.get("spec", {}).get("labels", {})
    if isinstance(labels, dict):
        return labels
    if isinstance(labels, list):
        parsed = {}
        for label in labels:
            if isinstance(label, str) and "=" in label:
                key, raw = label.split("=", 1)
                parsed[key] = raw
        return parsed
    return {}


def running_execution_ids(apoc, session):
    value = run_json(
        [
            apoc,
            "execution",
            "list",
            "--status",
            "running",
            "--limit",
            "100",
            "--format",
            "json",
            "--purpose",
            "Inspect running workenv native Herdr server.",
        ]
    )
    items = value.get("executions") if isinstance(value, dict) else value if isinstance(value, list) else []
    if not isinstance(items, list):
        return []
    names = {f"workenv-native-herdr-{session}", f"workenv-herdr-server-{session}"}
    return [item["id"] for item in items if isinstance(item, dict) and item.get("id") and item.get("name") in names]


def owned_server(apoc, session, worker, profile=None, digest=None):
    for execution_id in running_execution_ids(apoc, session):
        value = run_json(
            [
                apoc,
                "execution",
                "get",
                execution_id,
                "--verbosity",
                "trace",
                "--format",
                "json",
                "--purpose",
                "Inspect running workenv native Herdr server labels.",
            ]
        )
        labels = labels_from_execution(value)
        if labels.get("workenv.component") != "herdr-server":
            continue
        if labels.get("herdr.session") != session or labels.get("workenv.worker") != worker:
            continue
        if profile:
            if labels.get("workenv.profile") == profile and labels.get("workenv.profile.digest") == digest:
                return execution_id
        elif "workenv.profile" not in labels and "workenv.profile.digest" not in labels:
            return execution_id
    return None


def execution_id_from_start(value):
    if not isinstance(value, dict):
        return None
    if isinstance(value.get("data"), dict):
        nested = execution_id_from_start(value["data"])
        if nested:
            return nested
    if isinstance(value.get("execution"), dict):
        nested = execution_id_from_start(value["execution"])
        if nested:
            return nested
    return value.get("id") if isinstance(value.get("id"), str) else None


def start_session(report, session, worker, idempotency_key, profile=None, digest=None):
    if not worker:
        reject("--worker is required with --start-session")
    if not idempotency_key:
        reject("--idempotency-key is required with --start-session")
    if not report["ready"]:
        report["start_session"] = {"status": "needs_setup", "ready": False}
        json_exit(report, 1)

    herdr = report["tools"]["herdr"]["realpath"]
    apoc = report["tools"]["apoc"]["realpath"]
    status = run_json([herdr, "--session", session, "status", "server", "--json"])
    if bool(status.get("running")):
        execution_id = owned_server(apoc, session, worker, profile, digest)
        if herdr_healthy(status) and execution_id:
            report["start_session"] = {"status": "herdr_ready", "execution_id": execution_id, "started": False}
            json_exit(report)
        if not herdr_healthy(status):
            report["start_session"] = {
                "status": "herdr_running_unhealthy",
                "ready": False,
                "started": False,
                "error": "Herdr reports a running server that is not healthy; refusing to start a duplicate server",
            }
            json_exit(report, 1)
        report["start_session"] = {
            "status": "herdr_running_unowned",
            "ready": False,
            "started": False,
            "error": "compatible Herdr server is already running without matching APoC ownership labels",
        }
        json_exit(report, 1)

    command = [herdr, "--session", session, "server"]
    executable = command[0]
    child_args = command[1:]
    labels = [
        "workenv.component=herdr-server",
        f"herdr.session={session}",
        f"workenv.worker={worker}",
    ]
    if profile:
        profile_helper = str(Path(report["root"]) / "remote" / "profile.py")
        command = [
            report["tools"]["python3"]["realpath"],
            profile_helper,
            "--root",
            report["root"],
            "exec",
            "--name",
            profile,
            "--digest",
            digest,
            "--",
            herdr,
            "--session",
            session,
            "server",
        ]
        executable = command[0]
        child_args = command[1:]
        labels.extend([f"workenv.profile={profile}", f"workenv.profile.digest={digest}"])

    argv = [
        apoc,
        "execution",
        "start",
        executable,
        "--cwd",
        report["root"],
        "--pty",
        "--name",
        f"workenv-native-herdr-{session}",
        "--purpose",
        "Start workenv native Herdr server through APoC.",
        "--idempotency-key",
        idempotency_key,
        "--format",
        "json",
    ]
    for label in labels:
        argv.extend(["--label", label])
    argv.append("--")
    argv.extend(child_args)
    try:
        result = subprocess.run(
            argv,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            check=False,
            timeout=COMMAND_TIMEOUT_SECONDS,
        )
    except subprocess.TimeoutExpired as error:
        report["start_session"] = {
            "status": "start_timeout",
            "started": False,
            "timeout_seconds": COMMAND_TIMEOUT_SECONDS,
            "stderr": str(error),
        }
        json_exit(report, 1)
    start_payload = {}
    if result.stdout.strip():
        try:
            start_payload = json.loads(result.stdout)
        except json.JSONDecodeError:
            start_payload = {}
    report["start_session"] = {
        "status": "start_requested" if result.returncode == 0 else "start_failed",
        "started": result.returncode == 0,
        "execution_id": execution_id_from_start(start_payload),
        "returncode": result.returncode,
        "stdout": result.stdout.strip(),
        "stderr": result.stderr.strip(),
    }
    json_exit(report, 0 if result.returncode == 0 else 1)


def main():
    args = parse_args()
    validate_profile(args.profile, args.digest)
    if args.start_session and not re.fullmatch(r"[A-Za-z0-9_.:-]{1,80}", args.start_session):
        reject("--start-session contains unsupported characters")
    if args.worker and not re.fullmatch(r"[A-Za-z0-9_.:-]{1,120}", args.worker):
        reject("--worker contains unsupported characters")
    root = safe_root(args.root)
    report = prepare_runtime(root, args.profile, args.digest)
    if args.start_session:
        start_session(report, args.start_session, args.worker, args.idempotency_key, args.profile, args.digest)
    json_exit(report)


if __name__ == "__main__":
    main()
