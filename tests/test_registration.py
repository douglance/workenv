import json

from scripts import workenv_controller as controller


def test_similar_machine_name_is_not_an_existing_worker(monkeypatch):
    fleet = controller.load_fleet()
    target = controller.worker_host(fleet, "workenv-01")
    calls = []

    def run(args, **kwargs):
        calls.append(args)
        if args == ["herdr", "machine", "list", "--json"]:
            return {"status": "completed", "returncode": 0, "stdout": json.dumps([
                {"id": "old", "label": "workenv-010", "target": "old.example", "session": "default", "enabled": True},
                {"id": "existing", "label": "dlance", "target": "dlance", "session": "default", "enabled": True},
            ]), "stderr": ""}
        assert args == ["herdr", "machine", "add", target, "--label", "workenv-01", "--remote-session", "workenv"]
        return {"status": "completed", "returncode": 0, "stdout": "{}", "stderr": ""}

    monkeypatch.setattr(controller, "run", run)
    assert controller.register_herdr_machine(fleet, "workenv-01", target)["status"] == "registered"
    assert len(calls) == 2


def test_wrong_session_is_not_reported_as_registered(monkeypatch):
    fleet = controller.load_fleet()
    target = controller.worker_host(fleet, "workenv-01")

    def run(args, **kwargs):
        assert args == ["herdr", "machine", "list", "--json"]
        return {"status": "completed", "returncode": 0, "stdout": json.dumps([
            {"id": "worker", "label": "workenv-01", "target": target, "session": "default", "enabled": True},
        ]), "stderr": ""}

    monkeypatch.setattr(controller, "run", run)
    result = controller.register_herdr_machine(fleet, "workenv-01", target)
    assert result["status"] == "herdr_profile_mismatch"
    assert not result["ok"]
