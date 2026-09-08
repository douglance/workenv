import base64
import json
import stat
import subprocess
from pathlib import Path

import pytest

from enrollment import enroll


def write_fleet(tmp_path):
    path = tmp_path / "fleet.json"
    path.write_text(
        json.dumps(
            {
                "tailnet_suffix": "example.ts.net",
                "tailscale_tag": "tag:workenv",
                "remote_user": "exedev",
                "workers": [{"name": "workenv-01"}],
            }
        ),
        encoding="utf-8",
    )
    return path


def write_credentials(tmp_path, mode=0o600):
    path = tmp_path / ".state" / "tailscale-oauth.local.json"
    path.parent.mkdir(exist_ok=True)
    path.write_text(
        json.dumps({"client_id": "client-id", "client_secret": "oauth-secret"}),
        encoding="utf-8",
    )
    path.chmod(mode)
    return path


def encode_request(request):
    return base64.b64encode(json.dumps(request).encode("utf-8")).decode("ascii")


def enroll_argv(tmp_path, request, credentials=None):
    return [
        "--fleet",
        str(write_fleet(tmp_path)),
        "--state-dir",
        str(tmp_path / ".state" / "enrollment"),
        "--credentials-file",
        str(credentials or write_credentials(tmp_path)),
        "--request-base64",
        encode_request(request),
    ]


def enroll_request(request_id="enroll-1"):
    return {
        "operation": "enroll",
        "request_id": request_id,
        "worker": "workenv-01",
    }


class FakeTailscaleAPI:
    def __init__(self):
        self.requests = []

    def __call__(self, url, *, data=None, headers=None, timeout=None):
        self.requests.append({"url": url, "data": data, "headers": headers, "timeout": timeout})
        if url.endswith("/oauth/token"):
            return {"access_token": "access-token"}
        if url.endswith("/api/v2/tailnet/-/keys"):
            return {"key": "tskey-auth-one-use"}
        raise AssertionError(url)


def not_enrolled(args, **kwargs):
    if "status --json" in args[-1]:
        return subprocess.CompletedProcess(
            args=args,
            returncode=1,
            stdout=json.dumps({"BackendState": "NeedsLogin", "Self": {}}),
            stderr="",
        )
    return subprocess.CompletedProcess(args=args, returncode=0, stdout="", stderr="")


def enrolled(args, **kwargs):
    if "debug prefs" in args[-1]:
        return subprocess.CompletedProcess(
            args=args,
            returncode=0,
            stdout=json.dumps({"WantRunning": True, "RunSSH": True}),
            stderr="",
        )
    stdout = json.dumps(
        {
            "BackendState": "Running",
            "Self": {"DNSName": "workenv-01.example.ts.net.", "Tags": ["tag:workenv"]},
            "CurrentTailnet": {"MagicDNSSuffix": "example.ts.net"},
        }
    )
    return subprocess.CompletedProcess(args=args, returncode=0, stdout=stdout, stderr="")


def enrolling_successfully():
    enrolled_now = False

    def fake(args, **kwargs):
        nonlocal enrolled_now
        if "status --json" in args[-1]:
            if enrolled_now:
                return enrolled(args, **kwargs)
            return subprocess.CompletedProcess(
                args=args,
                returncode=1,
                stdout=json.dumps({"BackendState": "NeedsLogin", "Self": {}}),
                stderr="",
            )
        if "debug prefs" in args[-1]:
            return enrolled(args, **kwargs)
        enrolled_now = True
        return subprocess.CompletedProcess(args=args, returncode=0, stdout="", stderr="")

    return fake


def mismatched_enrollment(args, **kwargs):
    if "debug prefs" in args[-1]:
        return subprocess.CompletedProcess(
            args=args,
            returncode=0,
            stdout=json.dumps({"WantRunning": True, "RunSSH": True}),
            stderr="",
        )
    stdout = json.dumps(
        {
            "BackendState": "Running",
            "Self": {"DNSName": "personal.tailnet.ts.net.", "Tags": ["tag:workenv"]},
            "CurrentTailnet": {"MagicDNSSuffix": "tailnet.ts.net"},
        }
    )
    return subprocess.CompletedProcess(args=args, returncode=0, stdout=stdout, stderr="")


def test_enroll_does_not_put_oauth_secret_or_auth_key_in_argv_env_or_result(tmp_path, monkeypatch):
    api = FakeTailscaleAPI()
    subprocess_calls = []

    def fake_run(args, **kwargs):
        subprocess_calls.append({"args": args, "env": kwargs.get("env"), "input": kwargs.get("input")})
        return enrolling(args, **kwargs)

    monkeypatch.setattr(enroll, "post_form", api)
    monkeypatch.setattr(enroll, "post_json", api)
    enrolling = enrolling_successfully()
    monkeypatch.setattr(enroll.subprocess, "run", fake_run)

    result = enroll.main(enroll_argv(tmp_path, enroll_request()))

    assert result["status"] == "enrolled"
    logged_shape = [
        {"args": call["args"], "env": call["env"]}
        for call in subprocess_calls
    ]
    serialized = json.dumps({"api": api.requests, "subprocess": logged_shape, "result": result})
    assert "oauth-secret" not in serialized
    assert "tskey-auth-one-use" not in serialized
    worker_call = [call for call in subprocess_calls if call["input"] is not None][0]
    assert worker_call["input"] == "tskey-auth-one-use"
    assert "--auth-key=file:" in " ".join(worker_call["args"])
    assert "sudo -n tailscale up" in " ".join(worker_call["args"])
    assert "exedev@workenv-01.exe.xyz" in worker_call["args"]


def test_key_creation_request_is_scoped_single_use_and_preauthorized(tmp_path, monkeypatch):
    api = FakeTailscaleAPI()
    monkeypatch.setattr(enroll, "post_form", api)
    monkeypatch.setattr(enroll, "post_json", api)
    monkeypatch.setattr(enroll.subprocess, "run", enrolling_successfully())

    result = enroll.main(enroll_argv(tmp_path, enroll_request()))

    assert result["status"] == "enrolled"
    key_request = [request for request in api.requests if request["url"].endswith("/keys")][0]
    create = key_request["data"]["capabilities"]["devices"]["create"]
    assert create == {
        "reusable": False,
        "ephemeral": False,
        "preauthorized": True,
        "tags": ["tag:workenv"],
    }
    assert key_request["data"]["expirySeconds"] == 3600


def test_enroll_replays_completed_receipt_without_new_key(tmp_path, monkeypatch):
    api = FakeTailscaleAPI()
    calls = []

    def fake_run(args, **kwargs):
        calls.append(args)
        return enrolling(args, **kwargs)

    monkeypatch.setattr(enroll, "post_form", api)
    monkeypatch.setattr(enroll, "post_json", api)
    enrolling = enrolling_successfully()
    monkeypatch.setattr(enroll.subprocess, "run", fake_run)
    argv = enroll_argv(tmp_path, enroll_request())

    first = enroll.main(argv)
    second = enroll.main(argv)

    assert first["status"] == "enrolled"
    assert second["status"] == "enrolled"
    assert second["replay"] is True
    key_creations = [request for request in api.requests if request["url"].endswith("/keys")]
    assert len(key_creations) == 1


def test_retry_after_uncertain_key_creation_inspects_status_and_refuses_remint(tmp_path, monkeypatch):
    api = FakeTailscaleAPI()
    status_checks = 0

    def fake_run(args, **kwargs):
        nonlocal status_checks
        if "status --json" in args[-1]:
            status_checks += 1
            return subprocess.CompletedProcess(
                args=args,
                returncode=1,
                stdout=json.dumps({"BackendState": "NeedsLogin", "Self": {}}),
                stderr="",
            )
        return subprocess.CompletedProcess(args=args, returncode=255, stdout="", stderr="ssh failed")

    monkeypatch.setattr(enroll, "post_form", api)
    monkeypatch.setattr(enroll, "post_json", api)
    monkeypatch.setattr(enroll.subprocess, "run", fake_run)
    argv = enroll_argv(tmp_path, enroll_request())

    first = enroll.main(argv)
    second = enroll.main(argv)

    assert first["status"] == "worker_failed"
    assert second["status"] == "uncertain_key_creation"
    assert status_checks >= 2
    key_creations = [request for request in api.requests if request["url"].endswith("/keys")]
    assert len(key_creations) == 1


def test_already_enrolled_worker_is_not_reauthorized(tmp_path, monkeypatch):
    api = FakeTailscaleAPI()
    monkeypatch.setattr(enroll, "post_form", api)
    monkeypatch.setattr(enroll, "post_json", api)
    monkeypatch.setattr(enroll.subprocess, "run", enrolled)

    result = enroll.main(enroll_argv(tmp_path, enroll_request()))

    assert result["status"] == "already_enrolled"
    assert api.requests == []


def test_mismatched_or_unknown_enrollment_fails_closed(tmp_path, monkeypatch):
    api = FakeTailscaleAPI()
    monkeypatch.setattr(enroll, "post_form", api)
    monkeypatch.setattr(enroll, "post_json", api)
    monkeypatch.setattr(enroll.subprocess, "run", mismatched_enrollment)

    mismatch = enroll.main(enroll_argv(tmp_path, enroll_request()))

    assert mismatch["status"] == "mismatch"
    assert api.requests == []

    def invalid_status(args, **kwargs):
        return subprocess.CompletedProcess(args=args, returncode=0, stdout="{", stderr="")

    monkeypatch.setattr(enroll.subprocess, "run", invalid_status)
    unknown = enroll.main(enroll_argv(tmp_path, enroll_request("enroll-unknown")))

    assert unknown["status"] == "unknown"


def test_unreachable_ssh_status_fails_unknown_and_mints_no_key(tmp_path, monkeypatch):
    api = FakeTailscaleAPI()

    def unreachable(args, **kwargs):
        return subprocess.CompletedProcess(args=args, returncode=255, stdout="", stderr="no route")

    monkeypatch.setattr(enroll, "post_form", api)
    monkeypatch.setattr(enroll, "post_json", api)
    monkeypatch.setattr(enroll.subprocess, "run", unreachable)

    result = enroll.main(enroll_argv(tmp_path, enroll_request()))

    assert result["status"] == "unknown"
    assert api.requests == []


def test_needs_login_json_without_existing_identity_allows_enrollment(tmp_path, monkeypatch):
    api = FakeTailscaleAPI()
    enrolled_now = False

    def needs_login_then_enrolled(args, **kwargs):
        nonlocal enrolled_now
        if "status --json" in args[-1]:
            if enrolled_now:
                return enrolled(args, **kwargs)
            return subprocess.CompletedProcess(
                args=args,
                returncode=1,
                stdout=json.dumps({"BackendState": "NeedsLogin", "Self": {}}),
                stderr="",
            )
        if "debug prefs" in args[-1]:
            return enrolled(args, **kwargs)
        enrolled_now = True
        return subprocess.CompletedProcess(args=args, returncode=0, stdout="", stderr="")

    monkeypatch.setattr(enroll, "post_form", api)
    monkeypatch.setattr(enroll, "post_json", api)
    monkeypatch.setattr(enroll.subprocess, "run", needs_login_then_enrolled)

    result = enroll.main(enroll_argv(tmp_path, enroll_request()))

    assert result["status"] == "enrolled"


def test_stopped_existing_identity_is_not_reauthorized(tmp_path, monkeypatch):
    api = FakeTailscaleAPI()

    def stopped(args, **kwargs):
        return subprocess.CompletedProcess(
            args=args,
            returncode=0,
            stdout=json.dumps(
                {
                    "BackendState": "Stopped",
                    "Self": {"ID": "node-1", "DNSName": "workenv-01.example.ts.net."},
                    "CurrentTailnet": {"MagicDNSSuffix": "example.ts.net"},
                }
            ),
            stderr="",
        )

    monkeypatch.setattr(enroll, "post_form", api)
    monkeypatch.setattr(enroll, "post_json", api)
    monkeypatch.setattr(enroll.subprocess, "run", stopped)

    result = enroll.main(enroll_argv(tmp_path, enroll_request()))

    assert result["status"] == "inactive"
    assert api.requests == []


def test_running_worker_without_workenv_tag_or_ssh_is_configuration_blocked(tmp_path, monkeypatch):
    api = FakeTailscaleAPI()

    def tag_missing(args, **kwargs):
        if "debug prefs" in args[-1]:
            return subprocess.CompletedProcess(
                args=args,
                returncode=0,
                stdout=json.dumps({"WantRunning": True, "RunSSH": False}),
                stderr="",
            )
        return subprocess.CompletedProcess(
            args=args,
            returncode=0,
            stdout=json.dumps(
                {
                    "BackendState": "Running",
                    "Self": {"DNSName": "workenv-01.example.ts.net.", "Tags": []},
                    "CurrentTailnet": {"MagicDNSSuffix": "example.ts.net"},
                }
            ),
            stderr="",
        )

    monkeypatch.setattr(enroll, "post_form", api)
    monkeypatch.setattr(enroll, "post_json", api)
    monkeypatch.setattr(enroll.subprocess, "run", tag_missing)

    result = enroll.main(enroll_argv(tmp_path, enroll_request()))

    assert result["status"] == "configuration_blocked"
    assert "tag:workenv" in result["error"]
    assert api.requests == []


def test_retry_after_uncertain_key_create_response_refuses_second_key(tmp_path, monkeypatch):
    api = FakeTailscaleAPI()
    key_attempts = 0

    def fake_post_json(url, *, data=None, headers=None, timeout=None):
        nonlocal key_attempts
        key_attempts += 1
        raise enroll.EnrollmentError("api_failed", "lost key creation response")

    monkeypatch.setattr(enroll, "post_form", api)
    monkeypatch.setattr(enroll, "post_json", fake_post_json)
    monkeypatch.setattr(enroll.subprocess, "run", not_enrolled)
    argv = enroll_argv(tmp_path, enroll_request())

    first = enroll.main(argv)
    second = enroll.main(argv)

    assert first["status"] == "api_failed"
    assert second["status"] == "uncertain_key_creation"
    assert key_attempts == 1


def test_worker_failure_redacts_auth_key_from_returned_error(tmp_path, monkeypatch):
    api = FakeTailscaleAPI()

    def fake_run(args, **kwargs):
        if "status --json" in args[-1]:
            return subprocess.CompletedProcess(
                args=args,
                returncode=1,
                stdout=json.dumps({"BackendState": "NeedsLogin", "Self": {}}),
                stderr="",
            )
        return subprocess.CompletedProcess(
            args=args,
            returncode=1,
            stdout="",
            stderr="failed with tskey-auth-one-use",
        )

    monkeypatch.setattr(enroll, "post_form", api)
    monkeypatch.setattr(enroll, "post_json", api)
    monkeypatch.setattr(enroll.subprocess, "run", fake_run)

    result = enroll.main(enroll_argv(tmp_path, enroll_request()))

    assert result["status"] == "worker_failed"
    assert "tskey-auth-one-use" not in result["error"]
    assert "[redacted]" in result["error"]


def test_credentials_file_must_be_mode_0600(tmp_path, monkeypatch):
    monkeypatch.setattr(enroll.subprocess, "run", not_enrolled)
    credentials = write_credentials(tmp_path, mode=0o644)

    result = enroll.main(enroll_argv(tmp_path, enroll_request(), credentials=credentials))

    assert result["status"] == "conflict"
    assert "0600" in result["error"]


def test_policy_fragment_is_merge_only_and_workenv_scoped():
    fragment = json.loads((Path(__file__).resolve().parents[1] / "enrollment/policy_fragment.json").read_text())

    assert fragment["merge_only"] is True
    assert fragment["tagOwners"]["tag:workenv"] == ["operator@example.invalid"]
    assert fragment["ssh"] == [
        {"action": "accept", "src": ["operator@example.invalid"], "dst": ["tag:workenv"], "users": ["exedev"]}
    ]
    assert fragment["acls"] == [
        {"action": "accept", "src": ["operator@example.invalid"], "dst": ["tag:workenv:22", "tag:workenv:8000"]}
    ]
