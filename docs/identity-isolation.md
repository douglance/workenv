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

## A runner describes itself

The seed installs `~/.workenv/runner.json`, so the agent inside can read its own
limits rather than discover them by failing: the environment, the identity
profile, the repository and branch, whether the host fences it, and the agent it
was started as and whether that agent runs unattended.

It is meant to be read, so it must never carry a secret, and two things keep it
that way. The fields are a fixed list (`provisioning/workenv-runner-describe`),
so nothing arrives by accident. And before it is written, every long value in the
profile's credential files is searched for in it, so something passed in on
purpose is refused and the seed stops.

The wipe removes it with everything else. The identity check reads it back: a
runner whose description names another profile or another runner is DIRTY,
because it would be telling its agent something false about the one thing the
check establishes. A runner with no description is judged on its identity alone,
since the proof does not rest on it.

## The network fence

Identity isolation says what a runner *holds*. The fence says what it can
*reach*, and it exists because of who runs inside: the agent has passwordless
sudo, is a Nix trusted user, and is started with permission prompts off. A
firewall configured inside the guest is therefore one `nft flush ruleset` away
from gone.

So the fence is not inside the guest. Mac runners are created with Softnet
isolation (`network.isolated` on the Orchard binding in `presets/pool.nix`),
which Tart enforces as a packet filter on the worker Mac.

| An isolated runner can reach | It cannot reach |
|---|---|
| Globally routable IPv4 addresses: GitHub, package caches, model APIs | The LAN |
| Its host Mac's bridge address, which is where its DNS comes from | Other runners, on any Mac |
| | Non-routable ranges, including a tailnet's `100.64.0.0/10` |

What it does **not** give, stated so nobody reads more into it:

- **No hostname allowlist.** Softnet filters addresses, so "only GitHub and the
  model APIs" cannot be expressed. `allow` and `block` take IPv4 CIDRs; the
  longest prefix wins, and a prefix both allowed and blocked is blocked.
- **Not the host Mac's own services.** The bridge address is allowed so DNS
  works, which means anything on the hosting Mac that listens on all interfaces
  is reachable from its runners. Bind host services to loopback.
- **Nothing on cloud runners.** exe.dev offers no host-side filter this repository
  knows of, so cloud runners are unfenced. `environment status` reports that
  rather than implying otherwise.

Both the adapter and module evaluation refuse a malformed fence -- an unknown
key, a hostname, an IPv6 range -- instead of creating the guest without it,
because the failure worth preventing is a runner whose manifest says fenced and
which is not. The Orchard inventory reports each guest's fence as the controller
holds it, not as it was requested.

## Tests

- `modules/tests/fleet-slots.nix` -- the pool's shape, that every Mac runner is
  fenced, and probes that build configurations the guards must refuse.
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
