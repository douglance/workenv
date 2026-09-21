# The shipped fleet's slot pool carries an identity, and the guard that enforces
# that actually fires.
#
# `personal-preset.nix` proves the preset evaluates and every binding it names
# resolves. It cannot prove the thing this pool exists for: that a slot runs as a
# declared identity rather than on whatever credentials the controller happens to
# hold. A slot missing its profile evaluates perfectly and fails only much later,
# inside the adapter, on "profile.name is required" -- which reads as a broken
# adapter rather than an unisolated guest.
#
# So this suite asserts the property on the shipped fleet, and then -- in the
# other direction -- builds four configurations that should be refused and
# asserts each is. A guard nothing tries to break is decoration.
{ pkgs }:

let
  inherit (pkgs) lib;
  support = import ./preset-support.nix { inherit pkgs; };
  inherit (support) evaluate failures;

  # The fleet root, which is what the CLI loads: both presets together.
  fleet = evaluate support.fleetModules;
  environments = fleet.workenv.environments;

  prefix = "wkv-r";
  expectedProfile = "personal";
  # One pool, one numbering, both backends. The count is asserted rather than
  # derived so that losing a whole segment -- a host rename, a dropped import --
  # goes red instead of quietly halving the fleet's capacity.
  expectedSlots = 13;

  # Derived here from reading profile_files.rs, deliberately not imported from
  # modules/identity.nix. An expectation taken from the code under test passes on
  # corrupted input while looking rigorous.
  readsConfigKeys = [
    "profile"
    "profiles_dir"
    "nib_token_env"
    "nib_token_file"
    "replace_existing"
    "replace"
  ];
  readsProfileFields = [
    "name"
    "digest"
    "github_login"
    "git_name"
    "git_email"
    "anthropic_profile"
  ];

  identityOf =
    environment:
    let
      bound = lib.filter (binding: binding.extension == "workenv.identity") (
        environment.integrations ++ lib.optional (environment.connection != null) environment.connection
      );
    in
    if bound == [ ] then null else lib.head bound;

  profileNameOf =
    name:
    let
      binding = identityOf environments.${name};
    in
    if binding == null then null else binding.config.profile.name or null;

  names = lib.attrNames environments;
  slots = lib.filter (name: lib.hasPrefix prefix name) names;
  hosts = fleet.workenv.hosts;
  expectedNames = map (n: "${prefix}${lib.fixedWidthNumber 2 n}") (lib.range 1 expectedSlots);
  transports = lib.unique (map (name: hosts.${environments.${name}.host}.transport) slots);
  providerOf = name: hosts.${environments.${name}.host}.provider;
  # Mac runners: the ones Orchard schedules, which Softnet can fence.
  macSlots = lib.filter (
    name: providerOf name != null && (providerOf name).extension == "workenv.orchard"
  ) slots;
  fenced = name: ((providerOf name).config.network.isolated or false) == true;
  binders = lib.filter (name: identityOf environments.${name} != null) names;

  strayKeys = lib.concatMap (
    name:
    let
      binding = identityOf environments.${name};
    in
    if binding == null then [ ] else lib.subtractLists readsConfigKeys (lib.attrNames binding.config)
  ) names;
  strayFields = lib.concatMap (
    name:
    let
      binding = identityOf environments.${name};
      spec = if binding == null then null else binding.config.profile or null;
    in
    if spec == null then [ ] else lib.subtractLists readsProfileFields (lib.attrNames spec)
  ) names;

  # --- the guard, tried in the direction that should fail -------------------
  # One host and one environment, minimal, so the only thing that varies between
  # probes is the identity binding under test.
  probe =
    binding:
    evaluate [
      ../default.nix
      {
        workenv = {
          identity.enable = true;
          hosts.probe = {
            address = null;
            transport = null;
            provider = null;
            system = pkgs.stdenv.hostPlatform.system;
          };
          environments.probe = {
            host = "probe";
            directory = "/tmp/workenv-probe";
            source = "/tmp/workenv-probe";
            integrations = [ binding ];
          };
        };
      }
    ];

  # The baseline is a *correct* binding, not an empty configuration, so each
  # comparison below isolates exactly the defect introduced. Counting failures
  # rather than matching their text keeps this from silently stopping covering
  # whatever a reworded message no longer matches.
  accepted = probe {
    extension = "workenv.identity";
    config.profile.name = "probe";
  };
  baseline = lib.length (failures accepted);

  refused = binding: lib.length (failures (probe binding)) > baseline;

  # The pool's own bounds, tried the same way. These cannot be checked from the
  # merged `environments` set: overlapping segments collapse into one attribute
  # per name, so the overlap is invisible by the time the manifest exists. The
  # first version of this suite asserted uniqueness of attribute names and was
  # therefore vacuous -- it stayed green with the segments overlapping by one.
  pool =
    args:
    evaluate [
      ../default.nix
      (import ../../presets/pool.nix (
        {
          inherit lib;
          prefix = "probe-r";
          cloudHost = "probe-cloud";
          macHost = "probe-macs";
          profile.name = "probe";
        }
        // args
      ))
    ];
  poolBaseline = lib.length (
    failures (pool {
      cloudFirst = 1;
      cloudLast = 4;
      macFirst = 5;
      macLast = 8;
    })
  );
  poolRefused = args: lib.length (failures (pool args)) > poolBaseline;

  # The fence, tried in the direction that should fail. One Orchard host with a
  # well-formed fence is the baseline, so each probe varies only the fence.
  fenceProbe =
    network:
    evaluate [
      ../default.nix
      {
        workenv = {
          orchard.enable = true;
          hosts.probe = {
            address = null;
            transport = null;
            provider = {
              extension = "workenv.orchard";
              config = { inherit network; };
            };
            system = pkgs.stdenv.hostPlatform.system;
          };
        };
      }
    ];
  fenceBaseline = lib.length (
    failures (fenceProbe {
      isolated = true;
      allow = [ "192.168.0.0/24" ];
    })
  );
  fenceRefused = network: lib.length (failures (fenceProbe network)) > fenceBaseline;

  # What the shipped fleet is allowed to fail. Not a hardcoded count and not a
  # message match: the same module set, with a pool known to be well-formed,
  # evaluated independently. Both carry `rust.enable`'s toolchain assertion,
  # which a stub package cannot satisfy and which has nothing to do with the
  # configuration -- so anything the real fleet fails beyond this is a real
  # defect. An earlier version of this suite only *reported* these, and stayed
  # green with the pool's segments overlapping.
  allowedFailures = lib.length (
    failures (evaluate [
      ../default.nix
      ../../presets/personal.nix
      (import ../../presets/pool.nix {
        inherit lib;
        prefix = "baseline-r";
        cloudHost = "baseline-cloud";
        macHost = "baseline-macs";
        profile.name = "baseline";
      })
    ])
  );
in
# --- the shipped fleet ----------------------------------------------------
assert slots != [ ];
assert lib.length slots == expectedSlots;
# Slot names are exactly the generated shape. A hand-written environment that
# merely starts with the prefix would be swept by `reap` alongside the pool.
assert lib.all (name: builtins.match "wkv-r[0-9][0-9]" name != null) slots;
# The property this pool exists for.
assert lib.all (name: profileNameOf name == expectedProfile) slots;
# And nowhere in the fleet is identity bound without one.
assert binders != [ ];
assert lib.all (name: profileNameOf name != null) binders;
assert strayKeys == [ ];
assert strayFields == [ ];
# The numbering is contiguous and each name appears once. Segments are carved
# from one range by hand, so an off-by-one gives either a gap -- capacity that
# silently is not there -- or an overlap, which is two environments claiming one
# guest name and the second one adopting the first one's guest.
assert lib.length (lib.unique slots) == lib.length slots;
assert slots == expectedNames;
# Every runner is reachable: its host exists and declares a transport, which is
# what stands in for the address these guests do not have.
assert lib.all (name: hosts ? ${environments.${name}.host}) slots;
assert lib.all (name: hosts.${environments.${name}.host}.transport != null) slots;
# The pool spans more than one backend. A pool that collapsed onto a single
# transport would pass every check above while being half the fleet.
assert lib.length transports > 1;
# Nothing the fleet declares breaks a module assertion.
assert lib.length (failures fleet) <= allowedFailures;
# Every Mac runner is fenced from the host side. Counted non-empty first, so a
# pool that lost its Orchard segment cannot pass this by having nothing to check.
assert macSlots != [ ];
assert lib.all fenced macSlots;
# --- the guard ------------------------------------------------------------
# A configless binding: the shape every environment in this fleet had, which
# evaluated cleanly and left the adapter inert.
assert refused { extension = "workenv.identity"; };
# The `profiles_root` / `profiles_dir` spelling that migration got wrong once:
# accepted by the module system, never read by the adapter.
assert refused {
  extension = "workenv.identity";
  config = {
    profile.name = "probe";
    profiles_root = "/tmp/workenv-probe-profiles";
  };
};
# A name `validate_slug` rejects, caught at evaluation rather than at call time.
assert refused {
  extension = "workenv.identity";
  config.profile.name = "Probe";
};
# A profile field the adapter does not read, which would silently not apply.
assert refused {
  extension = "workenv.identity";
  config.profile = {
    name = "probe";
    git_user = "someone";
  };
};
# Segments that overlap by one: 8 names declared, 7 distinct, and the eighth
# runner's declaration silently replaced by the other segment's.
assert poolRefused {
  cloudFirst = 1;
  cloudLast = 4;
  macFirst = 4;
  macLast = 8;
};
# And a gap, which is capacity that is simply not there.
assert poolRefused {
  cloudFirst = 1;
  cloudLast = 4;
  macFirst = 6;
  macLast = 8;
};
# An inverted range, which produces a segment of nothing at all.
assert poolRefused {
  cloudFirst = 4;
  cloudLast = 1;
  macFirst = 5;
  macLast = 8;
};
# A misspelled key, which the adapter would refuse only at create time.
assert fenceRefused { isolate = true; };
# A hostname, which Softnet cannot express: it filters addresses.
assert fenceRefused {
  isolated = true;
  allow = [ "github.com" ];
};
# A prefix longer than an IPv4 address.
assert fenceRefused {
  isolated = true;
  block = [ "10.0.0.0/33" ];
};
{
  slotsCarryTheirProfile = true;
  macRunnersFenced = true;
  fenceRefusesEachDefect = true;
  macRunners = lib.length macSlots;
  noBindingWithoutAProfile = true;
  guardRefusesEachDefect = true;
  poolIsContiguous = true;
  poolSpansBackends = true;
  poolRefusesBadBounds = true;
  fleetBreaksNoAssertion = true;
  slotCount = lib.length slots;
  identityBindings = lib.length binders;
  inherit transports;
  fleetAssertionFailures = failures fleet;
}
