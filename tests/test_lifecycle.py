"""Prove that a collected task can be reconstructed without its worker."""

import json
import shutil
import subprocess
import tarfile
from pathlib import Path

from remote.workspace import WorkspaceManager
from scripts.workenv_controller import verify_local_collection


def git(repo: Path, *args: str) -> str:
    return subprocess.run(
        ["git", "-c", "core.hooksPath=/dev/null", *args],
        cwd=repo,
        check=True,
        capture_output=True,
        text=True,
    ).stdout.strip()


def configure(repo: Path) -> None:
    git(repo, "config", "user.name", "Workenv test")
    git(repo, "config", "user.email", "workenv-test@example.invalid")


def test_collection_restores_unpublished_commit_index_worktree_and_untracked(tmp_path):
    origin = tmp_path / "origin"
    origin.mkdir()
    git(origin, "init", "--initial-branch=main")
    configure(origin)
    (origin / "source.txt").write_text("base\n")
    git(origin, "add", "source.txt")
    git(origin, "commit", "-m", "Base")
    revision = git(origin, "rev-parse", "HEAD")

    manager = WorkspaceManager(tmp_path / "worker")
    claimed = manager.handle({
        "operation": "claim",
        "request_id": "claim-restoration",
        "task_id": "restore-proof",
        "repo": {"owner": "workenv", "name": "fixture"},
        "remote_url": origin.as_uri(),
        "revision": revision,
        "branch": "workenv/restore-proof",
    })
    assert claimed["status"] == "claimed"
    worktree = Path(claimed["worktree"])
    configure(worktree)
    (worktree / "source.txt").write_text("unpublished commit\n")
    git(worktree, "add", "source.txt")
    git(worktree, "commit", "-m", "Unpublished work")
    head = git(worktree, "rev-parse", "HEAD")
    (worktree / "source.txt").write_text("staged change\n")
    git(worktree, "add", "source.txt")
    (worktree / "source.txt").write_text("unstaged change\n")
    (worktree / "new.bin").write_bytes(b"\x00untracked\xff\n")
    (worktree / "evidence.json").write_text('{"test":"passed"}\n')

    collected = manager.handle({
        "operation": "collect",
        "request_id": "collect-restoration",
        "task_id": "restore-proof",
        "evidence_paths": ["evidence.json"],
    })
    assert collected["status"] == "collected"
    local = tmp_path / "controller-collection"
    shutil.copytree(Path(collected["metadata_path"]).parent, local)
    assert verify_local_collection(
        local, collected["collection_digest"], task_id="restore-proof",
        repo={"owner": "workenv", "name": "fixture"}, source="remote",
    )["ok"]

    restored = tmp_path / "restored"
    git(tmp_path, "clone", str(origin), str(restored))
    git(restored, "fetch", str(local / "committed.bundle"), "HEAD")
    git(restored, "checkout", "--detach", "FETCH_HEAD")
    metadata = json.loads((local / "metadata.json").read_text())
    git(restored, "apply", "--index", str(local / metadata["archives"]["staged_diff"]))
    git(restored, "apply", str(local / metadata["archives"]["unstaged_diff"]))
    with tarfile.open(local / metadata["archives"]["untracked"]) as archive:
        archive.extractall(restored, filter="data")

    assert git(restored, "rev-parse", "HEAD") == head
    assert git(restored, "diff", "--cached", "--binary") == git(worktree, "diff", "--cached", "--binary")
    assert git(restored, "diff", "--binary") == git(worktree, "diff", "--binary")
    assert (restored / "new.bin").read_bytes() == (worktree / "new.bin").read_bytes()
    assert (restored / "evidence.json").read_bytes() == (worktree / "evidence.json").read_bytes()

    (local / metadata["archives"]["unstaged_diff"]).write_text("corrupted transfer\n")
    assert not verify_local_collection(local, collected["collection_digest"])["ok"]
