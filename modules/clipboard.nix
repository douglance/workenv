{
  lib,
  pkgs,
  config,
  ...
}:

let
  cfg = config.workenv.clipboard;
  workspaceManifest = builtins.fromTOML (builtins.readFile ../Cargo.toml);
  object = properties: required: {
    type = "object";
    additionalProperties = true;
    inherit properties required;
  };
  output = {
    type = "object";
    additionalProperties = true;
  };
  operations = {
    config = {
      description = "Prepare ssh-clipboard configuration and state files.";
      mutating = true;
      input_schema = object {
        node_name.type = "string";
      } [ ];
      output_schema = output;
    };
    apply = {
      description = "Apply ssh-clipboard configuration and state files.";
      mutating = true;
      input_schema = object {
        node_name.type = "string";
      } [ ];
      output_schema = output;
    };
    inspect = {
      description = "Inspect ssh-clipboard runtime status.";
      mutating = false;
      input_schema = object { } [ ];
      output_schema = output;
    };
  };

  ensureConfig = pkgs.writeShellScript "workenv-clipboard-ensure-config" ''
        set -eu

        config_dir="''${SSH_CLIPBOARD_CONFIG_DIR:-$PWD/.state/clipboard/config}"
        state_dir="''${SSH_CLIPBOARD_STATE_DIR:-$PWD/.state/clipboard/state}"
        node_name="''${WORKENV_CLIPBOARD_NODE_NAME:-$(hostname -s)}"

        mkdir -p "$config_dir" "$state_dir"
        chmod 700 "$config_dir" "$state_dir"

        if [ ! -s "$state_dir/node-id" ]; then
          ${pkgs.python3}/bin/python3 - <<'PY' > "$state_dir/node-id"
    import uuid
    print(uuid.uuid4())
    PY
          chmod 600 "$state_dir/node-id"
        fi

        if [ ! -f "$config_dir/config.json" ]; then
          tmp="$(mktemp "$config_dir/config.json.XXXXXX")"
          ${pkgs.python3}/bin/python3 - "$tmp" "$state_dir/node-id" "$node_name" <<'PY'
    import json
    import pathlib
    import sys

    output = pathlib.Path(sys.argv[1])
    node_id = pathlib.Path(sys.argv[2]).read_text(encoding="utf-8").strip()
    node_name = sys.argv[3]
    config = {
        "version": 1,
        "node_id": node_id,
        "node_name": node_name,
        "peers": [],
        "max_bytes": 268435456,
        "poll_interval_ms": 75,
        "headless_x11": True,
    }
    output.write_text(json.dumps(config, sort_keys=True, separators=(",", ":")) + "\n", encoding="utf-8")
    PY
          chmod 600 "$tmp"
          mv "$tmp" "$config_dir/config.json"
        fi
  '';
in
{
  imports = [
    ./rust.nix
    ./tools.nix
  ];

  options.workenv.clipboard = {
    enable = lib.mkEnableOption "clipboard integration adapter";

    enableProcesses = lib.mkOption {
      type = lib.types.bool;
      default = true;
      description = "Declare Xvfb and ssh-clipboard devenv processes.";
    };

    extensionId = lib.mkOption {
      type = lib.types.str;
      default = "workenv.clipboard";
      description = "Manifest extension ID for the clipboard adapter.";
    };

    package = lib.mkOption {
      type = lib.types.package;
      default = config.workenv.rust.package;
      description = "Package containing the clipboard adapter executable.";
    };

    binaryName = lib.mkOption {
      type = lib.types.str;
      default = "workenv-adapter-clipboard";
      description = "Adapter binary name inside the selected package.";
    };

    operations = lib.mkOption {
      type = lib.types.attrs;
      default = operations;
      description = "Clipboard operation contracts.";
    };
  };

  config = lib.mkIf cfg.enable (
    lib.mkMerge [
      {
        workenv.rust.enable = lib.mkDefault true;
        workenv.tools = {
          enable = true;
          packageNames = [ "ssh-clipboard" ];
        };

        packages = with pkgs; [
          jq
          xclip
          xorg.xvfb
          xsel
        ];

        workenv.extensions = {
          ${cfg.extensionId} = {
            version = workspaceManifest.workspace.package.version;
            protocol_version = 1;
            executable = "${cfg.package}/bin/${cfg.binaryName}";
            location = "target";
            systems = [ "x86_64-linux" ];
            runtime_inputs = [ ];
            operations = cfg.operations;
          };
        };
      }
      (lib.mkIf cfg.enableProcesses {
        processes."clipboard-xvfb" = {
          exec = "Xvfb :99 -screen 0 1024x768x24 -nolisten tcp";
          ready.exec = "test -S /tmp/.X11-unix/X99";
        };

        processes."ssh-clipboard" = {
          after = [ "devenv:processes:clipboard-xvfb" ];
          exec = ''
            export DISPLAY=:99
            export SSH_CLIPBOARD_CONFIG_DIR="$PWD/.state/clipboard/config"
            export SSH_CLIPBOARD_STATE_DIR="$PWD/.state/clipboard/state"
            export SSH_CLIPBOARD_DISABLE_AUTO_UPDATE=1
            ${ensureConfig}
            exec ssh-clipboard daemon
          '';
          ready.exec = ''
            DISPLAY=:99 \
            SSH_CLIPBOARD_CONFIG_DIR="$PWD/.state/clipboard/config" \
            SSH_CLIPBOARD_STATE_DIR="$PWD/.state/clipboard/state" \
            ssh-clipboard status --json | jq -e '.running == true and .clipboard_backend == "X11"'
          '';
        };
      })
    ]
  );
}
