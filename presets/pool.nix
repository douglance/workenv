# The runner pool: every box in the fleet, one numbering, no tiers.
#
# A repository asks for a free runner and gets one. Where it lands -- a Mac in
# the house or a VM in the cloud -- is the scheduler's business and nothing
# above this file mentions it. The two backends differ in exactly three ways,
# all of them mechanical:
#
#   * who creates the guest      exe.dev's API, or Orchard's scheduler
#   * how the controller reaches it   the exe.dev relay, or `orchard ssh vm`
#   * the image's default user   exedev, or admin
#
# Everything that matters is identical: one first-boot script installs nix and
# devenv, one baked shell supplies the toolchain, one identity profile is seeded,
# and the guest is destroyed afterwards.
#
# Why a first-boot script rather than a baked image. `workenv-base` is a `local`
# Tart image on one Mac, so a guest scheduled anywhere else fails with `the
# specified VM "workenv-base" does not exist`, and distributing it needs either a
# bake per box or a registry. Orchard's `create` accepts a `startup_script`
# (`modules/orchard.nix:117`, passed straight through by
# `adapters/orchard/src/provider/create.rs:54`), so a Mac runner provisions
# itself exactly as a cloud runner does and the image can stay a stock Ubuntu
# that every worker already pulls. That removes the image-distribution problem
# from the pool entirely.
#
# Returns a module. Call it with the fleet's prefix, identity and host names.
{
  lib,
  prefix,
  profile,
  cloudHost,
  macHost,
  # Inclusive bounds per segment, carved from one numbering. Sized from measured
  # capacity, not guessed -- see the fleet that instantiates this.
  cloudFirst ? 1,
  cloudLast ? 8,
  macFirst ? 9,
  macLast ? 14,
  profilesDir ? null,
  # The Mac runners' host-side fence. A parameter so the fleet suite can build a
  # pool without one and prove the assertion below refuses it.
  macFence ? {
    isolated = true;
  },
  # What every runner starts, and whether it runs without asking.
  agent ? null,
}:

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
    ${builtins.readFile ../provisioning/workenv-herdr-install}
    cat > "$SHELL_DIR/devenv.nix" <<'WKV_DEVENV_NIX'
    ${literal shellNix}
    WKV_DEVENV_NIX
    cat > "$SHELL_DIR/devenv.yaml" <<'WKV_DEVENV_YAML'
    ${literal shellYaml}
    WKV_DEVENV_YAML
    # Resolves devenv.lock and warms the store, so the first real command in this
    # guest does not pay input resolution.
    (cd "$SHELL_DIR" && devenv shell -- true) || true
    # Written last, and only here. A provider reports a guest `running` when it
    # boots, while this script is still installing nix and devenv -- measured at
    # roughly three and a half minutes on a live guest. `up` raced that and died
    # with `devenv: command not found`, having been told the guest was present,
    # so both providers wait for this file rather than for the hypervisor. Only
    # exe.dev did until the Orchard adapter learned to; before that, a Mac
    # runner was handed over mid-install. A setup that outlasts a create's budget
    # comes back pending, and the next `up` finishes the wait. `set -eu` above is
    # what makes its absence meaningful: a script that failed part way never
    # reaches this line. The path is the protocol's provisioned marker, and a test
    # there fails if this line names any other.
    touch /opt/workenv/.provisioned
  '';

  source = "path:/opt/workenv/shells/default";

  # One binding per backend, reused as provider and transport. Core refuses an
  # environment whose repeated bindings of one extension carry different
  # configuration, and writing each once is what keeps them identical.
  exedevBinding = {
    extension = "workenv.exedev";
    config = {
      cpus = 4;
      memory_gb = 8;
      disk_gb = 50;
      setup_script = setupScript;
    };
  };

  # 4 cpu / 8192 MB because a runner has to be able to build: provisioning adds
  # an 8 GB swapfile precisely because rustc was SIGKILLed on a 7914 MB guest.
  # The 2 cpu / 2048 MB that `create.rs` falls back to cannot compile anything.
  #
  # No `image` pin and no `labels`. Pinning `workenv-base` pinned placement to
  # the one Mac holding it, which is what kept the whole Mac side of the pool on
  # a single box; a stock image plus `startup_script` lets the scheduler use
  # every worker. `lease_seconds` is deliberately absent -- it is accepted by
  # create's schema and discarded by its code, so declaring one would read as an
  # expiry policy that does not exist. Expiry lives in `reap`.
  #
  # `network.isolated` fences every Mac runner from the host side. The agent in a
  # runner has passwordless sudo and runs with permission prompts off, so a rule
  # inside the guest is a rule it can delete; Softnet runs on the worker Mac and
  # it cannot. Isolated, a runner reaches the public internet and its host's
  # bridge address -- which is where DNS comes from -- and nothing else: not the
  # LAN, not the other runners, not the tailnet. What it does NOT stop is written
  # down in docs/identity-isolation.md.
  orchardBinding = {
    extension = "workenv.orchard";
    config = {
      image = "ghcr.io/cirruslabs/ubuntu:latest";
      cpu = 4;
      memory = 8192;
      startup_script = setupScript;
      network = macFence;
    };
  };

  segment =
    args:
    import ./slots.nix (
      {
        inherit
          lib
          prefix
          profile
          profilesDir
          source
          agent
          ;
      }
      // args
    );
in
{
  # Checked here, because this is the only place that can see the bounds. The
  # merged `environments` attrset cannot: two overlapping segments produce one
  # attribute per name, so the overlap disappears into the merge and the pool
  # reports the right count while one runner's declaration has silently replaced
  # another's -- a different host, a different directory, and two environments
  # believing they own one guest name.
  assertions = [
    {
      assertion = macFirst == cloudLast + 1;
      message = "runner pool segments must be contiguous: cloud ends at ${toString cloudLast} and the Macs start at ${toString macFirst}, which leaves ${
        if macFirst > cloudLast + 1 then "a gap" else "an overlap"
      }.";
    }
    {
      assertion = cloudFirst <= cloudLast && macFirst <= macLast;
      message = "runner pool segments must be non-empty ranges.";
    }
    {
      # The one place an agent runs without asking must be a place it cannot
      # reach the operator's network from. A Mac runner sits on that network
      # unless it is fenced; a cloud runner was never on it.
      assertion = agent == null || !(agent.unattended or false) || (macFence.isolated or false);
      message = "runner pool declares an unattended agent, but its Mac runners are not fenced (macFence.isolated); an agent that never asks would be running on the operator's own network.";
    }
  ];

  workenv = {
    exedev.enable = true;
    orchard.enable = true;
    identity.enable = true;

    # No address, for two different reasons that land in the same place. exe.dev
    # advertises `ssh <name>.exe.xyz` and that answers "no ssh.config on the VM
    # host" on pooled placement, on `--no-pool`, and after a restart; an Orchard
    # guest is placed by the scheduler after this manifest is written, so there
    # is no address to declare in the first place. Each provider is therefore its
    # own transport.
    #
    # x86_64 for the cloud host and aarch64 for the Macs. `validate.rs` checks
    # every bound extension against the host's declared system, so getting this
    # wrong makes *every* command fail, not only the ones that would have used
    # that host.
    hosts.${cloudHost} = {
      address = null;
      transport = "workenv.exedev";
      system = "x86_64-linux";
      provider = exedevBinding;
    };

    hosts.${macHost} = {
      address = null;
      transport = "workenv.orchard";
      system = "aarch64-linux";
      provider = orchardBinding;
    };

    environments =
      segment {
        host = cloudHost;
        first = cloudFirst;
        last = cloudLast;
        directory = "/home/exedev/work";
      }
      // segment {
        host = macHost;
        first = macFirst;
        last = macLast;
        directory = "/home/admin/work";
      };
  };
}
