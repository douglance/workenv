import hashlib
import io
import json
import os
import shutil
import subprocess
import sys
import tarfile
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
BOOTSTRAP = ROOT / "bootstrap" / "bootstrap.sh"
BUILD_APOC = ROOT / "bootstrap" / "build-apoc.sh"
HERDR_BOOT_HELPER = ROOT / "bootstrap" / "workenv-herdr-bootstrap.sh"
MANIFEST = ROOT / "tools.json"
APOC_REVISION = "913b1e9a855fdf771a0688eab26b3ad03f271465"


def write_source_archive(path, *, revision=APOC_REVISION, members=None):
    members = members or {"apoc-source/Cargo.toml": b'[package]\nname = "apoc"\n'}
    with tarfile.open(path, "w:gz", format=tarfile.PAX_FORMAT, pax_headers={"comment": revision}) as archive:
        root = tarfile.TarInfo("apoc-source/")
        root.type = tarfile.DIRTYPE
        root.mode = 0o755
        archive.addfile(root)
        for name, data in members.items():
            info = tarfile.TarInfo(name)
            info.size = len(data)
            info.mode = 0o644
            archive.addfile(info, io.BytesIO(data))
    return hashlib.sha256(path.read_bytes()).hexdigest()


def fake_health_path(tmp_path):
    fake_bin = tmp_path / "bin"
    fake_bin.mkdir()
    (fake_bin / "python3").symlink_to(Path(sys.executable))
    for utility in ["dirname", "uname"]:
        utility_path = shutil.which(utility)
        assert utility_path is not None
        (fake_bin / utility).symlink_to(Path(utility_path))
    return fake_bin


def test_manifest_keeps_bootstrap_prerequisite_pins():
    manifest = json.loads(MANIFEST.read_text())

    assert manifest["target"] == {
        "os_id": "ubuntu",
        "version_id": "24.04",
        "architecture": "x86_64",
        "sudo": "passwordless",
    }
    assert manifest["nix"]["version"] == "2.35.2"
    assert manifest["nix"]["install_url"] == "https://releases.nixos.org/nix/nix-2.35.2/install"
    assert len(manifest["nix"]["install_sha256"]) == 64
    assert manifest["devenv"]["version"] == "2.3"
    assert manifest["devenv"]["source_revision"] == "e0781f7bee573eefcab4a7d2788fd9b455560ca2"
    assert manifest["devenv"]["flake_ref"] == "github:cachix/devenv/e0781f7bee573eefcab4a7d2788fd9b455560ca2"


def test_manifest_preserves_apoc_builder_contract():
    manifest = json.loads(MANIFEST.read_text())

    assert manifest["apoc_build"]["source_revision"] == APOC_REVISION
    assert manifest["apoc_build"]["package_version"] == "0.6.0"
    assert manifest["apoc_build"]["rust_toolchain"] == "1.95.0"
    assert len(manifest["apoc_build"]["rustup_init_sha256"]) == 64
    assert manifest["local_artifacts"]["apoc"]["source_revision"] == APOC_REVISION
    assert manifest["local_artifacts"]["apoc"]["archive_sha256"] == "9ecdb3115b7393b490dc3d09cc15a88cd24f90b3b278075cfe48ce54698ccfc4"
    assert manifest["local_artifacts"]["apoc"]["binary_sha256"] == "0cc82b04e5ba62b1eebc3c03e1d2eac72c8e04561ff84fb817869fcf04fe957c"


def test_bootstrap_script_has_valid_bash_syntax():
    subprocess.run(["bash", "-n", str(BOOTSTRAP)], check=True)
    subprocess.run(["bash", "-n", str(BUILD_APOC)], check=True)
    subprocess.run(["bash", "-n", str(HERDR_BOOT_HELPER)], check=True)


def test_health_only_reports_bootstrap_prerequisites_not_shared_tools(tmp_path):
    env = os.environ.copy()
    env["PATH"] = str(fake_health_path(tmp_path))
    result = subprocess.run(
        [
            "/bin/bash",
            str(BOOTSTRAP),
            "--health-only",
            "--json",
            "--prefix",
            str(tmp_path / "prefix"),
            "--link-dir",
            str(tmp_path / "links"),
        ],
        check=True,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        env=env,
    )

    health = json.loads(result.stdout)
    assert health["shared_tools"] == "provided_by_devenv"
    assert [item["name"] for item in health["prerequisites"]] == ["nix", "devenv"]
    assert set(health["missing_prerequisites"]) == {"nix", "devenv", "tailscaled.service"}
    assert health["tailscale_service"]["available"] is False
    assert health["credentials"] == "preserved"
    assert health["tailscale_enrollment"] == "not_configured_by_bootstrap"
    assert "tools" not in health
    assert "local_artifacts" not in health


def test_dry_run_does_not_require_download_outputs(tmp_path):
    result = subprocess.run(
        [
            "/bin/bash",
            str(BOOTSTRAP),
            "--dry-run",
            "--json",
            "--prefix",
            str(tmp_path / "prefix"),
            "--link-dir",
            str(tmp_path / "links"),
        ],
        check=True,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )

    health = json.loads(result.stdout)
    assert health["shared_tools"] == "provided_by_devenv"
    assert health["credentials"] == "preserved"
    assert health["tailscale_enrollment"] == "not_configured_by_bootstrap"


def test_bootstrap_rejects_removed_tools_dir_option(tmp_path):
    result = subprocess.run(
        ["/bin/bash", str(BOOTSTRAP), "--tools-dir", str(tmp_path)],
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )

    assert result.returncode == 2
    assert "unknown argument: --tools-dir" in result.stderr


def test_bootstrap_delegates_shared_tools_to_devenv():
    script = BOOTSTRAP.read_text()

    assert "install_herdr" not in script
    assert "install_devsql" not in script
    assert "install_npm_native" not in script
    assert "install_local_artifact" not in script
    assert "integrity_verify" not in script
    assert "sha1_verify" not in script
    assert "ai_cli" not in script
    assert "local_artifacts.$tool" not in script
    assert "--herdr-idle-checked" not in script
    assert "--tools-dir" not in script


def test_bootstrap_verifies_nix_installer_checksum_before_install():
    script = BOOTSTRAP.read_text()

    assert 'sha="$(manifest_value nix.install_sha256)"' in script
    assert 'download_to "$url" "$tmp/install-nix"' in script
    assert 'sha256_verify "$tmp/install-nix" "$sha"' in script
    assert 'run_user sh "$tmp/install-nix" --daemon --yes --no-channel-add' in script


def test_bootstrap_does_not_embed_runtime_identity_or_secret_material():
    combined = BOOTSTRAP.read_text() + "\n" + BUILD_APOC.read_text() + "\n" + MANIFEST.read_text()
    forbidden = [
        "pgrep",
        "tailscale up",
        "authkey",
        "TAILSCALE_AUTH",
        ".ssh/id_",
        "rsync $HOME",
        "tar -C $HOME",
    ]
    for needle in forbidden:
        assert needle not in combined


def test_bootstrap_script_has_herdr_boot_service_contract():
    script = BOOTSTRAP.read_text()

    assert 'BOOT_SERVICE_NAME="workenv-herdr-bootstrap.service"' in script
    assert "install_workenv_herdr_boot_service" in script
    assert 'helper_src="$SCRIPT_DIR/workenv-herdr-bootstrap.sh"' in script
    assert 'helper_dest="$PREFIX/bin/workenv-herdr-bootstrap"' in script
    assert "Type=oneshot" in script
    assert "ExecStart=$helper_dest" in script
    assert 'systemctl enable "$BOOT_SERVICE_NAME"' in script
    assert 'systemctl enable --now "$BOOT_SERVICE_NAME"' not in script


def test_bootstrap_readme_reports_simplified_boundary():
    readme = (ROOT / "bootstrap" / "README.md").read_text()

    assert "shared repository `devenv`" in readme
    assert "does not install Herdr, APoC, DevSQL, Codex, Claude, Grok, Pi, Nib, or ssh-clipboard directly" in readme
    assert "remote/tool_health.py" in readme
    assert "workenv-herdr-bootstrap.service" in readme
    assert "does not start Herdr during install" in readme
    assert "--tools-dir" not in readme


def test_build_apoc_plan_reports_pinned_contract(tmp_path):
    result = subprocess.run(
        [
            "/bin/bash",
            str(BUILD_APOC),
            "--plan",
            "--json",
            "--prefix",
            str(tmp_path / "prefix"),
            "--jobs",
            "2",
        ],
        check=True,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )

    plan = json.loads(result.stdout)
    assert plan["status"] == "plan"
    assert plan["source_revision"] == APOC_REVISION
    assert plan["source_archive_prefix"] == "apoc-source"
    assert plan["rust_toolchain"] == "1.95.0"
    assert plan["package_version"] == "0.6.0"
    assert plan["build_command"] == "cargo build --release --locked --bin apoc --jobs 2"


def test_build_apoc_never_uses_dirty_local_source():
    script = BUILD_APOC.read_text()

    assert "/Users/operator/Developer/src/apoc" not in script
    assert "cargo build --release --locked --bin apoc --jobs" in script
    assert "--install-prereqs" in script
    assert "rerun with --install-prereqs after no other apt mutation is active" in script


def test_build_apoc_version_probe_includes_required_purpose():
    build_script = BUILD_APOC.read_text()

    assert 'version --purpose "$probe_purpose" --format json' in build_script
    assert 'probe_purpose="Verify newly built worker APoC."' in build_script
    assert "printf '%s\\n' \"$probe_output\" >&2" in build_script


def test_build_apoc_validates_git_archive_source(tmp_path):
    source_archive = tmp_path / "apoc.tar.gz"
    source_sha = write_source_archive(source_archive)

    result = subprocess.run(
        [
            "/bin/bash",
            str(BUILD_APOC),
            "--validate-source-only",
            "--json",
            "--source-archive",
            str(source_archive),
            "--source-sha256",
            source_sha,
            "--build-root",
            str(tmp_path / "build"),
        ],
        check=True,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )

    report = json.loads(result.stdout)
    assert report["status"] == "source_valid"
    assert report["source_revision"] == APOC_REVISION
    assert report["source_sha256"] == source_sha


def test_build_apoc_rejects_traversal_source_member(tmp_path):
    source_archive = tmp_path / "apoc.tar.gz"
    source_sha = write_source_archive(source_archive, members={"apoc-source/../escape": b"bad"})

    result = subprocess.run(
        [
            "/bin/bash",
            str(BUILD_APOC),
            "--validate-source-only",
            "--json",
            "--source-archive",
            str(source_archive),
            "--source-sha256",
            source_sha,
            "--build-root",
            str(tmp_path / "build"),
        ],
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )

    assert result.returncode != 0
    assert "escaping archive member" in result.stderr


def test_build_apoc_rejects_wrong_git_archive_commit(tmp_path):
    source_archive = tmp_path / "apoc.tar.gz"
    source_sha = write_source_archive(source_archive, revision="bad")

    result = subprocess.run(
        [
            "/bin/bash",
            str(BUILD_APOC),
            "--validate-source-only",
            "--json",
            "--source-archive",
            str(source_archive),
            "--source-sha256",
            source_sha,
            "--build-root",
            str(tmp_path / "build"),
        ],
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )

    assert result.returncode != 0
    assert "source archive commit mismatch" in result.stderr


def test_build_apoc_requires_source_sha(tmp_path):
    source_archive = tmp_path / "apoc.tar.gz"
    write_source_archive(source_archive)

    result = subprocess.run(
        [
            "/bin/bash",
            str(BUILD_APOC),
            "--validate-source-only",
            "--json",
            "--source-archive",
            str(source_archive),
            "--build-root",
            str(tmp_path / "build"),
        ],
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )

    assert result.returncode != 0
    assert "source archive sha256 is required" in result.stderr


def test_build_apoc_reuses_validated_source_cache(tmp_path):
    source_archive = tmp_path / "apoc.tar.gz"
    source_sha = write_source_archive(source_archive)
    build_root = tmp_path / "build"
    command = [
        "/bin/bash",
        str(BUILD_APOC),
        "--validate-source-only",
        "--json",
        "--source-archive",
        str(source_archive),
        "--source-sha256",
        source_sha,
        "--build-root",
        str(build_root),
    ]

    first = subprocess.run(command, check=True, text=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    first_report = json.loads(first.stdout)
    target_cache = Path(first_report["source_dir"]) / "target" / "sentinel"
    target_cache.parent.mkdir(parents=True)
    target_cache.write_text("keep")

    second = subprocess.run(command, check=True, text=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    second_report = json.loads(second.stdout)

    assert second_report["source_dir"] == first_report["source_dir"]
    assert target_cache.read_text() == "keep"
