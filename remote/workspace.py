from __future__ import annotations

import argparse
import base64
import contextlib
import fcntl
import hashlib
import io
import json
import os
import re
import subprocess
import tarfile
import tempfile
import time
from pathlib import Path, PurePosixPath
from urllib.parse import urlparse


TASK_ID_RE = re.compile(r"^[A-Za-z0-9][A-Za-z0-9._-]{0,127}$")
REPO_SLUG_RE = re.compile(r"^[A-Za-z0-9][A-Za-z0-9._-]{0,127}$")
FULL_SHA_RE = re.compile(r"^[0-9a-f]{40}$")


class WorkspaceError(Exception):
    def __init__(self, status: str, message: str, code: int = 1):
        super().__init__(message)
        self.status = status
        self.message = message
        self.code = code


def main(argv: list[str] | None = None) -> dict:
    parser = argparse.ArgumentParser(description="Manage a remote task workspace.")
    parser.add_argument("--root", default=str(Path.home() / "workenv"))
    parser.add_argument("--request", help="JSON request payload.")
    parser.add_argument("--request-base64", help="Base64-encoded JSON request payload.")
    args = parser.parse_args(argv)

    try:
        request = _load_request(args)
        manager = WorkspaceManager(Path(args.root).expanduser())
        return manager.handle(request)
    except WorkspaceError as exc:
        return _result(exc.status, error=exc.message, code=exc.code)
    except subprocess.CalledProcessError as exc:
        return _result(
            "conflict",
            error=f"git command failed: {' '.join(exc.cmd)}: {exc.stderr.strip()}",
            code=1,
        )


class WorkspaceManager:
    def __init__(self, root: Path):
        self.root = root
        self.state_dir = root / "state"
        self.receipts_dir = self.state_dir / "receipts"
        self.services_dir = self.state_dir / "services"
        self.mirrors_dir = root / "mirrors"
        self.worktrees_dir = root / "worktrees"
        self.collections_dir = root / "collections"
        self.active_path = self.state_dir / "active.json"
        self.lock_path = self.state_dir / "workspace.lock"

    def handle(self, request: dict) -> dict:
        if not isinstance(request, dict):
            raise WorkspaceError("conflict", "request must be a JSON object")
        operation = request.get("operation")
        request_id = request.get("request_id")
        if operation not in {"status", "claim", "record-runtime", "collect", "release"}:
            raise WorkspaceError("conflict", "operation must be one of status, claim, record-runtime, collect, release")
        if not isinstance(request_id, str) or not request_id:
            raise WorkspaceError("conflict", "request_id is required")

        self._ensure_layout()
        payload_hash = _canonical_hash(request)
        with self._locked():
            replay = self._read_replay(request_id, payload_hash)
            if replay is not None:
                replay["replay"] = True
                return replay

            if self._receipt_path(request_id).exists():
                return _result("conflict", error="request_id was already used for a different payload", code=1)

            handler = {
                "status": self._handle_status,
                "claim": self._handle_claim,
                "record-runtime": self._handle_record_runtime,
                "collect": self._handle_collect,
                "release": self._handle_release,
            }[operation]
            result = handler(request)
            self._write_receipt(request_id, payload_hash, result)
            return result

    def _handle_status(self, request: dict) -> dict:
        active = self._read_active()
        if active is None:
            return _result("available", active=None)
        if active.get("status") == "released":
            return _result("available", active=_redact_active(active))
        return _result(active.get("status", "busy"), active=_redact_active(active))

    def _handle_claim(self, request: dict) -> dict:
        spec = _claim_spec(request, self.root)
        active = self._read_active()
        if active is not None and active.get("status") != "released":
            if active.get("task_id") != spec["task_id"]:
                return _result("busy", error="worker already has an active task", active=_redact_active(active), code=2)
            if active.get("repo") != spec["repo"]:
                return _result("conflict", error="worker is occupied by this task_id for a different repo", code=1)
            return _result("busy", error="task is already active under a different request_id", active=_redact_active(active), code=2)

        mirror = self.mirrors_dir / f"{_safe_name(spec['repo']['owner'])}__{_safe_name(spec['repo']['name'])}.git"
        worktree = self.worktrees_dir / spec["task_id"]
        service_state_dir = self.services_dir / spec["task_id"]
        if worktree.exists():
            raise WorkspaceError("conflict", f"worktree path already exists: {worktree}")
        if service_state_dir.exists():
            raise WorkspaceError("conflict", f"service state path already exists: {service_state_dir}")

        self._prepare_mirror(mirror, spec, spec["revision"])
        if _git_success(mirror, "show-ref", "--verify", "--quiet", f"refs/heads/{spec['branch']}"):
            raise WorkspaceError("conflict", f"branch already exists and will not be reset: {spec['branch']}")
        worktree.parent.mkdir(parents=True, exist_ok=True)
        _git(mirror, "worktree", "add", "-b", spec["branch"], str(worktree), spec["revision"])
        actual_head = _git(worktree, "rev-parse", "HEAD")
        if actual_head != spec["revision"]:
            raise WorkspaceError("conflict", f"claimed HEAD {actual_head} did not match requested revision")
        if _git(worktree, "status", "--porcelain=v1"):
            raise WorkspaceError("conflict", "claim produced a dirty worktree")
        # Nix environment caches contain store symlinks and are rebuildable.
        # Keep the project's tracked ignore rules and exact revision unchanged.
        exclude_path = Path(_git(worktree, "rev-parse", "--git-path", "info/exclude"))
        exclude_path.parent.mkdir(parents=True, exist_ok=True)
        existing_excludes = exclude_path.read_text() if exclude_path.exists() else ""
        with exclude_path.open("a") as exclude:
            for pattern in ("/.devenv/", "/.direnv/"):
                if pattern not in existing_excludes.splitlines():
                    exclude.write(f"\n{pattern}\n")
        service_state_dir.mkdir(parents=True, exist_ok=False)

        active = {
            "status": "claimed",
            "task_id": spec["task_id"],
            "repo": spec["repo"],
            "revision": spec["revision"],
            "branch": spec["branch"],
            "source": spec["source"],
            "mirror": str(mirror),
            "worktree": str(worktree),
            "service_state_dir": str(service_state_dir),
            "runtime": _empty_runtime(),
            "claimed_at": _now(),
        }
        self._write_json_atomic(self.active_path, active)
        return _result(
            "claimed",
            task_id=spec["task_id"],
            repo=spec["repo"],
            revision=spec["revision"],
            branch=spec["branch"],
            worktree=str(worktree),
            service_state_dir=str(service_state_dir),
        )

    def _handle_record_runtime(self, request: dict) -> dict:
        task_id = _task_id(request)
        revision = request.get("revision")
        if not isinstance(revision, str) or not FULL_SHA_RE.match(revision):
            raise WorkspaceError("conflict", "revision must be an exact full lowercase SHA")
        active = self._require_active_task(task_id)
        if active.get("revision") != revision:
            return _result("conflict", error="runtime revision does not match active task revision", code=1)

        observed = _runtime_links(request.get("runtime"))
        current = _normalize_runtime(active.get("runtime"))
        _append_unique(current["apoc_execution_ids"], observed["apoc_execution_ids"])
        for key, values in observed["herdr"].items():
            current["herdr"].setdefault(key, [])
            _append_unique(current["herdr"][key], values)

        active["runtime"] = current
        active["runtime_recorded_at"] = _now()
        self._write_json_atomic(self.active_path, active)
        return _result(
            "runtime_recorded",
            task_id=task_id,
            revision=revision,
            service_state_dir=active.get("service_state_dir"),
            runtime=current,
        )

    def _handle_collect(self, request: dict) -> dict:
        task_id = _task_id(request)
        active = self._require_active_task(task_id)
        worktree = Path(active["worktree"])
        if not worktree.exists():
            raise WorkspaceError("conflict", "active worktree is missing")

        collection_id = _canonical_hash({"task_id": task_id, "request_id": request["request_id"]})[:16]
        collection_dir = self.collections_dir / task_id / collection_id
        if collection_dir.exists():
            recovered = self._recover_collection(task_id, active, collection_dir)
            if recovered is not None:
                return recovered
            return _result(
                "conflict",
                error="collection directory already exists but is incomplete; retry with a new request_id",
                code=1,
            )

        evidence_paths = request.get("evidence_paths", [])
        if evidence_paths is None:
            evidence_paths = []
        if not isinstance(evidence_paths, list) or not all(isinstance(path, str) for path in evidence_paths):
            raise WorkspaceError("conflict", "evidence_paths must be a list of repo-relative paths")
        safe_evidence = [_resolve_repo_path(worktree, path) for path in evidence_paths]

        collection_dir.mkdir(parents=True, exist_ok=False)

        fingerprint = _fingerprint(worktree)
        metadata = {
            "task_id": task_id,
            "repo": active["repo"],
            "source": active["source"],
            "base": active["revision"],
            "head": fingerprint["head"],
            "branch": active["branch"],
            "dirty": fingerprint["dirty"],
            "runtime": _normalize_runtime(active.get("runtime")),
        }

        staged_patch = collection_dir / "staged.diff"
        staged_diff = _git_bytes(worktree, "diff", "--binary", "--full-index", "--cached", "--")
        staged_patch.write_bytes(staged_diff)

        unstaged_patch = collection_dir / "unstaged.diff"
        unstaged_diff = _git_bytes(worktree, "diff", "--binary", "--full-index", "--")
        unstaged_patch.write_bytes(unstaged_diff)

        bundle_path = None
        if fingerprint["head"] != active["revision"]:
            bundle_path = collection_dir / "committed.bundle"
            _git(worktree, "bundle", "create", str(bundle_path), f"{active['revision']}..HEAD")

        untracked_paths = _git_path_list(worktree, "ls-files", "--others", "--exclude-standard", "-z")
        untracked_archive = collection_dir / "untracked.tar"
        _write_tar(untracked_archive, worktree, untracked_paths)

        evidence_archive = None
        if safe_evidence:
            evidence_archive = collection_dir / "evidence.tar"
            rels = [path.relative_to(worktree).as_posix() for path in safe_evidence]
            _write_tar(evidence_archive, worktree, rels, reject_symlink=True)

        metadata_path = collection_dir / "metadata.json"
        archives = {
            "staged_diff": str(staged_patch),
            "unstaged_diff": str(unstaged_patch),
            "untracked": str(untracked_archive),
        }
        if bundle_path is not None:
            archives["bundle"] = str(bundle_path)
        if evidence_archive is not None:
            archives["evidence"] = str(evidence_archive)

        metadata["archives"] = {key: Path(value).name for key, value in archives.items()}
        metadata["fingerprint"] = fingerprint
        current_fingerprint = _fingerprint(worktree)
        if current_fingerprint != fingerprint:
            return _result(
                "changed_since_collection",
                error="worktree fingerprint changed while collecting artifacts",
                fingerprint=current_fingerprint,
                collection_fingerprint=fingerprint,
                code=4,
            )
        collection_digest = _collection_digest(metadata, [Path(path) for path in archives.values()])
        metadata["collection_digest"] = collection_digest
        metadata_path.write_text(_canonical_json(metadata), encoding="utf-8")

        active["last_collection"] = {
            "digest": collection_digest,
            "fingerprint": fingerprint,
            "metadata_path": str(metadata_path),
            "collection_dir": str(collection_dir),
            "collected_at": _now(),
        }
        active["status"] = "collected"
        self._write_json_atomic(self.active_path, active)

        return _result(
            "collected",
            task_id=task_id,
            collection_digest=collection_digest,
            fingerprint=fingerprint,
            metadata=metadata,
            metadata_path=str(metadata_path),
            archives=archives,
        )

    def _recover_collection(self, task_id: str, active: dict, collection_dir: Path) -> dict | None:
        metadata_path = collection_dir / "metadata.json"
        if not metadata_path.is_file():
            return None
        try:
            metadata = _read_json(metadata_path)
        except json.JSONDecodeError:
            return None
        if not isinstance(metadata, dict):
            return None
        if metadata.get("task_id") != task_id:
            return None
        if metadata.get("repo") != active.get("repo"):
            return None
        if metadata.get("base") != active.get("revision"):
            return None
        if metadata.get("branch") != active.get("branch"):
            return None
        if not isinstance(metadata.get("fingerprint"), dict):
            return None

        archive_names = metadata.get("archives")
        if not isinstance(archive_names, dict):
            return None
        archives: dict[str, str] = {}
        archive_paths = []
        for key, value in archive_names.items():
            if not isinstance(key, str) or not isinstance(value, str):
                return None
            archive_name = PurePosixPath(value)
            if archive_name.is_absolute() or ".." in archive_name.parts or archive_name.name != value:
                return None
            archive_path = collection_dir / value
            if not archive_path.is_file():
                return None
            archives[key] = str(archive_path)
            archive_paths.append(archive_path)

        collection_digest = metadata.get("collection_digest")
        if not isinstance(collection_digest, str) or not collection_digest:
            return None
        digest_input = dict(metadata)
        del digest_input["collection_digest"]
        if _collection_digest(digest_input, archive_paths) != collection_digest:
            return None

        last_collection = active.get("last_collection") if isinstance(active.get("last_collection"), dict) else {}
        active["last_collection"] = {
            "digest": collection_digest,
            "fingerprint": metadata["fingerprint"],
            "metadata_path": str(metadata_path),
            "collection_dir": str(collection_dir),
            "collected_at": last_collection.get("collected_at", _now()) if isinstance(last_collection, dict) else _now(),
        }
        active["status"] = "collected"
        self._write_json_atomic(self.active_path, active)

        return _result(
            "collected",
            task_id=task_id,
            collection_digest=collection_digest,
            fingerprint=metadata["fingerprint"],
            metadata=metadata,
            metadata_path=str(metadata_path),
            archives=archives,
        )

    def _handle_release(self, request: dict) -> dict:
        task_id = _task_id(request)
        collection_digest = request.get("collection_digest")
        if not isinstance(collection_digest, str) or not collection_digest:
            raise WorkspaceError("conflict", "collection_digest is required")
        active = self._require_active_task(task_id, allow_collected=True)
        collection = active.get("last_collection")
        if not collection:
            return _result("uncollected", error="task has not been collected", code=3)
        if collection.get("digest") != collection_digest:
            return _result("uncollected", error="collection_digest does not match the recorded collection", code=3)

        current = _fingerprint(Path(active["worktree"]))
        if current != collection.get("fingerprint"):
            return _result(
                "changed_since_collection",
                error="worktree fingerprint changed after collection",
                fingerprint=current,
                collection_fingerprint=collection.get("fingerprint"),
                code=4,
            )

        active["status"] = "released"
        active["released_at"] = _now()
        self._write_json_atomic(self.active_path, active)
        return _result(
            "released",
            task_id=task_id,
            collection_digest=collection_digest,
            worktree=active["worktree"],
            collection_dir=collection["collection_dir"],
        )

    def _prepare_mirror(self, mirror: Path, spec: dict, revision: str) -> None:
        if not mirror.exists():
            mirror.parent.mkdir(parents=True, exist_ok=True)
            _git(self.root, "init", "--bare", str(mirror))
        if spec["source"] == "remote":
            remote_url = spec["remote_url"]
            remotes = _git(mirror, "remote").splitlines()
            if "origin" not in remotes:
                _git(mirror, "remote", "add", "origin", remote_url)
            else:
                existing = _git(mirror, "remote", "get-url", "origin")
                if existing != remote_url:
                    raise WorkspaceError("conflict", "mirror remote_url does not match existing mirror")
            _git(mirror, "fetch", "--force", "--no-tags", "origin", f"+{revision}:refs/workenv/fetched/{revision}")
        else:
            bundle = Path(spec["source_bundle"])
            _git(mirror, "bundle", "verify", str(bundle))
            _git(mirror, "fetch", "--force", "--no-tags", str(bundle), f"+{revision}:refs/workenv/fetched/{revision}")
        obj_type = _git(mirror, "cat-file", "-t", revision)
        if obj_type != "commit":
            raise WorkspaceError("conflict", "revision is not a commit")

    def _require_active_task(self, task_id: str, allow_collected: bool = False) -> dict:
        active = self._read_active()
        if active is None or active.get("status") == "released":
            raise WorkspaceError("conflict", "no active task")
        if active.get("task_id") != task_id:
            raise WorkspaceError("busy", "worker is occupied by a different task", code=2)
        if active.get("status") not in ({"claimed", "collected"} if allow_collected else {"claimed", "collected"}):
            raise WorkspaceError("conflict", f"task is not collectable: {active.get('status')}")
        return active

    def _ensure_layout(self) -> None:
        for path in (
            self.state_dir,
            self.receipts_dir,
            self.services_dir,
            self.mirrors_dir,
            self.worktrees_dir,
            self.collections_dir,
        ):
            path.mkdir(parents=True, exist_ok=True)

    @contextlib.contextmanager
    def _locked(self):
        self.lock_path.parent.mkdir(parents=True, exist_ok=True)
        with self.lock_path.open("a+") as lock_file:
            fcntl.flock(lock_file.fileno(), fcntl.LOCK_EX)
            try:
                yield
            finally:
                fcntl.flock(lock_file.fileno(), fcntl.LOCK_UN)

    def _read_replay(self, request_id: str, payload_hash: str) -> dict | None:
        receipt_path = self._receipt_path(request_id)
        if not receipt_path.exists():
            return None
        receipt = _read_json(receipt_path)
        if receipt.get("payload_hash") != payload_hash:
            return None
        result = dict(receipt["result"])
        result["receipt_path"] = str(receipt_path)
        return result

    def _write_receipt(self, request_id: str, payload_hash: str, result: dict) -> None:
        receipt = {
            "request_id": request_id,
            "payload_hash": payload_hash,
            "operation": result.get("operation"),
            "created_at": _now(),
            "result": result,
        }
        self._write_json_atomic(self._receipt_path(request_id), receipt)

    def _receipt_path(self, request_id: str) -> Path:
        safe_id = _safe_name(request_id)
        digest = hashlib.sha256(request_id.encode("utf-8")).hexdigest()
        safe_id = f"{safe_id[:80]}.{digest}"
        return self.receipts_dir / f"{safe_id}.json"

    def _read_active(self) -> dict | None:
        if not self.active_path.exists():
            return None
        return _read_json(self.active_path)

    def _write_json_atomic(self, path: Path, payload: dict) -> None:
        path.parent.mkdir(parents=True, exist_ok=True)
        fd, tmp_name = tempfile.mkstemp(prefix=f".{path.name}.", dir=path.parent)
        try:
            with os.fdopen(fd, "w", encoding="utf-8") as tmp:
                tmp.write(_canonical_json(payload))
                tmp.write("\n")
                tmp.flush()
                os.fsync(tmp.fileno())
            os.replace(tmp_name, path)
        finally:
            with contextlib.suppress(FileNotFoundError):
                os.unlink(tmp_name)


def _load_request(args: argparse.Namespace) -> dict:
    if bool(args.request) == bool(args.request_base64):
        raise WorkspaceError("conflict", "pass exactly one of --request or --request-base64")
    if args.request_base64:
        try:
            raw = base64.b64decode(args.request_base64, validate=True).decode("utf-8")
        except Exception as exc:  # noqa: BLE001
            raise WorkspaceError("conflict", "request-base64 is not valid base64 JSON") from exc
    else:
        raw = args.request
    try:
        return json.loads(raw)
    except json.JSONDecodeError as exc:
        raise WorkspaceError("conflict", f"request is not valid JSON: {exc}") from exc


def _claim_spec(request: dict, root: Path) -> dict:
    task_id = _task_id(request)
    repo = request.get("repo")
    if (
        not isinstance(repo, dict)
        or not isinstance(repo.get("owner"), str)
        or not isinstance(repo.get("name"), str)
        or not repo["owner"]
        or not repo["name"]
    ):
        raise WorkspaceError("conflict", "repo owner/name are required")
    if not REPO_SLUG_RE.match(repo["owner"]) or not REPO_SLUG_RE.match(repo["name"]):
        raise WorkspaceError("conflict", "repo owner/name must be safe slugs")
    remote_url = request.get("remote_url")
    source_bundle = request.get("source_bundle")
    revision = request.get("revision")
    branch = request.get("branch")
    has_remote = isinstance(remote_url, str) and bool(remote_url)
    has_bundle = isinstance(source_bundle, str) and bool(source_bundle)
    if has_remote == has_bundle:
        raise WorkspaceError("conflict", "claim must pass exactly one of remote_url or source_bundle")
    if not isinstance(revision, str) or not FULL_SHA_RE.match(revision):
        raise WorkspaceError("conflict", "revision must be an exact full lowercase SHA")
    if branch != f"workenv/{task_id}":
        raise WorkspaceError("conflict", f"branch must be workenv/{task_id}")
    spec = {
        "task_id": task_id,
        "repo": {"owner": repo["owner"], "name": repo["name"]},
        "revision": revision,
        "branch": branch,
    }
    if has_remote:
        _validate_remote_url(remote_url, spec["repo"])
        spec["source"] = "remote"
        spec["remote_url"] = remote_url
    else:
        spec["source"] = "bundle"
        spec["source_bundle"] = str(_validate_source_bundle(source_bundle, root))
    return spec


def _validate_remote_url(remote_url: str, repo: dict) -> None:
    parsed = urlparse(remote_url)
    if parsed.scheme not in {"https", "file"}:
        raise WorkspaceError("conflict", "remote_url must be an HTTPS URL, or file:// for tests")
    if parsed.username or parsed.password:
        raise WorkspaceError("conflict", "remote_url must not contain credentials")
    if parsed.scheme != "https":
        return
    owner_name = parsed.path.strip("/")
    if owner_name.endswith(".git"):
        owner_name = owner_name[:-4]
    parts = owner_name.split("/")
    if len(parts) != 2 or parts[0] != repo["owner"] or parts[1] != repo["name"]:
        raise WorkspaceError("conflict", "remote_url path must exactly match repo owner/name")


def _validate_source_bundle(raw: str, root: Path) -> Path:
    bundle = Path(raw).expanduser()
    if not bundle.is_absolute():
        raise WorkspaceError("conflict", "source_bundle must be an absolute path")
    if bundle.suffix != ".bundle":
        raise WorkspaceError("conflict", "source_bundle must be a .bundle file")
    incoming = (root / "incoming").resolve()
    resolved = bundle.resolve()
    if resolved.parent != incoming:
        raise WorkspaceError("conflict", "source_bundle must be under the worker incoming directory")
    if not resolved.is_file():
        raise WorkspaceError("conflict", "source_bundle does not exist")
    return resolved


def _task_id(request: dict) -> str:
    task_id = request.get("task_id")
    if not isinstance(task_id, str) or not TASK_ID_RE.match(task_id):
        raise WorkspaceError("conflict", "task_id must be a safe slug")
    return task_id


def _runtime_links(raw: object) -> dict:
    if not isinstance(raw, dict):
        raise WorkspaceError("conflict", "runtime must be an object")
    allowed = {"herdr", "apoc_execution_ids"}
    extra = set(raw) - allowed
    if extra:
        raise WorkspaceError("conflict", f"runtime contains unsupported keys: {', '.join(sorted(extra))}")

    herdr = raw.get("herdr", {})
    if herdr is None:
        herdr = {}
    if not isinstance(herdr, dict):
        raise WorkspaceError("conflict", "runtime.herdr must be an object")
    herdr_map = {
        "machine_id": "machine_ids",
        "session_id": "session_ids",
        "pane_id": "pane_ids",
        "agent_id": "agent_ids",
    }
    normalized_herdr: dict[str, list[str]] = {}
    for source, target in herdr_map.items():
        if source in herdr:
            normalized_herdr[target] = [_observed_id(herdr[source], f"runtime.herdr.{source}")]
    extra_herdr = set(herdr) - set(herdr_map)
    if extra_herdr:
        raise WorkspaceError("conflict", f"runtime.herdr contains unsupported keys: {', '.join(sorted(extra_herdr))}")

    apoc_execution_ids = raw.get("apoc_execution_ids", [])
    if apoc_execution_ids is None:
        apoc_execution_ids = []
    if not isinstance(apoc_execution_ids, list):
        raise WorkspaceError("conflict", "runtime.apoc_execution_ids must be a list")
    return {
        "herdr": normalized_herdr,
        "apoc_execution_ids": [
            _observed_id(value, "runtime.apoc_execution_ids[]") for value in apoc_execution_ids
        ],
    }


def _normalize_runtime(raw: object) -> dict:
    if not isinstance(raw, dict):
        return _empty_runtime()
    herdr = raw.get("herdr")
    if not isinstance(herdr, dict):
        herdr = {}
    normalized = {
        "herdr": {
            key: list(values)
            for key, values in herdr.items()
            if key in {"machine_ids", "session_ids", "pane_ids", "agent_ids"} and isinstance(values, list)
        },
        "apoc_execution_ids": list(raw.get("apoc_execution_ids", []))
        if isinstance(raw.get("apoc_execution_ids"), list)
        else [],
    }
    return normalized


def _empty_runtime() -> dict:
    return {"herdr": {}, "apoc_execution_ids": []}


def _observed_id(value: object, label: str) -> str:
    if not isinstance(value, str) or not value or len(value) > 256 or "\n" in value or "\r" in value:
        raise WorkspaceError("conflict", f"{label} must be a nonempty single-line string")
    return value


def _append_unique(target: list[str], values: list[str]) -> None:
    seen = set(target)
    for value in values:
        if value not in seen:
            target.append(value)
            seen.add(value)


def _resolve_repo_path(worktree: Path, raw: str) -> Path:
    pure = PurePosixPath(raw)
    if pure.is_absolute() or ".." in pure.parts:
        raise WorkspaceError("conflict", f"evidence path escapes worktree: {raw}")
    target = worktree / Path(*pure.parts)
    if not target.exists() and not target.is_symlink():
        raise WorkspaceError("conflict", f"evidence path does not exist: {raw}")
    current = worktree
    for part in pure.parts:
        current = current / part
        if current.is_symlink():
            raise WorkspaceError("conflict", f"evidence path crosses symlink: {raw}")
    resolved = target.resolve()
    if not _is_relative_to(resolved, worktree.resolve()):
        raise WorkspaceError("conflict", f"evidence path escapes worktree: {raw}")
    return target


def _fingerprint(worktree: Path) -> dict:
    dirty_submodules = _dirty_submodules(worktree)
    if dirty_submodules:
        raise WorkspaceError("conflict", f"dirty submodule work is not collectable: {', '.join(dirty_submodules)}")
    head = _git(worktree, "rev-parse", "HEAD")
    status = _git_bytes(
        worktree,
        "status",
        "--porcelain=v1",
        "-z",
        "--untracked-files=all",
        "--ignore-submodules=none",
    )
    staged = _git_bytes(worktree, "diff", "--binary", "--full-index", "--cached", "--")
    unstaged = _git_bytes(worktree, "diff", "--binary", "--full-index", "--")
    untracked_hash = _untracked_content_hash(worktree)
    diff_digest = hashlib.sha256()
    diff_digest.update(b"status\0")
    diff_digest.update(status)
    diff_digest.update(b"staged\0")
    diff_digest.update(staged)
    diff_digest.update(b"unstaged\0")
    diff_digest.update(unstaged)
    diff_digest.update(b"untracked\0")
    diff_digest.update(untracked_hash.encode("ascii"))
    return {
        "head": head,
        "dirty": bool(status),
        "status_sha256": hashlib.sha256(status).hexdigest(),
        "staged_sha256": hashlib.sha256(staged).hexdigest(),
        "unstaged_sha256": hashlib.sha256(unstaged).hexdigest(),
        "diff_sha256": diff_digest.hexdigest(),
        "untracked_sha256": untracked_hash,
    }


def _dirty_submodules(worktree: Path) -> list[str]:
    submodules = set()
    for entry in _git_path_list(worktree, "ls-files", "-z", "--stage"):
        mode, _, rest = entry.partition(" ")
        if mode != "160000":
            continue
        _, _, path = rest.partition("\t")
        if path:
            submodules.add(path)
    if not submodules:
        return []

    dirty = set()
    status = _git_path_list(
        worktree,
        "status",
        "--porcelain=v1",
        "-z",
        "--untracked-files=all",
        "--ignore-submodules=none",
    )
    for entry in status:
        if len(entry) < 4:
            continue
        path = entry[3:]
        if entry[0] == "R" or entry[0] == "C":
            continue
        for submodule in submodules:
            if path == submodule or path.startswith(f"{submodule}/"):
                dirty.add(submodule)
    return sorted(dirty)


def _untracked_content_hash(worktree: Path) -> str:
    paths = _git_path_list(worktree, "ls-files", "--others", "--exclude-standard", "-z")
    digest = hashlib.sha256()
    for rel in sorted(paths):
        _reject_relpath(rel)
        path = worktree / rel
        digest.update(rel.encode("utf-8"))
        digest.update(b"\0")
        if path.is_symlink():
            digest.update(b"symlink\0")
            digest.update(os.readlink(path).encode("utf-8", "surrogateescape"))
        elif path.is_file():
            digest.update(b"file\0")
            digest.update(path.read_bytes())
        elif path.is_dir():
            digest.update(b"dir\0")
        digest.update(b"\0")
    return digest.hexdigest()


def _write_tar(archive_path: Path, worktree: Path, rel_paths: list[str], reject_symlink: bool = False) -> None:
    with tarfile.open(archive_path, "w", format=tarfile.PAX_FORMAT) as archive:
        for rel in sorted(rel_paths):
            _reject_relpath(rel)
            path = worktree / rel
            if reject_symlink and _path_has_symlink(worktree, Path(rel)):
                raise WorkspaceError("conflict", f"evidence path crosses symlink: {rel}")
            if path.is_dir() and not path.is_symlink():
                for child in sorted(path.rglob("*")):
                    child_rel = child.relative_to(worktree).as_posix()
                    if reject_symlink and _path_has_symlink(worktree, Path(child_rel)):
                        raise WorkspaceError("conflict", f"evidence path crosses symlink: {child_rel}")
                    _add_tar_member(archive, worktree, child_rel)
            elif path.exists() or path.is_symlink():
                _add_tar_member(archive, worktree, rel)


def _add_tar_member(archive: tarfile.TarFile, worktree: Path, rel: str) -> None:
    _reject_relpath(rel)
    path = worktree / rel
    stat = path.lstat()
    info = tarfile.TarInfo(rel)
    info.mode = stat.st_mode & 0o777
    info.uid = 0
    info.gid = 0
    info.uname = ""
    info.gname = ""
    info.mtime = 0
    if path.is_symlink():
        info.type = tarfile.SYMTYPE
        info.linkname = os.readlink(path)
        archive.addfile(info)
    elif path.is_dir():
        info.type = tarfile.DIRTYPE
        archive.addfile(info)
    elif path.is_file():
        data = path.read_bytes()
        info.size = len(data)
        archive.addfile(info, io.BytesIO(data))


def _collection_digest(metadata: dict, paths: list[Path]) -> str:
    digest = hashlib.sha256()
    digest.update(_canonical_json(metadata).encode("utf-8"))
    for path in sorted(paths, key=lambda item: item.name):
        digest.update(path.name.encode("utf-8"))
        digest.update(b"\0")
        digest.update(path.read_bytes())
        digest.update(b"\0")
    return digest.hexdigest()


def _reject_relpath(rel: str) -> None:
    pure = PurePosixPath(rel)
    if pure.is_absolute() or ".." in pure.parts or not rel:
        raise WorkspaceError("conflict", f"path escapes worktree: {rel}")


def _path_has_symlink(root: Path, rel: Path) -> bool:
    current = root
    for part in rel.parts:
        current = current / part
        if current.is_symlink():
            return True
    return False


def _git(cwd: Path, *args: str) -> str:
    return subprocess.run(
        ["git", *args],
        cwd=cwd,
        check=True,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    ).stdout.strip()


def _git_bytes(cwd: Path, *args: str) -> bytes:
    return subprocess.run(
        ["git", *args],
        cwd=cwd,
        check=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    ).stdout


def _git_success(cwd: Path, *args: str) -> bool:
    return subprocess.run(
        ["git", *args],
        cwd=cwd,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
        check=False,
    ).returncode == 0


def _git_path_list(cwd: Path, *args: str) -> list[str]:
    raw = _git_bytes(cwd, *args)
    return [item.decode("utf-8", "surrogateescape") for item in raw.split(b"\0") if item]


def _canonical_hash(payload: dict) -> str:
    return hashlib.sha256(_canonical_json(payload).encode("utf-8")).hexdigest()


def _canonical_json(payload: dict) -> str:
    return json.dumps(payload, sort_keys=True, separators=(",", ":"), ensure_ascii=True)


def _result(status: str, **fields) -> dict:
    result = {"status": status, "ok": status in {"available", "claimed", "runtime_recorded", "collected", "released"}}
    result.update(fields)
    return result


def _read_json(path: Path) -> dict:
    return json.loads(path.read_text(encoding="utf-8"))


def _safe_name(value: str) -> str:
    safe = re.sub(r"[^A-Za-z0-9._-]+", "_", value)
    if not safe:
        raise WorkspaceError("conflict", "empty value is not safe for filesystem use")
    return safe[:160]


def _redact_active(active: dict) -> dict:
    return {
        "status": active.get("status"),
        "task_id": active.get("task_id"),
        "repo": active.get("repo"),
        "revision": active.get("revision"),
        "branch": active.get("branch"),
        "worktree": active.get("worktree"),
        "service_state_dir": active.get("service_state_dir"),
        "runtime": _normalize_runtime(active.get("runtime")),
        "last_collection": active.get("last_collection"),
    }


def _is_relative_to(path: Path, base: Path) -> bool:
    try:
        path.relative_to(base)
        return True
    except ValueError:
        return False


def _now() -> int:
    return int(time.time())


if __name__ == "__main__":
    print(json.dumps(main(), sort_keys=True))
