import base64
import json
import os
import stat
import subprocess
import sys
import tarfile
from pathlib import Path

import pytest

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from remote import workspace


def run_git(cwd, *args):
    return subprocess.run(
        ["git", *args],
        cwd=cwd,
        check=True,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    ).stdout.strip()


@pytest.fixture
def origin_repo(tmp_path):
    source = tmp_path / "source"
    source.mkdir()
    run_git(source, "init", "-b", "main")
    run_git(source, "config", "user.email", "test@example.com")
    run_git(source, "config", "user.name", "Test User")
    (source / "README.md").write_text("base\n", encoding="utf-8")
    run_git(source, "add", "README.md")
    run_git(source, "commit", "-m", "base")
    first = run_git(source, "rev-parse", "HEAD")
    (source / "README.md").write_text("base\nnext\n", encoding="utf-8")
    run_git(source, "commit", "-am", "next")
    second = run_git(source, "rev-parse", "HEAD")

    bare = tmp_path / "origin.git"
    run_git(tmp_path, "clone", "--bare", str(source), str(bare))
    return {"url": bare.as_uri(), "source": source, "first": first, "second": second}


def call_helper(tmp_path, payload, *, lifetime="static", worker=None):
    encoded = base64.b64encode(json.dumps(payload).encode("utf-8")).decode("ascii")
    argv = ["--root", str(tmp_path / "worker"), "--request-base64", encoded, "--lifetime", lifetime]
    if worker is not None:
        argv.extend(["--worker", worker])
    return workspace.main(argv)


def claim_request(origin_repo, request_id="claim-1", task_id="task-a"):
    return {
        "operation": "claim",
        "request_id": request_id,
        "task_id": task_id,
        "repo": {"owner": "acme", "name": "widgets"},
        "remote_url": origin_repo["url"],
        "revision": origin_repo["first"],
        "branch": f"workenv/{task_id}",
    }


def release_request(task_id, request_id, collection_digest):
    return {
        "operation": "release",
        "request_id": request_id,
        "task_id": task_id,
        "collection_digest": collection_digest,
        "runtime_quiescent": True,
    }


def test_static_lifetime_keeps_existing_paths_and_reports_lifetime(tmp_path, origin_repo):
    claimed = call_helper(tmp_path, claim_request(origin_repo), lifetime="static")

    assert claimed["status"] == "claimed"
    assert claimed["worker_lifetime"] == "static"
    assert claimed["allocation_id"] is None
    assert claimed["allocation_root"] is None
    assert Path(claimed["worktree"]) == tmp_path / "worker/worktrees/task-a"
    assert Path(claimed["service_state_dir"]) == tmp_path / "worker/state/services/task-a"


def test_ephemeral_requires_worker_and_claims_under_allocation(tmp_path, origin_repo):
    missing_worker = call_helper(tmp_path, claim_request(origin_repo), lifetime="ephemeral")
    claimed = call_helper(tmp_path, claim_request(origin_repo), lifetime="ephemeral", worker="worker-a")
    replay = call_helper(tmp_path, claim_request(origin_repo), lifetime="ephemeral", worker="worker-a")
    status = call_helper(
        tmp_path,
        {"operation": "status", "request_id": "status-ephemeral"},
        lifetime="ephemeral",
        worker="worker-a",
    )

    allocation_root = Path(claimed["allocation_root"])
    marker = allocation_root / ".workenv-allocation.json"
    marker_payload = json.loads(marker.read_text(encoding="utf-8"))

    assert missing_worker["status"] == "conflict"
    assert "worker" in missing_worker["error"]
    assert claimed["status"] == "claimed"
    assert replay["replay"] is True
    assert replay["allocation_id"] == claimed["allocation_id"]
    assert status["active"]["allocation_id"] == claimed["allocation_id"]
    assert claimed["worker_lifetime"] == "ephemeral"
    assert allocation_root.parent == tmp_path / "worker/allocations"
    assert stat.S_IMODE(allocation_root.stat().st_mode) == 0o700
    assert Path(claimed["worktree"]) == allocation_root / "worktree"
    assert Path(claimed["service_state_dir"]) == allocation_root / "service-state"
    assert marker_payload["allocation_id"] == claimed["allocation_id"]
    assert marker_payload["allocation_root"] == str(allocation_root)
    assert marker_payload["worker"] == "worker-a"
    assert marker_payload["task_id"] == "task-a"
    assert marker_payload["revision"] == origin_repo["first"]
    assert marker_payload["request_id"] == "claim-1"


def test_status_same_request_id_reads_fresh_active_state(tmp_path, origin_repo):
    status_request = {"operation": "status", "request_id": "native-status-worker-a"}

    available = call_helper(tmp_path, status_request)
    claimed = call_helper(tmp_path, claim_request(origin_repo))
    busy = call_helper(tmp_path, status_request)
    collected = call_helper(
        tmp_path,
        {"operation": "collect", "request_id": "collect-status-fresh", "task_id": "task-a"},
    )
    released = call_helper(
        tmp_path,
        release_request("task-a", "release-status-fresh", collected["collection_digest"]),
    )
    available_after_release = call_helper(tmp_path, status_request)

    assert available["status"] == "available"
    assert claimed["status"] == "claimed"
    assert busy["status"] == "claimed"
    assert busy["active"]["task_id"] == "task-a"
    assert released["status"] == "released"
    assert available_after_release["status"] == "available"
    assert available_after_release["active"]["status"] == "released"


def test_ephemeral_new_claim_after_release_gets_unique_allocation(tmp_path, origin_repo):
    first = call_helper(tmp_path, claim_request(origin_repo), lifetime="ephemeral", worker="worker-a")
    collected = call_helper(
        tmp_path,
        {"operation": "collect", "request_id": "collect-first", "task_id": "task-a"},
        lifetime="ephemeral",
        worker="worker-a",
    )
    released = call_helper(
        tmp_path,
        release_request("task-a", "release-first", collected["collection_digest"]),
        lifetime="ephemeral",
        worker="worker-a",
    )

    second = call_helper(
        tmp_path,
        claim_request(origin_repo, request_id="claim-2", task_id="task-b"),
        lifetime="ephemeral",
        worker="worker-a",
    )

    assert released["status"] == "released"
    assert second["status"] == "claimed"
    assert second["allocation_id"] != first["allocation_id"]
    assert not Path(first["allocation_root"]).exists()
    assert Path(second["allocation_root"]).exists()


def test_ephemeral_failed_claim_cleans_preactive_allocation_and_retries(tmp_path, origin_repo, monkeypatch):
    original_prepare = workspace.WorkspaceManager._prepare_mirror
    failed = False

    def fail_once(self, mirror, spec, revision):
        nonlocal failed
        if not failed:
            failed = True
            raise workspace.WorkspaceError("conflict", "simulated fetch failure")
        return original_prepare(self, mirror, spec, revision)

    monkeypatch.setattr(workspace.WorkspaceManager, "_prepare_mirror", fail_once)
    request = claim_request(origin_repo)

    failed_claim = call_helper(tmp_path, request, lifetime="ephemeral", worker="worker-a")
    allocations = list((tmp_path / "worker/allocations").iterdir())
    retried = call_helper(tmp_path, request, lifetime="ephemeral", worker="worker-a")

    assert failed_claim["status"] == "conflict"
    assert "fetch failure" in failed_claim["error"]
    assert allocations == []
    assert retried["status"] == "claimed"
    assert Path(retried["allocation_root"]).exists()


def test_ephemeral_release_preserves_dirty_untracked_and_committed_work_before_cleanup(tmp_path, origin_repo):
    claimed = call_helper(tmp_path, claim_request(origin_repo), lifetime="ephemeral", worker="worker-a")
    worktree = Path(claimed["worktree"])
    run_git(
        worktree,
        "-c",
        "user.email=test@example.com",
        "-c",
        "user.name=Test User",
        "commit",
        "--allow-empty",
        "-m",
        "unpublished",
    )
    (worktree / "README.md").write_text("base\nstaged\n", encoding="utf-8")
    run_git(worktree, "add", "README.md")
    (worktree / "README.md").write_text("base\nstaged\nunstaged\n", encoding="utf-8")
    (worktree / "notes.txt").write_text("untracked\n", encoding="utf-8")

    collected = call_helper(
        tmp_path,
        {"operation": "collect", "request_id": "collect-dirty", "task_id": "task-a"},
        lifetime="ephemeral",
        worker="worker-a",
    )
    released = call_helper(
        tmp_path,
        release_request("task-a", "release-dirty", collected["collection_digest"]),
        lifetime="ephemeral",
        worker="worker-a",
    )

    assert released["status"] == "released"
    assert not Path(claimed["allocation_root"]).exists()
    assert Path(collected["metadata_path"]).exists()
    assert b"+staged" in Path(collected["archives"]["staged_diff"]).read_bytes()
    assert b"+unstaged" in Path(collected["archives"]["unstaged_diff"]).read_bytes()
    assert Path(collected["archives"]["bundle"]).is_file()
    with tarfile.open(collected["archives"]["untracked"]) as archive:
        assert archive.extractfile("notes.txt").read() == b"untracked\n"


def test_ephemeral_cleanup_failure_is_busy_and_same_release_retries(tmp_path, origin_repo, monkeypatch):
    claimed = call_helper(tmp_path, claim_request(origin_repo), lifetime="ephemeral", worker="worker-a")
    collected = call_helper(
        tmp_path,
        {"operation": "collect", "request_id": "collect-cleanup", "task_id": "task-a"},
        lifetime="ephemeral",
        worker="worker-a",
    )
    original_cleanup = workspace.WorkspaceManager._cleanup_ephemeral_allocation
    failed = False

    def fail_once(self, active):
        nonlocal failed
        if not failed:
            failed = True
            raise workspace.WorkspaceError("failed_cleanup_required", "simulated cleanup failure", code=5)
        return original_cleanup(self, active)

    monkeypatch.setattr(workspace.WorkspaceManager, "_cleanup_ephemeral_allocation", fail_once)
    request = release_request("task-a", "release-cleanup", collected["collection_digest"])

    failed_release = call_helper(tmp_path, request, lifetime="ephemeral", worker="worker-a")
    assert failed_release["status"] == "failed_cleanup_required"
    assert Path(claimed["allocation_root"]).exists()

    busy = call_helper(
        tmp_path,
        claim_request(origin_repo, request_id="claim-busy", task_id="task-b"),
        lifetime="ephemeral",
        worker="worker-a",
    )
    retried = call_helper(tmp_path, request, lifetime="ephemeral", worker="worker-a")

    assert busy["status"] == "busy"
    assert busy["active"]["status"] == "cleanup_required"
    assert retried["status"] == "released"
    assert not Path(claimed["allocation_root"]).exists()


def test_ephemeral_cleanup_retry_rechecks_dirty_worktree_before_delete(tmp_path, origin_repo, monkeypatch):
    claimed = call_helper(tmp_path, claim_request(origin_repo), lifetime="ephemeral", worker="worker-a")
    collected = call_helper(
        tmp_path,
        {"operation": "collect", "request_id": "collect-retry-dirty", "task_id": "task-a"},
        lifetime="ephemeral",
        worker="worker-a",
    )
    original_cleanup = workspace.WorkspaceManager._cleanup_ephemeral_allocation
    failed = False

    def fail_once(self, active):
        nonlocal failed
        if not failed:
            failed = True
            raise workspace.WorkspaceError("failed_cleanup_required", "simulated cleanup failure", code=5)
        return original_cleanup(self, active)

    monkeypatch.setattr(workspace.WorkspaceManager, "_cleanup_ephemeral_allocation", fail_once)
    request = release_request("task-a", "release-retry-dirty", collected["collection_digest"])

    failed_release = call_helper(tmp_path, request, lifetime="ephemeral", worker="worker-a")
    worktree = Path(claimed["worktree"])
    (worktree / "late.txt").write_text("uncollected\n", encoding="utf-8")
    retried = call_helper(tmp_path, request, lifetime="ephemeral", worker="worker-a")

    assert failed_release["status"] == "failed_cleanup_required"
    assert retried["status"] == "changed_since_collection"
    assert Path(claimed["allocation_root"]).exists()
    assert (worktree / "late.txt").read_text(encoding="utf-8") == "uncollected\n"


def test_ephemeral_cleanup_retry_rechecks_runtime_quiescent_before_delete(tmp_path, origin_repo, monkeypatch):
    claimed = call_helper(tmp_path, claim_request(origin_repo), lifetime="ephemeral", worker="worker-a")
    collected = call_helper(
        tmp_path,
        {"operation": "collect", "request_id": "collect-retry-runtime", "task_id": "task-a"},
        lifetime="ephemeral",
        worker="worker-a",
    )
    original_cleanup = workspace.WorkspaceManager._cleanup_ephemeral_allocation
    failed = False

    def fail_once(self, active):
        nonlocal failed
        if not failed:
            failed = True
            raise workspace.WorkspaceError("failed_cleanup_required", "simulated cleanup failure", code=5)
        return original_cleanup(self, active)

    monkeypatch.setattr(workspace.WorkspaceManager, "_cleanup_ephemeral_allocation", fail_once)
    request = release_request("task-a", "release-retry-runtime", collected["collection_digest"])

    failed_release = call_helper(tmp_path, request, lifetime="ephemeral", worker="worker-a")
    retry_without_ack = dict(request)
    retry_without_ack["request_id"] = "release-retry-runtime-without-ack"
    del retry_without_ack["runtime_quiescent"]
    retried = call_helper(tmp_path, retry_without_ack, lifetime="ephemeral", worker="worker-a")

    assert failed_release["status"] == "failed_cleanup_required"
    assert retried["status"] == "runtime_active"
    assert Path(claimed["allocation_root"]).exists()


def test_ephemeral_release_requires_runtime_quiescent_ack_before_cleanup(tmp_path, origin_repo):
    claimed = call_helper(tmp_path, claim_request(origin_repo), lifetime="ephemeral", worker="worker-a")
    collected = call_helper(
        tmp_path,
        {"operation": "collect", "request_id": "collect-runtime", "task_id": "task-a"},
        lifetime="ephemeral",
        worker="worker-a",
    )

    released = call_helper(
        tmp_path,
        {
            "operation": "release",
            "request_id": "release-runtime",
            "task_id": "task-a",
            "collection_digest": collected["collection_digest"],
        },
        lifetime="ephemeral",
        worker="worker-a",
    )

    assert released["status"] == "runtime_active"
    assert "runtime_quiescent" in released["error"]
    assert Path(claimed["allocation_root"]).exists()


def test_ephemeral_wrong_marker_blocks_cleanup(tmp_path, origin_repo):
    claimed = call_helper(tmp_path, claim_request(origin_repo), lifetime="ephemeral", worker="worker-a")
    collected = call_helper(
        tmp_path,
        {"operation": "collect", "request_id": "collect-marker", "task_id": "task-a"},
        lifetime="ephemeral",
        worker="worker-a",
    )
    marker = Path(claimed["allocation_root"]) / ".workenv-allocation.json"
    marker_payload = json.loads(marker.read_text(encoding="utf-8"))
    marker_payload["task_id"] = "other-task"
    marker.write_text(json.dumps(marker_payload), encoding="utf-8")

    released = call_helper(
        tmp_path,
        release_request("task-a", "release-marker", collected["collection_digest"]),
        lifetime="ephemeral",
        worker="worker-a",
    )

    assert released["status"] == "failed_cleanup_required"
    assert "marker" in released["error"]
    assert Path(claimed["allocation_root"]).exists()


def test_ephemeral_old_release_replay_does_not_delete_new_allocation(tmp_path, origin_repo):
    first = call_helper(tmp_path, claim_request(origin_repo), lifetime="ephemeral", worker="worker-a")
    collected = call_helper(
        tmp_path,
        {"operation": "collect", "request_id": "collect-old", "task_id": "task-a"},
        lifetime="ephemeral",
        worker="worker-a",
    )
    old_release = release_request("task-a", "release-old", collected["collection_digest"])
    released = call_helper(tmp_path, old_release, lifetime="ephemeral", worker="worker-a")
    second = call_helper(
        tmp_path,
        claim_request(origin_repo, request_id="claim-new", task_id="task-b"),
        lifetime="ephemeral",
        worker="worker-a",
    )
    replay = call_helper(tmp_path, old_release, lifetime="ephemeral", worker="worker-a")

    assert released["status"] == "released"
    assert replay["replay"] is True
    assert not Path(first["allocation_root"]).exists()
    assert Path(second["allocation_root"]).exists()


def test_ephemeral_release_deletes_only_allocation_and_preserves_stable_state(tmp_path, origin_repo):
    claimed = call_helper(tmp_path, claim_request(origin_repo), lifetime="ephemeral", worker="worker-a")
    collected = call_helper(
        tmp_path,
        {"operation": "collect", "request_id": "collect-stable", "task_id": "task-a"},
        lifetime="ephemeral",
        worker="worker-a",
    )
    stable_sentinel = tmp_path / "worker/allocations/keep.txt"
    stable_sentinel.write_text("keep\n", encoding="utf-8")

    released = call_helper(
        tmp_path,
        release_request("task-a", "release-stable", collected["collection_digest"]),
        lifetime="ephemeral",
        worker="worker-a",
    )

    assert released["status"] == "released"
    assert not Path(claimed["allocation_root"]).exists()
    assert stable_sentinel.read_text(encoding="utf-8") == "keep\n"
    assert (tmp_path / "worker/state/workspace.lock").is_file()
    assert Path(collected["metadata_path"]).is_file()
