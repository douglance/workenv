# The fleet root: point the CLI at this directory to get the real host set.
#
#   workenv --root fleet environment up wkv-fast
#
# The repository's own devenv.nix is the development shell and enables no
# adapters, so running the CLI from the repo root reports no environments at
# all. `presets/personal.nix` holds the hosts but is imported by nothing, which
# left the shipped fleet unreachable from the shipped CLI. This is the missing
# join.
{ ... }:

{
  imports = [
    ../presets/personal.nix
    # The cloud tier lives in its own module because Nix refuses a duplicate
    # attribute: a generated `environments` set cannot sit beside the individual
    # `environments."wkv-01"` assignments in one attrset, while the module system
    # merges the same option across two modules without complaint.
    ../presets/cloud.nix
  ];
}
