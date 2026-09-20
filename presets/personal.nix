{ pkgs, ... }:

let
  # The identity this fleet runs as. Bound, not merely enabled: a configless
  # `{ extension = "workenv.identity"; }` fails inside the adapter on
  # "profile.name is required", so every environment below declared an identity
  # integration and ran on whatever credentials the machine happened to hold.
  # `modules/identity.nix` now refuses to evaluate that shape.
  #
  # One binding value, reused, for the same reason as `orchardBinding`: core
  # rejects an environment whose repeated bindings of one extension differ, and
  # writing it once is what keeps them identical.
  identityBinding = {
    extension = "workenv.identity";
    config.profile = (import ./profiles.nix).personal;
  };
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
    tailscale.enable = true;
    identity.enable = true;
    # workenv.clipboard is not enabled. It is an xclip/xsel integration whose
    # package set and extension are both declared `systems = [ "x86_64-linux" ]`,
    # so on an Apple Silicon controller enabling it does not merely make the
    # integration unusable -- it puts an unbuildable package in the devenv shell
    # and every command fails with "Refusing to evaluate package
    # 'ssh-clipboard-unsupported-on-aarch64-darwin'". This fleet is Macs only.
    exedev.enable = true;
    # The provider, transport and connection for every remote environment here
    # since Lima was removed. Also what makes `workenv extension call
    # workenv.orchard inventory` reachable from the CLI and MCP.
    orchard.enable = true;
    ssh.enable = true;
    bootstrap.enable = true;
    # The checkout adapter. It had recorded live acceptance but was enabled only
    # in a `.state` working manifest, so `workenv.project` was unreachable from
    # the shipped CLI and every environment here had to carry its own source.
    project.enable = true;

    # No per-project hosts or environments. `wkv-01`, `wkv-fast` and
    # `wkv-devsql` lived here, one environment per project, each naming its own
    # repository and its own guest name. The pool in `runners.nix` replaces all
    # three: the environment name is the guest name globally, so one environment
    # per project can never give two workenvs on one project, and a pool of
    # identical runners gives arbitrary concurrency from static declarations --
    # with no registry, no adopt step, and no manifest change per project.
    hosts.local = {
      address = null;
      transport = null;
      provider = null;
      system = pkgs.stdenv.hostPlatform.system;
    };

    environments.workenv = {
      host = "local";
      directory = toString ../.;
      source = toString ../.;
      integrations = [
        { extension = "workenv.tailscale"; }
        identityBinding
        # workenv.clipboard is deliberately absent. It is an xclip/xsel
        # integration declared `systems = [ "x86_64-linux" ]`, and this host is
        # the controller Mac, so listing it made the whole manifest fail to load
        # with "workenv.clipboard does not support aarch64-darwin" -- every
        # command, not just the ones that would have used it.
        { extension = "workenv.bootstrap"; }
      ];
      # No connection extension: `connection` is `nullOr` and defaults to null,
      # and core then falls back to a devenv shell (connection.rs:15). This host
      # *is* the controller, so there is nothing to reach into -- herdr was
      # answering a question this environment does not ask.
    };
  };
}
