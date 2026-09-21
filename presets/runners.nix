# This fleet's runner pool.
#
# A separate module rather than more lines in `personal.nix`, because Nix refuses
# a duplicate attribute: a generated `environments` set cannot sit beside
# individual `environments."name" = ...` assignments in one attribute set, while
# the module system merges the same option across two modules without complaint.
#
# Segment sizes are measured, not guessed. As of 2026-09-14:
#
#   exe.dev   8 runners   paid capacity, 50 provisioned VMs available
#   desk-01   14 cpu / 64 GB   -> 2 runners (the controller's own desk)
#   desk-02   10 cpu / 32 GB   -> 2 runners
#   box-03     8 cpu /  8 GB   -> 1 runner, load ~800 and may not place an
#                                 8 GB guest
#
# A fourth Mac exists on the network and is deliberately not in this fleet.
#
# So five Mac runners, `wkv-r09..wkv-r13`, against a measured ceiling of five.
# The scheduler places them; nothing here names a machine. Raising the count is
# a one-line change once a box is added or freed.
{ lib, ... }:

import ./pool.nix {
  inherit lib;
  prefix = "wkv-r";
  cloudHost = "wkv-cloud";
  macHost = "wkv-macs";
  cloudFirst = 1;
  cloudLast = 8;
  macFirst = 9;
  macLast = 13;
  profile = (import ./profiles.nix).personal;
  # Unattended: the agent runs without asking for permission. Allowed because the
  # Mac runners are fenced from the host and the cloud runners are remote, and
  # the pool refuses to evaluate otherwise. This used to be a default inside
  # `provisioning/wkv`, where nobody reviewing the fleet would see it.
  agent = {
    command = "claude --dangerously-skip-permissions";
    unattended = true;
  };
}
