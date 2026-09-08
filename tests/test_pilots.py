import json
import subprocess
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
MANIFEST = ROOT / "pilots" / "manifest.json"
TOOL_PROBES = ROOT / "pilots" / "tool-probes.json"

EXPECTED = {
    "incurs": {
        "revision": "bee3d9a6ca0a574ac8eab1f8bedbb43b46cd4846",
        "commands": ["cargo test --workspace", "cargo clippy --workspace --all-targets -- -D warnings"],
    },
    "groktris": {
        "revision": "59f222c50ffedfb139bbdfe9fa395a0a19d72ec7",
        "commands": ["pnpm check", "pnpm exec playwright --version", "pnpm test:browser", "pnpm test:browser:pwa"],
    },
}


def load_manifest():
    return json.loads(MANIFEST.read_text(encoding="utf-8"))


def test_pilot_manifest_declares_exact_sources_and_bundles():
    manifest = load_manifest()
    assert manifest["schema"] == 1
    assert sorted(manifest["pilots"]) == ["groktris", "incurs"]

    for name, expected in EXPECTED.items():
        pilot = manifest["pilots"][name]
        assert pilot["source"]["revision"] == expected["revision"]
        bundle = ROOT / pilot["source"]["bundle"]
        assert bundle.exists(), f"missing source bundle for {name}"

        completed = subprocess.run(
            ["git", "bundle", "list-heads", str(bundle)],
            check=True,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
        assert expected["revision"] in completed.stdout


def test_pilot_devenv_files_are_present_and_pinned():
    manifest = load_manifest()
    for pilot in manifest["pilots"].values():
        for relative_path in pilot["devenv"]["files"]:
            assert (ROOT / relative_path).exists()

        lock = json.loads((ROOT / pilot["devenv"]["lock"]).read_text(encoding="utf-8"))
        assert lock["version"] == 7
        assert lock["root"] == "root"
        assert lock["nodes"]["devenv"]["locked"]["rev"] == "e0781f7bee573eefcab4a7d2788fd9b455560ca2"
        root_nixpkgs = lock["nodes"]["root"]["inputs"]["nixpkgs"]
        assert lock["nodes"][root_nixpkgs]["locked"]["rev"] == "e9b9cbecbf6c25ca26c150d78570e6332694a129"


def test_groktris_environment_pins_exact_web_toolchain_and_melint_gate():
    manifest = load_manifest()
    tools = manifest["pilots"]["groktris"]["devenv"]["toolchain"]

    assert tools["node"]["version"] == "26.6.0"
    assert tools["pnpm"]["version"] == "10.33.0"
    assert tools["playwright"]["version"] == "1.62.1"
    assert tools["melint"]["required"] is True

    nix_text = (ROOT / "pilots" / "groktris" / "devenv.nix").read_text(encoding="utf-8")
    for needle in ["26.6.0", "10.33.0", "melint --version", "tools/melint/dist/bin.js"]:
        assert needle in nix_text
    assert "pname = \"playwright\"" not in nix_text


def test_pilot_manifest_reports_relevant_acceptance_commands():
    manifest = load_manifest()
    for name, expected in EXPECTED.items():
        commands = manifest["pilots"][name]["acceptance_commands"]
        for command in expected["commands"]:
            assert command in commands

        joined = "\n".join(manifest["pilots"][name]["linux_lock_commands"])
        assert "/workspace/workenv" not in joined
        assert f"/home/exedev/workenv/pilots/sources/{name}" in joined


def test_worker_tool_probes_record_actual_versions():
    probes = json.loads(TOOL_PROBES.read_text(encoding="utf-8"))

    assert probes["schema"] == 1
    assert probes["worker"] == "workenv-01.exe.xyz"
    assert probes["base"]["nix"] == "nix (Nix) 2.35.2"
    assert probes["base"]["devenv"] == "devenv 2.3.0+e0781f7 (x86_64-linux)"
    assert probes["incurs"]["cargo"].startswith("cargo 1.97.0 ")
    assert probes["groktris"]["node"] == "v26.6.0"
    assert probes["groktris"]["pnpm"] == "10.33.0"
    assert probes["groktris"]["melint"] == "0.1.0"
    assert probes["groktris"]["playwright"] == "Version 1.62.1"


def test_groktris_snapshot_excludes_secret_generated_and_cache_files():
    manifest = load_manifest()
    snapshot = ROOT / manifest["pilots"]["groktris"]["source"]["snapshot_repo"]

    tracked = subprocess.run(
        ["git", "-C", str(snapshot), "ls-files"],
        check=True,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    ).stdout.splitlines()

    forbidden_parts = {
        ".dev.vars",
        ".env",
        ".git",
        ".wrangler",
        "artifacts",
        "coverage",
        "dist",
        "node_modules",
        "renders",
        "snapshots",
    }
    for path in tracked:
        assert not (set(Path(path).parts) & forbidden_parts), path

    assert "pnpm-workspace.yaml" in tracked
    assert "pnpm-lock.yaml" in tracked
    assert "devenv.nix" in tracked
    assert "devenv.yaml" in tracked
    assert "tools/melint/package.json" in tracked
    assert any(path.startswith("apps/web/") for path in tracked)
    assert any(path.startswith("packages/simulation/") for path in tracked)


def test_source_snapshots_carry_project_devenv_files():
    manifest = load_manifest()
    for pilot in manifest["pilots"].values():
        snapshot = ROOT / pilot["source"]["snapshot_repo"]
        tracked = subprocess.run(
            ["git", "-C", str(snapshot), "ls-files"],
            check=True,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        ).stdout.splitlines()
        assert "devenv.nix" in tracked
        assert "devenv.yaml" in tracked
        assert "devenv.lock" in tracked
