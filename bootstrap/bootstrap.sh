#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
MANIFEST="$SCRIPT_DIR/../tools.json"
PREFIX="/opt/workenv"
LINK_DIR="/usr/local/bin"
BOOT_SERVICE_NAME="workenv-herdr-bootstrap.service"
DRY_RUN=0
HEALTH_ONLY=0
JSON_ONLY=0

usage() {
  cat <<'USAGE'
Usage: bootstrap.sh [--manifest PATH] [--prefix PATH] [--link-dir PATH]
                    [--dry-run] [--health-only] [--json]

Prepares an Ubuntu 24.04 x86_64 worker host for the shared workenv devenv.

The bootstrap installs host prerequisites, installs pinned Nix and devenv, enables
an existing tailscaled.service when present, and installs the Herdr boot helper
unit. Shared tools such as Herdr, APoC, DevSQL, Codex, Claude, Grok, Pi, Nib, and
ssh-clipboard are provided by the repository devenv, not by this script.

The script does not enroll Tailscale, sync home directories, write credential
values, install shared tool binaries directly, or start Herdr during bootstrap.
USAGE
}

while (($#)); do
  case "$1" in
    --manifest) MANIFEST="$2"; shift 2 ;;
    --prefix) PREFIX="$2"; shift 2 ;;
    --link-dir) LINK_DIR="$2"; shift 2 ;;
    --dry-run) DRY_RUN=1; shift ;;
    --health-only) HEALTH_ONLY=1; shift ;;
    --json) JSON_ONLY=1; shift ;;
    -h|--help) usage; exit 0 ;;
    *) echo "unknown argument: $1" >&2; usage >&2; exit 2 ;;
  esac
done

if [[ ! -f "$MANIFEST" ]]; then
  echo "manifest not found: $MANIFEST" >&2
  exit 2
fi

sudo_cmd=()
if [[ ${EUID:-$(id -u)} -ne 0 ]]; then
  sudo_cmd=(sudo -n)
fi

log() {
  if [[ "$JSON_ONLY" -eq 0 ]]; then
    printf '%s\n' "$*" >&2
  fi
}

manifest_value() {
  local path="$1"
  python3 - "$MANIFEST" "$path" <<'PY'
import json
import sys

manifest_path, dotted_path = sys.argv[1:3]
with open(manifest_path, "r", encoding="utf-8") as fh:
    value = json.load(fh)
for part in dotted_path.split("."):
    value = value[part]
print(json.dumps(value, separators=(",", ":")) if isinstance(value, (dict, list)) else value)
PY
}

run_root() {
  if [[ "$DRY_RUN" -eq 1 ]]; then
    log "dry-run: ${sudo_cmd[*]} $*"
    return 0
  fi
  "${sudo_cmd[@]}" "$@"
}

run_user() {
  if [[ "$DRY_RUN" -eq 1 ]]; then
    log "dry-run: $*"
    return 0
  fi
  "$@"
}

command_version_matches() {
  local cmd="$1"
  local expected="$2"
  shift 2
  command -v "$cmd" >/dev/null 2>&1 || return 1
  local output
  output="$("$cmd" "$@" 2>/dev/null || true)"
  [[ "$output" == *"$expected"* ]]
}

ensure_target_or_dry_run() {
  local arch os_id version_id
  arch="$(uname -m)"
  os_id=""
  version_id=""
  if [[ -r /etc/os-release ]]; then
    # shellcheck disable=SC1091
    . /etc/os-release
    os_id="${ID:-}"
    version_id="${VERSION_ID:-}"
  fi

  if [[ "$arch" != "$(manifest_value target.architecture)" || "$os_id" != "$(manifest_value target.os_id)" || "$version_id" != "$(manifest_value target.version_id)" ]]; then
    if [[ "$DRY_RUN" -eq 1 || "$HEALTH_ONLY" -eq 1 ]]; then
      log "target mismatch: arch=$arch os=$os_id version=$version_id"
      return 0
    fi
    echo "unsupported target: arch=$arch os=$os_id version=$version_id" >&2
    exit 1
  fi
}

sha256_verify() {
  local file="$1"
  local expected="${2#sha256:}"
  local actual
  actual="$(sha256sum "$file" | awk '{print $1}')"
  [[ "$actual" == "$expected" ]]
}

download_to() {
  local url="$1"
  local dest="$2"
  run_user curl --fail --location --proto '=https' --tlsv1.2 --output "$dest" "$url"
}

install_apt_prereqs() {
  log "installing host prerequisites"
  run_root apt-get update
  run_root apt-get install -y ca-certificates curl gzip openssl python3 tar xz-utils
}

source_nix_profile() {
  export PATH="/nix/var/nix/profiles/default/bin:/nix/var/nix/profiles/per-user/${USER:-root}/profile/bin:$PATH"
  if [[ -r /nix/var/nix/profiles/default/etc/profile.d/nix-daemon.sh ]]; then
    # shellcheck disable=SC1091
    . /nix/var/nix/profiles/default/etc/profile.d/nix-daemon.sh
  fi
}

install_nix() {
  local expected url sha tmp
  source_nix_profile
  expected="$(manifest_value nix.version)"
  if command_version_matches nix "$expected" --version; then
    log "nix $expected already installed"
    return 0
  fi
  if [[ "$DRY_RUN" -eq 1 ]]; then
    log "dry-run: would install nix daemon $expected"
    return 0
  fi

  url="$(manifest_value nix.install_url)"
  sha="$(manifest_value nix.install_sha256)"
  tmp="$(mktemp -d)"
  download_to "$url" "$tmp/install-nix"
  sha256_verify "$tmp/install-nix" "$sha"
  log "installing nix daemon $expected"
  run_user sh "$tmp/install-nix" --daemon --yes --no-channel-add
  source_nix_profile
}

install_devenv() {
  local expected flake_ref profile nix_bin
  source_nix_profile
  expected="$(manifest_value devenv.version)"
  if command_version_matches devenv "$expected" version; then
    log "devenv $expected already installed"
    return 0
  fi
  if [[ "$DRY_RUN" -eq 1 ]]; then
    log "dry-run: would install devenv $expected"
    return 0
  fi
  command -v nix >/dev/null 2>&1 || { log "devenv skipped: nix missing"; return 0; }
  nix_bin="$(command -v nix)"
  flake_ref="$(manifest_value devenv.flake_ref)"
  profile="$PREFIX/nix-profile"
  log "installing devenv $expected from pinned flake"
  run_root install -d -m 0755 "$PREFIX"
  run_root "$nix_bin" --extra-experimental-features "nix-command flakes" profile install --accept-flake-config --profile "$profile" "$flake_ref"
  run_root install -d -m 0755 "$LINK_DIR"
  run_root ln -sfn "$profile/bin/devenv" "$LINK_DIR/devenv"
}

enable_tailscale_service_if_present() {
  local units
  if ! command -v systemctl >/dev/null 2>&1; then
    log "tailscale service skipped: systemctl missing"
    return 0
  fi
  units="$(systemctl list-unit-files --no-legend tailscaled.service 2>/dev/null || true)"
  if [[ "$units" != tailscaled.service* ]]; then
    log "tailscale service skipped: tailscaled.service missing"
    return 0
  fi
  log "enabling tailscaled.service without enrollment"
  if [[ "$DRY_RUN" -eq 1 ]]; then
    log "dry-run: would enable tailscaled.service"
    return 0
  fi
  run_root systemctl enable --now tailscaled.service
}

workenv_boot_service_user() {
  if [[ ${EUID:-$(id -u)} -eq 0 ]]; then
    if [[ -n "${SUDO_USER:-}" && "${SUDO_USER:-}" != "root" ]]; then
      printf '%s\n' "$SUDO_USER"
      return 0
    fi
    printf '%s\n' "${USER:-root}"
    return 0
  fi
  id -un
}

install_workenv_herdr_boot_service() {
  local helper_src helper_dest service_file service_tmp service_user
  helper_src="$SCRIPT_DIR/workenv-herdr-bootstrap.sh"
  helper_dest="$PREFIX/bin/workenv-herdr-bootstrap"
  service_file="/etc/systemd/system/$BOOT_SERVICE_NAME"
  service_user="$(workenv_boot_service_user)"

  if [[ ! -f "$helper_src" ]]; then
    echo "missing Herdr boot helper: $helper_src" >&2
    exit 1
  fi
  if ! command -v systemctl >/dev/null 2>&1; then
    log "workenv Herdr boot service skipped: systemctl missing"
    return 0
  fi
  if [[ "$DRY_RUN" -eq 1 ]]; then
    log "dry-run: would install $BOOT_SERVICE_NAME for $service_user"
    log "dry-run: would enable $BOOT_SERVICE_NAME"
    return 0
  fi

  run_root install -d -m 0755 "$PREFIX/bin" /etc/systemd/system
  run_root install -m 0755 "$helper_src" "$helper_dest"
  service_tmp="$(mktemp)"
  cat > "$service_tmp" <<SERVICE
[Unit]
Description=Start workenv Herdr server through shared devenv tools
Wants=network-online.target
After=network-online.target

[Service]
Type=oneshot
User=$service_user
Environment=PATH=$LINK_DIR:$PREFIX/bin:/usr/local/bin:/usr/bin:/bin
ExecStart=$helper_dest
RemainAfterExit=yes

[Install]
WantedBy=multi-user.target
SERVICE
  run_root install -m 0644 "$service_tmp" "$service_file"
  rm -f "$service_tmp"
  run_root systemctl daemon-reload
  run_root systemctl enable "$BOOT_SERVICE_NAME"
}

health_json() {
  python3 - "$MANIFEST" "$PREFIX" "$LINK_DIR" <<'PY'
import json
import platform
import shutil
import subprocess
import sys

manifest_path, prefix, link_dir = sys.argv[1:4]
with open(manifest_path, "r", encoding="utf-8") as fh:
    manifest = json.load(fh)

def os_release():
    values = {}
    try:
        with open("/etc/os-release", "r", encoding="utf-8") as fh:
            for line in fh:
                if "=" in line:
                    key, value = line.rstrip("\n").split("=", 1)
                    values[key] = value.strip('"')
    except FileNotFoundError:
        pass
    return values

def run_version(name, args, expected):
    path = shutil.which(name)
    item = {"name": name, "expected": expected, "path": path, "installed": bool(path), "ok": False, "version_output": None}
    if not path:
        item["reason"] = "missing"
        return item
    try:
        result = subprocess.run([path, *args], text=True, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, timeout=10, check=False)
        item["version_output"] = result.stdout.strip()
        item["ok"] = result.returncode == 0 and expected in item["version_output"]
        if not item["ok"]:
            item["reason"] = "version_mismatch"
    except Exception as exc:
        item["reason"] = f"version_probe_failed:{exc.__class__.__name__}"
    return item

def tailscale_service():
    systemctl = shutil.which("systemctl")
    if not systemctl:
        return {"name": "tailscaled.service", "available": False, "enabled": False, "active": False, "reason": "systemctl_missing"}
    try:
        listed = subprocess.run([systemctl, "list-unit-files", "--no-legend", "tailscaled.service"], text=True, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, timeout=10, check=False)
        available = listed.stdout.startswith("tailscaled.service")
        enabled = subprocess.run([systemctl, "is-enabled", "tailscaled.service"], text=True, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, timeout=10, check=False).returncode == 0 if available else False
        active = subprocess.run([systemctl, "is-active", "tailscaled.service"], text=True, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, timeout=10, check=False).returncode == 0 if available else False
        return {"name": "tailscaled.service", "available": available, "enabled": enabled, "active": active}
    except Exception as exc:
        return {"name": "tailscaled.service", "available": False, "enabled": False, "active": False, "reason": f"probe_failed:{exc.__class__.__name__}"}

osr = os_release()
target = manifest["target"]
platform_info = {"system": platform.system().lower(), "machine": platform.machine(), "os_id": osr.get("ID"), "version_id": osr.get("VERSION_ID")}
platform_supported = platform_info == {"system": "linux", "machine": target["architecture"], "os_id": target["os_id"], "version_id": target["version_id"]}
prerequisites = [run_version("nix", ["--version"], manifest["nix"]["version"]), run_version("devenv", ["version"], manifest["devenv"]["version"])]
missing = [item["name"] for item in prerequisites if not item["ok"]]
service = tailscale_service()
if not service["available"]:
    missing.append("tailscaled.service")

print(json.dumps({
    "schema": 1,
    "target": target,
    "platform": platform_info,
    "platform_supported": platform_supported,
    "prefix": prefix,
    "link_dir": link_dir,
    "prerequisites": prerequisites,
    "tailscale_service": service,
    "missing_prerequisites": missing,
    "credentials": "preserved",
    "tailscale_enrollment": "not_configured_by_bootstrap",
    "shared_tools": "provided_by_devenv",
}, indent=2, sort_keys=True))
PY
}

main() {
  ensure_target_or_dry_run
  source_nix_profile
  if [[ "$HEALTH_ONLY" -eq 1 ]]; then
    health_json
    return 0
  fi

  install_apt_prereqs
  install_nix
  source_nix_profile
  install_devenv
  install_workenv_herdr_boot_service
  enable_tailscale_service_if_present
  health_json
}

main "$@"
