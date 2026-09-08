from __future__ import annotations

import hashlib
import json
import sys


NAMESPACE = "workenv"
WORKENV_ROOT = "/Users/operator/Developer/src/workenv"


COMMON_JS = r"""
const root = input.workenv_root || "WORKENV_ROOT_PLACEHOLDER";
const controller = input.controller || `${root}/scripts/workenv_controller.py`;
const fleet = input.fleet || `${root}/fleet.json`;
const state = input.state || `${root}/.state/controller`;
const timeoutMs = input.timeout_ms || 300000;
if (!input.idempotency_key) throw new Error("input.idempotency_key is required");
const key = (suffix) => `workenv:${input.idempotency_key}:${suffix}`;
const commandResult = async (args, suffix, purpose) => {
  const started = await apoc.execution_start({
    executable: "python3",
    arg: [controller, "--fleet", fleet, "--state", state, ...args],
    cwd: root,
    timeout_ms: timeoutMs,
    progress_timeout_ms: input.progress_timeout_ms || 60000,
    artifact_bytes: 1048576,
    expect_exit_code: [0],
    verbosity: "trace",
    idempotency_key: key(suffix),
    purpose
  });
  const execution = started.execution || started;
  const executionId = execution.id || started.id;
  if (!executionId) return { status: "controller_unowned", ok: false, outcome: started.outcome };
  const waited = await apoc.execution_wait({
    id: executionId,
    timeout_ms: timeoutMs,
    verbosity: "error",
    purpose: `${purpose} Wait for completion.`
  });
  if (waited.outcome === "pending") {
    return { status: "pending", ok: false, execution_id: executionId };
  }
  if (waited.outcome !== "passed") {
    return { status: "controller_failed", ok: false, outcome: waited.outcome, execution_id: executionId };
  }
  const logs = await apoc.execution_logs({
    id: executionId,
    tail_bytes: 1048576,
    purpose: `${purpose} Read JSON output.`
  });
  const stdout = logs.stdout ? logs.stdout.trim() : "";
  if (!stdout) return { status: "controller_empty", ok: false, execution_id: executionId };
  const parsed = JSON.parse(stdout);
  parsed.execution_id = executionId;
  return parsed;
};
const openSession = async (operation) => apoc.session_open({
  actor: "workenv-controller",
  label: [`operation=${operation}`],
  ttl_ms: input.session_ttl_ms || 86400000,
  idempotency_key: key(`${operation}:session`),
  purpose: `Open the workenv ${operation} controller session.`
});
const reserveWorker = async (session, worker, suffix) => apoc.reservation_acquire({
  kind: "custom",
  key: `workenv/worker/${worker}`,
  lease: `session/${session.id}`,
  ttl_ms: input.reservation_ttl_ms || 86400000,
  idempotency_key: key(`reserve:${suffix}:${worker}`),
  purpose: `Reserve workenv worker ${worker}.`
});
const releaseReservation = async (reservation, suffix) => apoc.reservation_release({
  id: reservation.id,
  idempotency_key: key(`release-reservation:${suffix}:${reservation.id}`),
  purpose: `Release workenv reservation ${reservation.id}.`
});
const validateReservation = async (worker) => {
  if (!input.reservation_id) throw new Error("input.reservation_id is required");
  const reservation = await apoc.reservation_get({
    id: input.reservation_id,
    purpose: `Read workenv reservation ${input.reservation_id}.`
  });
  const now = Date.now();
  if (reservation.released || reservation.key !== `workenv/worker/${worker}`) {
    return { status: "reservation_mismatch", ok: false, reservation };
  }
  if (reservation.expires_at && reservation.expires_at <= now) {
    return { status: "reservation_expired", ok: false, reservation };
  }
  const leaseId = reservation.lease_id || reservation.lease;
  if (!leaseId || !leaseId.startsWith("session/")) {
    return { status: "reservation_lease_unknown", ok: false, reservation };
  }
  const sessionId = leaseId.slice("session/".length);
  let session;
  try {
    session = await apoc.session_get({
      id: sessionId,
      purpose: `Read workenv reservation ${input.reservation_id} owning session.`
    });
  } catch (error) {
    return { status: "reservation_session_unknown", ok: false, reservation, error: String(error) };
  }
  if (session.closed || session.closing) {
    return { status: "reservation_session_closed", ok: false, reservation, session };
  }
  if (session.expires_at && session.expires_at <= now) {
    return { status: "reservation_session_expired", ok: false, reservation, session };
  }
  return { status: "verified", ok: true, reservation, session };
};
""".replace("WORKENV_ROOT_PLACEHOLDER", WORKENV_ROOT)


def object_schema(required: list[str] | None = None) -> dict:
    schema = {"type": "object", "additionalProperties": True}
    if required:
        schema["required"] = required
    return schema


RECIPES = [
    {
        "name": "workenv.ensure",
        "description": "Reserve one worker at a time and run bootstrap health or reconciliation without claiming authentication success.",
        "code": COMMON_JS
        + r"""
const plan = await commandResult(["plan-workers", ...(input.worker ? ["--worker", input.worker] : [])], "ensure:plan", "Plan workenv workers for ensure.");
if (!plan.ok) return plan;
const session = await openSession("ensure");
const results = [];
for (const worker of plan.workers) {
  let reservation;
  try {
    reservation = await reserveWorker(session, worker.name, "ensure");
  } catch (error) {
    results.push({ worker: worker.name, status: "reserved", ok: false, error: String(error) });
    continue;
  }
  const args = ["ensure", "--worker", worker.name, "--request-id", `${input.idempotency_key}-${worker.name}`];
  if (input.reconcile) args.push("--reconcile");
  if (input.register_herdr) args.push("--register-herdr");
  if (input.registration_target) args.push("--registration-target", input.registration_target);
  if (input.provider_create) args.push("--provider-create");
  const ensured = await commandResult(args, `ensure:${worker.name}`, `Ensure workenv worker ${worker.name}.`);
  ensured.reservation_id = reservation.id;
  results.push(ensured);
  const uncertain = ["pending", "controller_failed", "controller_empty", "controller_unowned", "remote_invalid_json", "remote_failed", "auth_failed"];
  if (!uncertain.includes(ensured.status)) {
    await releaseReservation(reservation, `ensure:${worker.name}`);
  }
}
return { status: "ensured", ok: results.every((item) => item.ok), workers: results };
""",
        "input_schema": object_schema(),
        "output_schema": object_schema(),
        "annotations": {"read_only": False, "destructive": False, "idempotent": True, "open_world": True},
    },
    {
        "name": "workenv.claim",
        "description": "Atomically reserve an eligible worker on the controller, then claim the exact remote task identity and revision.",
        "code": COMMON_JS
        + r"""
for (const required of ["project", "task_id", "revision"]) {
  if (!input[required]) throw new Error(`input.${required} is required`);
}
const plan = await commandResult(["plan-claim", "--project", input.project, ...(input.worker ? ["--worker", input.worker] : [])], "claim:plan", "Plan workenv claim candidates.");
if (!plan.ok) return plan;
const session = await openSession("claim");
const attempts = [];
for (const worker of plan.workers) {
  let reservation;
  try {
    reservation = await reserveWorker(session, worker.name, "claim");
  } catch (error) {
    attempts.push({ worker: worker.name, status: "reserved", ok: false, error: String(error) });
    continue;
  }
  const args = ["claim", "--worker", worker.name, "--project", input.project, "--task-id", input.task_id, "--revision", input.revision, "--request-id", `${input.idempotency_key}-${worker.name}`, "--reservation-id", reservation.id, "--session-id", session.id];
  if (input.source_bundle) args.push("--source-bundle", input.source_bundle);
  const claimed = await commandResult(args, `claim:${worker.name}`, `Claim workenv worker ${worker.name} for ${input.task_id}.`);
  claimed.reservation_id = reservation.id;
  const recordableStatuses = ["claimed", "pending", "auth_failed", "remote_failed", "remote_invalid_json", "controller_failed", "controller_empty"];
  if (recordableStatuses.includes(claimed.status)) {
    const recordArgs = ["record-task", "--worker", worker.name, "--project", input.project, "--task-id", input.task_id, "--revision", input.revision, "--reservation-id", reservation.id, "--session-id", session.id, "--source", input.source_bundle ? "bundle" : "remote", "--status", claimed.status];
    if (claimed.worktree) recordArgs.push("--worktree", claimed.worktree);
    if (claimed.execution_id) recordArgs.push("--controller-execution-id", claimed.execution_id);
    claimed.task_record = await commandResult(recordArgs, `claim:record:${worker.name}`, `Record workenv claim ${input.task_id} for ${worker.name}.`);
  }
  attempts.push(claimed);
  const retainedStatuses = [...recordableStatuses, "busy"];
  if (retainedStatuses.includes(claimed.status)) return { ...claimed, attempts };
  await releaseReservation(reservation, `claim:${worker.name}`);
}
return { status: "busy", ok: false, attempts };
""",
        "input_schema": object_schema(["idempotency_key", "project", "task_id", "revision"]),
        "output_schema": object_schema(),
        "annotations": {"read_only": False, "destructive": False, "idempotent": True, "open_world": True},
    },
    {
        "name": "workenv.record-runtime",
        "description": "Record exact APoC and Herdr runtime IDs against the active remote task before release checks.",
        "code": COMMON_JS
        + r"""
for (const required of ["worker", "task_id", "revision", "runtime"]) {
  if (!input[required]) throw new Error(`input.${required} is required`);
}
const args = ["record-runtime", "--worker", input.worker, "--task-id", input.task_id, "--revision", input.revision, "--request-id", input.idempotency_key, "--runtime-json", JSON.stringify(input.runtime)];
return await commandResult(args, "record-runtime:remote", `Record workenv runtime for ${input.task_id}.`);
""",
        "input_schema": object_schema(["idempotency_key", "worker", "task_id", "revision", "runtime"]),
        "output_schema": object_schema(),
        "annotations": {"read_only": False, "destructive": False, "idempotent": True, "open_world": True},
    },
    {
        "name": "workenv.status",
        "description": "Read central APoC worker reservations and remote worker status without changing task ownership.",
        "code": COMMON_JS
        + r"""
const reservations = await apoc.reservation_list({
  active: true,
  purpose: "List active workenv worker reservations."
});
const args = ["status", "--request-id", input.idempotency_key];
if (input.worker) args.push("--worker", input.worker);
const remote = await commandResult(args, "status:remote", "Read remote workenv worker status.");
return { status: remote.status, ok: remote.ok, reservations: reservations.filter((item) => item.key.startsWith("workenv/worker/")), remote };
""",
        "input_schema": object_schema(["idempotency_key"]),
        "output_schema": object_schema(),
        "annotations": {"read_only": True, "destructive": False, "idempotent": True, "open_world": True},
    },
    {
        "name": "workenv.collect",
        "description": "Collect one claimed task, retrieve the remote collection, and verify the local copy before returning.",
        "code": COMMON_JS
        + r"""
for (const required of ["worker", "task_id", "reservation_id"]) {
  if (!input[required]) throw new Error(`input.${required} is required`);
}
const checked = await validateReservation(input.worker);
if (!checked.ok) return checked;
const args = ["collect", "--worker", input.worker, "--task-id", input.task_id, "--request-id", input.idempotency_key];
for (const path of (input.evidence_paths || [])) args.push("--evidence-path", path);
const collected = await commandResult(args, "collect:remote", `Collect workenv task ${input.task_id}.`);
collected.reservation_id = input.reservation_id;
return collected;
""",
        "input_schema": object_schema(["idempotency_key", "worker", "task_id", "reservation_id"]),
        "output_schema": object_schema(),
        "annotations": {"read_only": False, "destructive": False, "idempotent": True, "open_world": True},
    },
    {
        "name": "workenv.release",
        "description": "Release one worker only after a matching verified local collection and no live tracked task activity.",
        "code": COMMON_JS
        + r"""
for (const required of ["worker", "task_id", "reservation_id", "collection_digest"]) {
  if (!input[required]) throw new Error(`input.${required} is required`);
}
const checked = await validateReservation(input.worker);
if (!checked.ok) return checked;
const released = await commandResult(["release", "--worker", input.worker, "--task-id", input.task_id, "--request-id", input.idempotency_key, "--collection-digest", input.collection_digest], "release:remote", `Release workenv task ${input.task_id}.`);
released.reservation_id = input.reservation_id;
if (released.status === "released") {
  await releaseReservation(checked.reservation, "release:worker");
}
return released;
""",
        "input_schema": object_schema(["idempotency_key", "worker", "task_id", "reservation_id", "collection_digest"]),
        "output_schema": object_schema(),
        "annotations": {"read_only": False, "destructive": False, "idempotent": True, "open_world": True},
    },
]


def recipe_generation(recipe: dict) -> str:
    portable = {
        "name": recipe["name"],
        "description": recipe["description"],
        "code": recipe["code"],
        "input_schema": recipe.get("input_schema"),
        "output_schema": recipe.get("output_schema"),
        "dependencies": recipe.get("dependencies", []),
        "annotations": recipe.get("annotations", {}),
    }
    required_env = recipe.get("required_env", [])
    if required_env:
        portable["required_env"] = required_env
    encoded = json.dumps(portable, separators=(",", ":")).encode("utf-8")
    return f"sha256:{hashlib.sha256(encoded).hexdigest()}"


def bundle() -> dict:
    recipes = []
    for recipe in RECIPES:
        portable = {
            "name": recipe["name"],
            "description": recipe["description"],
            "code": recipe["code"],
            "input_schema": recipe.get("input_schema"),
            "output_schema": recipe.get("output_schema"),
            "dependencies": recipe.get("dependencies", []),
            "required_env": recipe.get("required_env", []),
            "annotations": recipe.get("annotations", {}),
        }
        portable["generation"] = recipe_generation(portable)
        recipes.append(portable)
    return {"schema_version": 1, "namespace": NAMESPACE, "recipes": recipes}


def main() -> int:
    json.dump(bundle(), sys.stdout, indent=2)
    sys.stdout.write("\n")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
