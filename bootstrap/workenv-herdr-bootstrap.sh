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

if [[ -z "$BOOT_ID" ]]; then
  if [[ ! -r /proc/sys/kernel/random/boot_id ]]; then
    echo "cannot derive per-boot idempotency key: /proc/sys/kernel/random/boot_id is missing" >&2
    exit 1
  fi
  BOOT_ID="$(cat /proc/sys/kernel/random/boot_id)"
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
  echo "workenv Herdr server is already healthy"
  exit 0
fi

if [[ "$running" == "1" ]]; then
  echo "workenv Herdr server is running but not healthy; not starting a duplicate server" >&2
  exit 1
fi

purpose="Start workenv Herdr server at boot through APoC."
server_command=("$HERDR_BIN" --session "$SESSION" server)
exec "$APOC_BIN" execution start "${server_command[0]}" \
  --cwd "$workenv_root" \
  --pty \
  --env "PATH=$PATH" \
  --name "workenv-herdr-server-$SESSION" \
  --purpose "$purpose" \
  --idempotency-key "workenv-herdr-server-pty:$SESSION:$BOOT_ID" \
  --label "workenv.component=herdr-server" \
  --label "herdr.session=$SESSION" \
  -- \
  "${server_command[@]:1}"
