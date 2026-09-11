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
  imports = [ ../presets/personal.nix ];
}
