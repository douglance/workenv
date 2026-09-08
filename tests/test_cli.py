import json
import os
import subprocess
import sys
from pathlib import Path


def test_controller_script_runs_from_outside_the_repository(tmp_path):
    root = Path(__file__).resolve().parents[1]
    completed = subprocess.run(
        [sys.executable, str(root / "scripts/workenv_controller.py"),
         "plan-workers", "--worker", "workenv-01"],
        cwd=tmp_path,
        env={**os.environ, "PYTHONPATH": ""},
        capture_output=True,
        text=True,
    )
    assert completed.returncode == 0, completed.stderr
    assert json.loads(completed.stdout)["workers"][0]["name"] == "workenv-01"
