import hashlib
import fcntl
import json
import os
import stat
import shutil
import subprocess
import sys
import time
from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parents[1]

sys.path.insert(0, str(ROOT))

from remote import install_workenv




@pytest.fixture(autouse=True)
def fake_apoc_on_path(tmp_path, monkeypatch):
    tool_dir = tmp_path / "tools"
    tool_dir.mkdir()
    apoc = tool_dir / "apoc"
    apoc.write_text("#!/usr/bin/env sh\necho fake apoc\n", encoding="utf-8")
    apoc.chmod(apoc.stat().st_mode | stat.S_IXUSR)
    monkeypatch.setenv("PATH", f"{tool_dir}{os.pathsep}{os.environ.get('PATH', '')}")
    return apoc


def sha256(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def write_candidate(path: Path, *, marker: str = "candidate", valid: bool = True) -> str:
    body = [
        "#!/usr/bin/env sh",
        "case \"$1\" in",
    ]
    if valid:
        body.extend(
            [
                "  --help) echo 'workenv candidate help'; exit 0 ;;",
                "  --llms|--llms-full) echo '{\"commands\":[\"status\"]}'; exit 0 ;;",
            ]
        )
    else:
        body.extend(
            [
                "  --help) echo 'broken candidate' >&2; exit 7 ;;",
                "  --llms|--llms-full) echo 'broken candidate' >&2; exit 7 ;;",
            ]
        )
    body.extend(
        [
            f"  --marker) echo '{marker}'; exit 0 ;;",
            "  *) exit 0 ;;",
            "esac",
        ]
    )
    path.write_text("\n".join(body) + "\n", encoding="utf-8")
    path.chmod(path.stat().st_mode | stat.S_IXUSR)
    return sha256(path)


def run_install(
    tmp_path: Path,
    *,
    binary: Path,
    digest: str,
    root: Path | None = None,
    bin_dir: Path | None = None,
    config: Path | None = None,
    create_fleet: bool = True,
):
    root = tmp_path / "workenv" if root is None else root
    bin_dir = tmp_path / "home" / ".local" / "bin" if bin_dir is None else bin_dir
    config = tmp_path / "home" / ".config" / "workenv" / "config.json" if config is None else config
    root.mkdir(parents=True, exist_ok=True)
    if create_fleet:
        (root / "fleet.json").write_text('{"workers":[]}\n', encoding="utf-8")
    return subprocess.run(
        [
            sys.executable,
            str(ROOT / "remote" / "install_workenv.py"),
            "--binary",
            str(binary),
            "--sha256",
            digest,
            "--root",
            str(root),
            "--bin-dir",
            str(bin_dir),
            "--config",
            str(config),
        ],
        check=False,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        env={**os.environ, "HOME": str(tmp_path / "home")},
    )


def parse_success(completed: subprocess.CompletedProcess[str]) -> dict:
    assert completed.returncode == 0, completed.stderr
    return json.loads(completed.stdout)


def test_install_verifies_checksum_and_preserves_existing_binary(tmp_path):
    installed = tmp_path / "home" / ".local" / "bin" / "workenv"
    installed.parent.mkdir(parents=True)
    old_hash = write_candidate(installed, marker="old")
    candidate = tmp_path / "build" / "workenv"
    candidate.parent.mkdir()
    write_candidate(candidate, marker="new")

    completed = run_install(tmp_path, binary=candidate, digest="0" * 64)

    assert completed.returncode != 0
    assert "checksum_mismatch" in completed.stderr
    assert sha256(installed) == old_hash
    assert subprocess.check_output([str(installed), "--marker"], text=True).strip() == "old"


def test_repeat_install_is_idempotent_and_persists_config_and_receipt(tmp_path):
    candidate = tmp_path / "build" / "workenv"
    candidate.parent.mkdir()
    digest = write_candidate(candidate, marker="v1")

    first = parse_success(run_install(tmp_path, binary=candidate, digest=digest))
    second = parse_success(run_install(tmp_path, binary=candidate, digest=digest))

    installed = Path(first["installed_path"])
    config = Path(first["config_path"])
    receipt = Path(second["receipt_path"])
    assert installed == tmp_path / "home" / ".local" / "bin" / "workenv"
    assert json.loads(config.read_text(encoding="utf-8")) == {"root": str(tmp_path / "workenv"), "schema": 1}
    assert first["changed"] is True
    assert second["changed"] is False
    assert second["old_sha256"] == digest
    assert second["new_sha256"] == digest
    assert json.loads(receipt.read_text(encoding="utf-8"))["new_sha256"] == digest
    assert subprocess.check_output([str(installed), "--marker"], text=True).strip() == "v1"


def test_invalid_native_candidate_preserves_existing_binary(tmp_path):
    installed = tmp_path / "home" / ".local" / "bin" / "workenv"
    installed.parent.mkdir(parents=True)
    old_hash = write_candidate(installed, marker="old")
    candidate = tmp_path / "build" / "workenv"
    candidate.parent.mkdir()
    digest = write_candidate(candidate, marker="bad", valid=False)

    completed = run_install(tmp_path, binary=candidate, digest=digest)

    assert completed.returncode != 0
    assert "candidate_invalid" in completed.stderr
    assert sha256(installed) == old_hash
    assert subprocess.check_output([str(installed), "--marker"], text=True).strip() == "old"


def test_write_failure_restores_previous_binary_and_config(tmp_path, monkeypatch):
    root = tmp_path / "workenv"
    root.mkdir()
    (root / "fleet.json").write_text('{"workers":[]}\n', encoding="utf-8")
    bin_dir = tmp_path / "home" / ".local" / "bin"
    bin_dir.mkdir(parents=True)
    config = tmp_path / "home" / ".config" / "workenv" / "config.json"
    config.parent.mkdir(parents=True)
    config.write_text(json.dumps({"schema": 1, "root": str(root), "note": "keep"}) + "\n", encoding="utf-8")
    old_config = config.read_bytes()
    installed = bin_dir / "workenv"
    old_hash = write_candidate(installed, marker="old")
    candidate = tmp_path / "build" / "workenv"
    candidate.parent.mkdir()
    digest = write_candidate(candidate, marker="new")
    real_write = install_workenv.write_json_atomic

    def failing_write(path, payload):
        real_write(path, payload)
        if path.name == "workenv.json":
            raise OSError("injected receipt write failure")

    monkeypatch.setattr(install_workenv, "write_json_atomic", failing_write)

    with pytest.raises(OSError, match="injected receipt write failure"):
        install_workenv.install(candidate, digest, root, bin_dir, config)

    assert sha256(installed) == old_hash
    assert config.read_bytes() == old_config
    assert subprocess.check_output([str(installed), "--marker"], text=True).strip() == "old"
    assert not (root / ".state" / "install" / "workenv.json").exists()
    backup = root / ".state" / "install" / "backups" / old_hash / "workenv"
    assert backup.is_file()
    assert sha256(backup) == old_hash


def test_success_keeps_previous_binary_backup_under_root_state(tmp_path):
    installed = tmp_path / "home" / ".local" / "bin" / "workenv"
    installed.parent.mkdir(parents=True)
    old_hash = write_candidate(installed, marker="old")
    candidate = tmp_path / "build" / "workenv"
    candidate.parent.mkdir()
    digest = write_candidate(candidate, marker="new")

    result = parse_success(run_install(tmp_path, binary=candidate, digest=digest))

    backup = Path(result["backup_path"])
    assert backup == tmp_path / "workenv" / ".state" / "install" / "backups" / old_hash / "workenv"
    assert sha256(backup) == old_hash
    assert subprocess.check_output([str(installed), "--marker"], text=True).strip() == "new"


def test_installer_waits_for_target_lock_before_replacing_binary(tmp_path):
    candidate = tmp_path / "build" / "workenv"
    candidate.parent.mkdir()
    digest = write_candidate(candidate, marker="new")
    bin_dir = tmp_path / "home" / ".local" / "bin"
    bin_dir.mkdir(parents=True)
    lock_path = bin_dir / ".workenv.lock"
    installed = bin_dir / "workenv"
    root = tmp_path / "workenv"
    root.mkdir()
    (root / "fleet.json").write_text('{"workers":[]}\n', encoding="utf-8")
    config = tmp_path / "home" / ".config" / "workenv" / "config.json"
    lock_fd = os.open(lock_path, os.O_RDWR | os.O_CREAT, 0o600)
    try:
        fcntl.flock(lock_fd, fcntl.LOCK_EX)
        process = subprocess.Popen(
            [
                sys.executable,
                str(ROOT / "remote" / "install_workenv.py"),
                "--binary",
                str(candidate),
                "--sha256",
                digest,
                "--root",
                str(root),
                "--bin-dir",
                str(bin_dir),
                "--config",
                str(config),
            ],
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            env={**os.environ, "HOME": str(tmp_path / "home")},
        )
        time.sleep(0.2)
        assert process.poll() is None
        assert not installed.exists()
        fcntl.flock(lock_fd, fcntl.LOCK_UN)
        stdout, stderr = process.communicate(timeout=10)
    finally:
        try:
            process.kill()
        except UnboundLocalError:
            pass
        except ProcessLookupError:
            pass
        os.close(lock_fd)

    assert process.returncode == 0, stderr
    assert json.loads(stdout)["new_sha256"] == digest
    assert subprocess.check_output([str(installed), "--marker"], text=True).strip() == "new"


def test_existing_config_for_another_root_is_rejected(tmp_path):
    candidate = tmp_path / "build" / "workenv"
    candidate.parent.mkdir()
    digest = write_candidate(candidate)
    config = tmp_path / "home" / ".config" / "workenv" / "config.json"
    config.parent.mkdir(parents=True)
    config.write_text(json.dumps({"schema": 1, "root": str(tmp_path / "other")}) + "\n", encoding="utf-8")

    completed = run_install(tmp_path, binary=candidate, digest=digest, config=config)

    assert completed.returncode != 0
    assert "conflicting_existing_config" in completed.stderr
    assert not (tmp_path / "home" / ".local" / "bin" / "workenv").exists()


def test_root_without_fleet_is_rejected_before_install(tmp_path):
    candidate = tmp_path / "build" / "workenv"
    candidate.parent.mkdir()
    digest = write_candidate(candidate)
    root = tmp_path / "workenv"
    root.mkdir()

    completed = run_install(tmp_path, binary=candidate, digest=digest, root=root, create_fleet=False)

    assert completed.returncode != 0
    assert "root_invalid" in completed.stderr
    assert not (tmp_path / "home" / ".local" / "bin" / "workenv").exists()


def test_existing_non_executable_target_is_rejected_as_unsafe(tmp_path):
    installed = tmp_path / "home" / ".local" / "bin" / "workenv"
    installed.parent.mkdir(parents=True)
    installed.write_text("not executable\n", encoding="utf-8")
    candidate = tmp_path / "build" / "workenv"
    candidate.parent.mkdir()
    digest = write_candidate(candidate)

    completed = run_install(tmp_path, binary=candidate, digest=digest)

    assert completed.returncode != 0
    assert "unsafe_overwrite" in completed.stderr
    assert installed.read_text(encoding="utf-8") == "not executable\n"


def test_symlink_ancestor_is_rejected_before_install(tmp_path):
    candidate = tmp_path / "build" / "workenv"
    candidate.parent.mkdir()
    digest = write_candidate(candidate)
    real_home = tmp_path / "real-home"
    real_home.mkdir()
    linked_home = tmp_path / "linked-home"
    linked_home.symlink_to(real_home)

    completed = run_install(
        tmp_path,
        binary=candidate,
        digest=digest,
        bin_dir=linked_home / ".local" / "bin",
        config=tmp_path / "home" / ".config" / "workenv" / "config.json",
    )

    assert completed.returncode != 0
    assert "symlink_ancestor" in completed.stderr
    assert not (real_home / ".local" / "bin" / "workenv").exists()


def make_root_and_candidate(tmp_path):
    root = tmp_path / "workenv"
    root.mkdir()
    (root / "fleet.json").write_text('{"workers":[]}\n', encoding="utf-8")
    candidate = tmp_path / "build" / "workenv"
    candidate.parent.mkdir()
    digest = write_candidate(candidate)
    return root, candidate, digest


def test_bash_shell_path_block_is_prepended_before_early_return_and_login_file(tmp_path, monkeypatch):
    home = tmp_path / "home"
    home.mkdir()
    bashrc = home / ".bashrc"
    bashrc.write_text("case $- in *i*) ;; *) return;; esac\necho after\n", encoding="utf-8")
    bash_profile = home / ".bash_profile"
    bash_profile.write_text("echo login\n", encoding="utf-8")
    root, candidate, digest = make_root_and_candidate(tmp_path)
    bin_dir = home / ".local" / "bin"
    config = home / ".config" / "workenv" / "config.json"
    monkeypatch.setattr(install_workenv, "current_user_home_shell", lambda: (home, "/bin/bash"))

    result = install_workenv.install(candidate, digest, root, bin_dir, config)

    assert result["shell_path"]["status"] == "shell_path_configured"
    for path in [bashrc, bash_profile]:
        text = path.read_text(encoding="utf-8")
        assert text.startswith(install_workenv.PATH_BLOCK_BEGIN)
        assert f"export PATH={bin_dir}:$PATH" in text
    assert bashrc.read_text(encoding="utf-8").index(install_workenv.PATH_BLOCK_BEGIN) < bashrc.read_text(encoding="utf-8").index("return")
    assert bash_profile.read_text(encoding="utf-8").endswith("echo login\n")


def test_zsh_shell_path_block_uses_zshenv(tmp_path, monkeypatch):
    home = tmp_path / "home"
    home.mkdir()
    zshenv = home / ".zshenv"
    zshenv.write_text("export EXISTING=1\n", encoding="utf-8")
    root, candidate, digest = make_root_and_candidate(tmp_path)
    bin_dir = home / ".local" / "bin"
    config = home / ".config" / "workenv" / "config.json"
    monkeypatch.setattr(install_workenv, "current_user_home_shell", lambda: (home, "/bin/zsh"))

    result = install_workenv.install(candidate, digest, root, bin_dir, config)

    assert result["shell_path"]["files"] == [str(zshenv)]
    text = zshenv.read_text(encoding="utf-8")
    assert text.startswith(install_workenv.PATH_BLOCK_BEGIN)
    assert "export EXISTING=1\n" in text


def test_shell_path_update_rejects_symlink_startup_file_and_rolls_back(tmp_path, monkeypatch):
    home = tmp_path / "home"
    home.mkdir()
    target = tmp_path / "real-bashrc"
    target.write_text("echo real\n", encoding="utf-8")
    (home / ".bashrc").symlink_to(target)
    root, candidate, digest = make_root_and_candidate(tmp_path)
    bin_dir = home / ".local" / "bin"
    config = home / ".config" / "workenv" / "config.json"
    monkeypatch.setattr(install_workenv, "current_user_home_shell", lambda: (home, "/bin/bash"))

    with pytest.raises(install_workenv.InstallError) as error:
        install_workenv.install(candidate, digest, root, bin_dir, config)

    assert error.value.code == "symlink_ancestor"
    assert not (bin_dir / "workenv").exists()
    assert target.read_text(encoding="utf-8") == "echo real\n"
    assert not config.exists()


def test_shell_path_update_is_rolled_back_when_receipt_write_fails(tmp_path, monkeypatch):
    home = tmp_path / "home"
    home.mkdir()
    bashrc = home / ".bashrc"
    bashrc.write_text("echo before\n", encoding="utf-8")
    root, candidate, digest = make_root_and_candidate(tmp_path)
    bin_dir = home / ".local" / "bin"
    config = home / ".config" / "workenv" / "config.json"
    monkeypatch.setattr(install_workenv, "current_user_home_shell", lambda: (home, "/bin/bash"))
    real_write = install_workenv.write_json_atomic

    def failing_write(path, payload):
        real_write(path, payload)
        if path.name == "workenv.json":
            raise OSError("injected receipt write failure")

    monkeypatch.setattr(install_workenv, "write_json_atomic", failing_write)

    with pytest.raises(OSError, match="injected receipt write failure"):
        install_workenv.install(candidate, digest, root, bin_dir, config)

    assert bashrc.read_text(encoding="utf-8") == "echo before\n"
    assert not (home / ".bash_profile").exists()


def test_missing_apoc_in_bin_dir_is_linked_from_validated_path(tmp_path, fake_apoc_on_path):
    candidate = tmp_path / "build" / "workenv"
    candidate.parent.mkdir()
    digest = write_candidate(candidate)

    result = parse_success(run_install(tmp_path, binary=candidate, digest=digest))

    apoc = tmp_path / "home" / ".local" / "bin" / "apoc"
    assert apoc.is_symlink()
    assert Path(os.readlink(apoc)) == fake_apoc_on_path
    assert result["apoc_link"] == {
        "path": str(apoc),
        "status": "apoc_link_created",
        "target": str(fake_apoc_on_path),
    }
    receipt = json.loads(Path(result["receipt_path"]).read_text(encoding="utf-8"))
    assert receipt["apoc_link"]["status"] == "apoc_link_created"


def test_existing_apoc_in_bin_dir_is_preserved(tmp_path, fake_apoc_on_path):
    bin_dir = tmp_path / "home" / ".local" / "bin"
    bin_dir.mkdir(parents=True)
    apoc = bin_dir / "apoc"
    apoc.write_text("#!/usr/bin/env sh\necho user apoc\n", encoding="utf-8")
    apoc.chmod(apoc.stat().st_mode | stat.S_IXUSR)
    candidate = tmp_path / "build" / "workenv"
    candidate.parent.mkdir()
    digest = write_candidate(candidate)

    result = parse_success(run_install(tmp_path, binary=candidate, digest=digest, bin_dir=bin_dir))

    assert not apoc.is_symlink()
    assert apoc.read_text(encoding="utf-8") == "#!/usr/bin/env sh\necho user apoc\n"
    assert result["apoc_link"] == {"path": str(apoc), "status": "apoc_link_preserved"}


def test_missing_apoc_on_path_rolls_back_install(tmp_path, monkeypatch):
    root, candidate, digest = make_root_and_candidate(tmp_path)
    home = tmp_path / "home"
    bin_dir = home / ".local" / "bin"
    config = home / ".config" / "workenv" / "config.json"
    monkeypatch.setenv("PATH", os.environ.get("PATH", ""))
    monkeypatch.setattr(install_workenv.shutil, "which", lambda name: None if name == "apoc" else shutil.which(name))

    with pytest.raises(install_workenv.InstallError) as error:
        install_workenv.install(candidate, digest, root, bin_dir, config)

    assert error.value.code == "apoc_unavailable"
    assert not (bin_dir / "workenv").exists()
    assert not (bin_dir / "apoc").exists()
    assert not config.exists()


def test_created_apoc_link_is_rolled_back_when_receipt_write_fails(tmp_path, monkeypatch, fake_apoc_on_path):
    root, candidate, digest = make_root_and_candidate(tmp_path)
    home = tmp_path / "home"
    bin_dir = home / ".local" / "bin"
    config = home / ".config" / "workenv" / "config.json"
    real_write = install_workenv.write_json_atomic

    def failing_write(path, payload):
        real_write(path, payload)
        if path.name == "workenv.json":
            raise OSError("injected receipt write failure")

    monkeypatch.setattr(install_workenv, "write_json_atomic", failing_write)

    with pytest.raises(OSError, match="injected receipt write failure"):
        install_workenv.install(candidate, digest, root, bin_dir, config)

    assert not (bin_dir / "workenv").exists()
    assert not (bin_dir / "apoc").exists()
    assert not config.exists()
