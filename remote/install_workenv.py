"""Install a verified native workenv binary into a stable user path."""

from __future__ import annotations

import argparse
import contextlib
import fcntl
import hashlib
import json
import os
import pwd
import shlex
import shutil
import stat
import subprocess
import sys
import tempfile
import time
from pathlib import Path
from typing import Any


INSTALL_NAME = "workenv"
VALIDATION_TIMEOUT_SECONDS = 10
PATH_BLOCK_BEGIN = "# >>> workenv managed PATH >>>"
PATH_BLOCK_END = "# <<< workenv managed PATH <<<"



class InstallError(Exception):
    def __init__(self, code: str, message: str) -> None:
        super().__init__(message)
        self.code = code
        self.message = message


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def canonical_json(payload: dict[str, Any]) -> str:
    return json.dumps(payload, sort_keys=True, separators=(",", ":")) + "\n"


def display_json(payload: dict[str, Any]) -> str:
    return json.dumps(payload, sort_keys=True) + "\n"


def absolute(path: Path) -> Path:
    return Path(os.path.abspath(os.path.expanduser(os.fspath(path))))


def assert_no_symlink_components(path: Path) -> None:
    current = absolute(path)
    candidates = [current, *current.parents]
    for component in candidates:
        if component.exists() or component.is_symlink():
            try:
                if component.is_symlink():
                    raise InstallError("symlink_ancestor", f"{component} is a symlink")
            except OSError as error:
                raise InstallError("path_invalid", f"could not inspect {component}: {error}") from error


def assert_regular(path: Path, *, executable: bool, code: str) -> None:
    try:
        metadata = path.stat()
    except FileNotFoundError as error:
        raise InstallError(code, f"{path} does not exist") from error
    if not stat.S_ISREG(metadata.st_mode):
        raise InstallError(code, f"{path} is not a regular file")
    if executable and not os.access(path, os.X_OK):
        raise InstallError(code, f"{path} is not executable")


def assert_user_writable_dir(path: Path) -> None:
    path.mkdir(parents=True, exist_ok=True)
    assert_no_symlink_components(path)
    if not path.is_dir():
        raise InstallError("path_invalid", f"{path} is not a directory")
    if not os.access(path, os.W_OK | os.X_OK):
        raise InstallError("not_user_writable", f"{path} is not writable by the current user")


def fsync_directory(path: Path) -> None:
    flags = os.O_RDONLY
    if hasattr(os, "O_DIRECTORY"):
        flags |= os.O_DIRECTORY
    try:
        fd = os.open(path, flags)
    except OSError:
        return
    try:
        os.fsync(fd)
    finally:
        os.close(fd)


def write_json_atomic(path: Path, payload: dict[str, Any]) -> None:
    assert_no_symlink_components(path.parent)
    path.parent.mkdir(parents=True, exist_ok=True)
    fd, tmp_name = tempfile.mkstemp(prefix=f".{path.name}.", dir=path.parent)
    try:
        with os.fdopen(fd, "w", encoding="utf-8") as handle:
            handle.write(canonical_json(payload))
            handle.flush()
            os.fsync(handle.fileno())
        os.replace(tmp_name, path)
        fsync_directory(path.parent)
    finally:
        try:
            os.unlink(tmp_name)
        except FileNotFoundError:
            pass


def write_bytes_atomic(path: Path, payload: bytes, mode: int) -> None:
    assert_no_symlink_components(path.parent)
    path.parent.mkdir(parents=True, exist_ok=True)
    fd, tmp_name = tempfile.mkstemp(prefix=f".{path.name}.", dir=path.parent)
    try:
        with os.fdopen(fd, "wb") as handle:
            handle.write(payload)
            handle.flush()
            os.fchmod(handle.fileno(), mode)
            os.fsync(handle.fileno())
        os.replace(tmp_name, path)
        fsync_directory(path.parent)
    finally:
        try:
            os.unlink(tmp_name)
        except FileNotFoundError:
            pass


def copy_executable_atomic(source: Path, target: Path) -> None:
    assert_no_symlink_components(target.parent)
    target.parent.mkdir(parents=True, exist_ok=True)
    fd, tmp_name = tempfile.mkstemp(prefix=f".{target.name}.", dir=target.parent)
    try:
        with os.fdopen(fd, "wb") as output, source.open("rb") as input_file:
            shutil.copyfileobj(input_file, output, length=1024 * 1024)
            output.flush()
            os.fchmod(output.fileno(), 0o755)
            os.fsync(output.fileno())
        os.replace(tmp_name, target)
        fsync_directory(target.parent)
    finally:
        try:
            os.unlink(tmp_name)
        except FileNotFoundError:
            pass


def copy_file_atomic(source: Path, target: Path, mode: int) -> None:
    assert_no_symlink_components(target.parent)
    target.parent.mkdir(parents=True, exist_ok=True)
    fd, tmp_name = tempfile.mkstemp(prefix=f".{target.name}.", dir=target.parent)
    try:
        with os.fdopen(fd, "wb") as output, source.open("rb") as input_file:
            shutil.copyfileobj(input_file, output, length=1024 * 1024)
            output.flush()
            os.fchmod(output.fileno(), mode)
            os.fsync(output.fileno())
        os.replace(tmp_name, target)
        fsync_directory(target.parent)
    finally:
        try:
            os.unlink(tmp_name)
        except FileNotFoundError:
            pass


def remove_file(path: Path) -> None:
    assert_no_symlink_components(path)
    try:
        path.unlink()
        fsync_directory(path.parent)
    except FileNotFoundError:
        pass


def snapshot_file(path: Path) -> tuple[bytes, int] | None:
    assert_no_symlink_components(path)
    if not path.exists():
        return None
    assert_regular(path, executable=False, code="path_invalid")
    metadata = path.stat()
    return path.read_bytes(), stat.S_IMODE(metadata.st_mode)


def restore_snapshot(path: Path, snapshot: tuple[bytes, int] | None) -> None:
    if snapshot is None:
        remove_file(path)
        return
    payload, mode = snapshot
    write_bytes_atomic(path, payload, mode)


def current_user_home_shell() -> tuple[Path, str]:
    entry = pwd.getpwuid(os.getuid())
    return absolute(Path(entry.pw_dir)), entry.pw_shell


def shell_startup_files(home: Path, shell: str) -> list[Path]:
    shell_name = Path(shell).name.lstrip("-")
    if shell_name == "bash":
        login_candidates = [home / ".bash_profile", home / ".bash_login", home / ".profile"]
        login_file = next((path for path in login_candidates if path.exists()), home / ".bash_profile")
        files = [home / ".bashrc", login_file]
    elif shell_name == "zsh":
        files = [home / ".zshenv"]
    else:
        return []
    deduped: list[Path] = []
    for path in files:
        if path not in deduped:
            deduped.append(path)
    return deduped


def managed_path_block(bin_dir: Path) -> str:
    bin_dir_text = str(bin_dir)
    quoted = shlex.quote(bin_dir_text)
    return (
        f"{PATH_BLOCK_BEGIN}\n"
        'case ":$PATH:" in\n'
        f"  *:{quoted}:*) ;;\n"
        f"  *) export PATH={quoted}:$PATH ;;\n"
        "esac\n"
        f"{PATH_BLOCK_END}\n"
    )


def remove_managed_path_blocks(text: str) -> str:
    result = text
    while True:
        start = result.find(PATH_BLOCK_BEGIN)
        if start == -1:
            return result
        end = result.find(PATH_BLOCK_END, start)
        if end == -1:
            raise InstallError("shell_config_invalid", "unterminated workenv managed PATH block")
        end += len(PATH_BLOCK_END)
        if end < len(result) and result[end] == "\n":
            end += 1
        result = result[:start] + result[end:]


def write_text_atomic(path: Path, text: str, mode: int) -> None:
    write_bytes_atomic(path, text.encode("utf-8"), mode)


def shell_path_files_for_bin_dir(bin_dir: Path) -> tuple[str, list[Path]]:
    home, shell = current_user_home_shell()
    try:
        absolute(bin_dir).relative_to(home)
    except ValueError:
        return shell, []
    return shell, shell_startup_files(home, shell)


def resolve_executable(name: str) -> Path:
    found = shutil.which(name)
    if found is None:
        raise InstallError(f"{name}_unavailable", f"{name} is not available on PATH")
    real = absolute(Path(os.path.realpath(found)))
    try:
        metadata = real.stat()
    except FileNotFoundError as error:
        raise InstallError(f"{name}_unavailable", f"{name} realpath {real} does not exist") from error
    if not stat.S_ISREG(metadata.st_mode):
        raise InstallError(f"{name}_unavailable", f"{name} realpath {real} is not a regular file")
    if not os.access(real, os.X_OK):
        raise InstallError(f"{name}_unavailable", f"{name} realpath {real} is not executable")
    return real


def symlink_atomic(path: Path, target: Path) -> None:
    assert_no_symlink_components(path.parent)
    path.parent.mkdir(parents=True, exist_ok=True)
    fd, tmp_name = tempfile.mkstemp(prefix=f".{path.name}.", dir=path.parent)
    os.close(fd)
    try:
        os.unlink(tmp_name)
        os.symlink(str(target), tmp_name)
        os.replace(tmp_name, path)
        fsync_directory(path.parent)
    finally:
        try:
            os.unlink(tmp_name)
        except FileNotFoundError:
            pass


def ensure_apoc_link(bin_dir: Path) -> dict[str, Any]:
    bin_dir = absolute(bin_dir)
    link = bin_dir / "apoc"
    if link.exists() or link.is_symlink():
        return {"status": "apoc_link_preserved", "path": str(link)}
    assert_no_symlink_components(link)
    target = resolve_executable("apoc")
    if target == link:
        return {"status": "apoc_link_current", "path": str(link), "target": str(target)}
    symlink_atomic(link, target)
    return {"status": "apoc_link_created", "path": str(link), "target": str(target)}


def remove_created_apoc_link(link_result: dict[str, Any] | None) -> None:
    if not link_result or link_result.get("status") != "apoc_link_created":
        return
    path = link_result.get("path")
    if not isinstance(path, str):
        return
    link = Path(path)
    assert_no_symlink_components(link.parent)
    try:
        link.unlink()
        fsync_directory(link.parent)
    except FileNotFoundError:
        pass


def ensure_shell_path(bin_dir: Path) -> dict[str, Any]:
    bin_dir = absolute(bin_dir)
    shell, files = shell_path_files_for_bin_dir(bin_dir)
    if not files:
        home, _shell = current_user_home_shell()
        try:
            bin_dir.relative_to(home)
        except ValueError:
            return {"status": "shell_path_skipped", "reason": "bin_dir_outside_user_home", "files": []}
        return {"status": "shell_path_skipped", "reason": "unsupported_shell", "shell": shell, "files": []}

    changed: list[str] = []
    block = managed_path_block(bin_dir)
    for path in files:
        path = absolute(path)
        assert_no_symlink_components(path)
        snapshot = snapshot_file(path)
        if snapshot is None:
            current = ""
            mode = 0o600
        else:
            current = snapshot[0].decode("utf-8")
            mode = snapshot[1]
        without_block = remove_managed_path_blocks(current)
        next_text = block + without_block
        if next_text != current:
            write_text_atomic(path, next_text, mode)
            changed.append(str(path))
    return {"status": "shell_path_configured", "shell": shell, "bin_dir": str(bin_dir), "files": [str(path) for path in files], "changed_files": changed}


def target_lock_path(target: Path) -> Path:
    return target.with_name(f".{target.name}.lock")


@contextlib.contextmanager
def locked_target(bin_dir: Path, target: Path):
    assert_user_writable_dir(bin_dir)
    lock_path = target_lock_path(target)
    assert_no_symlink_components(lock_path)
    flags = os.O_RDWR | os.O_CREAT
    if hasattr(os, "O_NOFOLLOW"):
        flags |= os.O_NOFOLLOW
    try:
        fd = os.open(lock_path, flags, 0o600)
    except OSError as error:
        raise InstallError("lock_failed", f"could not open {lock_path}: {error}") from error
    with os.fdopen(fd, "a+b") as handle:
        fcntl.flock(handle.fileno(), fcntl.LOCK_EX)
        try:
            yield
        finally:
            fcntl.flock(handle.fileno(), fcntl.LOCK_UN)


def durable_backup_path(root: Path, digest: str) -> Path:
    return root / ".state" / "install" / "backups" / digest / INSTALL_NAME


def ensure_durable_backup(source: Path, root: Path, digest: str) -> Path:
    backup = durable_backup_path(root, digest)
    if backup.exists():
        assert_regular(backup, executable=True, code="backup_invalid")
        if sha256_file(backup) != digest:
            raise InstallError("backup_invalid", f"{backup} does not match {digest}")
        return backup
    copy_file_atomic(source, backup, 0o755)
    if sha256_file(backup) != digest:
        raise InstallError("backup_invalid", f"{backup} does not match {digest}")
    return backup


def read_existing_config(path: Path) -> dict[str, Any] | None:
    if not path.exists():
        return None
    assert_regular(path, executable=False, code="config_invalid")
    try:
        value = json.loads(path.read_text(encoding="utf-8"))
    except json.JSONDecodeError as error:
        raise InstallError("conflicting_existing_config", f"{path} is not valid JSON") from error
    if not isinstance(value, dict):
        raise InstallError("conflicting_existing_config", f"{path} must contain a JSON object")
    return value


def validate_root(root: Path) -> Path:
    root = absolute(root)
    assert_no_symlink_components(root)
    if not root.is_dir():
        raise InstallError("root_invalid", f"{root} is not a directory")
    fleet = root / "fleet.json"
    assert_no_symlink_components(fleet)
    assert_regular(fleet, executable=False, code="root_invalid")
    return root


def validate_config(path: Path, root: Path) -> Path:
    path = absolute(path)
    assert_no_symlink_components(path)
    existing = read_existing_config(path)
    if existing is not None and existing.get("root") != str(root):
        raise InstallError("conflicting_existing_config", f"{path} points to a different workenv root")
    return path


def validate_target(path: Path) -> str | None:
    if not path.exists():
        return None
    assert_no_symlink_components(path)
    assert_regular(path, executable=True, code="unsafe_overwrite")
    return sha256_file(path)


def validate_candidate(binary: Path, root: Path) -> None:
    environment = {
        name: value
        for name, value in os.environ.items()
        if name in {"PATH", "HOME", "TMPDIR", "TMP", "TEMP", "LANG", "LC_ALL"}
    }
    environment["WORKENV_ROOT"] = str(root)
    attempts: list[str] = []
    for args in (["--help"], ["--llms"]):
        try:
            completed = subprocess.run(
                [str(binary), *args],
                check=False,
                text=True,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                timeout=VALIDATION_TIMEOUT_SECONDS,
                env=environment,
            )
        except (OSError, subprocess.TimeoutExpired) as error:
            attempts.append(f"{' '.join(args)}: {error}")
            continue
        if completed.returncode == 0:
            return
        attempts.append(f"{' '.join(args)} exited {completed.returncode}")
    raise InstallError("candidate_invalid", "; ".join(attempts))


def install(
    binary: Path,
    expected_sha256: str,
    root: Path,
    bin_dir: Path,
    config: Path,
    receipt_fields: dict[str, Any] | None = None,
) -> dict[str, Any]:
    expected_sha256 = expected_sha256.strip().lower()
    if len(expected_sha256) != 64 or any(character not in "0123456789abcdef" for character in expected_sha256):
        raise InstallError("invalid_sha256", "sha256 must be 64 lowercase hex characters")

    root = validate_root(root)
    config = absolute(config)
    assert_no_symlink_components(config)
    bin_dir = absolute(bin_dir)
    target = bin_dir / INSTALL_NAME
    binary = absolute(binary)

    assert_no_symlink_components(binary)
    assert_regular(binary, executable=True, code="candidate_invalid")
    if binary == target:
        raise InstallError("unsafe_overwrite", "candidate and install target are the same path")

    actual_sha256 = sha256_file(binary)
    if actual_sha256 != expected_sha256:
        raise InstallError(
            "checksum_mismatch",
            f"candidate sha256 mismatch: expected {expected_sha256} actual {actual_sha256}",
        )

    validate_candidate(binary, root)

    receipt = {
        "schema": 1,
        "status": "installed",
        "installed_at": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
        "root": str(root),
        "installed_path": str(target),
        "config_path": str(config),
        "old_sha256": None,
        "new_sha256": None,
        "candidate_sha256": actual_sha256,
        "changed": None,
        "backup_path": None,
    }
    receipt_path = root / ".state" / "install" / "workenv.json"
    with locked_target(bin_dir, target):
        root = validate_root(root)
        config = validate_config(config, root)
        old_sha256 = validate_target(target)
        changed = old_sha256 != expected_sha256
        backup_path = ensure_durable_backup(target, root, old_sha256) if old_sha256 is not None and changed else None
        config_snapshot = snapshot_file(config)
        receipt_snapshot = snapshot_file(receipt_path)
        _, shell_files = shell_path_files_for_bin_dir(bin_dir)
        shell_snapshots = {path: snapshot_file(path) for path in shell_files}
        apoc_link: dict[str, Any] | None = None
        try:
            if changed:
                copy_executable_atomic(binary, target)
                new_sha256 = sha256_file(target)
                if new_sha256 != expected_sha256:
                    raise InstallError("install_verification_failed", "installed binary sha256 mismatch after replace")
                validate_candidate(target, root)
            else:
                new_sha256 = old_sha256

            apoc_link = ensure_apoc_link(bin_dir)
            shell_path = ensure_shell_path(bin_dir)
            receipt.update(
                {
                    "old_sha256": old_sha256,
                    "new_sha256": new_sha256,
                    "changed": changed,
                    "backup_path": str(backup_path) if backup_path is not None else None,
                    "apoc_link": apoc_link,
                    "shell_path": shell_path,
                }
            )
            if receipt_fields:
                receipt.update(receipt_fields)
            write_json_atomic(config, {"schema": 1, "root": str(root)})
            write_json_atomic(receipt_path, receipt)
        except Exception:
            if changed:
                if backup_path is not None:
                    with contextlib.suppress(Exception):
                        copy_executable_atomic(backup_path, target)
                else:
                    with contextlib.suppress(Exception):
                        remove_file(target)
            with contextlib.suppress(Exception):
                remove_created_apoc_link(apoc_link)
            for shell_file, shell_snapshot in shell_snapshots.items():
                with contextlib.suppress(Exception):
                    restore_snapshot(shell_file, shell_snapshot)
            with contextlib.suppress(Exception):
                restore_snapshot(config, config_snapshot)
            with contextlib.suppress(Exception):
                restore_snapshot(receipt_path, receipt_snapshot)
            raise

    return {**receipt, "ok": True, "receipt_path": str(receipt_path)}


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description="Install a verified native workenv binary.")
    parser.add_argument("--binary", required=True, type=Path)
    parser.add_argument("--sha256", required=True)
    parser.add_argument("--root", required=True, type=Path)
    parser.add_argument("--bin-dir", default=Path.home() / ".local" / "bin", type=Path)
    parser.add_argument("--config", default=Path.home() / ".config" / "workenv" / "config.json", type=Path)
    return parser


def main(argv: list[str] | None = None) -> int:
    args = build_parser().parse_args(argv)
    try:
        result = install(args.binary, args.sha256, args.root, args.bin_dir, args.config)
    except InstallError as error:
        print(f"{error.code}: {error.message}", file=sys.stderr)
        return 1
    print(display_json(result), end="")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
