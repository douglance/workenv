"""Build and install the native workenv CLI from a checked-out source root."""

from __future__ import annotations

import argparse
import contextlib
import fcntl
import hashlib
import json
import os
import stat
import subprocess
import sys
import time
from pathlib import Path
from typing import Any

try:
    from remote import install_workenv
except ModuleNotFoundError:
    import install_workenv  # type: ignore[no-redef]


BUILD_LOCK = ".state/cli-build.lock"
INSTALL_RECEIPT = ".state/install/workenv.json"
TARGET_DIR = ".state/rust-target"
INSTALL_NAME = "workenv"


class BuildError(Exception):
    def __init__(self, code: str, message: str) -> None:
        super().__init__(message)
        self.code = code
        self.message = message


def display_json(payload: dict[str, Any]) -> str:
    return json.dumps(payload, sort_keys=True) + "\n"


def absolute(path: Path) -> Path:
    return Path(os.path.abspath(os.path.expanduser(os.fspath(path))))


def validate_regular(path: Path, code: str) -> None:
    install_workenv.assert_no_symlink_components(path)
    try:
        metadata = path.stat()
    except FileNotFoundError as error:
        raise BuildError(code, f"{path} does not exist") from error
    if not stat.S_ISREG(metadata.st_mode):
        raise BuildError(code, f"{path} is not a regular file")


def validate_source_root(root: Path) -> Path:
    root = absolute(root)
    install_workenv.assert_no_symlink_components(root)
    if not root.is_dir():
        raise BuildError("source_invalid", f"{root} is not a directory")
    validate_regular(root / "Cargo.toml", "source_invalid")
    validate_regular(root / "Cargo.lock", "source_invalid")
    src = root / "src"
    install_workenv.assert_no_symlink_components(src)
    if not src.is_dir():
        raise BuildError("source_invalid", f"{src} is not a directory")
    if any(path.is_symlink() for path in src.rglob("*")):
        raise BuildError("source_invalid", "src contains a symlink")
    return root


def source_files(root: Path) -> list[Path]:
    files: list[Path] = []
    for path in root.glob("Cargo*.toml"):
        validate_regular(path, "source_invalid")
        files.append(path)
    validate_regular(root / "Cargo.lock", "source_invalid")
    files.append(root / "Cargo.lock")
    build_rs = root / "build.rs"
    if build_rs.exists() or build_rs.is_symlink():
        validate_regular(build_rs, "source_invalid")
        files.append(build_rs)
    for path in sorted((root / "src").rglob("*")):
        if path.is_symlink():
            raise BuildError("source_invalid", f"{path} is a symlink")
        if path.is_file():
            validate_regular(path, "source_invalid")
            files.append(path)
    return sorted(set(files), key=lambda path: path.relative_to(root).as_posix())


def source_digest(root: Path) -> str:
    digest = hashlib.sha256()
    for path in source_files(root):
        relative = path.relative_to(root).as_posix()
        content = path.read_bytes()
        digest.update(relative.encode("utf-8"))
        digest.update(b"\0")
        digest.update(hashlib.sha256(content).hexdigest().encode("ascii"))
        digest.update(b"\0")
    return f"sha256:{digest.hexdigest()}"


@contextlib.contextmanager
def build_lock(root: Path):
    state = root / ".state"
    install_workenv.assert_no_symlink_components(state)
    state.mkdir(parents=True, exist_ok=True)
    lock_path = root / BUILD_LOCK
    install_workenv.assert_no_symlink_components(lock_path)
    flags = os.O_RDWR | os.O_CREAT
    if hasattr(os, "O_NOFOLLOW"):
        flags |= os.O_NOFOLLOW
    try:
        fd = os.open(lock_path, flags, 0o600)
    except OSError as error:
        raise BuildError("lock_failed", f"could not open {lock_path}: {error}") from error
    with os.fdopen(fd, "a+b") as handle:
        fcntl.flock(handle.fileno(), fcntl.LOCK_EX)
        try:
            yield
        finally:
            fcntl.flock(handle.fileno(), fcntl.LOCK_UN)


def read_receipt(path: Path) -> dict[str, Any] | None:
    if not path.exists():
        return None
    validate_regular(path, "receipt_invalid")
    try:
        value = json.loads(path.read_text(encoding="utf-8"))
    except json.JSONDecodeError as error:
        raise BuildError("receipt_invalid", f"{path} is not valid JSON") from error
    if not isinstance(value, dict):
        raise BuildError("receipt_invalid", f"{path} must contain a JSON object")
    return value


def installed_digest(path: Path) -> str | None:
    install_workenv.assert_no_symlink_components(path)
    if not path.exists():
        return None
    validate_regular(path, "installed_invalid")
    return install_workenv.sha256_file(path)


def reusable_install_candidate(root: Path, digest: str, target: Path) -> tuple[Path, str] | None:
    receipt = read_receipt(root / INSTALL_RECEIPT)
    if receipt is None or receipt.get("source_digest") != digest:
        return None
    expected = receipt.get("new_sha256")
    if not isinstance(expected, str):
        return None

    cached_candidate = root / TARGET_DIR / "release" / INSTALL_NAME
    if cached_candidate != target and installed_digest(cached_candidate) == expected:
        return cached_candidate, expected

    installed_path = receipt.get("installed_path")
    if not isinstance(installed_path, str):
        return None
    installed = absolute(Path(installed_path))
    if installed == target:
        return None
    if installed_digest(installed) != expected:
        return None
    return installed, expected


def run_cargo_build(root: Path) -> None:
    target_dir = root / TARGET_DIR
    install_workenv.assert_no_symlink_components(target_dir)
    target_dir.mkdir(parents=True, exist_ok=True)
    env = dict(os.environ)
    env["CARGO_TARGET_DIR"] = str(target_dir)
    completed = subprocess.run(
        ["cargo", "build", "--release", "--locked"],
        cwd=root,
        check=False,
        stdout=subprocess.DEVNULL,
        stderr=None,
        env=env,
    )
    if completed.returncode != 0:
        raise BuildError("build_failed", f"cargo build --release --locked exited {completed.returncode}")


def build(root: Path, bin_dir: Path, config: Path) -> dict[str, Any]:
    started = time.monotonic()
    root = validate_source_root(root)
    bin_dir = absolute(bin_dir)
    config = absolute(config)
    with build_lock(root):
        before = source_digest(root)
        reusable = reusable_install_candidate(root, before, bin_dir / INSTALL_NAME)
        if reusable is not None:
            candidate, candidate_sha256 = reusable
            duration_ms = int((time.monotonic() - started) * 1000)
            receipt_fields = {
                "source_digest": before,
                "duration_ms": duration_ms,
                "build_status": "skipped",
                "candidate_path": str(candidate),
            }
            installed = install_workenv.install(candidate, candidate_sha256, root, bin_dir, config, receipt_fields=receipt_fields)
            installed.update(
                {
                    "status": "skipped",
                    "build_status": "skipped",
                    "source_digest": before,
                    "duration_ms": duration_ms,
                    "candidate_path": str(candidate),
                }
            )
            return installed

        run_cargo_build(root)
        after = source_digest(root)
        if after != before:
            raise BuildError("source_changed", "source changed while cargo build was running")

        candidate = root / TARGET_DIR / "release" / INSTALL_NAME
        validate_regular(candidate, "candidate_missing")
        candidate_sha256 = install_workenv.sha256_file(candidate)
        duration_ms = int((time.monotonic() - started) * 1000)
        receipt_fields = {
            "source_digest": before,
            "duration_ms": duration_ms,
            "build_status": "built",
            "candidate_path": str(candidate),
        }
        installed = install_workenv.install(candidate, candidate_sha256, root, bin_dir, config, receipt_fields=receipt_fields)
        installed.update(
            {
                "status": "built",
                "build_status": "built",
                "source_digest": before,
                "duration_ms": duration_ms,
                "candidate_path": str(candidate),
            }
        )
        return installed


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description="Build and install the native workenv CLI.")
    parser.add_argument("--root", required=True, type=Path)
    parser.add_argument("--bin-dir", default=Path.home() / ".local" / "bin", type=Path)
    parser.add_argument("--config", default=Path.home() / ".config" / "workenv" / "config.json", type=Path)
    return parser


def main(argv: list[str] | None = None) -> int:
    args = build_parser().parse_args(argv)
    try:
        result = build(args.root, args.bin_dir, args.config)
    except (BuildError, install_workenv.InstallError) as error:
        code = getattr(error, "code", "build_error")
        message = getattr(error, "message", str(error))
        print(f"{code}: {message}", file=sys.stderr)
        return 1
    print(display_json(result), end="")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
