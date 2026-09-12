{ pkgs, ... }:

let
  # One binding config, reused. `workenv.orchard` is bound three ways for the
  # fast host -- as its provider, its transport and its connection -- and core
  # refuses an environment whose repeated bindings of one extension carry
  # different configuration ("ambiguous bindings with different configuration").
  # Writing it once is what keeps them identical.
  orchardBinding = {
    extension = "workenv.orchard";
    config = { };
  };

  # `source` is a path on the *target*: core runs `devenv shell --from <source>`
  # there, never here. A remote environment whose source was `toString ../.`
  # therefore named a macOS controller path that does not exist on a Linux
  # guest, and had nothing to evaluate once it got there. The project
  # integration is what puts a real checkout at that path, so for every remote
  # environment below `source` and `directory` are the same path and this
  # binding is what creates it.
  projectBinding = repository: {
    extension = "workenv.project";
    config = { inherit repository; };
  };

  workenvProject = projectBinding "https://github.com/douglance/workenv.git";
in

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
    # workenv.clipboard is not enabled. It is an xclip/xsel integration whose
    # package set and extension are both declared `systems = [ "x86_64-linux" ]`,
    # so on an Apple Silicon controller enabling it does not merely make the
    # integration unusable -- it puts an unbuildable package in the devenv shell
    # and every command fails with "Refusing to evaluate package
    # 'ssh-clipboard-unsupported-on-aarch64-darwin'". This fleet is Macs only.
    exedev.enable = true;
    lima.enable = true;
    # Read-only cluster inventory. Declares no hosts, so it cannot affect the
    # Lima-backed environments above; it only makes `workenv extension call
    # workenv.orchard inventory` reachable from the CLI and MCP.
    orchard.enable = true;
    ssh.enable = true;
    bootstrap.enable = true;
    # The checkout adapter. It had recorded live acceptance but was enabled only
    # in a `.state` working manifest, so `workenv.project` was unreachable from
    # the shipped CLI and every environment here had to carry its own source.
    project.enable = true;

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

    # An address-free host. This is the whole point of the Orchard path: the
    # scheduler chooses the machine after this manifest is written, so there is
    # no address to declare, and `workenv.orchard` is both the provider that
    # creates the guest and the transport that reaches it by name through the
    # controller. Nothing here names a machine, so the same environment lands on
    # whichever worker has capacity.
    hosts."wkv-fast" = {
      address = null;
      transport = "workenv.orchard";
      system = "aarch64-linux";
      # The controller URL is not repeated here. The adapter defaults to the
      # loopback controller, which is the only place it can be once the
      # controller is bound to 127.0.0.1, and an unset value cannot disagree
      # with the transport and connection bindings.
      provider = orchardBinding;
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
      source = "/home/box-03.linux/workenv";
      ephemeral = true;
      # herdr appears here as well as in `connection` because it declares a
      # `register` operation, and lifecycle up refuses a connection extension
      # that supports register unless the identical binding is also an
      # integration. Without this, `environment up` bails before doing any work.
      integrations = [
        { extension = "workenv.identity"; }
        { extension = "workenv.bootstrap"; }
        workenvProject
        { extension = "workenv.herdr"; }
      ];
      connection = {
        extension = "workenv.herdr";
      };
    };

    # Measured at 7s from create to ssh-ready against a 4354s baseline, because
    # the image carries the toolchain. Credentials are deliberately not baked:
    # a Tart image is cloned and can be pushed to a registry.
    environments."wkv-fast" = {
      host = "wkv-fast";
      directory = "/home/admin/workenv";
      source = "/home/admin/workenv";
      ephemeral = true;
      integrations = [
        { extension = "workenv.identity"; }
        { extension = "workenv.bootstrap"; }
        workenvProject
      ];
      # Reaching in is the provider's own job here. Every other connection
      # extension needs target.address, which this host does not have.
      connection = orchardBinding;
    };

    # A second project on the same scheduled-guest path. The environment name is
    # also the guest name (the Orchard adapter falls back to
    # `target.environment`), and `reap` sweeps by name prefix, so this is
    # `wkv-devsql` rather than `devsql`: a guest outside the `wkv-` prefix is
    # not covered by the sweep that expires the others, and would leak.
    environments."wkv-devsql" = {
      host = "wkv-fast";
      directory = "/home/admin/devsql";
      source = "/home/admin/devsql";
      ephemeral = true;
      integrations = [
        { extension = "workenv.identity"; }
        { extension = "workenv.bootstrap"; }
        (projectBinding "https://github.com/douglance/devsql.git")
      ];
      connection = orchardBinding;
    };

    environments.workenv = {
      host = "local";
      directory = toString ../.;
      source = toString ../.;
      integrations = [
        { extension = "workenv.tailscale"; }
        { extension = "workenv.identity"; }
        # workenv.clipboard is deliberately absent. It is an xclip/xsel
        # integration declared `systems = [ "x86_64-linux" ]`, and this host is
        # the controller Mac, so listing it made the whole manifest fail to load
        # with "workenv.clipboard does not support aarch64-darwin" -- every
        # command, not just the ones that would have used it.
        { extension = "workenv.bootstrap"; }
        { extension = "workenv.herdr"; }
      ];
      connection = {
        extension = "workenv.herdr";
      };
    };
  };
}
