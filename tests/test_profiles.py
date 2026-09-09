import base64
import hashlib
import json
import os
import subprocess
import sys
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
PROFILE = ROOT / "remote" / "profile.py"


def canonical_digest(spec):
    return hashlib.sha256(json.dumps(spec, sort_keys=True, separators=(",", ":"), ensure_ascii=False).encode()).hexdigest()


def encoded_spec(spec):
    return base64.b64encode(json.dumps(spec).encode()).decode("ascii")


def run_profile(tmp_path, args, env=None, check=True):
    completed = subprocess.run(
        [sys.executable, str(PROFILE), "--root", str(tmp_path / "worker"), *args],
        cwd=ROOT,
        env=env,
        capture_output=True,
        text=True,
    )
    if check:
        assert completed.returncode == 0, completed.stderr + completed.stdout
    return completed


def prepared_spec(name="lv", github_login="operator", **extra):
    spec = {"schema_version": 1, "name": name, **extra}
    if github_login is not None:
        spec["github_login"] = github_login
    return spec, canonical_digest(spec)


def prepare(tmp_path, env, spec, digest):
    return run_profile(
        tmp_path,
        ["prepare", "--name", spec["name"], "--spec-base64", encoded_spec(spec), "--digest", digest],
        env=env,
    )


def fake_bin(tmp_path, gh_login="operator", gh_exit=0):
    bin_dir = tmp_path / "fake-bin"
    bin_dir.mkdir()
    gh = bin_dir / "gh"
    gh.write_text(
        f"""#!/usr/bin/env bash
set -euo pipefail
if [ "${{1:-}}" = "auth" ] && [ "${{2:-}}" = "setup-git" ]; then
  echo "setup-git must not be called during prepare" >&2
  exit 42
fi
if [ "${{1:-}}" = "api" ] && [ "${{2:-}}" = "user" ]; then
  echo "raw auth stderr SECRET_TOKEN_SHOULD_NOT_LEAK" >&2
  exit {gh_exit if gh_exit else 0}
fi
exit 2
""",
        encoding="utf-8",
    )
    if gh_exit == 0:
        gh.write_text(
            gh.read_text(encoding="utf-8").replace(f"  exit {gh_exit if gh_exit else 0}\nfi", f"  echo {gh_login!r}\n  exit 0\nfi"),
            encoding="utf-8",
        )
    gh.chmod(0o700)
    wrangler = bin_dir / "wrangler"
    wrangler.write_text(
        """#!/usr/bin/env bash
set -euo pipefail
python3 - <<'PY'
import json, os
print(json.dumps({
  "xdg_config_home": os.environ.get("XDG_CONFIG_HOME"),
  "cf_token": os.environ.get("CLOUDFLARE_API_TOKEN"),
  "cf_email": os.environ.get("CLOUDFLARE_EMAIL"),
  "argv": os.environ.get("WRANGLER_ARGV")
}, sort_keys=True))
PY
""",
        encoding="utf-8",
    )
    wrangler.chmod(0o700)
    return bin_dir


def profile_env(tmp_path, fake_bin_dir):
    return {
        **os.environ,
        "HOME": str(tmp_path / "home"),
        "XDG_DATA_HOME": str(tmp_path / "xdg-data"),
        "XDG_CONFIG_HOME": str(tmp_path / "global-xdg-config"),
        "PATH": f"{fake_bin_dir}{os.pathsep}{os.environ['PATH']}",
        "GH_TOKEN": "poison-gh",
        "GITHUB_TOKEN": "poison-github",
        "ANTHROPIC_API_KEY": "poison-anthropic",
        "AUTH_TOKEN": "poison-auth-token",
        "OPENAI_API_KEY": "poison-openai",
        "CODEX_API_KEY": "poison-codex",
        "CLOUDFLARE_API_TOKEN": "poison-cloudflare",
        "GIT_AUTHOR_NAME": "poison-author",
        "GIT_CONFIG_COUNT": "1",
        "GIT_CONFIG_KEY_0": "user.name",
        "GIT_CONFIG_VALUE_0": "Poison",
    }


def parse_stdout(completed):
    return json.loads(completed.stdout)


def test_prepare_and_exec_use_real_direnv_with_scoped_clean_environment(tmp_path):
    fake = fake_bin(tmp_path)
    env = profile_env(tmp_path, fake)
    spec, digest = prepared_spec(git_name="LV Worker", git_email="lv@example.com", anthropic_profile="lv-claude")
    prepared = parse_stdout(prepare(tmp_path, env, spec, digest))
    profile_dir = Path(prepared["paths"]["profile_dir"])
    gitconfig = (profile_dir / ".gitconfig").read_text(encoding="utf-8")

    assert prepared["ok"] is True
    assert prepared["status"] == "profile_prepared"
    assert not (profile_dir / ".gh" / "setup-git").exists()
    assert "useConfigOnly = true" in gitconfig
    assert 'name = "LV Worker"' in gitconfig
    assert 'email = "lv@example.com"' in gitconfig
    assert "helper = !gh auth git-credential" in gitconfig

    code = """
import json, os, sys
print(json.dumps({
    "argv": sys.argv[1:],
    "cwd": os.getcwd(),
    "home": os.environ.get("HOME"),
    "xdg_config_home": os.environ.get("XDG_CONFIG_HOME"),
    "profile": os.environ.get("WORKENV_PROFILE"),
    "digest": os.environ.get("WORKENV_PROFILE_DIGEST"),
    "gh_config_dir": os.environ.get("GH_CONFIG_DIR"),
    "git_config_global": os.environ.get("GIT_CONFIG_GLOBAL"),
    "git_config_nosystem": os.environ.get("GIT_CONFIG_NOSYSTEM"),
    "codex_home": os.environ.get("CODEX_HOME"),
    "claude_config_dir": os.environ.get("CLAUDE_CONFIG_DIR"),
    "kubeconfig": os.environ.get("KUBECONFIG"),
    "anthropic_profile": os.environ.get("ANTHROPIC_PROFILE"),
    "cleared": {key: os.environ.get(key) for key in [
        "GH_TOKEN", "GITHUB_TOKEN", "ANTHROPIC_API_KEY", "AUTH_TOKEN", "OPENAI_API_KEY",
        "CODEX_API_KEY", "CLOUDFLARE_API_TOKEN", "GIT_AUTHOR_NAME",
        "GIT_CONFIG_COUNT", "GIT_CONFIG_KEY_0", "GIT_CONFIG_VALUE_0"
    ]},
}, sort_keys=True))
"""
    completed = run_profile(
        tmp_path,
        ["exec", "--name", "lv", "--digest", digest, "--", sys.executable, "-c", code, "two words", "semi;colon"],
        env=env,
    )
    result = json.loads(completed.stdout)
    assert result["argv"] == ["two words", "semi;colon"]
    assert result["cwd"] == str(ROOT)
    assert result["home"] == env["HOME"]
    assert result["xdg_config_home"] == env["XDG_CONFIG_HOME"]
    assert result["profile"] == "lv"
    assert result["digest"] == digest
    assert result["gh_config_dir"] == str(profile_dir / ".gh")
    assert result["git_config_global"] == str(profile_dir / ".gitconfig")
    assert result["git_config_nosystem"] == "1"
    assert result["codex_home"] == str(profile_dir / ".codex")
    assert result["claude_config_dir"] == str(profile_dir / ".claude")
    assert result["kubeconfig"] == str(profile_dir / ".kube" / "config")
    assert result["anthropic_profile"] == "lv-claude"
    assert all(value is None for value in result["cleared"].values())


def test_distinct_profiles_get_distinct_dirs_and_matching_github_status(tmp_path):
    fake = fake_bin(tmp_path, gh_login="DougLance")
    env = profile_env(tmp_path, fake)
    lv_spec, lv_digest = prepared_spec("lv", "operator")
    oc_spec, oc_digest = prepared_spec("oc", "other")
    lv = parse_stdout(prepare(tmp_path, env, lv_spec, lv_digest))
    oc = parse_stdout(prepare(tmp_path, env, oc_spec, oc_digest))

    status = parse_stdout(run_profile(tmp_path, ["status", "--name", "lv", "--digest", lv_digest], env=env))

    assert lv["paths"]["profile_dir"] != oc["paths"]["profile_dir"]
    assert status["ok"] is True
    assert status["status"] == "profile_ready"
    assert status["prepared"] is True
    assert status["github"] == {"expected": "operator", "actual": "DougLance", "matches": True, "reason": "matched"}


def test_status_without_expected_github_login_is_ready_without_auth_check(tmp_path):
    fake = fake_bin(tmp_path, gh_login="wrong-user", gh_exit=1)
    env = profile_env(tmp_path, fake)
    spec, digest = prepared_spec("lv", None)
    prepare(tmp_path, env, spec, digest)

    status = parse_stdout(run_profile(tmp_path, ["status", "--name", "lv", "--digest", digest], env=env))

    assert status["ok"] is True
    assert status["status"] == "profile_ready"
    assert status["prepared"] is True
    assert status["github"] == {
        "expected": None,
        "actual": None,
        "matches": False,
        "reason": "github_login_not_configured",
    }


def test_check_github_without_expected_login_runs_command_without_network_check(tmp_path):
    fake = fake_bin(tmp_path, gh_exit=1)
    env = profile_env(tmp_path, fake)
    spec, digest = prepared_spec("lv", None)
    prepare(tmp_path, env, spec, digest)

    completed = run_profile(
        tmp_path,
        ["exec", "--name", "lv", "--digest", digest, "--check-github", "--", sys.executable, "-c", "print('ran')"],
        env=env,
    )

    assert completed.stdout == "ran\n"


def test_git_identity_values_are_quoted_for_git_config_parser(tmp_path):
    fake = fake_bin(tmp_path)
    env = profile_env(tmp_path, fake)
    git_name = 'Example "Quoted" # Person \\ Team'
    spec, digest = prepared_spec("lv", "operator", git_name=git_name, git_email="quoted@example.com")
    prepare(tmp_path, env, spec, digest)

    completed = run_profile(
        tmp_path,
        ["exec", "--name", "lv", "--digest", digest, "--", "git", "config", "--global", "user.name"],
        env=env,
    )

    assert completed.stdout == f"{git_name}\n"


def test_exec_fails_closed_when_digest_or_generated_config_mismatches(tmp_path):
    fake = fake_bin(tmp_path)
    env = profile_env(tmp_path, fake)
    spec, digest = prepared_spec()
    prepared = parse_stdout(prepare(tmp_path, env, spec, digest))
    marker = tmp_path / "ran"

    wrong_digest = "0" * 64
    completed = run_profile(
        tmp_path,
        ["exec", "--name", "lv", "--digest", wrong_digest, "--", sys.executable, "-c", f"open({str(marker)!r}, 'w').write('ran')"],
        env=env,
        check=False,
    )
    assert completed.returncode != 0
    assert "digest_mismatch" in completed.stderr
    assert not marker.exists()

    envrc = Path(prepared["paths"]["profile_dir"]) / ".envrc"
    envrc.write_text(envrc.read_text(encoding="utf-8") + "\nexport EXTRA=bad\n", encoding="utf-8")
    completed = run_profile(
        tmp_path,
        ["exec", "--name", "lv", "--digest", digest, "--", sys.executable, "-c", f"open({str(marker)!r}, 'w').write('ran')"],
        env=env,
        check=False,
    )
    assert completed.returncode != 0
    assert "runtime_mismatch" in completed.stderr
    assert not marker.exists()

    status = parse_stdout(run_profile(tmp_path, ["status", "--name", "lv", "--digest", digest], env=env))
    assert status["ok"] is False
    assert status["prepared"] is False
    assert status["status"] == "runtime_mismatch"


def test_prepare_rejects_malicious_names_unknown_fields_and_secret_fields(tmp_path):
    fake = fake_bin(tmp_path)
    env = profile_env(tmp_path, fake)
    spec, digest = prepared_spec("../bad")
    bad_name = run_profile(
        tmp_path,
        ["prepare", "--name", "../bad", "--spec-base64", encoded_spec(spec), "--digest", digest],
        env=env,
        check=False,
    )
    assert bad_name.returncode != 0
    assert parse_stdout(bad_name)["reason"] == "invalid_name"

    spec = {"schema_version": 1, "name": "lv", "github_login": "operator", "color": "blue"}
    unknown = run_profile(
        tmp_path,
        ["prepare", "--name", "lv", "--spec-base64", encoded_spec(spec), "--digest", canonical_digest(spec)],
        env=env,
        check=False,
    )
    assert unknown.returncode != 0
    assert parse_stdout(unknown)["reason"] == "spec_unknown_field"

    spec = {"schema_version": 1, "name": "lv", "github_login": "operator", "github_token": "SECRET"}
    secret = run_profile(
        tmp_path,
        ["prepare", "--name", "lv", "--spec-base64", encoded_spec(spec), "--digest", canonical_digest(spec)],
        env=env,
        check=False,
    )
    assert secret.returncode != 0
    assert parse_stdout(secret)["reason"] == "spec_secret_field"
    assert "SECRET" not in secret.stdout + secret.stderr


def test_status_and_check_github_do_not_leak_raw_auth_errors_and_mismatch_blocks_command(tmp_path):
    fake = fake_bin(tmp_path, gh_login="wrong-user")
    env = profile_env(tmp_path, fake)
    spec, digest = prepared_spec("lv", "operator")
    prepare(tmp_path, env, spec, digest)

    status = run_profile(tmp_path, ["status", "--name", "lv", "--digest", digest], env=env)
    assert "SECRET_TOKEN_SHOULD_NOT_LEAK" not in status.stdout + status.stderr
    payload = parse_stdout(status)
    assert payload["ok"] is False
    assert payload["status"] == "profile_auth_required"
    assert payload["prepared"] is True
    assert payload["github"] == {
        "expected": "operator",
        "actual": "wrong-user",
        "matches": False,
        "reason": "identity_mismatch",
    }

    marker = tmp_path / "blocked"
    completed = run_profile(
        tmp_path,
        ["exec", "--name", "lv", "--digest", digest, "--check-github", "--", sys.executable, "-c", f"open({str(marker)!r}, 'w').write('ran')"],
        env=env,
        check=False,
    )
    assert completed.returncode != 0
    assert "identity_mismatch" in completed.stderr
    assert "SECRET_TOKEN_SHOULD_NOT_LEAK" not in completed.stdout + completed.stderr
    assert not marker.exists()


def test_status_validates_selected_env_before_github_identity_check(tmp_path):
    fake = fake_bin(tmp_path, gh_login="operator")
    env = profile_env(tmp_path, fake)
    spec, digest = prepared_spec("lv", "operator")
    prepared = parse_stdout(prepare(tmp_path, env, spec, digest))
    profile_dir = Path(prepared["paths"]["profile_dir"])
    envrc = profile_dir / ".envrc"
    text = envrc.read_text(encoding="utf-8")
    envrc.write_text(text.replace("unset GH_TOKEN GITHUB_TOKEN GH_ENTERPRISE_TOKEN GITHUB_ENTERPRISE_TOKEN\n", ""), encoding="utf-8")
    files = {
        "profile.json": (profile_dir / "profile.json").read_text(encoding="utf-8"),
        ".envrc": envrc.read_text(encoding="utf-8"),
        ".gitconfig": (profile_dir / ".gitconfig").read_text(encoding="utf-8"),
        ".bin/wrangler": (profile_dir / ".bin" / "wrangler").read_text(encoding="utf-8"),
    }
    runtime = json.loads((profile_dir / "runtime.json").read_text(encoding="utf-8"))
    runtime["config_hash"] = hashlib.sha256(
        json.dumps({key: files[key] for key in sorted(files)}, sort_keys=True, separators=(",", ":"), ensure_ascii=False).encode()
    ).hexdigest()
    (profile_dir / "runtime.json").write_text(json.dumps(runtime, sort_keys=True, separators=(",", ":")) + "\n", encoding="utf-8")
    subprocess.run(["direnv", "allow", str(profile_dir)], env=env, check=True, capture_output=True, text=True)

    status = parse_stdout(run_profile(tmp_path, ["status", "--name", "lv", "--digest", digest], env=env))

    assert status["ok"] is False
    assert status["status"] == "profile_auth_required"
    assert status["github"]["actual"] is None
    assert status["github"]["reason"] == "profile_env_poisoned"


def test_mutable_kube_config_is_not_sealed_and_survives_reprepare(tmp_path):
    fake = fake_bin(tmp_path)
    env = profile_env(tmp_path, fake)
    spec, digest = prepared_spec()
    prepared = parse_stdout(prepare(tmp_path, env, spec, digest))
    kubeconfig = Path(prepared["paths"]["kubeconfig"])
    kubeconfig.write_text("apiVersion: v1\nclusters: []\n", encoding="utf-8")
    kubeconfig.chmod(0o600)

    code = "import os, pathlib; print(pathlib.Path(os.environ['KUBECONFIG']).read_text(), end='')"
    completed = run_profile(tmp_path, ["exec", "--name", "lv", "--digest", digest, "--", sys.executable, "-c", code], env=env)
    assert completed.stdout == "apiVersion: v1\nclusters: []\n"

    prepare(tmp_path, env, spec, digest)
    assert kubeconfig.read_text(encoding="utf-8") == "apiVersion: v1\nclusters: []\n"


def test_exec_rejects_overly_permissive_generated_files(tmp_path):
    fake = fake_bin(tmp_path)
    env = profile_env(tmp_path, fake)
    spec, digest = prepared_spec()
    prepared = parse_stdout(prepare(tmp_path, env, spec, digest))
    Path(prepared["paths"]["profile_dir"], ".envrc").chmod(0o644)

    completed = run_profile(
        tmp_path,
        ["exec", "--name", "lv", "--digest", digest, "--", sys.executable, "-c", "print('ran')"],
        env=env,
        check=False,
    )

    assert completed.returncode != 0
    assert "profile_path_invalid" in completed.stderr


def test_wrangler_wrapper_uses_tool_only_xdg_config_and_clears_cloudflare_env(tmp_path):
    fake = fake_bin(tmp_path)
    env = profile_env(tmp_path, fake)
    spec, digest = prepared_spec()
    prepared = parse_stdout(prepare(tmp_path, env, spec, digest))

    completed = run_profile(tmp_path, ["exec", "--name", "lv", "--digest", digest, "--", "wrangler"], env=env)
    result = json.loads(completed.stdout)

    assert result["xdg_config_home"] == str(Path(prepared["paths"]["profile_dir"]) / ".config")
    assert result["cf_token"] is None
    assert result["cf_email"] is None
