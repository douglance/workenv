import json
import subprocess
import sys

import pytest

from enrollment.nib_auth import REMOTE_STORE
from remote.nib import credential_environment


def test_nib_loads_only_private_runtime_file_without_changing_parent_environment(tmp_path):
    credential = tmp_path / "nib-token"
    credential.write_text("test-token\n")
    credential.chmod(0o600)
    inherited = {"PATH": "/bin"}
    result = credential_environment(credential, inherited)
    assert result["NIB_AUTH_TOKEN"] == "test-token"
    assert "NIB_AUTH_TOKEN" not in inherited


def test_nib_preserves_explicit_auth_and_missing_file_reports_through_real_cli(tmp_path):
    assert credential_environment(tmp_path / "missing", {}) == {}
    assert credential_environment(tmp_path / "missing", {"NIB_AUTH_TOKEN": "explicit"})["NIB_AUTH_TOKEN"] == "explicit"


def test_nib_rejects_shared_readable_or_symlinked_credential(tmp_path):
    credential = tmp_path / "token"
    credential.write_text("test-token")
    credential.chmod(0o644)
    with pytest.raises(ValueError, match="owner-only"):
        credential_environment(credential, {})
    credential.chmod(0o600)
    link = tmp_path / "link"
    link.symlink_to(credential)
    with pytest.raises(OSError):
        credential_environment(link, {})


def test_transfer_is_idempotent_and_preserves_different_credential(tmp_path):
    script = REMOTE_STORE.replace("pathlib.Path.home()", "pathlib.Path(sys.argv[1])")

    def install(token):
        return subprocess.run([sys.executable, "-c", script, str(tmp_path)], input=token, text=True, capture_output=True)

    first = install("test-private-token")
    assert first.returncode == 0
    assert json.loads(first.stdout)["status"] == "installed"
    assert "test-private-token" not in first.stdout + first.stderr
    assert json.loads(install("test-private-token").stdout)["status"] == "already_present"
    different = install("different-token")
    assert different.returncode != 0
    path = tmp_path / ".config/workenv/nib-token"
    assert path.read_text().strip() == "test-private-token"
    assert path.stat().st_mode & 0o777 == 0o600
    assert not list(path.parent.glob(".nib-token-*"))
