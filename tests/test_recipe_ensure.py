import json
import subprocess

import pytest

from scripts.workenv_recipes import RECIPES


@pytest.mark.parametrize("status,expected_releases", [("pending", 0), ("controller_failed", 0), ("ready", 1), ("needs_reconcile", 1)])
def test_ensure_retains_ownership_when_remote_completion_is_uncertain(status, expected_releases):
    recipe = next(item for item in RECIPES if item["name"] == "workenv.ensure")
    harness = """
const input = {idempotency_key: "test"};
let releases = 0;
const commands = new Map();
const apoc = {
  execution_start: async (args) => { const id = String(commands.size); commands.set(id, args.arg); return {id}; },
  execution_wait: async () => ({outcome: "passed"}),
  execution_logs: async ({id}) => ({stdout: JSON.stringify(commands.get(id).includes("plan-workers")
    ? {ok: true, workers: [{name: "workenv-01"}]}
    : {ok: STATUS === "ready", status: STATUS})}),
  session_open: async () => ({id: "session"}),
  reservation_acquire: async () => ({id: "reservation"}),
  reservation_release: async () => { releases++; }
};
RESULT
""".replace("STATUS", json.dumps(status)).replace("RESULT", "(async () => {" + recipe["code"] + "})().then(result => console.log(JSON.stringify({result, releases})));" )
    result = subprocess.run(["node", "-e", harness], check=True, capture_output=True, text=True)
    observed = json.loads(result.stdout)
    assert observed["releases"] == expected_releases
    assert observed["result"]["workers"][0]["reservation_id"] == "reservation"
