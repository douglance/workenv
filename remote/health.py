from __future__ import annotations

import argparse
import json
import os
import re
import shutil
import subprocess
import sys
from collections.abc import Callable, Mapping, Sequence
from dataclasses import dataclass
from typing import Any


METERED_API_OVERRIDE_ENV = (
    "OPENAI_API_KEY",
    "ANTHROPIC_API_KEY",
    "ANTHROPIC_AUTH_TOKEN",
    "OPENAI_BASE_URL",
    "ANTHROPIC_BASE_URL",
)
SUBSCRIPTION_TOKEN_ENV = ("CLAUDE_CODE_OAUTH_TOKEN",)
CLAUDE_ALLOWED_AUTH_METHODS = {"claude.ai", "oauth"}


@dataclass(frozen=True)
class CommandResult:
    returncode: int
    stdout: str
    stderr: str
    timed_out: bool = False
    missing: bool = False


CommandRunner = Callable[[Sequence[str], float], CommandResult]


def evaluate_health(
    *,
    environ: Mapping[str, str] | None = None,
    command_runner: CommandRunner | None = None,
    timeout: float = 10.0,
) -> dict[str, Any]:
    env = os.environ if environ is None else environ
    runner = default_command_runner if command_runner is None else command_runner
    api_overrides = _api_override_presence(env)
    codex = probe_codex(runner, timeout)
    claude = probe_claude(runner, timeout)

    blockers: list[str] = []
    if not codex["chatgpt_subscription"]:
        blockers.append("codex_chatgpt_auth_required")
    if not claude["subscription_auth"]:
        blockers.append("claude_subscription_auth_required")
    if any(api_overrides["metered"].values()):
        blockers.append("unexpected_api_overrides_present")

    ready = not blockers
    return {
        "schema": 1,
        "status": "ready" if ready else "auth_required",
        "ok": ready,
        "ready": ready,
        "blockers": blockers,
        "auth": {
            "codex": codex,
            "claude": claude,
        },
        "environment": api_overrides,
        "automatic_api_fallback_allowed": False,
    }


def probe_codex(command_runner: CommandRunner, timeout: float) -> dict[str, Any]:
    result = command_runner(("codex", "login", "status"), timeout)
    text = f"{result.stdout}\n{result.stderr}"
    logged_in = result.returncode == 0 and bool(re.search(r"^logged in using ", text, re.I | re.M))
    chatgpt = logged_in and bool(re.search(r"^logged in using chatgpt\b", text, re.I | re.M))
    auth_method = "chatgpt" if chatgpt else ("unknown" if logged_in else None)

    return {
        "command_available": not result.missing,
        "status": _command_status(result),
        "logged_in": logged_in,
        "auth_method": auth_method,
        "chatgpt_subscription": chatgpt,
    }


def probe_claude(command_runner: CommandRunner, timeout: float) -> dict[str, Any]:
    result = command_runner(("claude", "auth", "status"), timeout)
    base: dict[str, Any] = {
        "command_available": not result.missing,
        "status": _command_status(result),
        "logged_in": False,
        "first_party": False,
        "auth_method": None,
        "allowed_auth_method": False,
        "subscription_auth": False,
    }
    if result.returncode != 0 or result.timed_out or result.missing:
        return base

    try:
        payload = json.loads(result.stdout)
    except json.JSONDecodeError:
        base["status"] = "invalid_json"
        return base
    if not isinstance(payload, dict):
        base["status"] = "invalid_json"
        return base

    auth_method = payload.get("authMethod")
    api_provider = payload.get("apiProvider")
    logged_in = payload.get("loggedIn") is True
    first_party = api_provider == "firstParty"
    allowed_method = isinstance(auth_method, str) and auth_method in CLAUDE_ALLOWED_AUTH_METHODS

    base.update(
        {
            "status": "ok",
            "logged_in": logged_in,
            "first_party": first_party,
            "auth_method": auth_method if isinstance(auth_method, str) else None,
            "allowed_auth_method": allowed_method,
            "subscription_auth": logged_in and first_party and allowed_method,
        }
    )
    return base


def default_command_runner(argv: Sequence[str], timeout: float) -> CommandResult:
    if shutil.which(argv[0]) is None:
        return CommandResult(returncode=127, stdout="", stderr="", missing=True)
    try:
        completed = subprocess.run(
            list(argv),
            check=False,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            timeout=timeout,
        )
    except subprocess.TimeoutExpired:
        return CommandResult(returncode=124, stdout="", stderr="", timed_out=True)
    return CommandResult(
        returncode=completed.returncode,
        stdout=completed.stdout,
        stderr=completed.stderr,
    )


def _api_override_presence(env: Mapping[str, str]) -> dict[str, dict[str, bool]]:
    return {
        "metered": {name: bool(env.get(name)) for name in METERED_API_OVERRIDE_ENV},
        "subscription_tokens": {name: bool(env.get(name)) for name in SUBSCRIPTION_TOKEN_ENV},
    }


def _command_status(result: CommandResult) -> str:
    if result.missing:
        return "missing"
    if result.timed_out:
        return "timeout"
    if result.returncode == 0:
        return "ok"
    return "error"


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description="Report worker auth health without exposing credential values.")
    parser.add_argument("--timeout", type=float, default=10.0)
    args = parser.parse_args(argv)
    report = evaluate_health(timeout=args.timeout)
    print(json.dumps(report, indent=2, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
