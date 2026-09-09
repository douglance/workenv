import base64
import hashlib
import json
import os
import subprocess
import sys
import tarfile
import threading
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


def call_helper(tmp_path, payload):
    encoded = base64.b64encode(json.dumps(payload).encode("utf-8")).decode("ascii")
    return workspace.main(["--root", str(tmp_path / "worker"), "--request-base64", encoded])


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


def bundle_claim_request(tmp_path, origin_repo, request_id="claim-bundle", task_id="task-a"):
    root = tmp_path / "worker"
    incoming = root / "incoming"
    incoming.mkdir(parents=True)
    bundle = incoming / "source.bundle"
    run_git(origin_repo["source"], "bundle", "create", str(bundle), "--all")
    return {
        "operation": "claim",
        "request_id": request_id,
        "task_id": task_id,
        "repo": {"owner": "operator", "name": "groktris"},
        "source_bundle": str(bundle),
        "revision": origin_repo["first"],
        "branch": f"workenv/{task_id}",
    }


def test_claim_replay_binds_request_to_exact_payload(tmp_path, origin_repo):
    request = claim_request(origin_repo)
    first = call_helper(tmp_path, request)
    replay = call_helper(tmp_path, request)

    assert first["status"] == "claimed"
    assert replay["status"] == "claimed"
    assert replay["replay"] is True
    assert replay["worktree"] == first["worktree"]

    mismatch = dict(request)
    mismatch["revision"] = origin_repo["second"]
    result = call_helper(tmp_path, mismatch)

    assert result["status"] == "conflict"
    assert "request_id" in result["error"]


def test_claim_accepts_incoming_source_bundle_for_exact_revision(tmp_path, origin_repo):
    request = bundle_claim_request(tmp_path, origin_repo)

    claimed = call_helper(tmp_path, request)

    assert claimed["status"] == "claimed"
    worktree = Path(claimed["worktree"])
    assert run_git(worktree, "rev-parse", "HEAD") == origin_repo["first"]
    assert (worktree / "README.md").read_text(encoding="utf-8") == "base\n"
    assert Path(claimed["service_state_dir"]).is_dir()
    assert Path(claimed["service_state_dir"]).name == "task-a"


def test_collection_skips_rebuildable_nix_cache_but_preserves_source(tmp_path, origin_repo):
    claimed = call_helper(tmp_path, claim_request(origin_repo))
    worktree = Path(claimed["worktree"])
    cache = worktree / ".devenv"
    cache.mkdir()
    (cache / "store").symlink_to("/nix/store/missing-example")
    (worktree / "new-source.txt").write_text("retain me\n")
    collected = call_helper(tmp_path, {"operation": "collect", "request_id": "collect-nix-cache", "task_id": "task-a"})
    assert collected["status"] == "collected"
    with tarfile.open(collected["archives"]["untracked"]) as archive:
        assert "new-source.txt" in archive.getnames()
        assert not any(name.startswith(".devenv") for name in archive.getnames())
    assert run_git(worktree, "rev-parse", "HEAD") == origin_repo["first"]


def test_cli_accepts_base64_request_and_writes_json_stdout(tmp_path):
    request = {"operation": "status", "request_id": "status-1"}
    encoded = base64.b64encode(json.dumps(request).encode("utf-8")).decode("ascii")

    completed = subprocess.run(
        [
            sys.executable,
            "-m",
            "remote.workspace",
            "--root",
            str(tmp_path / "worker"),
            "--request-base64",
            encoded,
        ],
        cwd=Path(__file__).resolve().parents[1],
        check=True,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )

    assert json.loads(completed.stdout)["status"] == "available"


def test_claim_rejects_source_bundle_outside_incoming_root(tmp_path, origin_repo):
    outside = tmp_path / "outside.bundle"
    run_git(origin_repo["source"], "bundle", "create", str(outside), "--all")
    request = bundle_claim_request(tmp_path, origin_repo)
    request["source_bundle"] = str(outside)

    result = call_helper(tmp_path, request)

    assert result["status"] == "conflict"
    assert "incoming" in result["error"]


def test_claim_rejects_https_remote_that_does_not_match_requested_repo(tmp_path, origin_repo):
    request = claim_request(origin_repo)
    request["remote_url"] = "https://github.com/acme/other.git"

    result = call_helper(tmp_path, request)

    assert result["status"] == "conflict"
    assert "owner/name" in result["error"]


def test_claim_rejects_credential_bearing_https_remote(tmp_path, origin_repo):
    request = claim_request(origin_repo)
    request["remote_url"] = "https://token@example.com/acme/widgets.git"

    result = call_helper(tmp_path, request)

    assert result["status"] == "conflict"
    assert "credentials" in result["error"]


def test_claim_rejects_unsafe_repo_owner_and_name_slugs(tmp_path, origin_repo):
    request = claim_request(origin_repo)
    request["repo"] = {"owner": "acme/../../x", "name": "widgets"}

    result = call_helper(tmp_path, request)

    assert result["status"] == "conflict"
    assert "repo owner/name" in result["error"]


def test_claim_rejects_existing_unpublished_branch_before_worktree_add(tmp_path, origin_repo):
    request = claim_request(origin_repo)
    first = call_helper(tmp_path, request)
    worktree = Path(first["worktree"])
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
    collected = call_helper(
        tmp_path,
        {"operation": "collect", "request_id": "collect-before-release", "task_id": "task-a"},
    )
    released = call_helper(
        tmp_path,
        {
            "operation": "release",
            "request_id": "release-before-reclaim",
            "task_id": "task-a",
            "collection_digest": collected["collection_digest"],
        },
    )
    unpublished = run_git(worktree, "rev-parse", "HEAD")
    mirror = tmp_path / "worker/mirrors/acme__widgets.git"
    run_git(mirror, "update-ref", "refs/heads/workenv/task-b", unpublished)
    second = claim_request(origin_repo, request_id="claim-2", task_id="task-b")

    result = call_helper(tmp_path, second)

    assert released["status"] == "released"
    assert result["status"] == "conflict"
    assert "branch" in result["error"]
    assert run_git(worktree, "rev-parse", "HEAD") != origin_repo["first"]


def test_status_is_available_after_release_while_retaining_previous_task(tmp_path, origin_repo):
    claimed = call_helper(tmp_path, claim_request(origin_repo))
    collected = call_helper(
        tmp_path,
        {"operation": "collect", "request_id": "collect-status", "task_id": "task-a"},
    )
    released = call_helper(
        tmp_path,
        {
            "operation": "release",
            "request_id": "release-status",
            "task_id": "task-a",
            "collection_digest": collected["collection_digest"],
        },
    )
    status = call_helper(tmp_path, {"operation": "status", "request_id": "status-after-release"})

    assert released["status"] == "released"
    assert status["status"] == "available"
    assert status["active"]["status"] == "released"
    assert status["active"]["task_id"] == "task-a"
    assert Path(claimed["worktree"]).exists()


def test_record_runtime_appends_observed_ids_idempotently_to_active_task_status(tmp_path, origin_repo):
    claimed = call_helper(tmp_path, claim_request(origin_repo))
    first = call_helper(
        tmp_path,
        {
            "operation": "record-runtime",
            "request_id": "runtime-1",
            "task_id": "task-a",
            "revision": origin_repo["first"],
            "runtime": {
                "herdr": {
                    "machine_id": "machine-1",
                    "session_id": "session-1",
                    "pane_id": "pane-1",
                    "agent_id": "agent-1",
                },
                "apoc_execution_ids": ["exec-1", "exec-2"],
            },
        },
    )
    second = call_helper(
        tmp_path,
        {
            "operation": "record-runtime",
            "request_id": "runtime-2",
            "task_id": "task-a",
            "revision": origin_repo["first"],
            "runtime": {
                "herdr": {
                    "machine_id": "machine-1",
                    "session_id": "session-1",
                    "pane_id": "pane-1",
                    "agent_id": "agent-2",
                },
                "apoc_execution_ids": ["exec-2", "exec-3"],
            },
        },
    )
    status = call_helper(tmp_path, {"operation": "status", "request_id": "status-runtime"})

    assert claimed["service_state_dir"] == status["active"]["service_state_dir"]
    assert first["status"] == "runtime_recorded"
    assert second["runtime"]["herdr"]["agent_ids"] == ["agent-1", "agent-2"]
    assert second["runtime"]["apoc_execution_ids"] == ["exec-1", "exec-2", "exec-3"]
    assert status["active"]["runtime"] == second["runtime"]


def test_record_runtime_replay_does_not_append_duplicate_observed_ids(tmp_path, origin_repo):
    call_helper(tmp_path, claim_request(origin_repo))
    request = {
        "operation": "record-runtime",
        "request_id": "runtime-replay",
        "task_id": "task-a",
        "revision": origin_repo["first"],
        "runtime": {
            "herdr": {"machine_id": "machine-1", "session_id": "session-1"},
            "apoc_execution_ids": ["exec-1"],
        },
    }

    first = call_helper(tmp_path, request)
    replay = call_helper(tmp_path, request)
    status = call_helper(tmp_path, {"operation": "status", "request_id": "status-runtime-replay"})

    assert replay["replay"] is True
    assert status["active"]["runtime"] == first["runtime"]
    assert status["active"]["runtime"]["apoc_execution_ids"] == ["exec-1"]


def test_record_runtime_refuses_wrong_task_and_wrong_revision(tmp_path, origin_repo):
    call_helper(tmp_path, claim_request(origin_repo))

    wrong_task = call_helper(
        tmp_path,
        {
            "operation": "record-runtime",
            "request_id": "runtime-wrong-task",
            "task_id": "task-b",
            "revision": origin_repo["first"],
            "runtime": {"apoc_execution_ids": ["exec-1"]},
        },
    )
    wrong_revision = call_helper(
        tmp_path,
        {
            "operation": "record-runtime",
            "request_id": "runtime-wrong-revision",
            "task_id": "task-a",
            "revision": origin_repo["second"],
            "runtime": {"apoc_execution_ids": ["exec-1"]},
        },
    )
    status = call_helper(tmp_path, {"operation": "status", "request_id": "status-runtime-refused"})

    assert wrong_task["status"] == "busy"
    assert wrong_revision["status"] == "conflict"
    assert "revision" in wrong_revision["error"]
    assert status["active"]["runtime"] == {"herdr": {}, "apoc_execution_ids": []}


def test_simultaneous_claims_allow_one_active_task_per_worker(tmp_path, origin_repo):
    barrier = threading.Barrier(2)
    results = []

    def claim(task_id):
        barrier.wait()
        results.append(call_helper(tmp_path, claim_request(origin_repo, f"claim-{task_id}", task_id)))

    threads = [threading.Thread(target=claim, args=(task_id,)) for task_id in ("a", "b")]
    for thread in threads:
        thread.start()
    for thread in threads:
        thread.join()

    statuses = sorted(result["status"] for result in results)
    assert statuses == ["busy", "claimed"]


def test_collect_refuses_if_source_changes_while_archiving(tmp_path, origin_repo, monkeypatch):
    claimed = call_helper(tmp_path, claim_request(origin_repo))
    worktree = Path(claimed["worktree"])
    (worktree / "notes.txt").write_text("before\n", encoding="utf-8")
    original_write_tar = workspace._write_tar
    changed = False

    def mutate_once(*args, **kwargs):
        nonlocal changed
        result = original_write_tar(*args, **kwargs)
        if not changed:
            (worktree / "notes.txt").write_text("after\n", encoding="utf-8")
            changed = True
        return result

    monkeypatch.setattr(workspace, "_write_tar", mutate_once)

    result = call_helper(
        tmp_path,
        {"operation": "collect", "request_id": "collect-race", "task_id": "task-a"},
    )

    assert result["status"] == "changed_since_collection"


def test_collect_refuses_dirty_submodules(tmp_path, origin_repo):
    submodule = tmp_path / "submodule"
    submodule.mkdir()
    run_git(submodule, "init", "-b", "main")
    run_git(submodule, "config", "user.email", "test@example.com")
    run_git(submodule, "config", "user.name", "Test User")
    (submodule / "nested.txt").write_text("nested\n", encoding="utf-8")
    run_git(submodule, "add", "nested.txt")
    run_git(submodule, "commit", "-m", "nested")

    source = tmp_path / "super"
    source.mkdir()
    run_git(source, "init", "-b", "main")
    run_git(source, "config", "user.email", "test@example.com")
    run_git(source, "config", "user.name", "Test User")
    run_git(
        source,
        "-c",
        "protocol.file.allow=always",
        "submodule",
        "add",
        str(submodule),
        "deps/sub",
    )
    run_git(source, "commit", "-m", "add submodule")
    revision = run_git(source, "rev-parse", "HEAD")
    bare = tmp_path / "super.git"
    run_git(tmp_path, "clone", "--bare", str(source), str(bare))

    request = {
        "operation": "claim",
        "request_id": "claim-submodule",
        "task_id": "task-a",
        "repo": {"owner": "acme", "name": "super"},
        "remote_url": bare.as_uri(),
        "revision": revision,
        "branch": "workenv/task-a",
    }
    claimed = call_helper(tmp_path, request)
    worktree = Path(claimed["worktree"])
    run_git(worktree, "-c", "protocol.file.allow=always", "submodule", "update", "--init", "--recursive")
    (worktree / "deps/sub/nested.txt").write_text("dirty\n", encoding="utf-8")
    result = call_helper(
        tmp_path,
        {"operation": "collect", "request_id": "collect-submodule", "task_id": "task-a"},
    )

    assert result["status"] == "conflict"
    assert "submodule" in result["error"]


def test_collect_preserves_dirty_untracked_work_and_release_refuses_changed_worktree(tmp_path, origin_repo):
    claimed = call_helper(tmp_path, claim_request(origin_repo))
    worktree = Path(claimed["worktree"])
    (worktree / "README.md").write_text("base\nagent change\n", encoding="utf-8")
    (worktree / "notes.txt").write_text("untracked\n", encoding="utf-8")

    collected = call_helper(
        tmp_path,
        {
            "operation": "collect",
            "request_id": "collect-1",
            "task_id": "task-a",
            "evidence_paths": ["notes.txt"],
        },
    )

    assert collected["status"] == "collected"
    assert collected["fingerprint"]["dirty"] is True
    assert collected["archives"]["untracked"].endswith(".tar")

    with tarfile.open(collected["archives"]["untracked"], "r") as archive:
        assert archive.extractfile("notes.txt").read() == b"untracked\n"
    assert (worktree / "notes.txt").read_text(encoding="utf-8") == "untracked\n"

    (worktree / "README.md").write_text("base\nagent change\nlate edit\n", encoding="utf-8")
    released = call_helper(
        tmp_path,
        {
            "operation": "release",
            "request_id": "release-1",
            "task_id": "task-a",
            "collection_digest": collected["collection_digest"],
        },
    )

    assert released["status"] == "changed_since_collection"
    assert worktree.exists()
    assert (worktree / "README.md").read_text(encoding="utf-8").endswith("late edit\n")


def test_collect_retry_after_receipt_crash_recovers_existing_archives(tmp_path, origin_repo, monkeypatch):
    call_helper(tmp_path, claim_request(origin_repo))
    worktree = tmp_path / "worker/worktrees/task-a"
    info_exclude = Path(run_git(worktree, "rev-parse", "--git-path", "info/exclude"))
    info_exclude.write_text(f"{info_exclude.read_text(encoding='utf-8')}\nignored-evidence.txt\n", encoding="utf-8")
    evidence = worktree / "ignored-evidence.txt"
    evidence.write_text("original evidence\n", encoding="utf-8")
    original_write_receipt = workspace.WorkspaceManager._write_receipt
    failed = False

    def fail_once_after_collect(self, request_id, payload_hash, result):
        nonlocal failed
        if request_id == "collect-crash" and result["status"] == "collected" and not failed:
            failed = True
            raise RuntimeError("simulated receipt write crash")
        return original_write_receipt(self, request_id, payload_hash, result)

    monkeypatch.setattr(workspace.WorkspaceManager, "_write_receipt", fail_once_after_collect)
    request = {
        "operation": "collect",
        "request_id": "collect-crash",
        "task_id": "task-a",
        "evidence_paths": ["ignored-evidence.txt"],
    }

    with pytest.raises(RuntimeError, match="simulated receipt write crash"):
        call_helper(tmp_path, request)

    collection_dir = next((tmp_path / "worker/collections/task-a").iterdir())
    first_metadata = json.loads((collection_dir / "metadata.json").read_text(encoding="utf-8"))
    evidence.unlink()

    recovered = call_helper(tmp_path, request)

    assert recovered["status"] == "collected"
    assert recovered["collection_digest"] == first_metadata["collection_digest"]
    assert recovered["metadata"] == first_metadata
    assert recovered["metadata_path"] == str(collection_dir / "metadata.json")
    with tarfile.open(recovered["archives"]["evidence"], "r") as archive:
        assert archive.extractfile("ignored-evidence.txt").read() == b"original evidence\n"
    assert not evidence.exists()


def test_collect_existing_incomplete_directory_is_preserved_and_conflicts(tmp_path, origin_repo):
    call_helper(tmp_path, claim_request(origin_repo))
    request = {"operation": "collect", "request_id": "collect-incomplete", "task_id": "task-a"}
    collection_id = workspace._canonical_hash({"request_id": "collect-incomplete", "task_id": "task-a"})[:16]
    collection_dir = tmp_path / "worker/collections/task-a" / collection_id
    collection_dir.mkdir(parents=True)
    sentinel = collection_dir / "partial.tmp"
    sentinel.write_text("partial\n", encoding="utf-8")

    result = call_helper(tmp_path, request)

    assert result["status"] == "conflict"
    assert "new request_id" in result["error"]
    assert sentinel.read_text(encoding="utf-8") == "partial\n"


def test_collect_preserves_staged_and_unstaged_patches_separately(tmp_path, origin_repo):
    claimed = call_helper(tmp_path, claim_request(origin_repo))
    worktree = Path(claimed["worktree"])
    (worktree / "README.md").write_text("base\nstaged\n", encoding="utf-8")
    run_git(worktree, "add", "README.md")
    (worktree / "README.md").write_text("base\nstaged\nunstaged\n", encoding="utf-8")

    collected = call_helper(
        tmp_path,
        {"operation": "collect", "request_id": "collect-index", "task_id": "task-a"},
    )

    assert collected["status"] == "collected"
    assert b"+staged" in Path(collected["archives"]["staged_diff"]).read_bytes()
    assert b"+unstaged" in Path(collected["archives"]["unstaged_diff"]).read_bytes()


def test_receipt_paths_are_collision_proof_for_similar_request_ids(tmp_path, origin_repo):
    call_helper(tmp_path, claim_request(origin_repo))
    first = call_helper(
        tmp_path,
        {
            "operation": "record-runtime",
            "request_id": "same/name",
            "task_id": "task-a",
            "revision": origin_repo["first"],
            "runtime": {"apoc_execution_ids": ["exec-1"]},
        },
    )
    second = call_helper(
        tmp_path,
        {
            "operation": "record-runtime",
            "request_id": "same_name",
            "task_id": "task-a",
            "revision": origin_repo["first"],
            "runtime": {"apoc_execution_ids": ["exec-2"]},
        },
    )

    assert first["status"] == "runtime_recorded"
    assert second["status"] == "runtime_recorded"
    receipts = sorted(path for path in (tmp_path / "worker/state/receipts").glob("*.json") if path.name.startswith("same_name."))
    assert len(receipts) == 2
    assert receipts[0].name != receipts[1].name


def test_release_retains_clean_worktree_after_acknowledged_collection(tmp_path, origin_repo):
    claimed = call_helper(tmp_path, claim_request(origin_repo))
    worktree = Path(claimed["worktree"])
    collected = call_helper(
        tmp_path,
        {"operation": "collect", "request_id": "collect-clean", "task_id": "task-a"},
    )

    released = call_helper(
        tmp_path,
        {
            "operation": "release",
            "request_id": "release-clean",
            "task_id": "task-a",
            "collection_digest": collected["collection_digest"],
        },
    )

    assert released["status"] == "released"
    assert worktree.exists()
    assert Path(collected["metadata_path"]).exists()


def test_path_safety_rejects_evidence_escape_and_symlink(tmp_path, origin_repo):
    claimed = call_helper(tmp_path, claim_request(origin_repo))
    worktree = Path(claimed["worktree"])
    (worktree / "safe.txt").write_text("safe\n", encoding="utf-8")
    os.symlink(Path.home(), worktree / "home-link")

    escaped = call_helper(
        tmp_path,
        {
            "operation": "collect",
            "request_id": "collect-escape",
            "task_id": "task-a",
            "evidence_paths": ["../outside"],
        },
    )
    assert escaped["status"] == "conflict"
    assert "escapes" in escaped["error"]

    symlinked = call_helper(
        tmp_path,
        {
            "operation": "collect",
            "request_id": "collect-link",
            "task_id": "task-a",
            "evidence_paths": ["home-link"],
        },
    )
    assert symlinked["status"] == "conflict"
    assert "symlink" in symlinked["error"]


def test_collect_binary_tracked_diff_archive_is_reproducible(tmp_path, origin_repo):
    claimed = call_helper(tmp_path, claim_request(origin_repo))
    worktree = Path(claimed["worktree"])
    blob = bytes(range(256))
    (worktree / "payload.bin").write_bytes(blob)
    run_git(worktree, "add", "payload.bin")
    run_git(
        worktree,
        "-c",
        "user.email=test@example.com",
        "-c",
        "user.name=Test User",
        "commit",
        "-m",
        "add binary",
    )
    (worktree / "payload.bin").write_bytes(bytes(reversed(blob)))

    collected = call_helper(
        tmp_path,
        {"operation": "collect", "request_id": "collect-binary", "task_id": "task-a"},
    )

    assert collected["status"] == "collected"
    assert collected["archives"]["bundle"].endswith(".bundle")
    assert b"GIT binary patch" in Path(collected["archives"]["unstaged_diff"]).read_bytes()
    assert collected["metadata"]["head"] == run_git(worktree, "rev-parse", "HEAD")
    assert len(collected["fingerprint"]["diff_sha256"]) == hashlib.sha256().digest_size * 2


def test_repeat_collection_digest_is_stable_for_same_source_fingerprint(tmp_path, origin_repo):
    claimed = call_helper(tmp_path, claim_request(origin_repo))
    worktree = Path(claimed["worktree"])
    (worktree / "README.md").write_text("base\nsame change\n", encoding="utf-8")

    first = call_helper(
        tmp_path,
        {"operation": "collect", "request_id": "collect-stable-1", "task_id": "task-a"},
    )
    second = call_helper(
        tmp_path,
        {"operation": "collect", "request_id": "collect-stable-2", "task_id": "task-a"},
    )

    assert second["status"] == "collected"
    assert second["collection_digest"] == first["collection_digest"]
