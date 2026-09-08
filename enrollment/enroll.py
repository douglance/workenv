from __future__ import annotations

import argparse
import base64
import contextlib
import fcntl
import hashlib
import json
import os
import shlex
import stat
import subprocess
import tempfile
import time
import urllib.error
import urllib.parse
import urllib.request
from pathlib import Path


TOKEN_URL = "https://api.tailscale.com/api/v2/oauth/token"
KEYS_URL = "https://api.tailscale.com/api/v2/tailnet/-/keys"
TAG = "tag:workenv"
AUTH_KEY_TTL_SECONDS = 3600


class EnrollmentError(Exception):
    def __init__(self, status: str, message: str, code: int = 1):
        super().__init__(message)
        self.status = status
        self.message = message
        self.code = code


def main(argv: list[str] | None = None) -> dict:
    parser = argparse.ArgumentParser(description="Safely enroll a workenv worker in Tailscale.")
    parser.add_argument("--fleet", default="fleet.json")
    parser.add_argument("--state-dir", default=".state/enrollment")
    parser.add_argument("--credentials-file", default=".state/tailscale-oauth.local.json")
    parser.add_argument("--request-base64", required=True)
    parser.add_argument("--timeout", type=int, default=20)
    args = parser.parse_args(argv)

    try:
        request = _decode_request(args.request_base64)
        driver = EnrollmentDriver(
            fleet_path=Path(args.fleet),
            state_dir=Path(args.state_dir),
            credentials_file=Path(args.credentials_file),
            timeout=args.timeout,
        )
        return driver.handle(request)
    except EnrollmentError as exc:
        return result(exc.status, ok=False, error=exc.message, code=exc.code)
    except urllib.error.URLError as exc:
        return result("api_failed", ok=False, error=str(exc), code=1)


class EnrollmentDriver:
    def __init__(self, *, fleet_path: Path, state_dir: Path, credentials_file: Path, timeout: int):
        self.fleet_path = fleet_path
        self.state_dir = state_dir
        self.receipts_dir = state_dir / "receipts"
        self.lock_path = state_dir / "enrollment.lock"
        self.credentials_file = credentials_file
        self.timeout = timeout

    def handle(self, request: dict) -> dict:
        if not isinstance(request, dict):
            raise EnrollmentError("conflict", "request must be a JSON object")
        if request.get("operation") != "enroll":
            raise EnrollmentError("conflict", "operation must be enroll")
        request_id = request.get("request_id")
        if not isinstance(request_id, str) or not request_id:
            raise EnrollmentError("conflict", "request_id is required")
        worker_name = request.get("worker")
        if not isinstance(worker_name, str) or not worker_name.startswith("workenv-"):
            raise EnrollmentError("conflict", "worker must be a workenv host name")

        self._ensure_layout()
        payload_hash = canonical_hash(request)
        with self._locked():
            receipt_path = self._receipt_path(request_id)
            receipt = read_json(receipt_path) if receipt_path.exists() else None
            if receipt and receipt.get("payload_hash") != payload_hash:
                return result("conflict", ok=False, error="request_id was already used for a different payload", code=1)
            if receipt and receipt.get("complete"):
                replayed = dict(receipt["result"])
                replayed["replay"] = True
                return replayed

            fleet = read_json(self.fleet_path)
            worker = self._worker(fleet, worker_name)

            if receipt and receipt.get("phase") in {"key_create_started", "key_created", "worker_enroll_started", "worker_failed"}:
                live = inspect_worker(worker, fleet, self.timeout)
                if live["status"] == "already_enrolled":
                    completed = result("already_enrolled", worker=worker_name, dns_name=live["dns_name"])
                    self._write_complete(receipt_path, request_id, payload_hash, completed)
                    return completed
                return result(
                    "uncertain_key_creation",
                    ok=False,
                    error="prior request may have created a one-use key; inspect or revoke before reminting",
                    live_status=live["status"],
                    code=3,
                )

            self._write_receipt(receipt_path, request_id, payload_hash, "intent_recorded")
            live = inspect_worker(worker, fleet, self.timeout)
            if live["status"] == "already_enrolled":
                completed = result("already_enrolled", worker=worker_name, dns_name=live["dns_name"])
                self._write_complete(receipt_path, request_id, payload_hash, completed)
                return completed
            if live["status"] in {"configuration_blocked", "inactive", "mismatch", "unknown"}:
                completed = result(live["status"], ok=False, worker=worker_name, error=live["error"], code=2)
                self._write_complete(receipt_path, request_id, payload_hash, completed)
                return completed
            credentials = load_credentials(self.credentials_file)
            access_token = exchange_oauth_token(credentials, self.timeout)
            self._write_receipt(receipt_path, request_id, payload_hash, "key_create_started")
            auth_key = create_auth_key(access_token, fleet.get("tailscale_tag", TAG), self.timeout)
            self._write_receipt(
                receipt_path,
                request_id,
                payload_hash,
                "key_created",
                key_sha256=hashlib.sha256(auth_key.encode("utf-8")).hexdigest(),
            )
            self._write_receipt(receipt_path, request_id, payload_hash, "worker_enroll_started")
            completed = enroll_worker(worker, fleet, auth_key, self.timeout)
            if completed["status"] != "enrolled":
                self._write_receipt(receipt_path, request_id, payload_hash, "worker_failed")
                return completed
            self._write_complete(receipt_path, request_id, payload_hash, completed)
            return completed

    def _worker(self, fleet: dict, worker_name: str) -> dict:
        for worker in fleet.get("workers", []):
            if worker.get("name") == worker_name:
                return worker
        raise EnrollmentError("conflict", f"worker is not in fleet: {worker_name}")

    def _ensure_layout(self) -> None:
        self.receipts_dir.mkdir(parents=True, exist_ok=True)

    @contextlib.contextmanager
    def _locked(self):
        self.lock_path.parent.mkdir(parents=True, exist_ok=True)
        with self.lock_path.open("a+") as lock_file:
            fcntl.flock(lock_file.fileno(), fcntl.LOCK_EX)
            try:
                yield
            finally:
                fcntl.flock(lock_file.fileno(), fcntl.LOCK_UN)

    def _receipt_path(self, request_id: str) -> Path:
        digest = hashlib.sha256(request_id.encode("utf-8")).hexdigest()
        return self.receipts_dir / f"{digest}.json"

    def _write_receipt(self, path: Path, request_id: str, payload_hash: str, phase: str, **extra: str) -> None:
        receipt = {
            "request_id": request_id,
            "payload_hash": payload_hash,
            "phase": phase,
            "complete": False,
            "updated_at": now(),
        }
        receipt.update(extra)
        write_json_atomic(path, receipt)

    def _write_complete(self, path: Path, request_id: str, payload_hash: str, completed: dict) -> None:
        write_json_atomic(
            path,
            {
                "request_id": request_id,
                "payload_hash": payload_hash,
                "phase": "complete",
                "complete": True,
                "updated_at": now(),
                "result": completed,
            },
        )


def load_credentials(path: Path) -> dict:
    resolved = path.expanduser().resolve()
    if resolved.name.endswith(".local.json") is False:
        raise EnrollmentError("conflict", "credentials file must be an ignored *.local.json file")
    if ".state" not in resolved.parts and "secrets" not in resolved.parts:
        raise EnrollmentError("conflict", "credentials file must live under ignored local state")
    try:
        mode = stat.S_IMODE(resolved.stat().st_mode)
    except FileNotFoundError as exc:
        raise EnrollmentError("conflict", "credentials file does not exist") from exc
    if mode != 0o600:
        raise EnrollmentError("conflict", "credentials file must be mode 0600")
    payload = read_json(resolved)
    client_id = payload.get("client_id")
    client_secret = payload.get("client_secret")
    if not isinstance(client_id, str) or not client_id or not isinstance(client_secret, str) or not client_secret:
        raise EnrollmentError("conflict", "credentials file must contain client_id and client_secret")
    return {"client_id": client_id, "client_secret": client_secret}


def inspect_worker(worker: dict, fleet: dict, timeout: int) -> dict:
    completed = subprocess.run(
        ssh_argv(worker, fleet, "tailscale status --json"),
        check=False,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        timeout=timeout,
    )
    try:
        status = json.loads(completed.stdout)
    except json.JSONDecodeError:
        return {
            "status": "unknown",
            "error": "tailscale status returned invalid JSON" if completed.stdout else "tailscale status was unreachable",
        }
    if completed.returncode != 0 and not (completed.returncode == 1 and status.get("BackendState") == "NeedsLogin"):
        return {"status": "unknown", "error": "tailscale status failed before reporting a login state"}

    worker_name = worker["name"]
    suffix = fleet["tailnet_suffix"]
    expected_dns = f"{worker_name}.{suffix}"
    self_status = status.get("Self", {})
    if not isinstance(self_status, dict):
        self_status = {}
    tailnet = status.get("CurrentTailnet", {})
    if not isinstance(tailnet, dict):
        tailnet = {}
    dns_name = str(self_status.get("DNSName", "")).rstrip(".")
    tailnet_suffix = tailnet.get("MagicDNSSuffix") or tailnet.get("Name")
    backend_state = status.get("BackendState")
    existing_identity = bool(self_status.get("ID") or tailnet)
    if backend_state != "Running":
        if not existing_identity:
            return {"status": "not_enrolled"}
        if dns_name in {"", expected_dns} and tailnet_suffix in {"", suffix}:
            return {"status": "inactive", "error": f"worker tailscale backend is {backend_state or '<missing>'}"}
        return {
            "status": "mismatch",
            "error": f"worker reports dns={dns_name or '<missing>'} tailnet={tailnet_suffix or '<missing>'}",
        }
    if dns_name == expected_dns and tailnet_suffix == suffix:
        tag = fleet.get("tailscale_tag", TAG)
        tags = self_status.get("Tags", [])
        if not isinstance(tags, list):
            tags = []
        if tag not in tags:
            return {"status": "configuration_blocked", "error": f"worker is missing required Tailscale tag {tag}"}
        prefs = inspect_worker_prefs(worker, fleet, timeout)
        if prefs["status"] != "ok":
            return prefs
        if prefs["prefs"].get("WantRunning") is not True:
            return {"status": "configuration_blocked", "error": "worker Tailscale is not configured to stay running"}
        if prefs["prefs"].get("RunSSH") is not True:
            return {"status": "configuration_blocked", "error": "worker Tailscale SSH is disabled"}
        return {"status": "already_enrolled", "dns_name": dns_name}
    return {
        "status": "mismatch",
        "error": f"worker reports dns={dns_name or '<missing>'} tailnet={tailnet_suffix or '<missing>'}",
    }


def inspect_worker_prefs(worker: dict, fleet: dict, timeout: int) -> dict:
    completed = subprocess.run(
        ssh_argv(worker, fleet, "tailscale debug prefs"),
        check=False,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        timeout=timeout,
    )
    if completed.returncode != 0:
        return {"status": "unknown", "error": "tailscale debug prefs failed"}
    try:
        prefs = json.loads(completed.stdout)
    except json.JSONDecodeError:
        return {"status": "unknown", "error": "tailscale debug prefs returned invalid JSON"}
    if not isinstance(prefs, dict):
        return {"status": "unknown", "error": "tailscale debug prefs did not return an object"}
    return {"status": "ok", "prefs": prefs}


def enroll_worker(worker: dict, fleet: dict, auth_key: str, timeout: int) -> dict:
    remote_command = enroll_remote_command(worker["name"], fleet.get("tailscale_tag", TAG))
    completed = subprocess.run(
        ssh_argv(worker, fleet, remote_command),
        check=False,
        text=True,
        input=auth_key,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        timeout=timeout,
    )
    if completed.returncode != 0:
        return result(
            "worker_failed",
            ok=False,
            worker=worker["name"],
            error=redact_secrets(completed.stderr.strip(), [auth_key]),
            code=1,
        )
    live = inspect_worker(worker, fleet, timeout)
    if live["status"] == "already_enrolled":
        return result("enrolled", worker=worker["name"], dns_name=live["dns_name"])
    return result(live["status"], ok=False, worker=worker["name"], error=live.get("error", "enrollment did not verify"), code=2)


def ssh_argv(worker: dict, fleet: dict, remote_command: str) -> list[str]:
    host = f"{worker['name']}.exe.xyz"
    return [
        "ssh",
        "-o",
        "BatchMode=yes",
        "-o",
        "StrictHostKeyChecking=accept-new",
        f"{fleet.get('remote_user', 'exedev')}@{host}",
        remote_command,
    ]


def enroll_remote_command(hostname: str, tag: str) -> str:
    tailscale_up = (
        "sudo -n tailscale up "
        "--auth-key=file:\"$key_file\" "
        f"--hostname {shlex.quote(hostname)} "
        "--ssh "
        f"--advertise-tags={shlex.quote(tag)} "
        "--accept-routes=false"
    )
    return (
        "set -eu; "
        "umask 077; "
        "key_file=$(mktemp /tmp/workenv-tailscale-key.XXXXXX); "
        "trap 'rm -f \"$key_file\"' EXIT; "
        "cat > \"$key_file\"; "
        "chmod 600 \"$key_file\"; "
        f"{tailscale_up}"
    )


def exchange_oauth_token(credentials: dict, timeout: int) -> str:
    basic = base64.b64encode(
        f"{credentials['client_id']}:{credentials['client_secret']}".encode("utf-8")
    ).decode("ascii")
    payload = post_form(
        TOKEN_URL,
        data={"grant_type": "client_credentials"},
        headers={"Authorization": f"Basic {basic}"},
        timeout=timeout,
    )
    access_token = payload.get("access_token")
    if not isinstance(access_token, str) or not access_token:
        raise EnrollmentError("api_failed", "OAuth token response did not include access_token")
    return access_token


def create_auth_key(access_token: str, tag: str, timeout: int) -> str:
    payload = post_json(
        KEYS_URL,
        data={
            "capabilities": {
                "devices": {
                    "create": {
                        "reusable": False,
                        "ephemeral": False,
                        "preauthorized": True,
                        "tags": [tag],
                    }
                }
            },
            "expirySeconds": AUTH_KEY_TTL_SECONDS,
        },
        headers={"Authorization": f"Bearer {access_token}"},
        timeout=timeout,
    )
    key = payload.get("key")
    if not isinstance(key, str) or not key:
        raise EnrollmentError("api_failed", "key creation response did not include key")
    return key


def post_json(url: str, *, data: dict, headers: dict, timeout: int) -> dict:
    body = json.dumps(data, sort_keys=True).encode("utf-8")
    request = urllib.request.Request(
        url,
        data=body,
        headers={**headers, "Content-Type": "application/json", "Accept": "application/json"},
        method="POST",
    )
    with urllib.request.urlopen(request, timeout=timeout) as response:
        return json.loads(response.read().decode("utf-8"))


def post_form(url: str, *, data: dict, headers: dict, timeout: int) -> dict:
    body = urllib.parse.urlencode(data).encode("utf-8")
    request = urllib.request.Request(
        url,
        data=body,
        headers={**headers, "Content-Type": "application/x-www-form-urlencoded", "Accept": "application/json"},
        method="POST",
    )
    with urllib.request.urlopen(request, timeout=timeout) as response:
        return json.loads(response.read().decode("utf-8"))


def _decode_request(encoded: str) -> dict:
    try:
        return json.loads(base64.b64decode(encoded, validate=True).decode("utf-8"))
    except Exception as exc:  # noqa: BLE001
        raise EnrollmentError("conflict", "request-base64 is not valid base64 JSON") from exc


def canonical_hash(payload: dict) -> str:
    return hashlib.sha256(json.dumps(payload, sort_keys=True, separators=(",", ":"), ensure_ascii=True).encode()).hexdigest()


def read_json(path: Path) -> dict:
    return json.loads(path.read_text(encoding="utf-8"))


def write_json_atomic(path: Path, payload: dict) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    fd, tmp_name = tempfile.mkstemp(prefix=f".{path.name}.", dir=path.parent)
    try:
        with os.fdopen(fd, "w", encoding="utf-8") as tmp:
            json.dump(payload, tmp, sort_keys=True, separators=(",", ":"))
            tmp.write("\n")
            tmp.flush()
            os.fsync(tmp.fileno())
        os.replace(tmp_name, path)
    finally:
        with contextlib.suppress(FileNotFoundError):
            os.unlink(tmp_name)


def result(status: str, *, ok: bool | None = None, **fields) -> dict:
    if ok is None:
        ok = status in {"enrolled", "already_enrolled"}
    payload = {"status": status, "ok": ok}
    payload.update(fields)
    return payload


def redact_secrets(text: str, secrets: list[str]) -> str:
    redacted = text
    for secret in secrets:
        if secret:
            redacted = redacted.replace(secret, "[redacted]")
    return redacted


def now() -> int:
    return int(time.time())


if __name__ == "__main__":
    print(json.dumps(main(), sort_keys=True))
