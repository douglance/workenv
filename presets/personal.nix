{ pkgs, ... }:

{
  imports = [ ../modules/default.nix ];

  workenv = {
    base.enable = true;
    rust.enable = true;
    tools = {
      enable = true;
      packageNames = [
        "devsql"
        "codex"
        "claude"
        "grok"
        "pi"
        "nib"
      ];
    };
    apoc.enable = true;
    herdr.enable = true;
    tailscale.enable = true;
    identity.enable = true;
    clipboard.enable = true;
    exedev.enable = true;
    lima.enable = true;
    # Read-only cluster inventory. Declares no hosts, so it cannot affect the
    # Lima-backed environments above; it only makes `workenv extension call
    # workenv.orchard inventory` reachable from the CLI and MCP.
    orchard.enable = true;
    ssh.enable = true;
    bootstrap.enable = true;

    # One ephemeral Lima slot on `box-03`, an Apple Silicon (M2) Mac mini. The
    # address is static because the controller reads it from this manifest and
    # never from a provider response, so a claim binds an identity to a slot and
    # never renames it. The user is `box-03` because that is the Lima guest user;
    # `fleet.json` says `exedev`, and the two want reconciling when the guest
    # gains its own account.
    hosts."wkv-01" = {
      address = "box-03@wkv-01.example.ts.net";
      transport = "workenv.ssh";
      system = "aarch64-linux";
      provider = {
        extension = "workenv.lima";
        config = {
          vm_host = "box-03.example.ts.net";
          command = "workenv-lima";
          slot = "wkv-01";
          lease_seconds = 14400;
        };
      };
    };

    hosts.local = {
      address = null;
      transport = null;
      provider = null;
      system = pkgs.stdenv.hostPlatform.system;
    };

    environments."wkv-01" = {
      host = "wkv-01";
      directory = "/home/box-03.linux/workenv";
      source = toString ../.;
      ephemeral = true;
      # herdr appears here as well as in `connection` because it declares a
      # `register` operation, and lifecycle up refuses a connection extension
      # that supports register unless the identical binding is also an
      # integration. Without this, `environment up` bails before doing any work.
      integrations = [
        { extension = "workenv.identity"; }
        { extension = "workenv.bootstrap"; }
        { extension = "workenv.herdr"; }
      ];
      connection = {
        extension = "workenv.herdr";
      };
    };

    environments.workenv = {
      host = "local";
      directory = toString ../.;
      source = toString ../.;
      integrations = [
        { extension = "workenv.tailscale"; }
        { extension = "workenv.identity"; }
        { extension = "workenv.clipboard"; }
        { extension = "workenv.bootstrap"; }
        { extension = "workenv.herdr"; }
      ];
      connection = {
        extension = "workenv.herdr";
      };
    };
  };
}
