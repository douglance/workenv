import json
import subprocess
from pathlib import Path

from scripts import workenv_controller as controller


FLEET = {
    "tailnet_suffix": "tail.example.ts.net",
    "remote_user": "exedev",
    "workers": [{"name": "workenv-01", "class": "linux"}],
}
REVISION = "a" * 40
REPO = {"owner": "operator", "name": "apoc"}


def completed(stdout="{}", stderr="", returncode=0):
    return {"status": "completed", "returncode": returncode, "stdout": stdout, "stderr": stderr}


def write_task(state: Path, *, status="claimed", remote_execution_ids=None, runtime=None):
    controller.record_task(
        state,
        task_id="task-a",
        worker="workenv-01",
        project="apoc",
        repo=REPO,
        revision=REVISION,
        reservation_id="reservation-a",
        session_id="session-a",
        source="remote",
        status=status,
        worktree="/remote/worktree",
    )
    path = controller.task_record_path(state, "task-a")
    record = json.loads(path.read_text(encoding="utf-8"))
    if remote_execution_ids is not None:
        record["remote_execution_ids"] = remote_execution_ids
    if runtime is not None:
        record["runtime"] = runtime
    controller.write_json_atomic(path, record)
    return record


def write_collection(state: Path, *, task_id="task-a", revision=REVISION):
    local_dir = state / "collections" / "workenv-01" / task_id
    tmp = local_dir / "placeholder"
    tmp.mkdir(parents=True)
    archive = tmp / "tracked.diff"
    archive.write_bytes(b"diff")
    metadata_without_digest = {
        "task_id": task_id,
        "repo": REPO,
        "source": "remote",
        "base": revision,
        "archives": {"tracked_diff": "tracked.diff"},
    }
    digest = controller.collection_digest(metadata_without_digest, [archive])
    final_dir = local_dir / digest
    final_dir.mkdir()
    archive.replace(final_dir / "tracked.diff")
    tmp.rmdir()
    metadata = {**metadata_without_digest, "collection_digest": digest}
    (final_dir / "metadata.json").write_text(controller.canonical_json(metadata), encoding="utf-8")
    return digest


def execution_payload(execution_id, *, status="running", outcome="pending", labels=None):
    return {
        "id": execution_id,
        "status": status,
        "outcome": outcome,
        "spec": {
            "labels": labels
            if labels is not None
            else {"workenv.task_id": "task-a", "workenv.worker": "workenv-01", "workenv.kind": "service"}
        },
    }


def test_task_run_service_adds_service_kind_label_and_keeps_identity_labels(monkeypatch, tmp_path):
    write_task(tmp_path)
    observed = {}

    def run(args, *, timeout, text):
        observed["remote"] = args[-1]
        return completed(json.dumps({"id": "exec-service"}))

    def run_workspace_request(*args, **kwargs):
        return {"status": "runtime_recorded", "ok": True, "runtime": {"apoc_execution_ids": ["exec-service"]}}

    monkeypatch.setattr(controller, "run", run)
    monkeypatch.setattr(controller, "run_workspace_request", run_workspace_request)

    result = controller.task_run(FLEET, "workenv-01", "task-a", "run-1", "serve", ["python3", "-m", "http.server"], local_state=tmp_path, service=True)

    assert result["status"] == "started"
    assert "--label workenv.task_id=task-a" in observed["remote"]
    assert "--label workenv.worker=workenv-01" in observed["remote"]
    assert "--label workenv.kind=service" in observed["remote"]


def test_release_verifies_collection_before_inspecting_or_stopping_services(monkeypatch, tmp_path):
    write_task(tmp_path, remote_execution_ids=["exec-service"])

    def forbidden(*args, **kwargs):
        raise AssertionError("invalid collection must stop before remote inspection")

    monkeypatch.setattr(controller, "run", forbidden)
    monkeypatch.setattr(controller, "inspect_task_activity", forbidden)
    monkeypatch.setattr(controller, "run_workspace_request", forbidden)

    result = controller.release(FLEET, "workenv-01", "task-a", "release-1", "b" * 64, local_state=tmp_path)

    assert result["status"] == "uncollected"


def test_release_cancels_only_matching_live_service_before_fresh_activity_and_remote_release(monkeypatch, tmp_path):
    write_task(tmp_path, remote_execution_ids=["exec-service"])
    digest = write_collection(tmp_path)
    events = []

    def run(args, *, timeout, text):
        remote = args[-1]
        if "execution get exec-service" in remote:
            events.append("get")
            assert "--verbosity trace" in remote
            assert "Inspect recorded workenv task task-a execution exec-service before release." in remote
            return completed(json.dumps(execution_payload("exec-service")))
        if "execution cancel exec-service" in remote:
            events.append("cancel")
            assert "--idempotency-key release-1-service-exec-service" in remote
            assert "Stop workenv service execution exec-service for task task-a." in remote
            return completed(json.dumps({"id": "exec-service", "status": "canceled"}))
        raise AssertionError(remote)

    def inspect_task_activity(*args, **kwargs):
        events.append("activity")
        return {"status": "clear", "ok": True}

    def run_workspace_request(*args, **kwargs):
        events.append("release")
        return {"status": "released", "ok": True}

    monkeypatch.setattr(controller, "run", run)
    monkeypatch.setattr(controller, "inspect_task_activity", inspect_task_activity)
    monkeypatch.setattr(controller, "run_workspace_request", run_workspace_request)

    result = controller.release(FLEET, "workenv-01", "task-a", "release-1", digest, local_state=tmp_path)

    assert result["status"] == "released"
    assert events == ["get", "cancel", "activity", "release"]


def test_release_refuses_live_recorded_execution_with_unmatched_identity_labels(monkeypatch, tmp_path):
    write_task(tmp_path, remote_execution_ids=["exec-other"])
    digest = write_collection(tmp_path)
    calls = []

    def run(args, *, timeout, text):
        calls.append(args[-1])
        return completed(json.dumps(execution_payload("exec-other", labels={"workenv.task_id": "other", "workenv.worker": "workenv-01", "workenv.kind": "service"})))

    monkeypatch.setattr(controller, "run", run)
    monkeypatch.setattr(controller, "inspect_task_activity", lambda *args, **kwargs: (_ for _ in ()).throw(AssertionError("unmatched execution must block before fresh activity")))
    monkeypatch.setattr(controller, "run_workspace_request", lambda *args, **kwargs: (_ for _ in ()).throw(AssertionError("unmatched execution must block release")))

    result = controller.release(FLEET, "workenv-01", "task-a", "release-2", digest, local_state=tmp_path)

    assert result["status"] == "unmatched_execution"
    assert len(calls) == 1
    assert "execution cancel" not in calls[0]


def test_release_refuses_live_non_service_execution_without_canceling(monkeypatch, tmp_path):
    write_task(tmp_path, remote_execution_ids=["exec-build"])
    digest = write_collection(tmp_path)
    calls = []

    def run(args, *, timeout, text):
        calls.append(args[-1])
        return completed(json.dumps(execution_payload("exec-build", labels={"workenv.task_id": "task-a", "workenv.worker": "workenv-01", "workenv.kind": "task"})))

    monkeypatch.setattr(controller, "run", run)
    monkeypatch.setattr(controller, "inspect_task_activity", lambda *args, **kwargs: (_ for _ in ()).throw(AssertionError("live build must block before fresh activity")))
    monkeypatch.setattr(controller, "run_workspace_request", lambda *args, **kwargs: (_ for _ in ()).throw(AssertionError("live build must block release")))

    result = controller.release(FLEET, "workenv-01", "task-a", "release-3", digest, local_state=tmp_path)

    assert result["status"] == "live_task_activity"
    assert all("execution cancel" not in call for call in calls)


def test_release_refuses_uncertain_recorded_execution_state(monkeypatch, tmp_path):
    write_task(tmp_path, remote_execution_ids=["exec-unknown"])
    digest = write_collection(tmp_path)

    monkeypatch.setattr(controller, "run", lambda *args, **kwargs: completed(json.dumps({"id": "exec-unknown", "status": "mystery", "spec": {"labels": {}}})))
    monkeypatch.setattr(controller, "inspect_task_activity", lambda *args, **kwargs: (_ for _ in ()).throw(AssertionError("uncertain execution must block before fresh activity")))

    result = controller.release(FLEET, "workenv-01", "task-a", "release-4", digest, local_state=tmp_path)

    assert result["status"] == "task_activity_unknown"


def test_release_fresh_activity_still_blocks_herdr_agents_after_service_stop(monkeypatch, tmp_path):
    write_task(tmp_path, remote_execution_ids=["exec-service"])
    digest = write_collection(tmp_path)
    events = []

    def run(args, *, timeout, text):
        remote = args[-1]
        if "execution get exec-service" in remote:
            events.append("get")
            return completed(json.dumps(execution_payload("exec-service")))
        if "execution cancel exec-service" in remote:
            events.append("cancel")
            return completed(json.dumps({"id": "exec-service", "status": "canceled"}))
        raise AssertionError(remote)

    def inspect_task_activity(*args, **kwargs):
        events.append("activity")
        return {"status": "live_task_activity", "ok": False, "live": [{"kind": "herdr_pane", "pane_id": "pane-a"}]}

    monkeypatch.setattr(controller, "run", run)
    monkeypatch.setattr(controller, "inspect_task_activity", inspect_task_activity)
    monkeypatch.setattr(controller, "run_workspace_request", lambda *args, **kwargs: (_ for _ in ()).throw(AssertionError("Herdr activity must block release")))

    result = controller.release(FLEET, "workenv-01", "task-a", "release-5", digest, local_state=tmp_path)

    assert result["status"] == "live_task_activity"
    assert events == ["get", "cancel", "activity"]


def test_apoc_help_declares_cancel_stop_alias_and_get_trace_controls():
    cancel = subprocess.run(["apoc", "execution", "cancel", "--help"], check=True, capture_output=True, text=True)
    get = subprocess.run(["apoc", "execution", "get", "--help"], check=True, capture_output=True, text=True)

    assert "Aliases: stop" in cancel.stdout
    assert "--idempotency-key" in cancel.stdout
    assert "--purpose" in cancel.stdout
    assert "--verbosity" in get.stdout
