"""Inspect the actual shared devenv commands without exposing login material."""

import argparse
import json
import shutil
import subprocess


TOOLS = {
    "git": ["--version"],
    "lazygit": ["--version"],
    "tailscale": ["version"],
    "herdr": ["--version"],
    "codex": ["--version"],
    "claude": ["--version"],
    "grok": ["--version"],
    "pi": ["--version"],
    "apoc": ["version", "--purpose", "Inspect the shared workenv APoC version.", "--format", "json"],
    "devsql": ["--version"],
    "ssh-clipboard": ["--version"],
    "nib": ["--version"],
}


def main() -> dict:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--tools-only", action="store_true")
    parser.add_argument("--require-nix", action="store_true")
    args = parser.parse_args()
    observed = {}
    for name, arguments in TOOLS.items():
        path = shutil.which(name)
        if not path:
            observed[name] = {"ok": False, "error": "missing_command"}
            continue
        if args.require_nix and not path.startswith("/nix/store/"):
            observed[name] = {"ok": False, "path": path, "error": "command_is_outside_devenv"}
            continue
        try:
            result = subprocess.run([path, *arguments], capture_output=True, text=True, timeout=20)
            observed[name] = {"ok": result.returncode == 0, "path": path, "version": result.stdout.strip()[:500], "error": result.stderr.strip()[:500] if result.returncode else None}
        except subprocess.TimeoutExpired:
            observed[name] = {"ok": False, "path": path, "error": "version_timeout"}
    nib_auth = {"authenticated": False, "status": "unavailable"}
    if observed["nib"]["ok"] and not args.tools_only:
        try:
            result = subprocess.run(["nib", "auth", "status", "--format", "json"], capture_output=True, text=True, timeout=20)
            if result.returncode == 0:
                payload = json.loads(result.stdout)
                nib_auth = {"authenticated": payload.get("authenticated") is True, "source": payload.get("source"), "status": "checked"}
            else:
                nib_auth = {"authenticated": False, "status": "auth_required"}
        except (subprocess.TimeoutExpired, ValueError):
            nib_auth = {"authenticated": False, "status": "unknown"}
    return {"schema": 1, "tools_ready": all(item["ok"] for item in observed.values()), "tools": observed, "nib_auth": nib_auth}


if __name__ == "__main__":
    result = main()
    print(json.dumps(result, sort_keys=True))
    raise SystemExit(0 if result["tools_ready"] else 1)
