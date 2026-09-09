import json
import fcntl
import os
import stat
import subprocess
import sys
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]

sys.path.insert(0, str(ROOT))

from remote.install_workenv import sha256_file


def write_candidate(path: Path, *, marker: str = "candidate") -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(
        "\n".join(
            [
                "#!/usr/bin/env sh",
                "case \"$1\" in",
                "  --help) echo 'workenv candidate help'; exit 0 ;;",
                "  --llms|--llms-full) echo '{\"commands\":[\"status\"]}'; exit 0 ;;",
                f"  --marker) echo '{marker}'; exit 0 ;;",
                "  *) exit 0 ;;",
                "esac",
            ]
        )
        + "\n",
        encoding="utf-8",
    )
    path.chmod(path.stat().st_mode | stat.S_IXUSR)


def write_project(root: Path) -> None:
    root.mkdir(parents=True, exist_ok=True)
    (root / "Cargo.toml").write_text(
        "[package]\nname = \"workenv\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        encoding="utf-8",
    )
    (root / "Cargo.lock").write_text("# lock\n", encoding="utf-8")
    (root / "fleet.json").write_text('{"workers":[]}\n', encoding="utf-8")
    (root / "src").mkdir()
    (root / "src" / "main.rs").write_text("fn main() {}\n", encoding="utf-8")


def write_fake_cargo(path: Path, *, marker: str = "built", fail: bool = False, mutate_source: bool = False) -> None:
    apoc = path.parent / "apoc"
    if not apoc.exists():
        apoc.write_text("#!/usr/bin/env sh\necho fake apoc\n", encoding="utf-8")
        apoc.chmod(apoc.stat().st_mode | stat.S_IXUSR)
    lines = [
        "#!/usr/bin/env sh",
        "set -eu",
        "printf '%s\\n' \"$@\" >> \"$FAKE_CARGO_LOG\"",
        "if [ \"$#\" -ne 3 ] || [ \"$1\" != build ] || [ \"$2\" != --release ] || [ \"$3\" != --locked ]; then",
        "  echo unexpected cargo args >&2",
        "  exit 64",
        "fi",
        "count=0",
        "if [ -f \"$FAKE_CARGO_COUNT\" ]; then count=$(cat \"$FAKE_CARGO_COUNT\"); fi",
        "count=$((count + 1))",
        "printf '%s\\n' \"$count\" > \"$FAKE_CARGO_COUNT\"",
    ]
    if mutate_source:
        lines.append("printf '%s\\n' '// changed during build' >> src/main.rs")
    if fail:
        lines.extend(["echo fake cargo failed >&2", "exit 42"])
    else:
        lines.extend(
            [
                "mkdir -p \"$CARGO_TARGET_DIR/release\"",
                f"""cat > "$CARGO_TARGET_DIR/release/workenv" <<'WORKENV_EOF'
#!/usr/bin/env sh
case "$1" in
  --help) echo 'workenv candidate help'; exit 0 ;;
  --llms|--llms-full) echo '{{"commands":["status"]}}'; exit 0 ;;
  --marker) echo '{marker}'; exit 0 ;;
  *) exit 0 ;;
esac
WORKENV_EOF""",
                "chmod 755 \"$CARGO_TARGET_DIR/release/workenv\"",
            ]
        )
    path.write_text("\n".join(lines) + "\n", encoding="utf-8")
    path.chmod(path.stat().st_mode | stat.S_IXUSR)


def run_build(tmp_path: Path, root: Path, fake_cargo: Path, *, bin_dir: Path | None = None, config: Path | None = None):
    bin_dir = tmp_path / "home" / ".local" / "bin" if bin_dir is None else bin_dir
    config = tmp_path / "home" / ".config" / "workenv" / "config.json" if config is None else config
    env = {
        **os.environ,
        "HOME": str(tmp_path / "home"),
        "PATH": f"{fake_cargo.parent}{os.pathsep}{os.environ['PATH']}",
        "FAKE_CARGO_LOG": str(tmp_path / "cargo.log"),
        "FAKE_CARGO_COUNT": str(tmp_path / "cargo-count"),
    }
    return subprocess.run(
        [
            sys.executable,
            str(ROOT / "remote" / "build_workenv.py"),
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
        env=env,
    )


def parse_success(completed: subprocess.CompletedProcess[str]) -> dict:
    assert completed.returncode == 0, completed.stderr
    return json.loads(completed.stdout)


def test_fake_cargo_builds_candidate_and_installs_receipt(tmp_path):
    root = tmp_path / "workenv"
    write_project(root)
    fake = tmp_path / "fake-bin" / "cargo"
    fake.parent.mkdir()
    write_fake_cargo(fake, marker="built")

    result = parse_success(run_build(tmp_path, root, fake))

    installed = tmp_path / "home" / ".local" / "bin" / "workenv"
    receipt = json.loads((root / ".state" / "install" / "workenv.json").read_text(encoding="utf-8"))
    assert result["status"] == "built"
    assert result["build_status"] == "built"
    assert result["source_digest"].startswith("sha256:")
    assert result["candidate_path"] == str(root / ".state" / "rust-target" / "release" / "workenv")
    assert receipt["source_digest"] == result["source_digest"]
    assert receipt["build_status"] == "built"
    assert subprocess.check_output([str(installed), "--marker"], text=True).strip() == "built"


def test_build_failure_preserves_existing_binary(tmp_path):
    root = tmp_path / "workenv"
    write_project(root)
    installed = tmp_path / "home" / ".local" / "bin" / "workenv"
    write_candidate(installed, marker="old")
    old_hash = sha256_file(installed)
    fake = tmp_path / "fake-bin" / "cargo"
    fake.parent.mkdir()
    write_fake_cargo(fake, fail=True)

    completed = run_build(tmp_path, root, fake)

    assert completed.returncode != 0
    assert "build_failed" in completed.stderr
    assert sha256_file(installed) == old_hash
    assert subprocess.check_output([str(installed), "--marker"], text=True).strip() == "old"


def test_repeat_with_matching_receipt_skips_compile(tmp_path):
    root = tmp_path / "workenv"
    write_project(root)
    fake = tmp_path / "fake-bin" / "cargo"
    fake.parent.mkdir()
    write_fake_cargo(fake, marker="built")

    first = parse_success(run_build(tmp_path, root, fake))
    second = parse_success(run_build(tmp_path, root, fake))

    assert first["build_status"] == "built"
    assert second["build_status"] == "skipped"
    assert second["installed_path"] == str(tmp_path / "home" / ".local" / "bin" / "workenv")
    assert (tmp_path / "cargo-count").read_text(encoding="utf-8").strip() == "1"


def test_matching_receipt_skip_repairs_config_and_missing_apoc_link_without_compile(tmp_path):
    root = tmp_path / "workenv"
    write_project(root)
    fake = tmp_path / "fake-bin" / "cargo"
    fake.parent.mkdir()
    write_fake_cargo(fake, marker="built")

    first = parse_success(run_build(tmp_path, root, fake))
    apoc = tmp_path / "home" / ".local" / "bin" / "apoc"
    config = tmp_path / "home" / ".config" / "workenv" / "config.json"
    apoc.unlink()
    config.unlink()

    second = parse_success(run_build(tmp_path, root, fake))

    assert first["build_status"] == "built"
    assert second["build_status"] == "skipped"
    assert second["apoc_link"]["status"] == "apoc_link_created"
    assert apoc.is_symlink()
    assert config.is_file()
    assert json.loads(config.read_text(encoding="utf-8")) == {"root": str(root), "schema": 1}
    assert (tmp_path / "cargo-count").read_text(encoding="utf-8").strip() == "1"


def test_matching_receipt_skip_honors_different_bin_dir_and_config_without_compile(tmp_path):
    root = tmp_path / "workenv"
    write_project(root)
    fake = tmp_path / "fake-bin" / "cargo"
    fake.parent.mkdir()
    write_fake_cargo(fake, marker="built")

    parse_success(run_build(tmp_path, root, fake))
    other_bin = tmp_path / "other-home" / ".local" / "bin"
    other_config = tmp_path / "other-home" / ".config" / "workenv" / "config.json"
    second = parse_success(run_build(tmp_path, root, fake, bin_dir=other_bin, config=other_config))

    assert second["build_status"] == "skipped"
    assert second["installed_path"] == str(other_bin / "workenv")
    assert second["config_path"] == str(other_config)
    assert (other_bin / "workenv").is_file()
    assert (other_bin / "apoc").is_symlink()
    assert json.loads(other_config.read_text(encoding="utf-8")) == {"root": str(root), "schema": 1}
    receipt = json.loads((root / ".state" / "install" / "workenv.json").read_text(encoding="utf-8"))
    assert receipt["installed_path"] == str(other_bin / "workenv")
    assert receipt["config_path"] == str(other_config)
    assert (tmp_path / "cargo-count").read_text(encoding="utf-8").strip() == "1"


def test_source_change_forces_build(tmp_path):
    root = tmp_path / "workenv"
    write_project(root)
    fake = tmp_path / "fake-bin" / "cargo"
    fake.parent.mkdir()
    write_fake_cargo(fake, marker="built")

    first = parse_success(run_build(tmp_path, root, fake))
    (root / "src" / "main.rs").write_text("fn main() { println!(\"changed\"); }\n", encoding="utf-8")
    second = parse_success(run_build(tmp_path, root, fake))

    assert first["source_digest"] != second["source_digest"]
    assert second["build_status"] == "built"
    assert (tmp_path / "cargo-count").read_text(encoding="utf-8").strip() == "2"


def test_source_change_during_build_fails_without_install(tmp_path):
    root = tmp_path / "workenv"
    write_project(root)
    installed = tmp_path / "home" / ".local" / "bin" / "workenv"
    write_candidate(installed, marker="old")
    old_hash = sha256_file(installed)
    fake = tmp_path / "fake-bin" / "cargo"
    fake.parent.mkdir()
    write_fake_cargo(fake, marker="new", mutate_source=True)

    completed = run_build(tmp_path, root, fake)

    assert completed.returncode != 0
    assert "source_changed" in completed.stderr
    assert sha256_file(installed) == old_hash
    assert subprocess.check_output([str(installed), "--marker"], text=True).strip() == "old"


def test_source_symlink_is_rejected_before_build(tmp_path):
    root = tmp_path / "workenv"
    write_project(root)
    (root / "src" / "linked.rs").symlink_to(root / "src" / "main.rs")
    fake = tmp_path / "fake-bin" / "cargo"
    fake.parent.mkdir()
    write_fake_cargo(fake)

    completed = run_build(tmp_path, root, fake)

    assert completed.returncode != 0
    assert "source_invalid" in completed.stderr
    assert not (tmp_path / "cargo-count").exists()


def test_build_waits_for_root_build_lock_before_running_cargo(tmp_path):
    root = tmp_path / "workenv"
    write_project(root)
    (root / ".state").mkdir()
    lock_path = root / ".state" / "cli-build.lock"
    fake = tmp_path / "fake-bin" / "cargo"
    fake.parent.mkdir()
    write_fake_cargo(fake, marker="built")
    bin_dir = tmp_path / "home" / ".local" / "bin"
    config = tmp_path / "home" / ".config" / "workenv" / "config.json"
    lock_fd = os.open(lock_path, os.O_RDWR | os.O_CREAT, 0o600)
    try:
        fcntl.flock(lock_fd, fcntl.LOCK_EX)
        process = subprocess.Popen(
            [
                sys.executable,
                str(ROOT / "remote" / "build_workenv.py"),
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
            env={
                **os.environ,
                "HOME": str(tmp_path / "home"),
                "PATH": f"{fake.parent}{os.pathsep}{os.environ['PATH']}",
                "FAKE_CARGO_LOG": str(tmp_path / "cargo.log"),
                "FAKE_CARGO_COUNT": str(tmp_path / "cargo-count"),
            },
        )
        time.sleep(0.2)
        assert process.poll() is None
        assert not (tmp_path / "cargo-count").exists()
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
    assert json.loads(stdout)["build_status"] == "built"
