use crate::config::{BootstrapConfig, SeedTool};

pub(crate) fn probe_script(config: &BootstrapConfig) -> String {
    let tool_probes = config
        .seed_tools
        .iter()
        .map(tool_probe_script)
        .collect::<String>();
    format!(
        r#"set -eu
link_dir={link_dir}
PATH="/nix/var/nix/profiles/default/bin:$link_dir:$PATH"
system="$(uname -s 2>/dev/null || true)"
machine="$(uname -m 2>/dev/null || true)"
nix_output="$(nix --version 2>&1 || true)"
devenv_output="$(devenv version 2>&1 || true)"
printf 'system\t%s\n' "$system"
printf 'machine\t%s\n' "$machine"
printf 'nix\t%s\n' "$nix_output"
printf 'devenv\t%s\n' "$devenv_output"
{tool_probes}
"#,
        link_dir = shell_quote(&config.link_dir)
    )
}

fn tool_probe_script(tool: &SeedTool) -> String {
    format!(
        r#"if path="$(command -v {name} 2>/dev/null)"; then
  hash="$(shasum -a 256 "$path" | awk '{{print $1}}')"
else
  path=""
  hash="missing"
fi
printf 'tool.{raw_name}\t%s\t%s\n' "$path" "$hash"
"#,
        name = shell_quote(&tool.name),
        raw_name = tool.name
    )
}

pub(crate) fn install_script(config: &BootstrapConfig) -> String {
    let tool_installs = config
        .seed_tools
        .iter()
        .map(tool_install_script)
        .collect::<String>();
    format!(
        r#"set -eu
prefix={prefix}
link_dir={link_dir}
nix_version={nix_version}
nix_url={nix_url}
nix_sha={nix_sha}
devenv_version={devenv_version}
devenv_flake={devenv_flake}
{writable_paths}
prefix_sudo=
link_sudo=
if sudo_for_path "$prefix"; then prefix_sudo="sudo -n"; fi
if sudo_for_path "$link_dir"; then link_sudo="sudo -n"; fi
PATH="/nix/var/nix/profiles/default/bin:$link_dir:$PATH"
if ! nix --version 2>/dev/null | grep -F "$nix_version" >/dev/null; then
  tmp="$(mktemp -d)"
  trap 'rm -rf "$tmp"' EXIT
  curl --fail --location --proto '=https' --tlsv1.2 --output "$tmp/install-nix" "$nix_url"
  actual="$(shasum -a 256 "$tmp/install-nix" | awk '{{print $1}}')"
  if [ "$actual" != "$nix_sha" ]; then echo "nix installer sha256 mismatch" >&2; exit 1; fi
  sh "$tmp/install-nix" --daemon --yes --no-channel-add
fi
if [ -r /nix/var/nix/profiles/default/etc/profile.d/nix-daemon.sh ]; then
  . /nix/var/nix/profiles/default/etc/profile.d/nix-daemon.sh
fi
if ! devenv version 2>/dev/null | grep -F "$devenv_version" >/dev/null; then
  maybe_configure_devenv_cachix /etc/nix/nix.conf
  $prefix_sudo install -d -m 0755 "$prefix"
  $link_sudo install -d -m 0755 "$link_dir"
  $prefix_sudo nix --extra-experimental-features "nix-command flakes" profile install --accept-flake-config --profile "$prefix/nix-profile" "$devenv_flake"
  $link_sudo ln -sfn "$prefix/nix-profile/bin/devenv" "$link_dir/devenv"
fi
{tool_installs}
"#,
        prefix = shell_quote(&config.prefix),
        link_dir = shell_quote(&config.link_dir),
        nix_version = shell_quote(&config.nix_version),
        nix_url = shell_quote(&config.nix_url),
        nix_sha = shell_quote(&config.nix_sha256),
        devenv_version = shell_quote(&config.devenv_version),
        devenv_flake = shell_quote(&config.devenv_flake),
        writable_paths = devenv_cache_setup_script()
    )
}

pub(crate) fn devenv_cache_setup_script() -> String {
    format!(
        "{}
{}",
        writable_paths_script(),
        DEVENV_CACHE_SETUP_SCRIPT
    )
}

const DEVENV_CACHE_SETUP_SCRIPT: &str = r#"devenv_cache_substituter=https://devenv.cachix.org
devenv_cache_key=devenv.cachix.org-1:w1cLUi8dv3hnoSPGAuibQv+f9TZLr6cv/Hm9XgU50cw=

nix_conf_has_value_awk() {
  awk -v key="$1" -v value="$2" '
    /^[[:space:]]*#/ { next }
    {
      line = $0
      sub(/[[:space:]]*#.*/, "", line)
      if (line !~ /=/) { next }
      split(line, parts, "=")
      name = parts[1]
      gsub(/^[[:space:]]+|[[:space:]]+$/, "", name)
      if (name != key) { next }
      values = line
      sub(/^[^=]*=/, "", values)
      count = split(values, tokens, /[[:space:]]+/)
      for (i = 1; i <= count; i++) {
        if (tokens[i] == value) { found = 1 }
      }
    }
    END { exit found ? 0 : 1 }
  '
}

nix_conf_has_value() {
  conf="$1"
  key="$2"
  value="$3"
  if [ ! -e "$conf" ]; then return 1; fi
  if [ -r "$conf" ]; then
    nix_conf_has_value_awk "$key" "$value" < "$conf"
  else
    sudo -n cat "$conf" | nix_conf_has_value_awk "$key" "$value"
  fi
}

nix_conf_admin_available() {
  conf="$1"
  dir="$(dirname "$conf")"
  if [ "$(id -u)" -eq 0 ]; then return 0; fi
  if { [ -e "$conf" ] && [ -w "$conf" ]; } || { [ ! -e "$conf" ] && can_write_path "$dir"; }; then return 0; fi
  command -v sudo >/dev/null 2>&1 && sudo -n true >/dev/null 2>&1
}

nix_conf_install_dir() {
  dir="$1"
  if [ "$(id -u)" -eq 0 ] || can_write_path "$(dirname "$dir")"; then
    install -d -m 0755 "$dir"
  else
    sudo -n install -d -m 0755 "$dir"
  fi
}

nix_conf_append_line() {
  conf="$1"
  line="$2"
  if [ "$(id -u)" -eq 0 ] || [ -w "$conf" ]; then
    printf '\n%s\n' "$line" >> "$conf"
  else
    printf '\n%s\n' "$line" | sudo -n tee -a "$conf" >/dev/null
  fi
}

reload_nix_daemon_config() {
  system="$(uname -s 2>/dev/null || true)"
  if [ "$system" = Darwin ] && command -v launchctl >/dev/null 2>&1; then
    if [ "$(id -u)" -eq 0 ]; then
      launchctl stop org.nixos.nix-daemon >/dev/null 2>&1 || true
      launchctl start org.nixos.nix-daemon >/dev/null 2>&1 || true
    else
      sudo -n launchctl stop org.nixos.nix-daemon >/dev/null 2>&1 || true
      sudo -n launchctl start org.nixos.nix-daemon >/dev/null 2>&1 || true
    fi
    return 0
  fi
  if command -v systemctl >/dev/null 2>&1; then
    if [ "$(id -u)" -eq 0 ]; then
      systemctl restart nix-daemon.service >/dev/null 2>&1
    else
      sudo -n systemctl restart nix-daemon.service >/dev/null 2>&1
    fi
    return $?
  fi
  return 1
}

configure_devenv_cachix() {
  conf="$1"
  dir="$(dirname "$conf")"
  if ! nix_conf_admin_available "$conf"; then return 1; fi
  nix_conf_install_dir "$dir"
  if [ ! -e "$conf" ]; then
    if [ "$(id -u)" -eq 0 ] || can_write_path "$dir"; then
      : > "$conf"
    else
      sudo -n sh -c 'umask 022; : > "$1"' sh "$conf"
    fi
  fi
  changed=0
  if ! nix_conf_has_value "$conf" extra-substituters "$devenv_cache_substituter"; then
    nix_conf_append_line "$conf" "extra-substituters = $devenv_cache_substituter"
    changed=1
  fi
  if ! nix_conf_has_value "$conf" extra-trusted-public-keys "$devenv_cache_key"; then
    nix_conf_append_line "$conf" "extra-trusted-public-keys = $devenv_cache_key"
    changed=1
  fi
  if [ "$changed" = 1 ]; then
    if ! reload_nix_daemon_config; then
      echo "workenv bootstrap: updated devenv Cachix config but could not restart nix-daemon; cache activation is unconfirmed" >&2
    fi
  fi
}

maybe_configure_devenv_cachix() {
  if ! configure_devenv_cachix "$1"; then
    echo "workenv bootstrap: skipping devenv Cachix setup; root/passwordless sudo unavailable" >&2
  fi
}
"#;

pub(crate) fn can_install_without_privilege_script(
    config: &BootstrapConfig,
    report: &serde_json::Value,
) -> String {
    let nix_ready = report["nix"]["ok"] == true;
    format!(
        r#"set -eu
prefix={prefix}
link_dir={link_dir}
{writable_paths}
if [ {nix_ready} != true ]; then exit 1; fi
can_write_path "$prefix" && can_write_path "$link_dir"
"#,
        prefix = shell_quote(&config.prefix),
        link_dir = shell_quote(&config.link_dir),
        nix_ready = if nix_ready { "true" } else { "false" },
        writable_paths = writable_paths_script()
    )
}

fn writable_paths_script() -> &'static str {
    r#"can_write_path() {
  path="$1"
  while [ ! -e "$path" ]; do
    parent="$(dirname "$path")"
    if [ "$parent" = "$path" ]; then return 1; fi
    path="$parent"
  done
  [ -d "$path" ] && [ -w "$path" ] && [ -x "$path" ]
}
sudo_for_path() {
  if [ "$(id -u)" -eq 0 ]; then return 1; fi
  if can_write_path "$1"; then return 1; fi
  return 0
}"#
}

fn tool_install_script(tool: &SeedTool) -> String {
    format!(
        r#"tool_name={name}
tool_source={source}
tool_sha={sha}
tool_path="$(command -v "$tool_name" 2>/dev/null || true)"
if [ -n "$tool_path" ]; then
  tool_actual="$(shasum -a 256 "$tool_path" | awk '{{print $1}}')"
else
  tool_actual=missing
fi
if [ "$tool_actual" != "$tool_sha" ]; then
  tmp_tool="$(mktemp)"
  case "$tool_source" in
    http://*|https://*) curl --fail --location --proto '=https' --tlsv1.2 --output "$tmp_tool" "$tool_source" ;;
    *) cp "$tool_source" "$tmp_tool" ;;
  esac
  actual="$(shasum -a 256 "$tmp_tool" | awk '{{print $1}}')"
  if [ "$actual" != "$tool_sha" ]; then echo "$tool_name sha256 mismatch" >&2; exit 1; fi
  $link_sudo install -d -m 0755 "$link_dir"
  $link_sudo install -m 0755 "$tmp_tool" "$link_dir/$tool_name"
  rm -f "$tmp_tool"
fi
"#,
        name = shell_quote(&tool.name),
        source = shell_quote(&tool.source),
        sha = shell_quote(&tool.sha256)
    )
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}
