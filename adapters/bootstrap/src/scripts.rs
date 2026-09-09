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
sudo_cmd=
if [ "$(id -u)" -ne 0 ]; then sudo_cmd="sudo -n"; fi
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
  $sudo_cmd install -d -m 0755 "$prefix" "$link_dir"
  nix --extra-experimental-features "nix-command flakes" profile install --accept-flake-config --profile "$prefix/nix-profile" "$devenv_flake"
  $sudo_cmd ln -sfn "$prefix/nix-profile/bin/devenv" "$link_dir/devenv"
fi
{tool_installs}
"#,
        prefix = shell_quote(&config.prefix),
        link_dir = shell_quote(&config.link_dir),
        nix_version = shell_quote(&config.nix_version),
        nix_url = shell_quote(&config.nix_url),
        nix_sha = shell_quote(&config.nix_sha256),
        devenv_version = shell_quote(&config.devenv_version),
        devenv_flake = shell_quote(&config.devenv_flake)
    )
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
  $sudo_cmd install -m 0755 "$tmp_tool" "$link_dir/$tool_name"
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
