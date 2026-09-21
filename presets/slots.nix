# One segment of the runner pool.
#
# A runner is a disposable guest that any repository can take: identical to every
# other runner, naming no project, carrying one identity. The pool is a single
# numbering -- `wkv-r01`, `wkv-r02`, ... -- and a segment is the contiguous run of
# it that lives on one host. Nothing above this file distinguishes a runner on a
# Mac from a runner in the cloud, which is the point: a repository asks for a free
# runner, not for a kind of machine.
#
# Segments exist only because an environment names exactly one host, and the
# environment name is the guest name. They are a consequence of that, not a tier.
#
# A plain function rather than a module so a second fleet, in its own repository,
# builds its pool from this code instead of a copy that drifts.
{
  lib,
  # Shared by the whole pool. Also what `reap` sweeps by, so two fleets sharing a
  # prefix would expire each other's runners.
  prefix,
  # Inclusive bounds within the pool's single numbering. Segments must not
  # overlap; `modules/tests/fleet-slots.nix` asserts they do not, because an
  # overlap is two environments claiming one guest name and the second one
  # silently adopting the first one's guest.
  first,
  last,
  host,
  # The identity every runner in this segment holds. Not optional and not
  # defaulted: a runner with no profile runs on whatever credentials the
  # controller happens to hold, which is the failure the pool exists to prevent.
  # `modules/identity.nix` asserts the same thing independently.
  profile,
  # The guest's own home for the checkout, which differs only because the image's
  # default user does.
  directory,
  source,
  profilesDir ? null,
  # The agent every runner in this segment starts. Declared by the fleet, not
  # defaulted here, so that "runs without asking" is a line someone wrote.
  agent ? null,
}:

let
  names = map (n: "${prefix}${lib.fixedWidthNumber 2 n}") (lib.range first last);

  # The binding is what makes a runner's identity a declared fact rather than a
  # convention in a shell script: it is in the manifest, `environment list` prints
  # it, and `provisioning/wkv` reads the profile name from there rather than
  # deciding one of its own.
  identity = {
    extension = "workenv.identity";
    config = {
      inherit profile;
    }
    // lib.optionalAttrs (profilesDir != null) { profiles_dir = profilesDir; };
  };
in
lib.genAttrs names (_: {
  inherit
    host
    directory
    source
    agent
    ;
  ephemeral = true;
  integrations = [ identity ];
  # No connection binding. Core refuses an environment whose repeated bindings of
  # one extension differ, so a connection binding repeating the provider's config
  # would put a copy of the setup script in the manifest once per runner --
  # measured at 19 copies, 80 KB, and an evaluation past its 300 s budget.
  # Absent, core falls back to the host's transport for `connect`, which is the
  # same adapter reached with an empty config, and the script appears once.
  connection = null;
})
