#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
MANIFEST="$SCRIPT_DIR/../tools.json"
SOURCE_ARCHIVE=""
SOURCE_SHA256=""
PREFIX="/opt/workenv"
BUILD_ROOT="${TMPDIR:-/tmp}/workenv-apoc-build"
LINK_DIR="/usr/local/bin"
JOBS=2
PLAN_ONLY=0
JSON_ONLY=0
INSTALL_PREREQS=0
VALIDATE_SOURCE_ONLY=0
LOCK_DIR=""
SOURCE_SHA_ACTUAL=""

usage() {
  cat <<'USAGE'
Usage: build-apoc.sh [--manifest PATH] [--source-archive PATH]
                     [--source-sha256 HEX] [--prefix PATH]
                     [--link-dir PATH] [--build-root PATH]
                     [--jobs N] [--install-prereqs] [--validate-source-only]
                     [--plan] [--json]

Builds APoC from the pinned source archive with the pinned Rust toolchain.
The default build command is:
  cargo build --release --locked --bin apoc --jobs 2

The script installs /opt/workenv/bin/apoc and writes a replication archive plus
.sha256 sidecar under /opt/workenv/artifacts/apoc. It never builds from a local
dirty checkout.
USAGE
}

while (($#)); do
  case "$1" in
    --manifest) MANIFEST="$2"; shift 2 ;;
    --source-archive) SOURCE_ARCHIVE="$2"; shift 2 ;;
    --source-sha256) SOURCE_SHA256="$2"; shift 2 ;;
    --prefix) PREFIX="$2"; shift 2 ;;
    --link-dir) LINK_DIR="$2"; shift 2 ;;
    --build-root) BUILD_ROOT="$2"; shift 2 ;;
    --jobs) JOBS="$2"; shift 2 ;;
    --install-prereqs) INSTALL_PREREQS=1; shift ;;
    --validate-source-only) VALIDATE_SOURCE_ONLY=1; shift ;;
    --plan) PLAN_ONLY=1; shift ;;
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
print(value)
PY
}

json_report() {
  python3 - "$@" <<'PY'
import json
import sys

pairs = sys.argv[1:]
report = {}
for idx in range(0, len(pairs), 2):
    report[pairs[idx]] = pairs[idx + 1]
print(json.dumps(report, indent=2, sort_keys=True))
PY
}

run_root() {
  "${sudo_cmd[@]}" "$@"
}

sha256_file() {
  python3 - "$1" <<'PY'
import hashlib
import sys

with open(sys.argv[1], "rb") as fh:
    print(hashlib.file_digest(fh, "sha256").hexdigest())
PY
}

sha256_verify() {
  local file="$1"
  local expected="$2"
  local actual
  actual="$(sha256_file "$file")"
  [[ "$actual" == "$expected" ]]
}

ensure_target() {
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
    echo "unsupported target: arch=$arch os=$os_id version=$version_id" >&2
    exit 1
  fi
}

install_prereqs_if_requested() {
  local missing=()
  for cmd in cc pkg-config make curl tar xz awk python3; do
    command -v "$cmd" >/dev/null 2>&1 || missing+=("$cmd")
  done
  if pkg-config --exists openssl 2>/dev/null; then
    :
  else
    missing+=("openssl.pc")
  fi
  if ((${#missing[@]} == 0)); then
    return 0
  fi
  if [[ "$INSTALL_PREREQS" -ne 1 ]]; then
    echo "missing build prerequisites: ${missing[*]}; rerun with --install-prereqs after no other apt mutation is active" >&2
    exit 1
  fi
  run_root apt-get update
  run_root apt-get install -y build-essential ca-certificates curl pkg-config libssl-dev make tar xz-utils
}

prepare_build_root() {
  local resolved build_parent
  case "$BUILD_ROOT" in
    ""|"/"|"/home"|"/home/"*|"${HOME:-__unset__}"|"${HOME:-__unset__}/"*)
      echo "unsafe build root: $BUILD_ROOT" >&2
      exit 1
      ;;
  esac
  if [[ -L "$BUILD_ROOT" ]]; then
    echo "unsafe build root is a symlink: $BUILD_ROOT" >&2
    exit 1
  fi
  build_parent="$(dirname -- "$BUILD_ROOT")"
  mkdir -p "$build_parent"
  resolved="$(cd "$build_parent" && pwd -P)/$(basename -- "$BUILD_ROOT")"
  case "$resolved" in
    ""|"/"|"/home"|"/home/"*|"${HOME:-__unset__}"|"${HOME:-__unset__}/"*)
      echo "unsafe resolved build root: $resolved" >&2
      exit 1
      ;;
  esac
  mkdir -p "$BUILD_ROOT"
  LOCK_DIR="$BUILD_ROOT/.lock"
  if ! mkdir "$LOCK_DIR" 2>/dev/null; then
    echo "build root is locked: $BUILD_ROOT" >&2
    exit 1
  fi
  trap 'rmdir "$LOCK_DIR" 2>/dev/null || true' EXIT
}

install_rustup_toolchain() {
  local rustup_url rustup_sha rustup_bin toolchain rustc_path cargo_path
  rustup_url="$(manifest_value apoc_build.rustup_init_url)"
  rustup_sha="$(manifest_value apoc_build.rustup_init_sha256)"
  toolchain="$(manifest_value apoc_build.rust_toolchain)"
  mkdir -p "$BUILD_ROOT/rustup" "$BUILD_ROOT/cargo/bin" "$BUILD_ROOT/downloads"
  export RUSTUP_HOME="$BUILD_ROOT/rustup"
  export CARGO_HOME="$BUILD_ROOT/cargo"
  export PATH="$CARGO_HOME/bin:$PATH"
  rustup_bin="$BUILD_ROOT/downloads/rustup-init"
  if [[ ! -x "$rustup_bin" ]]; then
    curl --fail --location --proto '=https' --tlsv1.2 --output "$rustup_bin" "$rustup_url"
    sha256_verify "$rustup_bin" "$rustup_sha"
    chmod +x "$rustup_bin"
  else
    sha256_verify "$rustup_bin" "$rustup_sha"
  fi
  "$rustup_bin" -y --no-modify-path --profile minimal --default-toolchain "$toolchain"
  rustc_path="$CARGO_HOME/bin/rustc"
  cargo_path="$CARGO_HOME/bin/cargo"
  "$rustc_path" --version | grep -F "$(manifest_value apoc_build.rust_toolchain)" >/dev/null
  "$cargo_path" --version >/dev/null
}

extract_source() {
  local expected_prefix expected_revision source_dir marker stage_dir
  expected_prefix="$(manifest_value apoc_build.source_archive_prefix)"
  expected_revision="$(manifest_value apoc_build.source_revision)"
  [[ -f "$SOURCE_ARCHIVE" ]] || { echo "source archive not found: $SOURCE_ARCHIVE" >&2; exit 2; }
  if [[ -z "$SOURCE_SHA256" ]]; then
    echo "source archive sha256 is required; pass --source-sha256" >&2
    exit 2
  fi
  SOURCE_SHA_ACTUAL="$(sha256_file "$SOURCE_ARCHIVE")"
  if [[ "$SOURCE_SHA256" != "$SOURCE_SHA_ACTUAL" ]]; then
    echo "source archive sha256 mismatch: expected $SOURCE_SHA256 actual $SOURCE_SHA_ACTUAL" >&2
    exit 1
  fi
  python3 - "$SOURCE_ARCHIVE" "$expected_prefix" "$expected_revision" <<'PY'
import os
import posixpath
import sys
import tarfile

archive, expected_prefix, expected_revision = sys.argv[1:4]
try:
    with tarfile.open(archive, "r:gz") as tar:
        members = tar.getmembers()
        if not members:
            raise SystemExit("source archive is empty")
        comment = tar.pax_headers.get("comment")
        if comment != expected_revision:
            raise SystemExit(f"source archive commit mismatch: {comment!r}")
        for member in members:
            name = member.name
            if name.startswith("/"):
                raise SystemExit(f"absolute archive member: {name}")
            if ".." in name.split("/"):
                raise SystemExit(f"escaping archive member: {name}")
            normalized = posixpath.normpath(name)
            if normalized == "." or normalized.startswith("../") or "/../" in normalized:
                raise SystemExit(f"escaping archive member: {name}")
            root = normalized.split("/", 1)[0]
            if root != expected_prefix:
                raise SystemExit(f"unexpected archive root {root!r} for {name!r}")
            if member.issym() or member.islnk():
                link = member.linkname
                if link.startswith("/"):
                    raise SystemExit(f"absolute archive link target: {name} -> {link}")
                if ".." in link.split("/"):
                    raise SystemExit(f"escaping archive link target: {name} -> {link}")
                link_base = posixpath.dirname(normalized)
                link_normalized = posixpath.normpath(posixpath.join(link_base, link))
                if link_normalized.startswith("../") or "/../" in link_normalized:
                    raise SystemExit(f"escaping archive link target: {name} -> {link}")
                if link_normalized.split("/", 1)[0] != expected_prefix:
                    raise SystemExit(f"archive link leaves root: {name} -> {link}")
except tarfile.TarError as exc:
    raise SystemExit(f"invalid source archive: {exc}") from exc
PY
  source_dir="$BUILD_ROOT/sources/$SOURCE_SHA_ACTUAL"
  marker="$source_dir/.workenv-source-sha256"
  if [[ -d "$source_dir" && -f "$marker" && "$(cat "$marker")" == "$SOURCE_SHA_ACTUAL" ]]; then
    BUILD_SOURCE_DIR="$source_dir"
    return 0
  fi
  if [[ -e "$source_dir" && ! -d "$source_dir" ]]; then
    echo "source path exists and is not a directory: $source_dir" >&2
    exit 1
  fi
  if [[ -e "$source_dir" ]]; then
    echo "source directory exists without matching marker: $source_dir" >&2
    exit 1
  fi
  stage_dir="$(mktemp -d "$BUILD_ROOT/source-stage-$SOURCE_SHA_ACTUAL.XXXXXX")"
  tar -xzf "$SOURCE_ARCHIVE" -C "$stage_dir" --strip-components=1
  printf '%s\n' "$SOURCE_SHA_ACTUAL" > "$stage_dir/.workenv-source-sha256"
  mkdir -p "$BUILD_ROOT/sources"
  mv "$stage_dir" "$source_dir"
  BUILD_SOURCE_DIR="$source_dir"
}

verify_cargo_metadata() {
  local cargo expected_version expected_rust metadata
  cargo="$BUILD_ROOT/cargo/bin/cargo"
  expected_version="$(manifest_value apoc_build.package_version)"
  expected_rust="$(manifest_value apoc_build.rust_version_requirement)"
  metadata="$("$cargo" metadata --locked --format-version 1 --no-deps --manifest-path "$BUILD_SOURCE_DIR/Cargo.toml")"
  python3 - "$metadata" "$expected_version" "$expected_rust" <<'PY'
import json
import sys

metadata = json.loads(sys.argv[1])
expected_version = sys.argv[2]
expected_rust = sys.argv[3]
matches = [
    package for package in metadata["packages"]
    if package["name"] == "apoc"
]
if not matches:
    raise SystemExit("Cargo metadata has no apoc package")
package = matches[0]
if package["version"] != expected_version:
    raise SystemExit(f"apoc package version mismatch: {package['version']}")
if package.get("rust_version") != expected_rust:
    raise SystemExit(f"apoc rust-version mismatch: {package.get('rust_version')}")
PY
}

build_and_install() {
  local cargo target release_binary artifact_dir archive archive_name metadata_file binary_sha archive_sha archive_stage expected_version probe_purpose probe_output
  cargo="$BUILD_ROOT/cargo/bin/cargo"
  target="$(manifest_value apoc_build.target)"
  expected_version="$(manifest_value apoc_build.package_version)"
  artifact_dir="$PREFIX/artifacts/apoc"
  archive_name="apoc-$expected_version+$(manifest_value apoc_build.source_short_revision)-$target.tar.gz"
  archive="$artifact_dir/$archive_name"
  "$cargo" build --release --locked --bin apoc --jobs "$JOBS" --manifest-path "$BUILD_SOURCE_DIR/Cargo.toml"
  release_binary="$BUILD_SOURCE_DIR/target/release/apoc"
  probe_purpose="Verify newly built worker APoC."
  if ! probe_output="$("$release_binary" version --purpose "$probe_purpose" --format json 2>&1)"; then
    printf '%s\n' "$probe_output" >&2
    exit 1
  fi
  python3 - "$probe_output" "$expected_version" <<'PY'
import json
import sys

payload = json.loads(sys.argv[1])
expected_version = sys.argv[2]
actual = str(payload.get("version", ""))
if actual != expected_version:
    raise SystemExit(f"built apoc version mismatch: {actual}")
PY
  binary_sha="$(sha256_file "$release_binary")"
  run_root install -d -m 0755 "$PREFIX/bin" "$LINK_DIR" "$artifact_dir"
  run_root install -m 0755 "$release_binary" "$PREFIX/bin/apoc"
  run_root ln -sfn "$PREFIX/bin/apoc" "$LINK_DIR/apoc"

  metadata_file="$BUILD_ROOT/build-metadata.json"
  json_report \
    source_revision "$(manifest_value apoc_build.source_revision)" \
    source_archive "$SOURCE_ARCHIVE" \
    source_sha256 "$SOURCE_SHA_ACTUAL" \
    rust_toolchain "$("$BUILD_ROOT/cargo/bin/rustc" --version)" \
    cargo_version "$("$cargo" --version)" \
    apoc_probe "$probe_output" \
    build_command "cargo build --release --locked --bin apoc --jobs $JOBS" \
    binary_sha256 "$binary_sha" \
    installed_binary "$PREFIX/bin/apoc" > "$metadata_file"

  archive_stage="$BUILD_ROOT/archive-$SOURCE_SHA_ACTUAL"
  if [[ -e "$archive_stage" && ! -d "$archive_stage" ]]; then
    echo "archive stage exists and is not a directory: $archive_stage" >&2
    exit 1
  fi
  mkdir -p "$archive_stage/bin"
  install -m 0755 "$release_binary" "$archive_stage/bin/apoc"
  cp "$metadata_file" "$archive_stage/build-metadata.json"
  tar -C "$archive_stage" -czf "$BUILD_ROOT/$archive_name" .
  archive_sha="$(sha256_file "$BUILD_ROOT/$archive_name")"
  run_root install -m 0644 "$BUILD_ROOT/$archive_name" "$archive"
  printf '%s  %s\n' "$archive_sha" "$archive_name" > "$BUILD_ROOT/$archive_name.sha256"
  run_root install -m 0644 "$BUILD_ROOT/$archive_name.sha256" "$archive.sha256"

  json_report \
    status installed \
    source_revision "$(manifest_value apoc_build.source_revision)" \
    source_sha256 "$SOURCE_SHA_ACTUAL" \
    rust_toolchain "$("$BUILD_ROOT/cargo/bin/rustc" --version)" \
    cargo_version "$("$cargo" --version)" \
    installed_binary "$PREFIX/bin/apoc" \
    binary_sha256 "$binary_sha" \
    archive "$archive" \
    archive_sha256 "$archive_sha"
}

plan_json() {
  SOURCE_ARCHIVE="${SOURCE_ARCHIVE:-$(manifest_value apoc_build.source_archive_default)}"
  if [[ "$SOURCE_ARCHIVE" != /* ]]; then
    SOURCE_ARCHIVE="$(cd "$SCRIPT_DIR/.." && pwd)/$SOURCE_ARCHIVE"
  fi
  json_report \
    status plan \
    source_archive "$SOURCE_ARCHIVE" \
    source_revision "$(manifest_value apoc_build.source_revision)" \
    source_archive_prefix "$(manifest_value apoc_build.source_archive_prefix)" \
    rust_toolchain "$(manifest_value apoc_build.rust_toolchain)" \
    rustup_init_sha256 "$(manifest_value apoc_build.rustup_init_sha256)" \
    package_version "$(manifest_value apoc_build.package_version)" \
    build_command "cargo build --release --locked --bin apoc --jobs $JOBS" \
    prefix "$PREFIX"
}

main() {
  SOURCE_ARCHIVE="${SOURCE_ARCHIVE:-$(manifest_value apoc_build.source_archive_default)}"
  if [[ "$SOURCE_ARCHIVE" != /* ]]; then
    SOURCE_ARCHIVE="$(cd "$SCRIPT_DIR/.." && pwd)/$SOURCE_ARCHIVE"
  fi
  if [[ "$PLAN_ONLY" -eq 1 ]]; then
    plan_json
    return 0
  fi
  prepare_build_root
  extract_source
  if [[ "$VALIDATE_SOURCE_ONLY" -eq 1 ]]; then
    json_report \
      status source_valid \
      source_archive "$SOURCE_ARCHIVE" \
      source_revision "$(manifest_value apoc_build.source_revision)" \
      source_sha256 "$SOURCE_SHA_ACTUAL" \
      source_dir "$BUILD_SOURCE_DIR"
    return 0
  fi
  ensure_target
  install_prereqs_if_requested
  install_rustup_toolchain
  verify_cargo_metadata
  build_and_install
}

main "$@"
