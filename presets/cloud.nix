# The cloud tier: exe.dev VMs as a pool of identical, project-agnostic slots.
#
# A separate module rather than more lines in `personal.nix`, because Nix refuses
# a duplicate attribute: `environments = <generated>` cannot sit beside
# `environments."wkv-01" = ...` in one attribute set, while the module system
# merges the same option across two modules without complaint.
#
# Why this tier exists at all. Measured on the fleet as it stands: the controller
# Mac is the operator's own workstation, running at load 824 with 63 GB of 64 GB
# resident, where launchd could not reliably start a background job at all; the
# one other worker has 8 GB. Against that, exe.dev creates a VM in 2s, reaches
# ssh-ready in 4s, and the plan already pays for 50 of them. Concurrency comes
# from here, not from the Macs.
{ lib, ... }:

let
  # First-boot provisioning, composed from the installer fragment and the default
  # shell's own files so the shell has one definition rather than a copy that
  # drifts. `devenv.lock` is deliberately not embedded: at 8.8 KiB it would not
  # fit inside exe.dev's 10 KiB setup-script cap, so the guest resolves it once at
  # first boot instead of carrying it.
  shellNix = builtins.readFile ../provisioning/shells/default/devenv.nix;
  shellYaml = builtins.readFile ../devenv.yaml;
  # A `${` in either embedded file would be read by Nix as antiquotation and
  # silently interpolate, producing a setup script that is not the file it claims
  # to be -- a guest provisioned from a corrupted shell definition that still
  # reports success. Neither file contains one today; this refuses to build
  # rather than ship that.
  literal =
    text:
    if builtins.match ".*[$][{].*" text == null then
      text
    else
      throw "embedded shell file contains Nix antiquotation and cannot be inlined";

  setupScript = ''
    ${builtins.readFile ../provisioning/workenv-cloud-setup}
    cat > "$SHELL_DIR/devenv.nix" <<'WKV_DEVENV_NIX'
    ${literal shellNix}
    WKV_DEVENV_NIX
    cat > "$SHELL_DIR/devenv.yaml" <<'WKV_DEVENV_YAML'
    ${literal shellYaml}
    WKV_DEVENV_YAML
    # Resolves devenv.lock and warms the store, so the first real command in this
    # guest does not pay input resolution.
    (cd "$SHELL_DIR" && devenv shell -- true) || true
  '';

  # One binding, reused as provider, transport and connection. Core refuses an
  # environment whose repeated bindings of one extension carry different
  # configuration, and writing it once is what keeps them identical.
  exedevBinding = {
    extension = "workenv.exedev";
    config = {
      cpus = 4;
      memory_gb = 8;
      disk_gb = 50;
      setup_script = setupScript;
    };
  };

  slotNames = map (n: "wkv-c${lib.fixedWidthNumber 2 n}") (lib.range 1 8);
in
{
  workenv = {
    exedev.enable = true;

    # No address, like the Orchard hosts, but for a different reason: exe.dev
    # advertises `ssh <name>.exe.xyz` and that answers "no ssh.config on the VM
    # host" on pooled placement, on `--no-pool`, and after a restart. So
    # `workenv.exedev` is both the provider that creates the VM and the transport
    # that reaches it through the relay.
    #
    # x86_64, unlike every other host in this fleet. exe.dev VMs are Intel Linux
    # while the Macs are aarch64, and `validate.rs` checks every bound extension
    # against the host's declared system -- so getting this wrong makes *every*
    # command fail, not only the ones that would have used this host.
    hosts."wkv-cloud" = {
      address = null;
      transport = "workenv.exedev";
      system = "x86_64-linux";
      provider = exedevBinding;
    };

    # A pool of identical slots. The environment name *is* the VM name, globally,
    # so "several workenvs on one project at once" cannot come from one
    # environment per project. Slots give arbitrary concurrency from static
    # declarations, and because no slot names a repository, any project can take
    # any free one -- which is what removes the need for a project registry, an
    # adopt step, and the 107s manifest re-evaluation that adding a project to a
    # tracked file would cost every time.
    environments = lib.genAttrs slotNames (_: {
      host = "wkv-cloud";
      directory = "/home/exedev/work";
      # The baked shell, not the checkout. `source` and `directory` are
      # independent -- core runs `devenv shell --from <source>` with cwd
      # `<directory>` -- and that is the entire reason a repository carrying no
      # devenv.nix of its own can come up here. Of 46 projects touched in the
      # last 30 days, exactly one has one.
      source = "path:/opt/workenv/shells/default";
      ephemeral = true;
      # Nothing bound, deliberately. `bootstrap` is what installs nix and devenv,
      # and it is controller-located and reaches a target over ssh at
      # `target.address` -- which this host does not have. The provider's
      # `setup_script` does that job at first boot instead, and `apply_bootstrap`
      # skips cleanly when no binding is present, leaving `apply` as
      # directory -> realize. `project` is absent because its repository comes
      # from static binding config and a generic slot has no single repository.
      integrations = [ ];
      # No connection binding, and that is a size decision as much as a design
      # one. Core refuses an environment whose repeated bindings of one extension
      # differ, so a connection binding here would have to repeat the provider's
      # config -- setup script included -- once per slot. Measured, that put 19
      # copies of the script into the manifest, inflated it to 80 KB and pushed
      # evaluation past its 300s budget, so `up` died with "manifest evaluation
      # failed with exit code 128". With this absent, core falls back to the
      # host's transport for `connect`, which is the same adapter reached with an
      # empty config, and the script appears exactly once.
      connection = null;
    });
  };
}
