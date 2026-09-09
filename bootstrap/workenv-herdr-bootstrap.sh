#!/usr/bin/env bash
set -euo pipefail

workenv_root="${WORKENV_ROOT:-$HOME/workenv}"
if [[ "${WORKENV_BOOTSTRAP_SHELL:-}" != "1" ]]; then
  export PATH="/usr/local/bin:/usr/bin:/bin:${PATH:-}"
  cd "$workenv_root"
  exec devenv shell -- env WORKENV_BOOTSTRAP_SHELL=1 bash "$0"
fi

# Herdr's SSH setup probes PATH before entering a shell. Keep that entrypoint
# on the same devenv profile as the server so binary identity checks agree.
profile_herdr="$(command -v herdr)"
if [[ -x "$profile_herdr" ]]; then
  sudo ln -sfn "$profile_herdr" /usr/local/bin/herdr
  mkdir -p "$HOME/.local/bin"
  ln -sfn "$profile_herdr" "$HOME/.local/bin/herdr"
fi

SESSION="${WORKENV_HERDR_SESSION:-workenv}"
APOC_BIN="${APOC_BIN:-apoc}"
HERDR_BIN="${HERDR_BIN:-herdr}"
BOOT_ID="${WORKENV_BOOT_ID:-}"
IDENTITY_PROFILE="${WORKENV_IDENTITY_PROFILE:-}"
IDENTITY_DIGEST="${WORKENV_IDENTITY_DIGEST:-}"
identity_file="$workenv_root/.state/identity-profile.json"

if [[ -z "$BOOT_ID" ]]; then
  if [[ ! -r /proc/sys/kernel/random/boot_id ]]; then
    echo "cannot derive per-boot idempotency key: /proc/sys/kernel/random/boot_id is missing" >&2
    exit 1
  fi
  BOOT_ID="$(cat /proc/sys/kernel/random/boot_id)"
fi

if [[ -z "$IDENTITY_PROFILE" && -e "$identity_file" ]]; then
  if [[ ! -r "$identity_file" ]]; then
    echo "workenv identity profile descriptor exists but is not readable: $identity_file" >&2
    exit 1
  fi
  identity_values="$(python3 - "$identity_file" <<'PY'
import json
import pathlib
import re
import sys

try:
    value = json.loads(pathlib.Path(sys.argv[1]).read_text())
except Exception as error:
    print(f"invalid workenv identity profile descriptor: {error}", file=sys.stderr)
    raise SystemExit(1)
if value is None:
    print("\t")
    raise SystemExit(0)
if not isinstance(value, dict) or set(value) != {"name", "digest"}:
    print("invalid workenv identity profile descriptor", file=sys.stderr)
    raise SystemExit(1)
name = value.get("name")
digest = value.get("digest")
if (
    not isinstance(name, str)
    or not isinstance(digest, str)
    or not re.fullmatch(r"[a-z][a-z0-9-]{0,47}", name)
    or not re.fullmatch(r"[0-9a-f]{64}", digest)
):
    print("invalid workenv identity profile descriptor", file=sys.stderr)
    raise SystemExit(1)
print(name, digest)
PY
  )" || exit 1
  read -r IDENTITY_PROFILE IDENTITY_DIGEST <<<"$identity_values"
fi

if [[ -n "$IDENTITY_PROFILE" || -n "$IDENTITY_DIGEST" ]]; then
  if ! python3 - "$IDENTITY_PROFILE" "$IDENTITY_DIGEST" <<'PY'
import re
import sys

name = sys.argv[1]
digest = sys.argv[2]
if not re.fullmatch(r"[a-z][a-z0-9-]{0,47}", name) or not re.fullmatch(r"[0-9a-f]{64}", digest):
    raise SystemExit(1)
PY
  then
    echo "WORKENV_IDENTITY_PROFILE and WORKENV_IDENTITY_DIGEST must be a valid profile name and lowercase sha256 digest" >&2
    exit 1
  fi
fi

status_json="$($HERDR_BIN --session "$SESSION" status server --json 2>/dev/null || true)"
read -r healthy running <<<"$(python3 - "$status_json" <<'PY'
import json
import sys

try:
    status = json.loads(sys.argv[1]) if len(sys.argv) > 1 and sys.argv[1] else {}
except json.JSONDecodeError:
    status = {}
running = bool(status.get("running"))
healthy = (
    running
    and status.get("compatible") is True
    and status.get("restart_needed") is False
    and status.get("server_binary_stale") is False
    and status.get("capabilities", {}).get("detached_server_daemon") is True
)
print(int(healthy), int(running))
PY
)"

if [[ "$healthy" == "1" ]]; then
  execution_json="$($APOC_BIN execution list --status running --limit 100 --format json --purpose "Inspect running workenv Herdr server profile." 2>/dev/null || true)"
  candidate_ids="$(python3 - "$execution_json" "$SESSION" <<'PY'
import json
import sys

try:
    value = json.loads(sys.argv[1]) if len(sys.argv) > 1 and sys.argv[1] else {}
except json.JSONDecodeError:
    value = {}
items = value.get("executions") if isinstance(value, dict) else None
if items is None and isinstance(value, list):
    items = value
if not isinstance(items, list):
    raise SystemExit(0)
name = f"workenv-herdr-server-{sys.argv[2]}"
for item in items:
    if not isinstance(item, dict):
        continue
    if item.get("name") == name and item.get("id"):
        print(item["id"])
PY
  )"
  profile_match=0
  while IFS= read -r execution_id; do
    if [[ -z "$execution_id" ]]; then
      continue
    fi
    execution_get="$($APOC_BIN execution get "$execution_id" --verbosity trace --format json --purpose "Inspect running workenv Herdr server profile." 2>/dev/null || true)"
    candidate_match="$(python3 - "$execution_get" "$SESSION" "$IDENTITY_PROFILE" "$IDENTITY_DIGEST" <<'PY'
import json
import sys

try:
    value = json.loads(sys.argv[1]) if len(sys.argv) > 1 and sys.argv[1] else {}
except json.JSONDecodeError:
    print(0)
    raise SystemExit(0)
if isinstance(value, dict) and isinstance(value.get("data"), dict):
    value = value["data"]
if isinstance(value, dict) and isinstance(value.get("execution"), dict):
    value = value["execution"]
if not isinstance(value, dict) or value.get("status") in {"completed", "failed", "canceled", "cancelled", "skipped"}:
    print(0)
    raise SystemExit(0)
labels = value.get("spec", {}).get("labels", {})
if isinstance(labels, list):
    parsed = {}
    for label in labels:
        if isinstance(label, str) and "=" in label:
            key, raw = label.split("=", 1)
            parsed[key] = raw
    labels = parsed
if not isinstance(labels, dict):
    labels = {}
session = sys.argv[2]
expected_profile = sys.argv[3]
expected_digest = sys.argv[4]
if labels.get("workenv.component") != "herdr-server" or labels.get("herdr.session") != session:
    print(0)
    raise SystemExit(0)
if expected_profile:
    print(int(labels.get("workenv.profile") == expected_profile and labels.get("workenv.profile.digest") == expected_digest))
else:
    print(int("workenv.profile" not in labels and "workenv.profile.digest" not in labels))
PY
    )"
    if [[ "$candidate_match" == "1" ]]; then
      profile_match=1
      break
    fi
  done <<<"$candidate_ids"
  if [[ "$profile_match" == "1" ]]; then
    echo "workenv Herdr server is already healthy"
    exit 0
  fi
fi

if [[ "$running" == "1" ]]; then
  echo "workenv Herdr server is running without the expected APoC profile labels; not starting a duplicate server" >&2
  exit 1
fi

purpose="Start workenv Herdr server at boot through APoC."
server_command=("$HERDR_BIN" --session "$SESSION" server)
profile_labels=()
if [[ -n "$IDENTITY_PROFILE" ]]; then
  server_command=(
    python3 "$workenv_root/remote/profile.py"
    --root "$workenv_root"
    exec
    --name "$IDENTITY_PROFILE"
    --digest "$IDENTITY_DIGEST"
    --
    "$HERDR_BIN" --session "$SESSION" server
  )
  profile_labels=(
    --label "workenv.profile=$IDENTITY_PROFILE"
    --label "workenv.profile.digest=$IDENTITY_DIGEST"
  )
fi
exec "$APOC_BIN" execution start "${server_command[0]}" \
  --cwd "$workenv_root" \
  --pty \
  --env "PATH=$PATH" \
  --name "workenv-herdr-server-$SESSION" \
  --purpose "$purpose" \
  --idempotency-key "workenv-herdr-server-pty:$SESSION:$BOOT_ID" \
  --label "workenv.component=herdr-server" \
  --label "herdr.session=$SESSION" \
  "${profile_labels[@]}" \
  -- \
  "${server_command[@]:1}"
