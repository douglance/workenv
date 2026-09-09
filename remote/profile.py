from __future__ import annotations

import argparse
import base64
import hashlib
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path


GENERATOR_VERSION = 1
SLUG_RE = re.compile(r"^[a-z][a-z0-9-]{0,47}$")
GITHUB_LOGIN_RE = re.compile(r"^[A-Za-z0-9][A-Za-z0-9-]{0,38}$")
SPEC_KEYS = {
    "schema_version",
    "name",
    "github_login",
    "git_name",
    "git_email",
    "anthropic_profile",
}
SECRET_FIELD_FRAGMENTS = ("token", "secret", "password", "credential", "api_key", "auth")
MANAGED_DIRS = (".gh", ".codex", ".claude", ".kube", ".config", ".bin")
STATIC_MANAGED_FILES = ("profile.json", "runtime.json", ".envrc", ".gitconfig", ".bin/wrangler")
MUTABLE_PROFILE_FILES = (".kube/config",)
MANAGED_FILES = (*STATIC_MANAGED_FILES, *MUTABLE_PROFILE_FILES)
DIR_MODE = 0o700
FILE_MODE = 0o600
EXEC_MODE = 0o700
MAX_PROFILE_VALUE_LEN = 254

CLEARED_VARS = (
    "GH_TOKEN",
    "GITHUB_TOKEN",
    "GH_ENTERPRISE_TOKEN",
    "GITHUB_ENTERPRISE_TOKEN",
    "ANTHROPIC_API_KEY",
    "ANTHROPIC_AUTH_TOKEN",
    "AUTH_TOKEN",
    "OPENAI_API_KEY",
    "CODEX_API_KEY",
    "CLOUDFLARE_API_TOKEN",
    "CLOUDFLARE_API_KEY",
    "CLOUDFLARE_EMAIL",
    "CLOUDFLARE_ACCOUNT_ID",
    "GIT_AUTHOR_NAME",
    "GIT_AUTHOR_EMAIL",
    "GIT_AUTHOR_DATE",
    "GIT_COMMITTER_NAME",
    "GIT_COMMITTER_EMAIL",
    "GIT_COMMITTER_DATE",
    "GIT_CONFIG_COUNT",
    "GIT_CONFIG_PARAMETERS",
)
CLEARED_PREFIXES = ("GIT_CONFIG_KEY_", "GIT_CONFIG_VALUE_")
CF_VARS = ("CLOUDFLARE_API_TOKEN", "CLOUDFLARE_API_KEY", "CLOUDFLARE_EMAIL", "CLOUDFLARE_ACCOUNT_ID")


class ProfileError(Exception):
    def __init__(self, reason: str, message: str, code: int = 1):
        super().__init__(message)
        self.reason = reason
        self.message = message
        self.code = code


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description="Prepare and execute isolated workenv profiles.")
    parser.add_argument("--root", required=True)
    subparsers = parser.add_subparsers(dest="command", required=True)

    prepare_parser = subparsers.add_parser("prepare")
    prepare_parser.add_argument("--name", required=True)
    prepare_parser.add_argument("--spec-base64", required=True)
    prepare_parser.add_argument("--digest", required=True)

    status_parser = subparsers.add_parser("status")
    status_parser.add_argument("--name", required=True)
    status_parser.add_argument("--digest", required=True)

    exec_parser = subparsers.add_parser("exec")
    exec_parser.add_argument("--name", required=True)
    exec_parser.add_argument("--digest", required=True)
    exec_parser.add_argument("--check-github", action="store_true")
    exec_parser.add_argument("argv", nargs=argparse.REMAINDER)

    check_parser = subparsers.add_parser("__check-and-exec")
    check_parser.add_argument("--name", required=True)
    check_parser.add_argument("--digest", required=True)
    check_parser.add_argument("--check-github", action="store_true")
    check_parser.add_argument("argv", nargs=argparse.REMAINDER)

    github_status_parser = subparsers.add_parser("__github-status")
    github_status_parser.add_argument("--name", required=True)
    github_status_parser.add_argument("--digest", required=True)

    args = parser.parse_args(argv)
    manager = ProfileManager(Path(args.root).expanduser())

    if args.command == "exec":
        try:
            return manager.exec_profile(args.name, args.digest, _command_argv(args.argv), args.check_github)
        except ProfileError as exc:
            _write_safe_error(exc, stream=sys.stderr)
            return exc.code
    if args.command == "__check-and-exec":
        try:
            command = _command_argv(args.argv)
            if not command:
                raise ProfileError("missing_command", "exec requires a command after --")
            manager.check_selected_environment(args.name, args.digest, args.check_github)
            os.execvp(command[0], command)
        except ProfileError as exc:
            _write_safe_error(exc, stream=sys.stderr)
            return exc.code
        except OSError as exc:
            _write_json({"ok": False, "reason": "exec_failed", "error": exc.strerror or "exec failed"}, stream=sys.stderr)
            return 127
    if args.command == "__github-status":
        try:
            result = manager.github_status(args.name, args.digest)
            _write_json(result)
            return 0
        except ProfileError as exc:
            _write_safe_error(exc, stream=sys.stderr)
            return exc.code

    try:
        if args.command == "prepare":
            result = manager.prepare(args.name, args.spec_base64, args.digest)
        elif args.command == "status":
            result = manager.status(args.name, args.digest)
        else:
            raise AssertionError(args.command)
        _write_json(result)
        return 0
    except ProfileError as exc:
        _write_safe_error(exc)
        return exc.code


class ProfileManager:
    def __init__(self, root: Path):
        self.root = root
        self.profiles_root = Path.home() / ".config" / "workenv" / "profiles"

    def prepare(self, name: str, spec_base64: str, digest: str) -> dict:
        _validate_slug(name)
        _validate_digest(digest)
        spec = _decode_spec(spec_base64, name, digest)
        profile_dir = self._profile_dir(name)
        self._ensure_profile_path(profile_dir)

        profile_dir.mkdir(mode=DIR_MODE, parents=True, exist_ok=True)
        _chmod(profile_dir, DIR_MODE)
        self._reject_managed_symlinks(profile_dir)
        for relative in MANAGED_DIRS:
            directory = profile_dir / relative
            directory.mkdir(mode=DIR_MODE, exist_ok=True)
            _chmod(directory, DIR_MODE)
        self._ensure_mutable_file(profile_dir / ".kube" / "config")

        existing_spec = profile_dir / "profile.json"
        existing_runtime = profile_dir / "runtime.json"
        if existing_spec.exists() and existing_runtime.exists():
            self._load_prepared_spec(profile_dir, name, digest)
            direnv_allow = self._run_direnv(profile_dir, "allow")
            return {
                "ok": True,
                "status": "profile_prepared",
                "prepared": True,
                "name": name,
                "digest": digest,
                "paths": _safe_paths(profile_dir),
                "direnv": {"allowed": direnv_allow},
            }

        contents = self._generated_contents(profile_dir, spec, digest)
        existing_spec = profile_dir / "profile.json"
        if existing_spec.exists() and _read_text(existing_spec) != contents["profile.json"]:
            raise ProfileError("profile_conflict", "existing profile spec does not match requested digest")

        for relative, content in contents.items():
            if relative == "runtime.json":
                continue
            mode = EXEC_MODE if relative == ".bin/wrangler" else FILE_MODE
            _write_owned_file(profile_dir / relative, content, mode)

        self._reject_managed_symlinks(profile_dir)
        direnv_allow = self._run_direnv(profile_dir, "allow")
        runtime = self._runtime_text(profile_dir, spec, digest)
        _write_owned_file(profile_dir / "runtime.json", runtime, FILE_MODE)
        return {
            "ok": True,
            "status": "profile_prepared",
            "prepared": True,
            "name": name,
            "digest": digest,
            "paths": _safe_paths(profile_dir),
            "direnv": {"allowed": direnv_allow},
        }

    def status(self, name: str, digest: str) -> dict:
        _validate_slug(name)
        _validate_digest(digest)
        profile_dir = self._profile_dir(name)
        base = {
            "ok": False,
            "status": "profile_not_prepared",
            "prepared": False,
            "name": name,
            "digest": digest,
            "paths": _safe_paths(profile_dir),
            "github": {"expected": None, "actual": None, "matches": False, "reason": "not_prepared"},
        }
        try:
            spec = self._load_prepared_spec(profile_dir, name, digest)
        except ProfileError as exc:
            base["status"] = exc.reason
            base["reason"] = exc.reason
            base["error"] = exc.message
            return base

        base["prepared"] = True
        base["github"] = self._github_status_via_direnv(profile_dir, name, digest, spec)
        base["ok"] = spec.get("github_login") is None or base["github"]["matches"]
        base["status"] = "profile_ready" if base["ok"] else "profile_auth_required"
        return base

    def exec_profile(self, name: str, digest: str, command: list[str], check_github: bool) -> int:
        _validate_slug(name)
        _validate_digest(digest)
        if not command:
            raise ProfileError("missing_command", "exec requires a command after --")
        profile_dir = self._profile_dir(name)
        self._load_prepared_spec(profile_dir, name, digest)
        direnv = shutil.which("direnv")
        if not direnv:
            raise ProfileError("direnv_missing", "direnv is not installed")
        internal = [
            sys.executable,
            str(Path(__file__).resolve()),
            "--root",
            str(self.root),
            "__check-and-exec",
            "--name",
            name,
            "--digest",
            digest,
        ]
        if check_github:
            internal.append("--check-github")
        internal.extend(["--", *command])
        os.execvp(direnv, [direnv, "exec", str(profile_dir), *internal])
        raise AssertionError("os.execvp returned")

    def check_selected_environment(self, name: str, digest: str, check_github: bool) -> None:
        profile_dir = self._profile_dir(name)
        spec = self._load_prepared_spec(profile_dir, name, digest)
        expected = {
            "WORKENV_PROFILE": name,
            "WORKENV_PROFILE_DIGEST": digest,
            "GH_CONFIG_DIR": str(profile_dir / ".gh"),
            "GIT_CONFIG_GLOBAL": str(profile_dir / ".gitconfig"),
            "GIT_CONFIG_NOSYSTEM": "1",
            "CODEX_HOME": str(profile_dir / ".codex"),
            "CLAUDE_CONFIG_DIR": str(profile_dir / ".claude"),
            "KUBECONFIG": str(profile_dir / ".kube" / "config"),
        }
        for key, value in expected.items():
            if os.environ.get(key) != value:
                raise ProfileError("profile_env_mismatch", f"{key} does not match selected profile")
        for key in CLEARED_VARS:
            if key in os.environ:
                raise ProfileError("profile_env_poisoned", f"{key} was inherited into profile")
        for key in os.environ:
            if key.startswith(CLEARED_PREFIXES):
                raise ProfileError("profile_env_poisoned", "git config environment override was inherited into profile")
        if spec.get("anthropic_profile"):
            if os.environ.get("ANTHROPIC_PROFILE") != spec["anthropic_profile"]:
                raise ProfileError("profile_env_mismatch", "ANTHROPIC_PROFILE does not match selected profile")
        elif "ANTHROPIC_PROFILE" in os.environ:
            raise ProfileError("profile_env_poisoned", "ANTHROPIC_PROFILE was inherited into profile")
        if check_github and spec.get("github_login"):
            github = _check_github_login(spec.get("github_login"))
            if not github["matches"]:
                raise ProfileError(github["reason"], "GitHub identity does not match selected profile")

    def github_status(self, name: str, digest: str) -> dict:
        self.check_selected_environment(name, digest, False)
        spec = self._load_prepared_spec(self._profile_dir(name), name, digest)
        return _check_github_login(spec.get("github_login"))

    def _github_status_via_direnv(self, profile_dir: Path, name: str, digest: str, spec: dict) -> dict:
        direnv = shutil.which("direnv")
        if not direnv:
            return _github_status_payload(spec.get("github_login"), None, "direnv_missing")
        completed = subprocess.run(
            [
                direnv,
                "exec",
                str(profile_dir),
                sys.executable,
                str(Path(__file__).resolve()),
                "--root",
                str(self.root),
                "__github-status",
                "--name",
                name,
                "--digest",
                digest,
            ],
            capture_output=True,
            text=True,
            timeout=30,
        )
        if completed.returncode != 0:
            return _safe_github_error(completed.stderr, spec.get("github_login"))
        try:
            result = json.loads(completed.stdout)
        except json.JSONDecodeError:
            return _github_status_payload(spec.get("github_login"), None, "github_status_invalid")
        return {
            "expected": _safe_optional(result.get("expected")),
            "actual": _safe_optional(result.get("actual")),
            "matches": result.get("matches") is True,
            "reason": _safe_optional(result.get("reason")) or "unknown",
        }

    def _load_prepared_spec(self, profile_dir: Path, name: str, digest: str) -> dict:
        self._ensure_profile_path(profile_dir)
        self._reject_managed_symlinks(profile_dir)
        if not profile_dir.is_dir():
            raise ProfileError("profile_missing", "profile is not prepared")
        profile_json = profile_dir / "profile.json"
        runtime_json = profile_dir / "runtime.json"
        if not profile_json.is_file() or not runtime_json.is_file():
            raise ProfileError("profile_incomplete", "profile runtime files are missing")
        try:
            sealed = json.loads(profile_json.read_text(encoding="utf-8"))
            runtime = json.loads(runtime_json.read_text(encoding="utf-8"))
        except json.JSONDecodeError:
            raise ProfileError("profile_invalid", "profile runtime files are invalid")
        spec = sealed.get("spec")
        if not isinstance(spec, dict):
            raise ProfileError("profile_invalid", "profile spec is invalid")
        if sealed.get("digest") != digest or spec.get("name") != name:
            raise ProfileError("digest_mismatch", "profile digest does not match requested profile")
        if _canonical_digest(spec) != digest:
            raise ProfileError("digest_mismatch", "profile spec digest is invalid")
        if runtime.get("generator_version") != GENERATOR_VERSION or runtime.get("digest") != digest:
            raise ProfileError("runtime_mismatch", "profile runtime metadata does not match requested profile")
        expected_hash = runtime.get("config_hash")
        if not isinstance(expected_hash, str) or expected_hash != self._config_hash(profile_dir):
            raise ProfileError("runtime_mismatch", "profile generated config hash does not match runtime metadata")
        return spec

    def _generated_contents(self, profile_dir: Path, spec: dict, digest: str) -> dict[str, str]:
        profile_json = _json_text({"schema_version": 1, "digest": digest, "spec": spec})
        envrc = _envrc_text(profile_dir, spec, digest)
        gitconfig = _gitconfig_text(spec)
        wrangler = _wrangler_wrapper_text(profile_dir)
        files = {
            "profile.json": profile_json,
            ".envrc": envrc,
            ".gitconfig": gitconfig,
            ".bin/wrangler": wrangler,
        }
        files["runtime.json"] = self._runtime_text_from_files(files, spec, digest)
        return files

    def _runtime_text(self, profile_dir: Path, spec: dict, digest: str) -> str:
        files = {}
        for relative in ("profile.json", ".envrc", ".gitconfig", ".bin/wrangler"):
            path = profile_dir / relative
            if not path.is_file() or path.is_symlink():
                raise ProfileError("profile_incomplete", f"{relative} is missing")
            files[relative] = path.read_text(encoding="utf-8")
        return self._runtime_text_from_files(files, spec, digest)

    def _runtime_text_from_files(self, files: dict[str, str], spec: dict, digest: str) -> str:
        runtime = {
            "schema_version": 1,
            "generator_version": GENERATOR_VERSION,
            "name": spec["name"],
            "digest": digest,
            "config_hash": _hash_generated(files),
            "managed_files": sorted(files),
        }
        return _json_text(runtime)

    def _config_hash(self, profile_dir: Path) -> str:
        files = {}
        for relative in ("profile.json", ".envrc", ".gitconfig", ".bin/wrangler"):
            path = profile_dir / relative
            if not path.is_file() or path.is_symlink():
                raise ProfileError("profile_incomplete", f"{relative} is missing")
            files[relative] = path.read_text(encoding="utf-8")
        return _hash_generated(files)

    def _run_direnv(self, profile_dir: Path, action: str) -> bool:
        direnv = shutil.which("direnv")
        if not direnv:
            raise ProfileError("direnv_missing", "direnv is not installed")
        completed = subprocess.run([direnv, action, str(profile_dir)], capture_output=True, text=True, timeout=30)
        if completed.returncode != 0:
            raise ProfileError("direnv_failed", f"direnv {action} failed")
        return True

    def _profile_dir(self, name: str) -> Path:
        return self.profiles_root / name

    def _ensure_profile_path(self, profile_dir: Path) -> None:
        try:
            profile_resolved = profile_dir.resolve(strict=False)
            root_resolved = self.root.resolve(strict=False)
        except OSError as exc:
            raise ProfileError("profile_path_invalid", str(exc))
        _reject_symlink_ancestors(profile_dir)
        if profile_resolved == root_resolved or root_resolved in profile_resolved.parents:
            raise ProfileError("profile_path_invalid", "profile directory must be outside the workenv root")

    def _reject_managed_symlinks(self, profile_dir: Path) -> None:
        for relative in (*MANAGED_DIRS, *MANAGED_FILES):
            path = profile_dir / relative
            if path.is_symlink():
                raise ProfileError("profile_path_invalid", f"{relative} must not be a symlink")
        if profile_dir.exists():
            self._validate_profile_modes(profile_dir)

    def _ensure_mutable_file(self, path: Path) -> None:
        if path.is_symlink():
            raise ProfileError("profile_path_invalid", f"{path.name} must not be a symlink")
        if path.exists():
            if not path.is_file():
                raise ProfileError("profile_path_invalid", f"{path.name} must be a file")
            _validate_path_mode(path, FILE_MODE)
            return
        _write_owned_file(path, "", FILE_MODE)

    def _validate_profile_modes(self, profile_dir: Path) -> None:
        _validate_path_mode(profile_dir, DIR_MODE)
        for relative in MANAGED_DIRS:
            path = profile_dir / relative
            if path.exists():
                if not path.is_dir():
                    raise ProfileError("profile_path_invalid", f"{relative} must be a directory")
                _validate_path_mode(path, DIR_MODE)
        for relative in STATIC_MANAGED_FILES:
            path = profile_dir / relative
            if path.exists():
                if not path.is_file():
                    raise ProfileError("profile_path_invalid", f"{relative} must be a file")
                _validate_path_mode(path, EXEC_MODE if relative == ".bin/wrangler" else FILE_MODE)
        for relative in MUTABLE_PROFILE_FILES:
            path = profile_dir / relative
            if path.exists():
                if not path.is_file():
                    raise ProfileError("profile_path_invalid", f"{relative} must be a file")
                _validate_path_mode(path, FILE_MODE)


def _decode_spec(spec_base64: str, name: str, digest: str) -> dict:
    try:
        raw = base64.b64decode(spec_base64, validate=True)
        spec = json.loads(raw.decode("utf-8"))
    except (ValueError, UnicodeDecodeError) as exc:
        raise ProfileError("spec_invalid", f"invalid profile spec: {exc}")
    _validate_spec(spec, name)
    actual = _canonical_digest(spec)
    if actual != digest:
        raise ProfileError("digest_mismatch", "spec digest does not match canonical JSON")
    return spec


def _validate_spec(spec: object, name: str) -> None:
    if not isinstance(spec, dict):
        raise ProfileError("spec_invalid", "profile spec must be a JSON object")
    unknown = set(spec) - SPEC_KEYS
    secret = sorted(key for key in unknown if any(fragment in key.lower() for fragment in SECRET_FIELD_FRAGMENTS))
    if secret:
        raise ProfileError("spec_secret_field", "profile spec must not contain secret fields")
    if unknown:
        raise ProfileError("spec_unknown_field", f"profile spec contains unknown fields: {', '.join(sorted(unknown))}")
    if spec.get("schema_version") != 1:
        raise ProfileError("spec_invalid", "schema_version must be 1")
    if spec.get("name") != name:
        raise ProfileError("spec_invalid", "spec name must match --name")
    _validate_slug(name)
    for key in ("github_login", "git_name", "git_email", "anthropic_profile"):
        if key not in spec:
            continue
        value = spec[key]
        if not isinstance(value, str):
            raise ProfileError("spec_invalid", f"{key} must be a string")
        if value == "":
            raise ProfileError("spec_invalid", f"{key} must not be empty")
        if value != value.strip():
            raise ProfileError("spec_invalid", f"{key} must not have leading or trailing whitespace")
        if len(value) > MAX_PROFILE_VALUE_LEN:
            raise ProfileError("spec_invalid", f"{key} must be at most {MAX_PROFILE_VALUE_LEN} characters")
        if _has_control_char(value):
            raise ProfileError("spec_invalid", f"{key} must not contain control characters or newlines")
    github_login = spec.get("github_login")
    if github_login is not None and not GITHUB_LOGIN_RE.match(github_login):
        raise ProfileError("spec_invalid", "github_login is not a conventional GitHub login")


def _validate_slug(name: str) -> None:
    if not isinstance(name, str) or not SLUG_RE.match(name):
        raise ProfileError("invalid_name", "profile name must match [a-z][a-z0-9-]{0,47}")


def _validate_digest(digest: str) -> None:
    if not isinstance(digest, str) or not re.fullmatch(r"[0-9a-f]{64}", digest):
        raise ProfileError("invalid_digest", "digest must be a lowercase sha256 hex string")


def _canonical_digest(spec: dict) -> str:
    encoded = json.dumps(spec, sort_keys=True, separators=(",", ":"), ensure_ascii=False).encode("utf-8")
    return hashlib.sha256(encoded).hexdigest()


def _hash_generated(files: dict[str, str]) -> str:
    payload = {key: files[key] for key in sorted(files)}
    return hashlib.sha256(json.dumps(payload, sort_keys=True, separators=(",", ":"), ensure_ascii=False).encode()).hexdigest()


def _json_text(value: dict) -> str:
    return json.dumps(value, sort_keys=True, separators=(",", ":"), ensure_ascii=False) + "\n"


def _envrc_text(profile_dir: Path, spec: dict, digest: str) -> str:
    lines = [
        "# Generated by workenv profile. Do not put secrets in this file.",
        "unset GH_TOKEN GITHUB_TOKEN GH_ENTERPRISE_TOKEN GITHUB_ENTERPRISE_TOKEN",
        "unset ANTHROPIC_API_KEY ANTHROPIC_AUTH_TOKEN AUTH_TOKEN OPENAI_API_KEY CODEX_API_KEY",
        "unset CLOUDFLARE_API_TOKEN CLOUDFLARE_API_KEY CLOUDFLARE_EMAIL CLOUDFLARE_ACCOUNT_ID",
        "unset GIT_AUTHOR_NAME GIT_AUTHOR_EMAIL GIT_AUTHOR_DATE",
        "unset GIT_COMMITTER_NAME GIT_COMMITTER_EMAIL GIT_COMMITTER_DATE",
        "unset GIT_CONFIG_COUNT GIT_CONFIG_PARAMETERS",
        "for workenv_var in ${!GIT_CONFIG_KEY_@} ${!GIT_CONFIG_VALUE_@}; do",
        '  unset "$workenv_var"',
        "done",
        f"export WORKENV_PROFILE={_shell_quote(spec['name'])}",
        f"export WORKENV_PROFILE_DIGEST={_shell_quote(digest)}",
        f"export GH_CONFIG_DIR={_shell_quote(str(profile_dir / '.gh'))}",
        f"export GIT_CONFIG_GLOBAL={_shell_quote(str(profile_dir / '.gitconfig'))}",
        "export GIT_CONFIG_NOSYSTEM=1",
        f"export CODEX_HOME={_shell_quote(str(profile_dir / '.codex'))}",
        f"export CLAUDE_CONFIG_DIR={_shell_quote(str(profile_dir / '.claude'))}",
        f"export KUBECONFIG={_shell_quote(str(profile_dir / '.kube' / 'config'))}",
    ]
    if spec.get("anthropic_profile"):
        lines.append(f"export ANTHROPIC_PROFILE={_shell_quote(spec['anthropic_profile'])}")
    else:
        lines.append("unset ANTHROPIC_PROFILE")
    lines.append(f"PATH_add {_shell_quote(str(profile_dir / '.bin'))}")
    return "\n".join(lines) + "\n"


def _gitconfig_text(spec: dict) -> str:
    lines = ["[user]", "\tuseConfigOnly = true"]
    if spec.get("git_name"):
        lines.append(f"\tname = {_git_config_quote(spec['git_name'])}")
    if spec.get("git_email"):
        lines.append(f"\temail = {_git_config_quote(spec['git_email'])}")
    lines.extend(
        [
            '[credential "https://github.com"]',
            "\thelper =",
            "\thelper = !gh auth git-credential",
        ]
    )
    return "\n".join(lines) + "\n"


def _wrangler_wrapper_text(profile_dir: Path) -> str:
    unset_vars = " ".join(CF_VARS)
    return f"""#!/usr/bin/env bash
set -euo pipefail
profile_dir={_shell_quote(str(profile_dir))}
profile_bin="$profile_dir/.bin"
unset {unset_vars}
if [ -L "$profile_dir/.cloudflare.env" ]; then
  echo "profile-local Cloudflare env file must not be a symlink" >&2
  exit 1
fi
if [ -f "$profile_dir/.cloudflare.env" ]; then
  set -a
  . "$profile_dir/.cloudflare.env"
  set +a
fi
workenv_path=""
IFS=: read -r -a workenv_parts <<< "${{PATH:-}}"
for workenv_part in "${{workenv_parts[@]}}"; do
  if [ "$workenv_part" != "$profile_bin" ]; then
    if [ -z "$workenv_path" ]; then
      workenv_path="$workenv_part"
    else
      workenv_path="$workenv_path:$workenv_part"
    fi
  fi
done
wrangler_path="$(PATH="$workenv_path" command -v wrangler || true)"
if [ -z "$wrangler_path" ]; then
  echo "wrangler not found outside profile bin" >&2
  exit 127
fi
exec env XDG_CONFIG_HOME="$profile_dir/.config" PATH="$workenv_path" "$wrangler_path" "$@"
"""


def _check_github_login(expected: str | None) -> dict:
    if expected is None:
        return _github_status_payload(None, None, "github_login_not_configured")
    gh = shutil.which("gh")
    if not gh:
        return _github_status_payload(expected, None, "gh_missing")
    try:
        completed = subprocess.run(
            [gh, "api", "user", "--jq", ".login"],
            capture_output=True,
            text=True,
            timeout=20,
        )
    except subprocess.TimeoutExpired:
        return _github_status_payload(expected, None, "gh_timeout")
    if completed.returncode != 0:
        return _github_status_payload(expected, None, "gh_auth_failed")
    actual = completed.stdout.strip().splitlines()[0] if completed.stdout.strip() else None
    if actual is not None and not GITHUB_LOGIN_RE.match(actual):
        actual = None
    reason = "matched" if actual is not None and actual.lower() == expected.lower() else "identity_mismatch"
    return _github_status_payload(expected, actual, reason)


def _github_status_payload(expected: str | None, actual: str | None, reason: str) -> dict:
    matches = expected is not None and actual is not None and actual.lower() == expected.lower()
    return {"expected": expected, "actual": actual, "matches": matches, "reason": reason}


def _safe_github_error(stderr: str, expected: str | None) -> dict:
    payload = None
    for line in reversed(stderr.splitlines()):
        try:
            parsed = json.loads(line)
        except json.JSONDecodeError:
            continue
        if isinstance(parsed, dict):
            payload = parsed
            break
    if payload is None:
        return _github_status_payload(expected, None, "github_status_failed")
    reason = payload.get("reason")
    if not isinstance(reason, str) or _has_control_char(reason):
        reason = "github_status_failed"
    return _github_status_payload(expected, None, reason)


def _safe_paths(profile_dir: Path) -> dict:
    return {
        "profile_dir": str(profile_dir),
        "gh_config_dir": str(profile_dir / ".gh"),
        "git_config_global": str(profile_dir / ".gitconfig"),
        "codex_home": str(profile_dir / ".codex"),
        "claude_config_dir": str(profile_dir / ".claude"),
        "kubeconfig": str(profile_dir / ".kube" / "config"),
        "bin_dir": str(profile_dir / ".bin"),
        "wrangler": str(profile_dir / ".bin" / "wrangler"),
    }


def _write_owned_file(path: Path, content: str, mode: int) -> None:
    if path.exists():
        if path.is_symlink():
            raise ProfileError("profile_path_invalid", f"{path.name} must not be a symlink")
        if path.read_text(encoding="utf-8") != content:
            raise ProfileError("profile_conflict", f"{path.name} already exists with different content")
        _chmod(path, mode)
        return
    path.parent.mkdir(mode=DIR_MODE, parents=True, exist_ok=True)
    fd, tmp_name = tempfile.mkstemp(prefix=f".{path.name}.", dir=path.parent)
    tmp = Path(tmp_name)
    try:
        with os.fdopen(fd, "w", encoding="utf-8") as file:
            file.write(content)
            file.flush()
            os.fsync(file.fileno())
        os.chmod(tmp, mode)
        os.replace(tmp, path)
    finally:
        if tmp.exists():
            tmp.unlink()


def _read_text(path: Path) -> str:
    if path.is_symlink():
        raise ProfileError("profile_path_invalid", f"{path.name} must not be a symlink")
    return path.read_text(encoding="utf-8")


def _chmod(path: Path, mode: int) -> None:
    if path.is_symlink():
        raise ProfileError("profile_path_invalid", f"{path.name} must not be a symlink")
    os.chmod(path, mode)


def _reject_symlink_ancestors(path: Path) -> None:
    try:
        relative_parts = path.relative_to(Path.home()).parts
        current = Path.home()
    except ValueError:
        relative_parts = path.parts
        current = Path(path.anchor)
        if current.exists() and current.is_symlink():
            raise ProfileError("profile_path_invalid", "profile path ancestors must not be symlinks")
    for part in relative_parts:
        current = current / part
        if current.exists() and current.is_symlink():
            raise ProfileError("profile_path_invalid", "profile path ancestors must not be symlinks")


def _validate_path_mode(path: Path, expected_mode: int) -> None:
    stat = path.stat()
    if stat.st_uid != os.getuid():
        raise ProfileError("profile_path_invalid", f"{path.name} is not owned by the current user")
    actual_mode = stat.st_mode & 0o777
    if actual_mode != expected_mode:
        raise ProfileError("profile_path_invalid", f"{path.name} mode must be {expected_mode:o}")


def _has_control_char(value: str) -> bool:
    return any(ord(char) < 32 or ord(char) == 127 for char in value)


def _shell_quote(value: str) -> str:
    return "'" + value.replace("'", "'\"'\"'") + "'"


def _git_config_quote(value: str) -> str:
    return '"' + value.replace("\\", "\\\\").replace('"', '\\"') + '"'


def _command_argv(argv: list[str]) -> list[str]:
    if argv and argv[0] == "--":
        return argv[1:]
    return argv


def _safe_optional(value: object) -> str | None:
    if isinstance(value, str) and not _has_control_char(value):
        return value
    return None


def _write_json(value: dict, stream=sys.stdout) -> None:
    print(json.dumps(value, sort_keys=True), file=stream)


def _write_safe_error(exc: ProfileError, stream=sys.stdout) -> None:
    _write_json({"ok": False, "reason": exc.reason, "error": exc.message}, stream=stream)


if __name__ == "__main__":
    raise SystemExit(main())
