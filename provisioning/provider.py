"""Reconcile exe.dev VMs under an APoC-owned controller execution."""

from __future__ import annotations

import argparse
import fcntl
import hashlib
import json
import os
import re
import subprocess
import tempfile
from pathlib import Path


class ProviderError(Exception):
    def __init__(self, status: str, message: str):
        super().__init__(message)
        self.status = status


def command(args: list[str]) -> dict:
    try:
        completed = subprocess.run(
            ["ssh", "-o", "BatchMode=yes", "-o", "ConnectTimeout=15", "exe.dev", *args],
            text=True, capture_output=True, timeout=180, check=False,
        )
    except (OSError, subprocess.TimeoutExpired) as exc:
        raise ProviderError("unknown", f"provider command did not complete: {type(exc).__name__}") from exc
    try:
        value = json.loads(completed.stdout)
    except json.JSONDecodeError as exc:
        detail = completed.stderr.strip()[:1000]
        raise ProviderError("unknown", f"provider returned no valid JSON: {detail}" if detail else "provider returned no valid JSON") from exc
    if completed.returncode != 0 or not isinstance(value, dict) or value.get("error"):
        raise ProviderError("unknown", str(value.get("error", "provider command failed")) if isinstance(value, dict) else "invalid provider response")
    return value


def digest(value: dict) -> str:
    return hashlib.sha256(json.dumps(value, sort_keys=True, separators=(",", ":")).encode()).hexdigest()


def write_atomic(path: Path, value: dict) -> None:
    fd, temporary = tempfile.mkstemp(dir=path.parent)
    try:
        with os.fdopen(fd, "w") as stream:
            json.dump(value, stream, sort_keys=True)
            stream.flush()
            os.fsync(stream.fileno())
        os.replace(temporary, path)
    finally:
        if os.path.exists(temporary):
            os.unlink(temporary)


class Provider:
    def __init__(self, fleet: dict, state: Path, runner=command):
        self.fleet = fleet
        self.state = Path(state)
        self.run = runner

    def worker(self, name: str) -> dict:
        if not re.fullmatch(r"workenv-[0-9]{2}", name):
            raise ProviderError("conflict", "invalid worker name")
        matches = [item for item in self.fleet["workers"] if item["name"] == name]
        if len(matches) != 1:
            raise ProviderError("conflict", "worker must occur exactly once in fleet.json")
        spec = matches[0]
        if any(type(spec.get(key)) is not int or spec[key] <= 0 for key in ("cpus", "memory_gb", "disk_gb")):
            raise ProviderError("conflict", "worker capacity must be positive integers")
        return spec

    def inventory(self) -> list[dict]:
        value = self.run(["ls", "--json"])
        if not isinstance(value.get("vms"), list):
            raise ProviderError("unknown", "provider inventory is incomplete")
        return value["vms"]

    def inspect(self, name: str, inventory: list[dict] | None = None) -> dict:
        spec = self.worker(name)
        rows = self.inventory() if inventory is None else inventory
        matches = [item for item in rows if item.get("vm_name") == name]
        if not matches:
            return {"status": "missing", "ok": False, "worker": name}
        if len(matches) != 1:
            return {"status": "unknown", "ok": False, "worker": name, "error": "ambiguous provider inventory"}
        actual = matches[0]
        expected = {"cpus": spec["cpus"], "memory_bytes": spec["memory_gb"] * 1024**3, "disk_bytes": spec["disk_gb"] * 1024**3, "region": self.fleet["region"], "private_preview": True, "workenv_tag": True}
        observed = {"cpus": actual.get("allocated_cpus"), "memory_bytes": actual.get("memory_capacity_bytes"), "disk_bytes": actual.get("disk_capacity_bytes"), "region": actual.get("region"), "private_preview": actual.get("proxy_share") == "private", "workenv_tag": "workenv" in actual.get("tags", [])}
        differences = {key: {"expected": value, "actual": observed[key]} for key, value in expected.items() if observed[key] != value}
        status = "drift" if differences else "present" if actual.get("status") == "running" else "not_ready"
        fields = ("vm_name", "allocated_cpus", "memory_capacity_bytes", "disk_capacity_bytes", "region", "status", "tags", "image", "dns_name", "ssh_dest", "https_url", "proxy_port", "proxy_share")
        return {"status": status, "ok": status == "present", "worker": name, "vm": {key: actual[key] for key in fields if key in actual}, "differences": differences}

    def ensure(self, name: str, request_id: str, *, create: bool = False) -> dict:
        if not request_id:
            raise ProviderError("conflict", "request_id is required")
        spec = self.worker(name)
        self.state.mkdir(parents=True, exist_ok=True)
        fingerprint = digest({"worker": spec, "region": self.fleet["region"], "create": create})
        receipt_path = self.state / (hashlib.sha256(request_id.encode()).hexdigest() + ".json")
        with (self.state / "provider.lock").open("a+") as lock:
            fcntl.flock(lock, fcntl.LOCK_EX)
            receipt = json.loads(receipt_path.read_text()) if receipt_path.exists() else None
            if receipt and receipt["fingerprint"] != fingerprint:
                return {"status": "conflict", "ok": False, "error": "request_id is bound to a different provider operation"}
            rows = self.inventory()
            observed = self.inspect(name, rows)
            record = {"request_id": request_id, "fingerprint": fingerprint, "worker": name}
            if observed["status"] != "missing":
                write_atomic(receipt_path, {**record, "phase": "observed", "result": observed})
                return observed
            if receipt and receipt.get("phase") in {"creating", "created", "unknown", "observed"}:
                return {"status": "unknown", "ok": False, "worker": name, "error": "previous operation may have created this worker; inspect before a new creation request"}
            plan = self.run(["billing", "plan", "--json"])
            used_cpus = sum(int(item["allocated_cpus"]) for item in rows)
            used_memory = sum(int(item["memory_capacity_bytes"]) for item in rows)
            if used_cpus + spec["cpus"] > plan["max_cpus"] or used_memory + spec["memory_gb"] * 1024**3 > plan["max_memory_gb"] * 1024**3 or len(rows) + 1 > plan["max_vms"]:
                return {"status": "capacity_required", "ok": False, "worker": name, "current_plan": plan, "required_cpus": used_cpus + spec["cpus"], "required_memory_gb": used_memory / 1024**3 + spec["memory_gb"]}
            if not create:
                return {**observed, "capacity_available": True}
            write_atomic(receipt_path, {**record, "phase": "creating"})
            creation_error = None
            try:
                self.run(["new", "--name", name, "--cpu", str(spec["cpus"]), "--memory", f"{spec['memory_gb']}GB", "--disk", f"{spec['disk_gb']}GB", "--tag", "workenv", "--no-email", "--json"])
            except ProviderError as exc:
                # A lost response may follow successful creation. Inspect by the
                # unique requested name; never issue another create in this call.
                creation_error = str(exc)
            try:
                observed = self.inspect(name)
            except ProviderError:
                observed = {"status": "unknown", "ok": False, "worker": name}
            if observed["status"] == "missing":
                observed = {"status": "unknown", "ok": False, "worker": name, "error": "creation outcome is uncertain; inspect before retrying"}
            if creation_error:
                observed["creation_error"] = creation_error
            phase = "unknown" if observed["status"] == "unknown" else "created"
            write_atomic(receipt_path, {**record, "phase": phase, "result": observed})
            return observed


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--fleet", default="fleet.json")
    parser.add_argument("--state", default=".state/provider")
    parser.add_argument("--worker", required=True)
    parser.add_argument("--request-id", required=True)
    parser.add_argument("--create", action="store_true")
    args = parser.parse_args()
    try:
        value = Provider(json.loads(Path(args.fleet).read_text()), Path(args.state)).ensure(args.worker, args.request_id, create=args.create)
    except (ProviderError, KeyError, ValueError, OSError) as exc:
        value = {"status": getattr(exc, "status", "unknown"), "ok": False, "error": str(exc)}
    print(json.dumps(value, sort_keys=True))


if __name__ == "__main__":
    main()
