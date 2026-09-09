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
    ssh.enable = true;
    bootstrap.enable = true;

    # One ephemeral Lima slot on the Intel Mac `box-03`. The address is static
    # because the controller reads it from this manifest, never from a provider
    # response, so a claim binds an identity to a slot and never renames it.
    hosts."wkv-01" = {
      address = "exedev@wkv-01.example.ts.net";
      transport = "workenv.ssh";
      system = "x86_64-linux";
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
      directory = "/home/exedev/workenv";
      source = toString ../.;
      ephemeral = true;
      integrations = [
        { extension = "workenv.identity"; }
        { extension = "workenv.bootstrap"; }
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
