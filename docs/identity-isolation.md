# Identity isolation in the runner pool

Every runner in the pool is a disposable guest that holds exactly **one**
identity. This is the security claim of the fleet, so it is written down here --
in a tracked file -- rather than only in `AGENTS.md`, which this operator's
global gitignore excludes from every checkout.

## The mechanism, in one sentence

A guest's `$HOME` only ever receives one profile's credentials, and that is
proved by digest before the guest is handed over.

## Where each part lives

| Part | File | What it stops |
|---|---|---|
| A runner's identity is declared, not decided | `presets/slots.nix` binds `config.profile` on every runner | A script choosing an identity that the manifest does not record |
| A binding with no profile will not evaluate | `modules/identity.nix` assertions | The inert state the whole integration sat in: a configless binding fails inside the adapter on "profile.name is required", so the guest quietly runs on the controller's own credentials |
| The seed reads one tree and never `$HOME` | `provisioning/workenv-seed --identity` | "This profile has no login yet" becoming "this guest silently ran as the other account" |
| Wipe before every seed and every release | `provisioning/workenv-identity-wipe` | The deleted Lima pool's worst bug: a slot re-let with the previous tenant's `~/.claude/.credentials.json` still on disk, because the incoming profile had no file of its own to overwrite it |
| Proof by digest, computed locally | `provisioning/workenv-identity-check` | Trusting that the seed worked. Nothing secret crosses in either direction: the guest reports SHA-256 digests and the comparison happens on the controller |
| Refuse to attach to a dirty guest | `provisioning/wkv` runs the check on every claim | A check that only runs in a dedicated test run, and is therefore a claim about the test run |

## Shared credentials are named as shared

Anthropic auth is a single store holding several accounts, selected by
`ANTHROPIC_PROFILE`. Two profiles therefore carry a byte-identical
`.claude/.credentials.json` and are still different identities. Such a file is
reported `SHARED` rather than waved through, and the selection itself -- written
into the guest as `~/.workenv-identity` and sourced by its login shells -- is
read back by the checker. A shared credential with the wrong selection is
precisely a guest working as the other account, and no digest comparison can see
it.

## Exit codes are not evidence on the Orchard path

`orchard ssh` returns its own status, not the command's
(`adapters/orchard/src/provider/execute.rs:14-24`), and returns success for a
guest it never reached. So the wipe and the check both end in a sentinel line and
decide from what the command *printed*. An empty inventory therefore reads as
DIRTY, not as a guest that happens to hold nothing --
`provisioning/tests/identity-isolation` covers exactly that case.

## Tests

- `modules/tests/fleet-slots.nix` -- the pool's shape, plus five probes that
  build configurations the guards must refuse.
- `provisioning/tests/identity-isolation` -- fifteen cases, most of which must
  fail rather than pass.
- `provisioning/tests/wkv-routing` -- a repository reaches the fleet whose
  identity it should hold.

## Recorded exception: seeding identities into disposable guests

`workenv-seed` copies an identity profile's credentials into a guest. That is a
deliberate exception to "never copy authenticated machine identities", taken
with these limits, and not an oversight to tidy away.

Why. A guest exists to run an agent that pushes work back out, and pushing needs
a credential the guest holds. Short-lived scoped tokens would be better and do
not exist for Claude or Codex auth, which is what makes this the choice rather
than the shortcut. Revisit the moment they do.

What bounds it.

- A guest receives exactly one profile. `workenv-seed --identity` reads only
  that profile's tree and never falls back to the controller's `$HOME`; the
  fallback is the bug, because it turns "this profile has no login yet" into
  "this guest silently ran as the other account".
- Every guest is wiped before it is seeded and again before it is released, and
  `workenv-identity-check` proves the result by digest rather than by assumption.
  `wkv` runs that check on every claim and refuses to hand over a guest that
  fails it.
- The image stays clean. `workenv-bake`'s audit treats these same files as
  contamination and is left exactly as it is: a Tart image is cloned and can be
  pushed to a registry, so a credential baked into one escapes every boundary
  above. Credentials arrive at spawn, into a guest that is about to be destroyed.
- Shared credentials are named as shared. Anthropic auth is one store holding
  several accounts, so two profiles legitimately carry identical bytes and
  `ANTHROPIC_PROFILE` decides which account acts. That selection is installed as
  the guest's `~/.workenv-identity` and read back by the check, because a shared
  credential with the wrong selection is a guest working as the other account.
