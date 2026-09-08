"""Transfer only the authorized Nib session to a declared personal worker."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import shlex
import subprocess
import sys
from pathlib import Path


REMOTE_STORE = r'''
import hashlib,json,os,pathlib,stat,sys,tempfile
token=sys.stdin.buffer.read(16385).strip()
if not token or len(token)>16384 or any(chr(c).isspace() for c in token):
    raise SystemExit("invalid credential format")
directory=pathlib.Path.home()/".config/workenv"
directory.mkdir(mode=0o700,parents=True,exist_ok=True)
if directory.is_symlink() or directory.stat().st_uid!=os.getuid():
    raise SystemExit("invalid credential directory")
os.chmod(directory,0o700)
path=directory/"nib-token"
fd,temporary=tempfile.mkstemp(prefix=".nib-token-",dir=directory)
try:
    with os.fdopen(fd,"wb") as handle:
        handle.write(token+b"\n")
        handle.flush()
        os.fsync(handle.fileno())
    try:
        os.link(temporary,path)
        state="installed"
    except FileExistsError:
        state="already_present"
finally:
    os.unlink(temporary)
if state=="already_present":
    fd=os.open(path,os.O_RDONLY|os.O_NOFOLLOW)
    with os.fdopen(fd,"rb") as handle:
        meta=os.fstat(handle.fileno())
        if not stat.S_ISREG(meta.st_mode) or meta.st_uid!=os.getuid() or meta.st_mode&0o077:
            raise SystemExit("existing credential permissions are unsafe")
        existing=handle.read(16385).strip()
    if existing!=token:
        raise SystemExit("a different Nib credential already exists; it was preserved")
print(json.dumps({"status":state,"mode":"0600","credential_sha256":hashlib.sha256(token).hexdigest()}))
'''


def read_mac_credential() -> bytes:
    result = subprocess.run(
        ["/usr/bin/security", "find-generic-password", "-s", "com.operator.nib.auth", "-a", "nibtool.com", "-w"],
        check=False, capture_output=True, timeout=15,
    )
    if result.returncode != 0 or not result.stdout.strip():
        raise ValueError("the Mac Nib login is unavailable")
    return result.stdout.strip()


def verify_credential(token: bytes) -> dict:
    environment = dict(os.environ)
    environment["NIB_AUTH_TOKEN"] = token.decode()
    result = subprocess.run(["nib", "auth", "status", "--format", "json"], env=environment, capture_output=True, text=True, timeout=20)
    if result.returncode != 0:
        raise ValueError("the Nib CLI could not verify the Mac credential")
    try:
        status = json.loads(result.stdout)
    except json.JSONDecodeError:
        raise ValueError("the Nib CLI verification response was not JSON") from None
    if not isinstance(status, dict) or status.get("authenticated") is not True:
        raise ValueError("the Mac Nib login is not authenticated")
    return {"authenticated": True}


def main() -> dict:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--fleet", type=Path, default=Path("fleet.json"))
    parser.add_argument("--worker", required=True)
    parser.add_argument("--request-id", required=True)
    parser.add_argument("--state", type=Path, default=Path(".state/nib-auth"))
    parser.add_argument("--recovery", action="store_true")
    args = parser.parse_args()
    fleet = json.loads(args.fleet.read_text())
    if not any(worker["name"] == args.worker for worker in fleet["workers"]):
        raise ValueError("worker is not declared in the personal fleet")
    host = f"{fleet['remote_user']}@{args.worker}.{fleet['tailnet_suffix']}"
    remote_command = shlex.join(["python3", "-c", REMOTE_STORE])
    argv = ["ssh", "-o", "BatchMode=yes", "-o", "ConnectTimeout=15"]
    if args.recovery:
        argv += [f"{fleet['remote_user']}@{args.worker}.exe.xyz", remote_command]
    else:
        argv += [host, remote_command]
    token = read_mac_credential()
    authenticated = verify_credential(token)
    args.state.mkdir(mode=0o700, parents=True, exist_ok=True)
    request_path = args.state / (hashlib.sha256(args.request_id.encode()).hexdigest() + ".json")
    identity = {"worker": args.worker, "host": host, "credential_sha256": hashlib.sha256(token).hexdigest()}
    if request_path.exists():
        previous = json.loads(request_path.read_text())
        if previous["identity"] != identity:
            raise ValueError("request ID is bound to another worker or credential")
    result = subprocess.run(argv, input=token, capture_output=True, timeout=60)
    if result.returncode != 0:
        # Remote output is never trusted to be free of credentials.
        raise ValueError("worker credential transfer failed; any existing credential was preserved")
    try:
        installed = json.loads(result.stdout)
    except json.JSONDecodeError:
        raise ValueError("the SSH transfer acknowledgement was not JSON") from None
    if installed.get("credential_sha256") != identity["credential_sha256"]:
        raise ValueError("worker credential acknowledgement did not match")
    receipt = {"identity": identity, "worker": args.worker, "authenticated_source": authenticated["authenticated"], "status": installed["status"], "source": "existing_mac_nib_session", "mode": "0600"}
    fd = os.open(request_path, os.O_WRONLY | os.O_CREAT | os.O_TRUNC | os.O_NOFOLLOW, 0o600)
    with os.fdopen(fd, "w") as handle:
        json.dump(receipt, handle)
    return {key: value for key, value in receipt.items() if key != "identity"}


if __name__ == "__main__":
    try:
        print(json.dumps(main()))
    except (ValueError, OSError, subprocess.SubprocessError) as error:
        print(json.dumps({
            "status": "auth_required",
            "error_code": type(error).__name__,
            "http_status": getattr(error, "code", None),
            "error": str(error) if type(error) is ValueError else "Nib authentication transfer did not complete",
        }))
        raise SystemExit(1)
