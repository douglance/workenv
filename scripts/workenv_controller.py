from __future__ import annotations

import argparse
import base64
import contextlib
import concurrent.futures
import hashlib
import io
import json
import os
import re
import shlex
import subprocess
import sys
import tarfile
import tempfile
import time
from pathlib import Path, PurePosixPath

# APoC invokes this file by absolute path, including from other repositories.
if __package__ in (None, ""):
    sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from provisioning.provider import Provider, ProviderError
from enrollment.enroll import EnrollmentDriver, EnrollmentError


DEFAULT_ROOT = Path(__file__).resolve().parents[1]
DEFAULT_FLEET = DEFAULT_ROOT / "fleet.json"
DEFAULT_REMOTE_ROOT = "/home/exedev/workenv"
DEFAULT_STATE = DEFAULT_ROOT / ".state" / "controller"
SSH_OPTIONS = ("-o", "BatchMode=yes")
HEX64_RE = re.compile(r"^[0-9a-f]{64}$")
LIVE_EXECUTION_STATUSES = {"queued", "running", "stalled", "interrupted"}
TERMINAL_EXECUTION_STATUSES = {"completed", "failed", "canceled", "cancelled", "skipped"}
TERMINAL_EXECUTION_OUTCOMES = {"passed", "failed", "error", "skipped"}
AMBIGUOUS_REMOTE_STATUSES = {"pending", "auth_failed", "remote_failed", "remote_invalid_json"}
TERMINAL_TASK_STATUSES = {"released"}
RUNNABLE_TASK_STATUSES = {"claimed", "runtime_recorded"}
TOOL_BUILDS_DIR = DEFAULT_ROOT / ".state" / "tool-builds"
APOC_TOOL_ARCHIVE = "apoc-linux-x86_64.tar.gz"
APOC_TOOL_SHA = f"{APOC_TOOL_ARCHIVE}.sha256"


class ControllerError(Exception):
    def __init__(self, status: str, message: str):
        super().__init__(message)
        self.status = status
        self.message = message


def load_fleet(path: Path | str = DEFAULT_FLEET) -> dict:
    return json.loads(Path(path).read_text(encoding="utf-8"))


def plan_claim(
    fleet: dict,
    project: str,
    worker: str | None = None,
    *,
    local_state: Path = DEFAULT_STATE,
) -> dict:
    project_spec = project_config(fleet, project)
    workers = candidate_workers(fleet, project_spec["class"], worker)
    available = []
    blocked = []
    for item in workers:
        open_records = open_task_records_for_worker(local_state, item["name"])
        if open_records:
            blocked.append({"worker": item["name"], "tasks": [_task_record_summary(record) for record in open_records]})
        else:
            available.append(item)
    if worker is not None and blocked:
        return failed("worker_has_open_task", "worker has a central task record that is not released", worker=worker, tasks=blocked[0]["tasks"])
    return ok("planned", project=project_spec, workers=available, blocked=blocked)


def plan_workers(fleet: dict, worker: str | None = None) -> dict:
    workers = candidate_workers(fleet, None, worker)
    return ok("planned", workers=workers)


def candidate_workers(fleet: dict, worker_class: str | None, selected: str | None = None) -> list[dict]:
    workers = fleet.get("workers", [])
    if selected is not None:
        matches = [worker for worker in workers if worker.get("name") == selected]
        if not matches:
            raise ControllerError("conflict", f"unknown worker: {selected}")
        if worker_class is not None and matches[0].get("class") != worker_class:
            raise ControllerError("conflict", f"worker {selected} is not class {worker_class}")
        return [matches[0]]
    return [worker for worker in workers if worker_class is None or worker.get("class") == worker_class]


def project_config(fleet: dict, project: str) -> dict:
    projects = fleet.get("projects", {})
    if project not in projects:
        raise ControllerError("conflict", f"unknown project: {project}")
    spec = dict(projects[project])
    owner, name = repository_parts(spec["repository"])
    spec["name"] = project
    spec["repo"] = {"owner": owner, "name": name}
    spec["remote_url"] = f"https://github.com/{owner}/{name}.git"
    return spec


def repository_parts(repository: str) -> tuple[str, str]:
    parts = repository.split("/")
    if len(parts) != 2 or not all(parts):
        raise ControllerError("conflict", f"invalid repository: {repository}")
    return parts[0], parts[1]


def worker_host(fleet: dict, worker_name: str) -> str:
    worker = worker_config(fleet, worker_name)
    suffix = fleet["tailnet_suffix"]
    return f"{fleet['remote_user']}@{worker['name']}.{suffix}"


def worker_config(fleet: dict, worker_name: str) -> dict:
    for worker in fleet.get("workers", []):
        if worker.get("name") == worker_name:
            return worker
    raise ControllerError("conflict", f"unknown worker: {worker_name}")


def workspace_ssh_argv(
    fleet: dict,
    worker_name: str,
    request: dict,
    *,
    connect_timeout: int = 15,
    remote_root: str = DEFAULT_REMOTE_ROOT,
    recovery: bool = False,
) -> list[str]:
    encoded = base64.b64encode(canonical_json(request).encode("utf-8")).decode("ascii")
    remote_argv = [
        "python3",
        f"{remote_root}/remote/workspace.py",
        "--root",
        remote_root,
        "--request-base64",
        encoded,
    ]
    builder = recovery_ssh_argv if recovery else ssh_argv
    return builder(fleet, worker_name, remote_argv, connect_timeout=connect_timeout)


def ssh_argv(
    fleet: dict,
    worker_name: str,
    remote_argv: list[str],
    *,
    connect_timeout: int = 15,
) -> list[str]:
    if remote_argv and remote_argv[0] in {"apoc", "herdr"}:
        root = fleet.get("remote_root", DEFAULT_REMOTE_ROOT)
        if remote_argv[:3] == ["apoc", "execution", "start"]:
            delimiter = remote_argv.index("--")
            # APoC's daemon has its own environment. Pass only the shell's tool
            # PATH explicitly; subscription credentials stay in their files.
            invocation = shlex.join(remote_argv[:delimiter]) + ' --env "PATH=$PATH" -- ' + shlex.join(remote_argv[delimiter + 1:])
            remote_argv = ["bash", "-c", f"exec {invocation}"]
        command = f"cd {shlex.quote(root)} && /usr/local/bin/devenv shell -- {shlex.join(remote_argv)}"
        remote_argv = ["bash", "-lc", command]
    return [
        "ssh",
        *SSH_OPTIONS,
        "-o",
        f"ConnectTimeout={connect_timeout}",
        worker_host(fleet, worker_name),
        shlex.join(remote_argv),
    ]


def recovery_ssh_argv(
    fleet: dict,
    worker_name: str,
    remote_argv: list[str],
    *,
    connect_timeout: int = 15,
) -> list[str]:
    return [
        "ssh",
        *SSH_OPTIONS,
        "-o", "StrictHostKeyChecking=accept-new",
        "-o",
        f"ConnectTimeout={connect_timeout}",
        f"{fleet.get('remote_user', 'exedev')}@{worker_name}.exe.xyz",
        shlex.join(remote_argv),
    ]


def run_workspace_request(
    fleet: dict,
    worker_name: str,
    request: dict,
    *,
    timeout: int = 60,
    remote_root: str = DEFAULT_REMOTE_ROOT,
    recovery: bool = False,
) -> dict:
    argv = workspace_ssh_argv(fleet, worker_name, request, remote_root=remote_root, recovery=recovery)
    completed = run(argv, timeout=timeout, text=True)
    if completed["status"] != "completed":
        return completed
    if completed["returncode"] == 255 or "Permission denied" in completed["stderr"]:
        return failed("auth_failed", completed["stderr"].strip() or "SSH authentication failed", worker=worker_name)
    if completed["returncode"] != 0:
        return failed(
            "remote_failed",
            completed["stderr"].strip() or f"remote command exited {completed['returncode']}",
            worker=worker_name,
        )
    try:
        result = json.loads(completed["stdout"])
    except json.JSONDecodeError:
        return failed("remote_invalid_json", "remote workspace helper did not return JSON", worker=worker_name)
    result.setdefault("ok", result.get("status") in {"available", "claimed", "collected", "released"})
    result["worker"] = worker_name
    return result


def run(argv: list[str], *, timeout: int, text: bool) -> dict:
    try:
        completed = subprocess.run(
            argv,
            check=False,
            text=text,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            timeout=timeout,
        )
    except subprocess.TimeoutExpired as exc:
        stdout = decode_output(exc.stdout, text)
        stderr = decode_output(exc.stderr, text)
        return {
            "status": "pending",
            "ok": False,
            "error": "command timed out; remote state is unknown",
            "timeout": timeout,
            "stdout": stdout,
            "stderr": stderr,
        }
    return {
        "status": "completed",
        "returncode": completed.returncode,
        "stdout": completed.stdout,
        "stderr": completed.stderr,
    }


def run_with_input(argv: list[str], *, timeout: int, text: bool, input_data: bytes) -> dict:
    try:
        completed = subprocess.run(
            argv,
            check=False,
            text=False,
            input=input_data,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            timeout=timeout,
        )
    except subprocess.TimeoutExpired as exc:
        stdout = decode_output(exc.stdout, text)
        stderr = decode_output(exc.stderr, text)
        return {
            "status": "pending",
            "ok": False,
            "error": "command timed out; remote state is unknown",
            "timeout": timeout,
            "stdout": stdout,
            "stderr": stderr,
        }
    return {
        "status": "completed",
        "returncode": completed.returncode,
        "stdout": decode_output(completed.stdout, text),
        "stderr": decode_output(completed.stderr, text),
    }


def decode_output(value, text: bool):
    if value is None:
        return "" if text else b""
    if text and isinstance(value, bytes):
        return value.decode("utf-8", "replace")
    if not text and isinstance(value, str):
        return value.encode("utf-8", "replace")
    return value


def status(
    fleet: dict,
    request_id: str,
    *,
    worker: str | None = None,
    remote_root: str = DEFAULT_REMOTE_ROOT,
    local_state: Path = DEFAULT_STATE,
) -> dict:
    workers = candidate_workers(fleet, None, worker)
    if not workers:
        return ok("busy", workers=[])
    with concurrent.futures.ThreadPoolExecutor(max_workers=min(8, len(workers))) as executor:
        statuses = list(
            executor.map(
                lambda item: worker_status_summary(
                    fleet,
                    item,
                    f"{request_id}-{item['name']}",
                    remote_root=remote_root,
                    local_state=local_state,
                ),
                workers,
            )
        )
    aggregate = "available" if any(item.get("status") == "available" for item in statuses) else "busy"
    if any(item.get("connectivity", {}).get("status") in {"pending", "auth_failed", "remote_failed", "remote_invalid_json"} for item in statuses):
        aggregate = "partial"
    return ok(aggregate, workers=statuses)


def worker_status_summary(
    fleet: dict,
    worker_spec: dict,
    request_id: str,
    *,
    remote_root: str,
    local_state: Path,
) -> dict:
    worker = worker_spec["name"]
    agent_state = run_workspace_request(
        fleet,
        worker,
        {"operation": "status", "request_id": request_id},
        timeout=30,
        remote_root=remote_root,
    )
    auth = worker_auth_status(fleet, worker, remote_root=remote_root)
    active_tasks = []
    executions = []
    blockers = []
    for record in open_task_records_for_worker(local_state, worker):
        task = _task_record_summary(record)
        task["runtime"] = record.get("runtime") if isinstance(record.get("runtime"), dict) else {}
        activity = inspect_task_activity(fleet, worker, str(record.get("task_id", "")), record)
        task["activity"] = activity
        active_tasks.append(task)
        executions.extend(runtime_apoc_execution_ids(record))
        if activity.get("status") == "live_task_activity" or not activity.get("ok"):
            blockers.append({"kind": "task_activity", "task_id": record.get("task_id"), "status": activity.get("status"), "error": activity.get("error")})
    if agent_state.get("status") != "available":
        blockers.append({"kind": "agent_state", "status": agent_state.get("status"), "error": agent_state.get("error")})
    if auth.get("ready") is not True:
        blockers.append({"kind": "auth", "status": auth.get("status"), "error": auth.get("error")})
    if active_tasks:
        blockers.append({"kind": "ownership", "status": "worker_has_open_task", "tasks": [_task_record_summary(record) for record in open_task_records_for_worker(local_state, worker)]})
    return {
        "status": "available" if not blockers else "blocked",
        "ok": not blockers,
        "worker": worker,
        "capacity": declared_capacity(worker_spec),
        "connectivity": agent_state,
        "agent_state": agent_state,
        "auth": auth,
        "ownership": {"open_tasks": active_tasks},
        "executions": sorted(set(executions)),
        "blockers": blockers,
    }


def declared_capacity(worker_spec: dict) -> dict:
    return {
        key: worker_spec[key]
        for key in ("class", "cpus", "memory_gb", "disk_gb")
        if key in worker_spec
    }


def claim(
    fleet: dict,
    worker: str,
    project: str,
    task_id: str,
    revision: str,
    request_id: str,
    *,
    remote_root: str = DEFAULT_REMOTE_ROOT,
    source_bundle: str | None = None,
    reservation_id: str | None = None,
    session_id: str | None = None,
    local_state: Path = DEFAULT_STATE,
) -> dict:
    project_spec = project_config(fleet, project)
    worker_spec = worker_config(fleet, worker)
    if worker_spec.get("class") != project_spec.get("class"):
        raise ControllerError("conflict", f"worker {worker} is not class {project_spec['class']}")
    open_records = open_task_records_for_worker(local_state, worker)
    if open_records:
        return failed(
            "worker_has_open_task",
            "worker has a central task record that is not released",
            worker=worker,
            tasks=[_task_record_summary(record) for record in open_records],
        )
    request = {
        "operation": "claim",
        "request_id": request_id,
        "task_id": task_id,
        "repo": project_spec["repo"],
        "revision": revision,
        "branch": f"workenv/{task_id}",
    }
    if source_bundle:
        request["source_bundle"] = source_bundle
    else:
        request["remote_url"] = project_spec["remote_url"]
    result = run_workspace_request(fleet, worker, request, timeout=180, remote_root=remote_root)
    if reservation_id and result.get("status") in {"claimed", *AMBIGUOUS_REMOTE_STATUSES}:
        record_task(
            local_state,
            task_id=task_id,
            worker=worker,
            project=project,
            repo=project_spec["repo"],
            revision=revision,
            reservation_id=reservation_id,
            session_id=session_id,
            source="bundle" if source_bundle else "remote",
            status=result.get("status", "unknown"),
            worktree=result.get("worktree"),
        )
    return result


def collect(
    fleet: dict,
    worker: str,
    task_id: str,
    request_id: str,
    *,
    local_state: Path = DEFAULT_STATE,
    remote_root: str = DEFAULT_REMOTE_ROOT,
    evidence_paths: list[str] | None = None,
) -> dict:
    request = {
        "operation": "collect",
        "request_id": request_id,
        "task_id": task_id,
        "evidence_paths": evidence_paths or [],
    }
    remote = run_workspace_request(fleet, worker, request, timeout=180, remote_root=remote_root)
    if remote.get("status") != "collected":
        return remote
    local_dir = retrieve_collection(fleet, worker, task_id, remote, local_state=local_state)
    verified = verify_local_collection(local_dir, remote["collection_digest"], task_id=task_id)
    if not verified["ok"]:
        return verified
    remote["local_collection_dir"] = str(local_dir)
    return remote


def retrieve_collection(
    fleet: dict,
    worker: str,
    task_id: str,
    remote: dict,
    *,
    local_state: Path,
) -> Path:
    metadata_path = Path(remote["metadata_path"])
    remote_dir = metadata_path.parent.as_posix()
    expected_prefix = PurePosixPath(DEFAULT_REMOTE_ROOT) / "collections" / task_id
    if not PurePosixPath(remote_dir).is_relative_to(expected_prefix):
        raise ControllerError("conflict", "remote collection path is outside the worker collection root")
    digest = remote["collection_digest"]
    if not valid_digest(digest):
        raise ControllerError("conflict", "remote collection digest is not a lowercase SHA-256 hex digest")
    local_dir = local_state / "collections" / worker / task_id / digest
    if local_dir.exists():
        return local_dir
    local_dir.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(dir=local_dir.parent) as tmp:
        tmp_dir = Path(tmp)
        archive = fetch_remote_tar(fleet, worker, remote_dir)
        extract_tar_safely(archive, tmp_dir)
        verify_local_collection(tmp_dir, digest, task_id=task_id, require_ok=True)
        tmp_dir.replace(local_dir)
    return local_dir


def fetch_remote_tar(fleet: dict, worker: str, remote_dir: str) -> bytes:
    argv = ssh_argv(fleet, worker, ["tar", "-C", remote_dir, "-cf", "-", "."])
    completed = run(argv, timeout=180, text=False)
    if completed["status"] != "completed":
        raise ControllerError("pending", "collection retrieval timed out; remote state is unknown")
    if completed["returncode"] != 0:
        stderr = completed["stderr"].decode("utf-8", "replace")
        raise ControllerError("remote_failed", stderr.strip() or "remote tar failed")
    return completed["stdout"]


def extract_tar_safely(raw: bytes, destination: Path) -> None:
    destination.mkdir(parents=True, exist_ok=True)
    stream = io.BytesIO(raw)
    with tarfile.open(fileobj=stream, mode="r:*") as archive:
        for member in archive.getmembers():
            target = destination / member.name
            if not target.resolve().is_relative_to(destination.resolve()):
                raise ControllerError("conflict", f"collection archive path escapes destination: {member.name}")
            if member.issym() or member.islnk():
                raise ControllerError("conflict", f"collection archive contains a link: {member.name}")
        archive.extractall(destination)


def verify_local_collection(
    local_dir: Path,
    expected_digest: str,
    *,
    task_id: str | None = None,
    repo: dict | None = None,
    source: str | None = None,
    revision: str | None = None,
    require_ok: bool = False,
) -> dict:
    if not valid_digest(expected_digest):
        result = failed("uncollected", "collection digest is not a lowercase SHA-256 hex digest", local_collection_dir=str(local_dir))
        if require_ok:
            raise ControllerError(result["status"], result["error"])
        return result
    metadata_path = local_dir / "metadata.json"
    if not metadata_path.is_file():
        result = failed("uncollected", "local collection metadata is missing", local_collection_dir=str(local_dir))
        if require_ok:
            raise ControllerError(result["status"], result["error"])
        return result
    metadata = json.loads(metadata_path.read_text(encoding="utf-8"))
    if task_id is not None and metadata.get("task_id") != task_id:
        result = failed("uncollected", "local collection task_id does not match request", local_collection_dir=str(local_dir))
        if require_ok:
            raise ControllerError(result["status"], result["error"])
        return result
    if repo is not None and metadata.get("repo") != repo:
        result = failed("uncollected", "local collection repo does not match task record", local_collection_dir=str(local_dir))
        if require_ok:
            raise ControllerError(result["status"], result["error"])
        return result
    if source is not None and metadata.get("source") != source:
        result = failed("uncollected", "local collection source does not match task record", local_collection_dir=str(local_dir))
        if require_ok:
            raise ControllerError(result["status"], result["error"])
        return result
    if revision is not None and metadata.get("base") != revision:
        result = failed("uncollected", "local collection base revision does not match task record", local_collection_dir=str(local_dir))
        if require_ok:
            raise ControllerError(result["status"], result["error"])
        return result
    archives = metadata.get("archives", {})
    if not isinstance(archives, dict) or not archives:
        result = failed("uncollected", "local collection metadata has no archives", local_collection_dir=str(local_dir))
        if require_ok:
            raise ControllerError(result["status"], result["error"])
        return result
    archive_paths = []
    for name in archives.values():
        if not safe_archive_name(name):
            result = failed("uncollected", f"local collection archive name is unsafe: {name}", local_collection_dir=str(local_dir))
            if require_ok:
                raise ControllerError(result["status"], result["error"])
            return result
        archive_path = local_dir / name
        if not archive_path.is_file():
            result = failed("uncollected", f"local collection archive is missing: {name}", local_collection_dir=str(local_dir))
            if require_ok:
                raise ControllerError(result["status"], result["error"])
            return result
        archive_paths.append(archive_path)
    metadata_without_digest = dict(metadata)
    metadata_without_digest.pop("collection_digest", None)
    actual_digest = collection_digest(metadata_without_digest, archive_paths)
    if metadata.get("collection_digest") != expected_digest or actual_digest != expected_digest:
        result = failed("uncollected", "local collection digest does not match archive bytes", local_collection_dir=str(local_dir))
        if require_ok:
            raise ControllerError(result["status"], result["error"])
        return result
    return ok("verified", local_collection_dir=str(local_dir), metadata=metadata)


def release(
    fleet: dict,
    worker: str,
    task_id: str,
    request_id: str,
    collection_digest: str,
    *,
    local_state: Path = DEFAULT_STATE,
    remote_root: str = DEFAULT_REMOTE_ROOT,
) -> dict:
    task_record = read_task_record(local_state, task_id)
    if task_record is None:
        return failed("untracked_task", "central task record is missing", task_id=task_id)
    if task_record.get("worker") != worker:
        return failed("task_worker_mismatch", "central task record is bound to a different worker", task_id=task_id)
    local_dir = local_state / "collections" / worker / task_id / collection_digest
    verified = verify_local_collection(
        local_dir,
        collection_digest,
        task_id=task_id,
        repo=task_record.get("repo"),
        source=task_record.get("source"),
        revision=task_record.get("revision"),
    )
    if not verified["ok"]:
        return verified
    service_stop = stop_recorded_task_services(fleet, worker, task_id, request_id, task_record)
    if not service_stop["ok"]:
        return service_stop
    activity = inspect_task_activity(fleet, worker, task_id, task_record)
    if not activity["ok"]:
        return activity
    request = {
        "operation": "release",
        "request_id": request_id,
        "task_id": task_id,
        "collection_digest": collection_digest,
    }
    remote = run_workspace_request(fleet, worker, request, timeout=120, remote_root=remote_root)
    if remote.get("status") == "released":
        remote["local_collection_dir"] = str(local_dir)
        record_task(
            local_state,
            task_id=task_record["task_id"],
            worker=task_record["worker"],
            project=task_record["project"],
            repo=task_record["repo"],
            revision=task_record["revision"],
            reservation_id=task_record["reservation_id"],
            session_id=task_record.get("session_id"),
            source=task_record["source"],
            status="released",
            worktree=task_record.get("worktree"),
        )
    return remote


def stop_recorded_task_services(fleet: dict, worker: str, task_id: str, request_id: str, task_record: dict) -> dict:
    stopped = []
    inspected = []
    recorded_ids = recorded_apoc_execution_ids(task_record)
    for execution_id in recorded_ids:
        execution = get_remote_apoc_execution(fleet, worker, task_id, execution_id)
        if not execution["ok"]:
            return execution
        payload = execution["execution"]
        inspected.append(execution_id)
        state = apoc_execution_state(payload)
        if state == "terminal":
            continue
        if state == "unknown":
            return failed("task_activity_unknown", "recorded remote APoC execution liveness is unknown", execution_id=execution_id)
        labels = apoc_execution_labels(payload)
        if labels.get("workenv.task_id") != task_id or labels.get("workenv.worker") != worker:
            return failed(
                "unmatched_execution",
                "recorded remote APoC execution labels do not match this task and worker",
                execution_id=execution_id,
                labels=labels,
            )
        if labels.get("workenv.kind") != "service":
            return failed(
                "live_task_activity",
                "recorded non-service APoC execution is still live",
                live=[{"kind": "apoc_execution", "id": execution_id, "labels": labels, "status": payload.get("status"), "outcome": payload.get("outcome")}],
            )
        canceled = cancel_remote_apoc_execution(fleet, worker, task_id, request_id, execution_id)
        if not canceled["ok"]:
            return canceled
        stopped.append({"id": execution_id, "cancel": canceled["cancel"]})
    return ok("services_stopped", inspected_execution_ids=inspected, stopped=stopped)


def get_remote_apoc_execution(fleet: dict, worker: str, task_id: str, execution_id: str) -> dict:
    completed = run(
        ssh_argv(
            fleet,
            worker,
            [
                "apoc",
                "execution",
                "get",
                execution_id,
                "--purpose",
                f"Inspect recorded workenv task {task_id} execution {execution_id} before release.",
                "--verbosity",
                "trace",
                "--format",
                "json",
            ],
        ),
        timeout=60,
        text=True,
    )
    if completed["status"] != "completed" or completed["returncode"] != 0:
        return failed("task_activity_unknown", "could not inspect recorded remote APoC execution", execution_id=execution_id)
    try:
        payload = json.loads(completed["stdout"])
    except json.JSONDecodeError:
        return failed("task_activity_unknown", "remote APoC execution get did not return JSON", execution_id=execution_id)
    execution = payload.get("data", payload) if isinstance(payload, dict) else None
    if not isinstance(execution, dict):
        return failed("task_activity_unknown", "remote APoC execution get did not return an object", execution_id=execution_id)
    return ok("inspected", execution_id=execution_id, execution=execution)


def cancel_remote_apoc_execution(fleet: dict, worker: str, task_id: str, request_id: str, execution_id: str) -> dict:
    completed = run(
        ssh_argv(
            fleet,
            worker,
            [
                "apoc",
                "execution",
                "cancel",
                execution_id,
                "--idempotency-key",
                f"{request_id}-service-{execution_id}",
                "--timeout-ms",
                "5000",
                "--purpose",
                f"Stop workenv service execution {execution_id} for task {task_id}.",
                "--format",
                "json",
            ],
        ),
        timeout=60,
        text=True,
    )
    if completed["status"] != "completed":
        return completed
    if completed["returncode"] != 0:
        return failed("service_stop_failed", completed["stderr"].strip() or "remote APoC execution cancel failed", execution_id=execution_id)
    try:
        payload = json.loads(completed["stdout"]) if completed["stdout"].strip() else {}
    except json.JSONDecodeError:
        return failed("service_stop_failed", "remote APoC execution cancel did not return JSON", execution_id=execution_id)
    return ok("service_stopped", execution_id=execution_id, cancel=payload)


def task_run(
    fleet: dict,
    worker: str,
    task_id: str,
    request_id: str,
    purpose: str,
    command: list[str],
    *,
    local_state: Path = DEFAULT_STATE,
    service: bool = False,
) -> dict:
    if not command:
        return failed("conflict", "task-run requires a command")
    record = read_task_record(local_state, task_id)
    if record is None:
        return failed("untracked_task", "central task record is missing", task_id=task_id)
    if record.get("worker") != worker:
        return failed("task_worker_mismatch", "central task record is bound to a different worker", task_id=task_id)
    guard = require_runnable_task(record)
    if not guard["ok"]:
        return guard
    worktree = record.get("worktree")
    if not worktree:
        return failed("untracked_task", "central task record has no remote worktree", task_id=task_id)
    executable, *args = command
    remote_argv = [
        "apoc",
        "execution",
        "start",
        executable,
        "--cwd",
        worktree,
        "--idempotency-key",
        request_id,
        "--purpose",
        purpose,
        "--label",
        f"workenv.task_id={task_id}",
        "--label",
        f"workenv.worker={worker}",
        "--label",
        f"workenv.kind={'service' if service else 'task'}",
        "--format",
        "json",
        "--",
        *args,
    ]
    completed = run(ssh_argv(fleet, worker, remote_argv), timeout=60, text=True)
    if completed["status"] != "completed":
        return completed
    if completed["returncode"] != 0:
        return failed("remote_failed", completed["stderr"].strip() or "remote APoC execution start failed", worker=worker)
    try:
        payload = json.loads(completed["stdout"])
    except json.JSONDecodeError:
        return failed("remote_invalid_json", "remote APoC execution start did not return JSON", worker=worker)
    execution = payload.get("execution", payload)
    execution_id = execution.get("id") or payload.get("id")
    if not execution_id:
        return failed("remote_invalid_json", "remote APoC execution start returned no execution id", worker=worker)
    recorded = record_remote_execution(local_state, task_id, execution_id)
    if not recorded["ok"]:
        return recorded
    runtime_recorded = record_runtime(
        fleet,
        worker,
        task_id,
        record["revision"],
        f"{request_id}-runtime",
        {"apoc_execution_ids": [execution_id]},
        local_state=local_state,
    )
    return ok("started", worker=worker, task_id=task_id, execution_id=execution_id, remote=payload, runtime_recorded=runtime_recorded)


def record_runtime(
    fleet: dict,
    worker: str,
    task_id: str,
    revision: str,
    request_id: str,
    runtime: dict,
    *,
    local_state: Path = DEFAULT_STATE,
    remote_root: str = DEFAULT_REMOTE_ROOT,
) -> dict:
    record = read_task_record(local_state, task_id)
    if record is None:
        return failed("untracked_task", "central task record is missing", task_id=task_id)
    if record.get("worker") != worker:
        return failed("task_worker_mismatch", "central task record is bound to a different worker", task_id=task_id)
    if record.get("revision") != revision:
        return failed("conflict", "runtime revision does not match central task record", task_id=task_id)
    guard = require_runnable_task(record)
    if not guard["ok"]:
        return guard
    request = {
        "operation": "record-runtime",
        "request_id": request_id,
        "task_id": task_id,
        "revision": revision,
        "runtime": runtime,
    }
    remote = run_workspace_request(fleet, worker, request, timeout=120, remote_root=remote_root)
    if remote.get("status") == "runtime_recorded":
        record["runtime"] = remote.get("runtime", merge_runtime(record.get("runtime"), runtime))
        record["status"] = "runtime_recorded"
        record["updated_at"] = int(time.time() * 1000)
        write_json_atomic(task_record_path(local_state, task_id), record)
    return remote


def ensure(
    fleet: dict,
    worker: str,
    request_id: str,
    *,
    reconcile: bool = False,
    register_herdr: bool = False,
    registration_target: str | None = None,
    remote_root: str = DEFAULT_REMOTE_ROOT,
    local_state: Path = DEFAULT_STATE,
    provider_create: bool = False,
) -> dict:
    target = None
    if register_herdr:
        host = worker_host(fleet, worker)
        target = registration_target or host
        if target != host:
            return failed("registration_target_mismatch", "Herdr target must be the selected worker host", worker=worker)

    provider = ensure_provider_worker(fleet, worker, request_id, local_state=local_state, create=provider_create)
    if provider.get("status") != "present":
        return {**provider, "provider": provider}

    tailscale_before = tailscale_status(fleet, worker)
    use_recovery = not tailscale_before.get("ok")
    source_sync = {"status": "skipped"}
    enrollment = {"status": "skipped"}
    if use_recovery and not reconcile:
        return failed(
            "needs_reconcile",
            "worker is present but Tailscale SSH is not ready",
            worker=worker,
            provider=provider,
            tailscale=tailscale_before,
        )

    if reconcile:
        open_records = open_task_records_for_worker(local_state, worker)
        if open_records:
            return failed(
                "worker_has_open_task",
                "worker has a central task record that is not released",
                worker=worker,
                provider=provider,
                tasks=[_task_record_summary(record) for record in open_records],
            )
        if use_recovery:
            source_sync = sync_worker_sources(fleet, worker, remote_root=remote_root, recovery=True)
            if not source_sync["ok"]:
                return {**source_sync, "provider": provider, "tailscale": tailscale_before}
        remote_status = run_workspace_request(
            fleet,
            worker,
            {"operation": "status", "request_id": f"{request_id}-idle"},
            timeout=60,
            remote_root=remote_root,
            recovery=use_recovery,
        )
        if remote_status.get("status") != "available":
            return failed(
                "worker_busy",
                "worker is not idle for reconciliation",
                worker=worker,
                provider=provider,
                tailscale=tailscale_before,
                remote_status=remote_status,
            )
        if not use_recovery:
            source_sync = sync_worker_sources(fleet, worker, remote_root=remote_root, recovery=False)
            if not source_sync["ok"]:
                return {**source_sync, "provider": provider, "tailscale": tailscale_before, "remote_status": remote_status}
        install_args = [f"{remote_root}/bootstrap/bootstrap.sh"]
        builder = recovery_ssh_argv if use_recovery else ssh_argv
        installed = run(builder(fleet, worker, ["bash", "-lc", shlex.join(install_args)]), timeout=900, text=True)
        if installed["status"] != "completed":
            return {**installed, "provider": provider, "tailscale": tailscale_before, "source_sync": source_sync}
        if installed["returncode"] == 255 or "Permission denied" in installed["stderr"]:
            return failed("auth_failed", installed["stderr"].strip() or "SSH authentication failed", worker=worker, provider=provider, source_sync=source_sync)
        if installed["returncode"] != 0:
            return failed("bootstrap_failed", installed["stderr"].strip() or "bootstrap failed", worker=worker, provider=provider, source_sync=source_sync)

    builder = recovery_ssh_argv if use_recovery else ssh_argv
    bootstrap_args = [f"{remote_root}/bootstrap/bootstrap.sh", "--health-only", "--json"]
    result = run(builder(fleet, worker, ["bash", "-lc", shlex.join(bootstrap_args)]), timeout=120, text=True)
    if result["status"] != "completed":
        return {**result, "provider": provider, "tailscale": tailscale_before, "source_sync": source_sync}
    if result["returncode"] == 255 or "Permission denied" in result["stderr"]:
        return failed("auth_failed", result["stderr"].strip() or "SSH authentication failed", worker=worker, provider=provider, source_sync=source_sync)
    if result["returncode"] != 0:
        return failed("bootstrap_failed", result["stderr"].strip() or "bootstrap failed", worker=worker, provider=provider, source_sync=source_sync)
    try:
        health = json.loads(result["stdout"])
    except json.JSONDecodeError:
        return failed("bootstrap_invalid_json", "bootstrap did not return JSON", worker=worker, provider=provider, source_sync=source_sync)

    tools = worker_tool_status(fleet, worker, remote_root=remote_root, recovery=use_recovery)
    herdr = {"status": "skipped"}
    if tools.get("tools_ready") is True:
        herdr = start_worker_herdr(fleet, worker, request_id, remote_root=remote_root, recovery=use_recovery)
        if not herdr.get("ok"):
            return {
                **herdr,
                "worker": worker,
                "provider": provider,
                "health": health,
                "tools": tools,
                "tailscale_before": tailscale_before,
                "source_sync": source_sync,
            }

    tailscale = tailscale_status(fleet, worker)
    if reconcile and not tailscale.get("ok"):
        enrollment = enroll_worker_if_credentials_available(fleet, worker, request_id, local_state=local_state)
        if enrollment.get("status") == "auth_required" or not enrollment.get("ok"):
            return {
                **enrollment,
                "worker": worker,
                "provider": provider,
                "health": health,
                "tools": tools,
                "herdr": herdr,
                "tailscale": tailscale,
                "tailscale_before": tailscale_before,
                "source_sync": source_sync,
            }
        tailscale = tailscale_status(fleet, worker)
        use_recovery = not tailscale.get("ok")

    auth = worker_auth_status(fleet, worker, remote_root=remote_root, recovery=use_recovery)
    registration = {"status": "skipped"}
    if register_herdr and tailscale.get("ok"):
        registration = register_herdr_machine(fleet, worker, target)
    elif register_herdr:
        registration = failed("herdr_registration_skipped", "Tailscale SSH is not ready for Herdr registration", worker=worker)
    missing = health.get("missing_prerequisites") or health.get("missing_tools") or []
    ready = (
        not missing
        and tools.get("tools_ready") is True
        and tools.get("nib_auth", {}).get("authenticated") is True
        and herdr.get("herdr_ready") is True
        and tailscale.get("ok")
        and registration.get("ok", True)
        and auth.get("ready") is True
    )
    return {
        "status": "ready" if ready else "needs_reconcile",
        "ok": ready,
        "worker": worker,
        "provider": provider,
        "health": health,
        "tools": tools,
        "herdr": herdr,
        "auth": auth,
        "tailscale": tailscale,
        "tailscale_before": tailscale_before,
        "source_sync": source_sync,
        "enrollment": enrollment,
        "registration": registration,
        "missing_tools": missing,
    }


def worker_tool_status(fleet: dict, worker: str, *, remote_root: str = DEFAULT_REMOTE_ROOT, recovery: bool = False) -> dict:
    builder = recovery_ssh_argv if recovery else ssh_argv
    command = f"cd {shlex.quote(remote_root)} && /usr/local/bin/devenv shell -- python3 remote/tool_health.py --require-nix"
    completed = run(builder(fleet, worker, ["bash", "-lc", command]), timeout=900, text=True)
    try:
        payload = json.loads(completed.get("stdout", ""))
    except (TypeError, ValueError):
        payload = None
    if completed.get("status") != "completed" or completed.get("returncode") != 0 or not isinstance(payload, dict) or payload.get("schema") != 1:
        return failed("tools_unknown", "shared devenv tool verification did not pass", worker=worker)
    return payload


def start_worker_herdr(fleet: dict, worker: str, request_id: str, *, remote_root: str = DEFAULT_REMOTE_ROOT, recovery: bool = False) -> dict:
    session = fleet.get("herdr_session", "workenv")
    builder = recovery_ssh_argv if recovery else ssh_argv
    command = (
        f"WORKENV_ROOT={shlex.quote(remote_root)} "
        f"WORKENV_HERDR_SESSION={shlex.quote(session)} "
        "/opt/workenv/bin/workenv-herdr-bootstrap"
    )
    completed = run(builder(fleet, worker, ["bash", "-lc", command]), timeout=120, text=True)
    if completed.get("status") != "completed":
        return completed
    if completed.get("returncode") != 0:
        return failed("herdr_boot_failed", completed.get("stderr", "").strip() or "worker Herdr boot helper failed", worker=worker)
    status = worker_herdr_status(fleet, worker, remote_root=remote_root, recovery=recovery)
    if not status.get("ok"):
        return {**status, "boot_stdout": completed.get("stdout", "").strip()}
    return ok("herdr_ready", worker=worker, boot_stdout=completed.get("stdout", "").strip(), herdr_status=status, herdr_ready=True)


def worker_herdr_status(fleet: dict, worker: str, *, remote_root: str = DEFAULT_REMOTE_ROOT, recovery: bool = False) -> dict:
    session = fleet.get("herdr_session", "workenv")
    builder = recovery_ssh_argv if recovery else ssh_argv
    command = f"cd {shlex.quote(remote_root)} && /usr/local/bin/devenv shell -- herdr --session {shlex.quote(session)} status server --json"
    completed = run(builder(fleet, worker, ["bash", "-lc", command]), timeout=60, text=True)
    if completed.get("status") != "completed" or completed.get("returncode") != 0:
        return failed("herdr_unknown", "could not inspect worker Herdr server", worker=worker)
    try:
        payload = json.loads(completed.get("stdout", ""))
    except json.JSONDecodeError:
        return failed("herdr_unknown", "worker Herdr server status returned invalid JSON", worker=worker)
    if not isinstance(payload, dict):
        return failed("herdr_unknown", "worker Herdr server status returned an unknown schema", worker=worker)
    capabilities = payload.get("capabilities") if isinstance(payload.get("capabilities"), dict) else {}
    ready = (
        payload.get("running") is True
        and payload.get("compatible") is True
        and payload.get("version") == "0.9.0"
        and payload.get("protocol_version") == 22
        and payload.get("server_binary_stale") is False
        and capabilities.get("detached_server_daemon") is True
    )
    if not ready:
        status = "herdr_stale" if payload.get("server_binary_stale") is True else "herdr_not_ready"
        return failed(status, "worker Herdr server is not healthy", worker=worker, herdr_ready=False, server=payload)
    return ok("herdr_ready", worker=worker, herdr_ready=True, server=payload)


def worker_auth_status(fleet: dict, worker: str, *, remote_root: str = DEFAULT_REMOTE_ROOT, recovery: bool = False) -> dict:
    builder = recovery_ssh_argv if recovery else ssh_argv
    command = f"cd {shlex.quote(remote_root)} && /usr/local/bin/devenv shell -- python3 remote/health.py"
    completed = run(builder(fleet, worker, ["bash", "-lc", command]), timeout=120, text=True)
    if completed.get("status") != "completed" or completed.get("returncode") != 0:
        return failed("auth_unknown", "could not inspect worker subscription authentication", worker=worker)
    try:
        health = json.loads(completed["stdout"])
    except json.JSONDecodeError:
        return failed("auth_unknown", "worker authentication probe returned invalid JSON", worker=worker)
    if not isinstance(health, dict) or health.get("schema") != 1:
        return failed("auth_unknown", "worker authentication probe returned an unknown schema", worker=worker)
    return health


def ensure_provider_worker(fleet: dict, worker: str, request_id: str, *, local_state: Path, create: bool) -> dict:
    try:
        return Provider(fleet, local_state / "provider").ensure(worker, request_id, create=create)
    except (ProviderError, KeyError, ValueError, OSError) as exc:
        return failed(getattr(exc, "status", "provider_unknown"), str(exc), worker=worker)


def sync_worker_sources(fleet: dict, worker: str, *, remote_root: str, recovery: bool) -> dict:
    artifacts = required_tool_artifacts()
    if not artifacts["ok"]:
        return artifacts
    raw = build_source_archive(artifacts["paths"])
    remote = (
        "import pathlib,sys,tarfile; "
        "root=pathlib.Path(sys.argv[1]); root.mkdir(parents=True,exist_ok=True); "
        "archive=tarfile.open(fileobj=sys.stdin.buffer,mode='r|gz'); "
        "archive.extractall(path=root,filter='data'); "
        "assert (root/'devenv.nix').is_file(); "
        "assert (root/'remote/tool_health.py').is_file(); "
        "print('workenv-source-sync-v1')"
    )
    builder = recovery_ssh_argv if recovery else ssh_argv
    completed = run_with_input(builder(fleet, worker, ["python3", "-c", remote, remote_root]), timeout=180, text=True, input_data=raw)
    if completed["status"] != "completed":
        return completed
    if completed["returncode"] == 255 or "Permission denied" in completed["stderr"]:
        return failed("auth_failed", completed["stderr"].strip() or "SSH authentication failed", worker=worker)
    if completed["returncode"] != 0:
        return failed("source_sync_failed", completed["stderr"].strip() or "source sync failed", worker=worker)
    if completed["stdout"].strip() != "workenv-source-sync-v1":
        return failed("source_sync_unverified", "worker did not acknowledge the extracted environment files", worker=worker)
    return ok("synced", worker=worker, tools_dir=f"{remote_root}/tool-builds")


def required_tool_artifacts() -> dict:
    paths = [TOOL_BUILDS_DIR / APOC_TOOL_ARCHIVE, TOOL_BUILDS_DIR / APOC_TOOL_SHA]
    missing = [str(path) for path in paths if not path.is_file()]
    if missing:
        return failed("missing_tool_artifact", "required pinned APoC tool artifact is missing", missing=missing)
    return ok("tool_artifacts", paths=paths)


def enrollment_credentials_path(local_state: Path) -> Path:
    return local_state.parent / "tailscale-oauth.local.json"


def enroll_worker_if_credentials_available(fleet: dict, worker: str, request_id: str, *, local_state: Path) -> dict:
    credentials = enrollment_credentials_path(local_state)
    if not credentials.is_file():
        return failed("auth_required", "Tailscale OAuth credentials are required to enroll this worker", worker=worker)
    safe_request = re.sub(r"[^A-Za-z0-9._-]+", "_", request_id)[:80] or "request"
    enrollment_state = local_state.parent / "enrollment"
    fleet_path = enrollment_state / f"fleet-{safe_request}.json"
    write_json_atomic(fleet_path, fleet)
    try:
        return EnrollmentDriver(
            fleet_path=fleet_path,
            state_dir=enrollment_state,
            credentials_file=credentials,
            timeout=20,
        ).handle({"operation": "enroll", "request_id": f"{request_id}-enroll", "worker": worker})
    except EnrollmentError as exc:
        return failed(exc.status, exc.message, worker=worker)


def build_source_archive(tool_paths: list[Path]) -> bytes:
    stream = io.BytesIO()
    with tarfile.open(fileobj=stream, mode="w:gz") as archive:
        for relative_root in ("bootstrap", "remote", "devenv"):
            root = DEFAULT_ROOT / relative_root
            for path in sorted(root.rglob("*")):
                if path.is_file() and "__pycache__" not in path.parts and path.suffix != ".pyc":
                    archive.add(path, arcname=path.relative_to(DEFAULT_ROOT).as_posix())
        for name in ("AGENTS.md", "devenv.nix", "devenv.yaml", "devenv.lock", "tools.json"):
            path = DEFAULT_ROOT / name
            if path.is_file():
                archive.add(path, arcname=name)
        for path in tool_paths:
            archive.add(path, arcname=f"tool-builds/{path.name}")
    return stream.getvalue()


def register_herdr_machine(fleet: dict, worker: str, target: str) -> dict:
    session = fleet.get("herdr_session", "workenv")
    listed = run(["herdr", "machine", "list", "--json"], timeout=60, text=True)
    if listed["status"] != "completed":
        return listed
    if listed["returncode"] != 0:
        return failed("herdr_registration_failed", listed["stderr"].strip() or "herdr machine list failed")
    try:
        machines = json.loads(listed["stdout"])
    except json.JSONDecodeError:
        return failed("herdr_registration_failed", "herdr machine list did not return JSON")
    for machine in machines if isinstance(machines, list) else machines.get("machines", []):
        if machine.get("target") != target and machine.get("label") != worker:
            continue
        if machine.get("target") == target and machine.get("session") == session and machine.get("enabled") is True:
            return ok("already_registered", target=target, machine_id=machine.get("id"), session=session)
        return failed("herdr_profile_mismatch", "existing worker profile has a different target, session, or enabled state", machine=machine)
    completed = run(
        ["herdr", "machine", "add", target, "--label", worker, "--remote-session", session],
        timeout=60,
        text=True,
    )
    if completed["status"] != "completed":
        return completed
    if completed["returncode"] != 0:
        return failed("herdr_registration_failed", completed["stderr"].strip() or "herdr registration failed")
    return ok("registered", stdout=completed["stdout"].strip())


def tailscale_status(fleet: dict, worker: str) -> dict:
    completed = run(ssh_argv(fleet, worker, ["tailscale", "status", "--json"]), timeout=60, text=True)
    if completed["status"] != "completed":
        return completed
    try:
        payload = json.loads(completed["stdout"])
    except json.JSONDecodeError:
        return failed("tailscale_status_invalid_json", "tailscale status did not return JSON", worker=worker)
    if not isinstance(payload, dict):
        return failed("tailscale_status_invalid_json", "tailscale status did not return an object", worker=worker)
    if completed["returncode"] != 0 and payload.get("BackendState") != "NeedsLogin":
        return failed("tailscale_status_failed", completed["stderr"].strip() or "tailscale status failed", worker=worker)

    expected_dns = f"{worker}.{fleet['tailnet_suffix']}"
    expected_tag = fleet.get("tailscale_tag", "tag:workenv")
    self_status = payload.get("Self") if isinstance(payload.get("Self"), dict) else {}
    tailnet = payload.get("CurrentTailnet") if isinstance(payload.get("CurrentTailnet"), dict) else {}
    dns_name = str(self_status.get("DNSName", "")).rstrip(".")
    tailnet_suffix = tailnet.get("MagicDNSSuffix") or tailnet.get("Name")
    backend_state = payload.get("BackendState")
    tags = self_status.get("Tags", [])
    tags = tags if isinstance(tags, list) else []

    if backend_state != "Running":
        if not self_status and not tailnet:
            return failed("tailscale_not_enrolled", "worker is not enrolled in Tailscale", worker=worker, backend_state=backend_state)
        return failed("tailscale_inactive", f"worker tailscale backend is {backend_state or '<missing>'}", worker=worker, dns_name=dns_name, tailnet=tailnet_suffix)
    if dns_name != expected_dns or tailnet_suffix != fleet["tailnet_suffix"]:
        return failed(
            "tailscale_mismatch",
            "worker Tailscale identity does not match fleet",
            worker=worker,
            dns_name=dns_name,
            expected_dns=expected_dns,
            tailnet=tailnet_suffix,
            expected_tailnet=fleet["tailnet_suffix"],
        )
    if expected_tag not in tags:
        return failed("tailscale_configuration_blocked", f"worker is missing required Tailscale tag {expected_tag}", worker=worker, tags=tags)

    prefs = tailscale_prefs(fleet, worker)
    if not prefs["ok"]:
        return prefs
    if prefs["prefs"].get("WantRunning") is not True:
        return failed("tailscale_configuration_blocked", "worker Tailscale is not configured to stay running", worker=worker, prefs=prefs["prefs"])
    if prefs["prefs"].get("RunSSH") is not True:
        return failed("tailscale_configuration_blocked", "worker Tailscale SSH is disabled", worker=worker, prefs=prefs["prefs"])
    return ok(
        "tailscale_ready",
        worker=worker,
        self=self_status,
        backend_state=backend_state,
        dns_name=dns_name,
        tailnet=tailnet_suffix,
        tag=expected_tag,
        prefs=prefs["prefs"],
    )


def tailscale_prefs(fleet: dict, worker: str) -> dict:
    completed = run(ssh_argv(fleet, worker, ["tailscale", "debug", "prefs"]), timeout=60, text=True)
    if completed["status"] != "completed":
        return completed
    if completed["returncode"] != 0:
        return failed("tailscale_prefs_failed", completed["stderr"].strip() or "tailscale debug prefs failed", worker=worker)
    try:
        prefs = json.loads(completed["stdout"])
    except json.JSONDecodeError:
        return failed("tailscale_prefs_invalid_json", "tailscale debug prefs did not return JSON", worker=worker)
    if not isinstance(prefs, dict):
        return failed("tailscale_prefs_invalid_json", "tailscale debug prefs did not return an object", worker=worker)
    return ok("tailscale_prefs", worker=worker, prefs={key: prefs.get(key) for key in ("WantRunning", "RunSSH")})


def inspect_task_activity(fleet: dict, worker: str, task_id: str, task_record: dict) -> dict:
    apoc_activity = inspect_remote_apoc_activity(fleet, worker, task_id, task_record)
    if not apoc_activity["ok"]:
        return apoc_activity
    herdr_activity = inspect_remote_herdr_activity(fleet, worker, task_id, task_record)
    if not herdr_activity["ok"]:
        return herdr_activity
    live = apoc_activity.get("live_executions", []) + herdr_activity.get("live_panes", [])
    if live:
        return failed("live_task_activity", "task still has live remote APoC executions or Herdr panes", live=live)
    return ok("clear", apoc=apoc_activity, herdr=herdr_activity)


def inspect_remote_apoc_activity(fleet: dict, worker: str, task_id: str, task_record: dict) -> dict:
    live = []
    recorded_ids = recorded_apoc_execution_ids(task_record)
    for execution_id in recorded_ids:
        completed = run(
            ssh_argv(
                fleet,
                worker,
                ["apoc", "execution", "get", execution_id, "--purpose", f"Inspect workenv task {task_id} execution.", "--format", "json"],
            ),
            timeout=60,
            text=True,
        )
        if completed["status"] != "completed" or completed["returncode"] != 0:
            return failed("task_activity_unknown", "could not inspect recorded remote APoC execution", execution_id=execution_id)
        try:
            execution = json.loads(completed["stdout"])
        except json.JSONDecodeError:
            return failed("task_activity_unknown", "remote APoC execution get did not return JSON", execution_id=execution_id)
        state = apoc_execution_state(execution)
        if state == "live":
            live.append({"kind": "apoc_execution", "id": execution_id, "status": execution.get("status"), "outcome": execution.get("outcome")})
        elif state == "unknown":
            return failed("task_activity_unknown", "remote APoC execution liveness is unknown", execution_id=execution_id, execution=execution)

    completed = run(
        ssh_argv(
            fleet,
            worker,
            ["apoc", "execution", "list", "--purpose", f"List workenv task {task_id} executions.", "--format", "json", "--limit", "100"],
        ),
        timeout=60,
        text=True,
    )
    if completed["status"] != "completed" or completed["returncode"] != 0:
        return failed("task_activity_unknown", "could not list remote APoC executions")
    try:
        payload = json.loads(completed["stdout"])
    except json.JSONDecodeError:
        return failed("task_activity_unknown", "remote APoC execution list did not return JSON")
    if payload.get("next_cursor"):
        return failed("task_activity_unknown", "remote APoC execution list is truncated")
    if not isinstance(payload.get("executions"), list):
        return failed("task_activity_unknown", "remote APoC execution list did not return executions")
    worktree = task_record.get("worktree")
    for execution in payload["executions"]:
        if execution.get("cwd_truncated"):
            return failed("task_activity_unknown", "remote APoC execution cwd is truncated", execution_id=execution.get("id"))
        if execution.get("status") not in LIVE_EXECUTION_STATUSES:
            continue
        cwd = execution.get("cwd")
        if worktree and (cwd == worktree or (isinstance(cwd, str) and cwd.startswith(f"{worktree}/"))):
            live.append({"kind": "apoc_execution", "id": execution.get("id"), "status": execution.get("status"), "cwd": cwd})
    return ok("apoc_clear", live_executions=live, inspected_execution_ids=recorded_ids)


def apoc_execution_state(execution: dict) -> str:
    outcome = execution.get("outcome")
    if outcome == "pending":
        return "live"
    if outcome in TERMINAL_EXECUTION_OUTCOMES:
        return "terminal"
    status = execution.get("status")
    if status in LIVE_EXECUTION_STATUSES:
        return "live"
    if status in TERMINAL_EXECUTION_STATUSES:
        return "terminal"
    return "unknown"


def apoc_execution_labels(execution: dict) -> dict:
    spec = execution.get("spec") if isinstance(execution.get("spec"), dict) else {}
    labels = spec.get("labels", {})
    if isinstance(labels, dict):
        return {str(key): str(value) for key, value in labels.items() if isinstance(key, str) and isinstance(value, str)}
    if isinstance(labels, list):
        parsed = {}
        for label in labels:
            if isinstance(label, str) and "=" in label:
                key, value = label.split("=", 1)
                parsed[key] = value
        return parsed
    return {}


def inspect_remote_herdr_activity(fleet: dict, worker: str, task_id: str, task_record: dict) -> dict:
    live = []
    pane_ids = runtime_herdr_ids(task_record, "pane_ids")
    session = fleet.get("herdr_session", "workenv")
    for pane_id in pane_ids:
        completed = run(
            ssh_argv(fleet, worker, ["herdr", "--session", session, "pane", "get", pane_id]),
            timeout=60,
            text=True,
        )
        if completed["status"] != "completed" or completed["returncode"] != 0:
            return failed("task_activity_unknown", "could not inspect recorded remote Herdr pane", pane_id=pane_id)
        try:
            payload = json.loads(completed["stdout"])
        except json.JSONDecodeError:
            return failed("task_activity_unknown", "remote Herdr pane get did not return JSON", pane_id=pane_id)
        if payload.get("error"):
            code = payload.get("error", {}).get("code")
            if code in {"pane_not_found", "not_found", "server_not_running"}:
                continue
            return failed("task_activity_unknown", "remote Herdr pane state is unknown", pane_id=pane_id, herdr=payload)
        pane = payload.get("result", payload)
        if pane:
            live.append({"kind": "herdr_pane", "pane_id": pane_id, "pane": pane})
    return ok("herdr_clear", live_panes=live, inspected_pane_ids=pane_ids)


def runtime_apoc_execution_ids(task_record: dict) -> list[str]:
    runtime = task_record.get("runtime")
    if not isinstance(runtime, dict):
        return []
    values = runtime.get("apoc_execution_ids")
    return [value for value in values if isinstance(value, str)] if isinstance(values, list) else []


def recorded_apoc_execution_ids(task_record: dict) -> list[str]:
    direct = task_record.get("remote_execution_ids")
    direct_ids = [value for value in direct if isinstance(value, str)] if isinstance(direct, list) else []
    return sorted(set(direct_ids) | set(runtime_apoc_execution_ids(task_record)))


def runtime_herdr_ids(task_record: dict, key: str) -> list[str]:
    runtime = task_record.get("runtime")
    if not isinstance(runtime, dict):
        return []
    herdr = runtime.get("herdr")
    if not isinstance(herdr, dict):
        return []
    values = herdr.get(key)
    return [value for value in values if isinstance(value, str)] if isinstance(values, list) else []


def merge_runtime(existing: object, observed: dict) -> dict:
    merged = {"herdr": {}, "apoc_execution_ids": []}
    if isinstance(existing, dict):
        merged["apoc_execution_ids"] = list(existing.get("apoc_execution_ids", [])) if isinstance(existing.get("apoc_execution_ids"), list) else []
        herdr = existing.get("herdr")
        if isinstance(herdr, dict):
            merged["herdr"] = {key: list(value) for key, value in herdr.items() if isinstance(value, list)}
    for value in observed.get("apoc_execution_ids", []) if isinstance(observed.get("apoc_execution_ids"), list) else []:
        if value not in merged["apoc_execution_ids"]:
            merged["apoc_execution_ids"].append(value)
    observed_herdr = observed.get("herdr", {})
    if isinstance(observed_herdr, dict):
        key_map = {"machine_id": "machine_ids", "session_id": "session_ids", "pane_id": "pane_ids", "agent_id": "agent_ids"}
        for source, target in key_map.items():
            if source in observed_herdr and isinstance(observed_herdr[source], str):
                merged["herdr"].setdefault(target, [])
                if observed_herdr[source] not in merged["herdr"][target]:
                    merged["herdr"][target].append(observed_herdr[source])
    return merged


def collection_digest(metadata_without_digest: dict, archive_paths: list[Path]) -> str:
    digest = hashlib.sha256()
    digest.update(canonical_json(metadata_without_digest).encode("utf-8"))
    for path in sorted(archive_paths, key=lambda item: item.name):
        digest.update(path.name.encode("utf-8"))
        digest.update(b"\0")
        digest.update(path.read_bytes())
        digest.update(b"\0")
    return digest.hexdigest()


def valid_digest(value: str) -> bool:
    return isinstance(value, str) and bool(HEX64_RE.match(value))


def safe_archive_name(value: object) -> bool:
    if not isinstance(value, str) or not value:
        return False
    pure = PurePosixPath(value)
    return not pure.is_absolute() and ".." not in pure.parts and Path(value).name == value


def task_record_path(local_state: Path, task_id: str) -> Path:
    safe = re.sub(r"[^A-Za-z0-9._-]+", "_", task_id)
    digest = hashlib.sha256(task_id.encode("utf-8")).hexdigest()
    return local_state / "tasks" / f"{safe[:80]}.{digest}.json"


def read_task_record(local_state: Path, task_id: str) -> dict | None:
    path = task_record_path(local_state, task_id)
    if not path.exists():
        return None
    return json.loads(path.read_text(encoding="utf-8"))


def open_task_records_for_worker(local_state: Path, worker: str) -> list[dict]:
    tasks_dir = local_state / "tasks"
    if not tasks_dir.exists():
        return []
    records = []
    for path in tasks_dir.glob("*.json"):
        try:
            record = json.loads(path.read_text(encoding="utf-8"))
        except (OSError, json.JSONDecodeError):
            records.append({"worker": worker, "task_id": path.name, "status": "unknown"})
            continue
        if record.get("worker") == worker and record.get("status") not in TERMINAL_TASK_STATUSES:
            records.append(record)
    return records


def _task_record_summary(record: dict) -> dict:
    return {
        "task_id": record.get("task_id"),
        "project": record.get("project"),
        "revision": record.get("revision"),
        "status": record.get("status"),
        "reservation_id": record.get("reservation_id"),
        "session_id": record.get("session_id"),
    }


def record_task(
    local_state: Path,
    *,
    task_id: str,
    worker: str,
    project: str,
    repo: dict,
    revision: str,
    reservation_id: str,
    session_id: str | None,
    source: str,
    status: str,
    worktree: str | None,
    controller_execution_id: str | None = None,
) -> dict:
    existing = read_task_record(local_state, task_id) or {}
    controller_execution_ids = list(existing.get("controller_execution_ids", []))
    if controller_execution_id and controller_execution_id not in controller_execution_ids:
        controller_execution_ids.append(controller_execution_id)
    record = {
        **existing,
        "task_id": task_id,
        "worker": worker,
        "project": project,
        "repo": repo,
        "revision": revision,
        "reservation_id": reservation_id,
        "session_id": session_id,
        "source": source,
        "status": status,
        "worktree": worktree or existing.get("worktree"),
        "controller_execution_ids": controller_execution_ids,
        "remote_execution_ids": existing.get("remote_execution_ids", []),
        "updated_at": int(time.time() * 1000),
    }
    write_json_atomic(task_record_path(local_state, task_id), record)
    return ok("recorded", task=record)


def record_remote_execution(local_state: Path, task_id: str, execution_id: str) -> dict:
    record = read_task_record(local_state, task_id)
    if record is None:
        return failed("untracked_task", "central task record is missing", task_id=task_id)
    guard = require_runnable_task(record)
    if not guard["ok"]:
        return guard
    remote_execution_ids = list(record.get("remote_execution_ids", []))
    if execution_id not in remote_execution_ids:
        remote_execution_ids.append(execution_id)
    record["remote_execution_ids"] = remote_execution_ids
    record["runtime"] = merge_runtime(record.get("runtime"), {"apoc_execution_ids": [execution_id]})
    record["updated_at"] = int(time.time() * 1000)
    write_json_atomic(task_record_path(local_state, task_id), record)
    return ok("recorded", task=record)


def require_runnable_task(record: dict) -> dict:
    status = record.get("status")
    if status not in RUNNABLE_TASK_STATUSES:
        return failed(
            "task_not_running",
            "central task record is not in a runnable state",
            task_id=record.get("task_id"),
            task_status=status,
        )
    return ok("runnable")


def write_json_atomic(path: Path, payload: dict) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    fd, tmp_name = tempfile.mkstemp(prefix=f".{path.name}.", dir=path.parent)
    try:
        with os.fdopen(fd, "w", encoding="utf-8") as tmp:
            tmp.write(canonical_json(payload))
            tmp.write("\n")
            tmp.flush()
            os.fsync(tmp.fileno())
        os.replace(tmp_name, path)
    finally:
        with contextlib.suppress(FileNotFoundError):
            os.unlink(tmp_name)


def ok(status: str, **fields) -> dict:
    result = {"status": status, "ok": True}
    result.update(fields)
    return result


def failed(status: str, error: str, **fields) -> dict:
    result = {"status": status, "ok": False, "error": error}
    result.update(fields)
    return result


def canonical_json(payload: dict) -> str:
    return json.dumps(payload, sort_keys=True, separators=(",", ":"), ensure_ascii=True)


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description="Coordinate workenv workers through APoC-owned executions.")
    parser.add_argument("--fleet", default=str(DEFAULT_FLEET))
    parser.add_argument("--remote-root", default=DEFAULT_REMOTE_ROOT)
    parser.add_argument("--state", default=str(DEFAULT_STATE))
    subcommands = parser.add_subparsers(dest="command", required=True)

    plan_claim_parser = subcommands.add_parser("plan-claim")
    plan_claim_parser.add_argument("--project", required=True)
    plan_claim_parser.add_argument("--worker")

    plan_workers_parser = subcommands.add_parser("plan-workers")
    plan_workers_parser.add_argument("--worker")

    status_parser = subcommands.add_parser("status")
    status_parser.add_argument("--request-id", required=True)
    status_parser.add_argument("--worker")

    claim_parser = subcommands.add_parser("claim")
    claim_parser.add_argument("--worker", required=True)
    claim_parser.add_argument("--project", required=True)
    claim_parser.add_argument("--task-id", required=True)
    claim_parser.add_argument("--revision", required=True)
    claim_parser.add_argument("--request-id", required=True)
    claim_parser.add_argument("--source-bundle")
    claim_parser.add_argument("--reservation-id")
    claim_parser.add_argument("--session-id")

    record_parser = subcommands.add_parser("record-task")
    record_parser.add_argument("--task-id", required=True)
    record_parser.add_argument("--worker", required=True)
    record_parser.add_argument("--project", required=True)
    record_parser.add_argument("--revision", required=True)
    record_parser.add_argument("--reservation-id", required=True)
    record_parser.add_argument("--session-id")
    record_parser.add_argument("--source", choices=["remote", "bundle"], required=True)
    record_parser.add_argument("--status", required=True)
    record_parser.add_argument("--worktree")
    record_parser.add_argument("--controller-execution-id")

    remote_execution_parser = subcommands.add_parser("record-remote-execution")
    remote_execution_parser.add_argument("--task-id", required=True)
    remote_execution_parser.add_argument("--execution-id", required=True)

    runtime_parser = subcommands.add_parser("record-runtime")
    runtime_parser.add_argument("--worker", required=True)
    runtime_parser.add_argument("--task-id", required=True)
    runtime_parser.add_argument("--revision", required=True)
    runtime_parser.add_argument("--request-id", required=True)
    runtime_parser.add_argument("--runtime-json", required=True)

    task_run_parser = subcommands.add_parser("task-run")
    task_run_parser.add_argument("--worker", required=True)
    task_run_parser.add_argument("--task-id", required=True)
    task_run_parser.add_argument("--request-id", required=True)
    task_run_parser.add_argument("--purpose", required=True)
    task_run_parser.add_argument("--service", action="store_true")
    task_run_parser.add_argument("task_argv", nargs=argparse.REMAINDER)

    collect_parser = subcommands.add_parser("collect")
    collect_parser.add_argument("--worker", required=True)
    collect_parser.add_argument("--task-id", required=True)
    collect_parser.add_argument("--request-id", required=True)
    collect_parser.add_argument("--evidence-path", action="append", default=[])

    release_parser = subcommands.add_parser("release")
    release_parser.add_argument("--worker", required=True)
    release_parser.add_argument("--task-id", required=True)
    release_parser.add_argument("--request-id", required=True)
    release_parser.add_argument("--collection-digest", required=True)

    ensure_parser = subcommands.add_parser("ensure")
    ensure_parser.add_argument("--worker", required=True)
    ensure_parser.add_argument("--request-id", required=True)
    ensure_parser.add_argument("--reconcile", action="store_true")
    ensure_parser.add_argument("--register-herdr", action="store_true")
    ensure_parser.add_argument("--registration-target")
    ensure_parser.add_argument("--provider-create", action="store_true")

    args = parser.parse_args(argv)
    try:
        fleet = load_fleet(args.fleet)
        state = Path(args.state)
        if args.command == "plan-claim":
            result = plan_claim(fleet, args.project, args.worker, local_state=state)
        elif args.command == "plan-workers":
            result = plan_workers(fleet, args.worker)
        elif args.command == "status":
            result = status(fleet, args.request_id, worker=args.worker, remote_root=args.remote_root, local_state=state)
        elif args.command == "claim":
            result = claim(
                fleet,
                args.worker,
                args.project,
                args.task_id,
                args.revision,
                args.request_id,
                remote_root=args.remote_root,
                source_bundle=args.source_bundle,
                reservation_id=args.reservation_id,
                session_id=args.session_id,
                local_state=state,
            )
        elif args.command == "record-task":
            project_spec = project_config(fleet, args.project)
            result = record_task(
                state,
                task_id=args.task_id,
                worker=args.worker,
                project=args.project,
                repo=project_spec["repo"],
                revision=args.revision,
                reservation_id=args.reservation_id,
                session_id=args.session_id,
                source=args.source,
                status=args.status,
                worktree=args.worktree,
                controller_execution_id=args.controller_execution_id,
            )
        elif args.command == "record-remote-execution":
            result = record_remote_execution(state, args.task_id, args.execution_id)
        elif args.command == "record-runtime":
            result = record_runtime(
                fleet,
                args.worker,
                args.task_id,
                args.revision,
                args.request_id,
                json.loads(args.runtime_json),
                local_state=state,
                remote_root=args.remote_root,
            )
        elif args.command == "task-run":
            command = args.task_argv
            if command and command[0] == "--":
                command = command[1:]
            result = task_run(
                fleet,
                args.worker,
                args.task_id,
                args.request_id,
                args.purpose,
                command,
                local_state=state,
                service=args.service,
            )
        elif args.command == "collect":
            result = collect(
                fleet,
                args.worker,
                args.task_id,
                args.request_id,
                local_state=state,
                remote_root=args.remote_root,
                evidence_paths=args.evidence_path,
            )
        elif args.command == "release":
            result = release(
                fleet,
                args.worker,
                args.task_id,
                args.request_id,
                args.collection_digest,
                local_state=state,
                remote_root=args.remote_root,
            )
        else:
            result = ensure(
                fleet,
                args.worker,
                args.request_id,
                reconcile=args.reconcile,
                register_herdr=args.register_herdr,
                registration_target=args.registration_target,
                remote_root=args.remote_root,
                local_state=state,
                provider_create=args.provider_create,
            )
    except ControllerError as exc:
        result = failed(exc.status, exc.message)
    print(json.dumps(result, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
