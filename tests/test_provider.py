import pytest

from provisioning.provider import Provider, ProviderError


@pytest.fixture
def fleet():
    return {
        "region": "dal",
        "workers": [
            {"name": "workenv-01", "cpus": 2, "memory_gb": 8, "disk_gb": 50},
            {"name": "workenv-02", "cpus": 2, "memory_gb": 8, "disk_gb": 50},
        ],
    }


def vm(name="workenv-01"):
    return {
        "vm_name": name,
        "allocated_cpus": 2,
        "memory_capacity_bytes": 8 * 1024**3,
        "disk_capacity_bytes": 50 * 1024**3,
        "region": "dal",
        "tags": ["workenv"],
        "proxy_share": "private",
        "proxy_port": 8000,
        "status": "running",
    }


def test_existing_matching_worker_never_creates(fleet, tmp_path):
    calls = []

    def run(args):
        calls.append(args)
        assert args == ["ls", "--json"]
        return {"vms": [vm()]}

    result = Provider(fleet, tmp_path, run).ensure("workenv-01", "existing", create=True)
    assert result["status"] == "present"
    assert len(calls) == 1


def test_capacity_failure_happens_before_creation(fleet, tmp_path):
    calls = []

    def run(args):
        calls.append(args)
        if args[0] == "ls":
            return {"vms": [vm()]}
        assert args == ["billing", "plan", "--json"]
        return {"max_cpus": 2, "max_memory_gb": 8, "max_vms": 50}

    result = Provider(fleet, tmp_path, run).ensure("workenv-02", "capacity", create=True)
    assert result["status"] == "capacity_required"
    assert not result["ok"]
    assert not any(args[0] == "new" for args in calls)


def test_timeout_never_creates_twice(fleet, tmp_path):
    created = []

    def run(args):
        if args[0] == "ls":
            return {"vms": []}
        if args[0] == "billing":
            return {"max_cpus": 16, "max_memory_gb": 64, "max_vms": 50}
        created.append(args)
        raise ProviderError("unknown", "connection lost")

    provider = Provider(fleet, tmp_path, run)
    result = provider.ensure("workenv-01", "uncertain", create=True)
    assert result["status"] == "unknown"
    assert result["creation_error"] == "connection lost"
    assert provider.ensure("workenv-01", "uncertain", create=True)["status"] == "unknown"
    assert len(created) == 1


def test_timeout_reconciles_created_vm_without_retry(fleet, tmp_path):
    created = []

    def run(args):
        if args[0] == "ls":
            return {"vms": [vm()] if created else []}
        if args[0] == "billing":
            return {"max_cpus": 16, "max_memory_gb": 64, "max_vms": 50}
        created.append(args)
        raise ProviderError("unknown", "connection lost after creation")

    provider = Provider(fleet, tmp_path, run)
    result = provider.ensure("workenv-01", "created-uncertain", create=True)
    assert result["status"] == "present"
    assert provider.ensure("workenv-01", "created-uncertain", create=True)["status"] == "present"
    assert len(created) == 1


def test_drifted_worker_is_preserved_and_reported(fleet, tmp_path):
    actual = vm()
    actual["allocated_cpus"] = 4
    result = Provider(fleet, tmp_path, lambda args: {"vms": [actual]}).ensure(
        "workenv-01", "drift", create=True
    )
    assert result["status"] == "drift"
    assert not result["ok"]
    assert "cpus" in result["differences"]


def test_request_id_cannot_change_worker(fleet, tmp_path):
    provider = Provider(fleet, tmp_path, lambda args: {"vms": [vm(), vm("workenv-02")]})
    provider.ensure("workenv-01", "same-key", create=True)
    result = provider.ensure("workenv-02", "same-key", create=True)
    assert result["status"] == "conflict"
