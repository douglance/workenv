# Ephemeral worker provisioning

Three scripts turn a bare Linux guest on any host into a ready-to-use workenv
worker. All are arch-aware (`x86_64` and `aarch64`) and idempotent.

| script | runs on | purpose |
|---|---|---|
| `workenv-up` | controller | one-command bring-up: create, seed, provision, verify |
| `workenv-lima` | VM host (macOS) | slot registry, capacity probe, claim/release/reap |
| `workenv-lima-proxy` | controller | ssh ProxyCommand resolving Lima's port at connect time |
| `workenv-provision` | the guest | nix, devenv, agent CLIs, apoc, PATH |
| `workenv-seed` | the controller | skills, agent configs, credentials |
| `workenv-desktop` | the guest | Xvnc + openbox + browser + noVNC, on demand |
| `workenv-desk-forward` | VM host (macOS) | supervised tailnet publish of one guest's noVNC port |
| `workenv-desk-publish` | controller | publish desktops for a set of slots, verified from here |
| `workenv-worker-enroll` | controller | enrol another Mac as an Orchard worker |

## Usage

One command, from nothing:

```sh
./provisioning/workenv-up wkv-01
```

It probes host capacity and refuses if the host cannot spare the memory, creates the
guest, authorises the controller key, seeds sources/skills/credentials, provisions the
toolchain, and verifies. Re-running against an existing slot re-seeds and re-provisions
instead of recreating, so it is safe to use as a repair.

The individual scripts remain callable: `workenv-seed <target>` and, on the guest,
`workenv-provision`.

## Remote desktops

`workenv-desktop` installs a deliberately light desktop in a guest — Xvnc, openbox,
xterm, a WebKit browser and noVNC — and starts it. It is installed on demand rather
than baked into the image, so a re-created guest simply installs again.

```sh
# in the guest
workenv-desktop --number 3          # install + start, idempotent
workenv-desktop --status            # exit 0 only if everything is actually up
```

`--status` prints the recorded manifest and measures liveness. The counter check
samples twice and requires the value to have advanced, so a server that responds
perfectly while frozen reports `DOWN`, which a single request cannot detect.

From the controller, publish one or more guests on the tailnet and verify each URL:

```sh
./provisioning/workenv-desk-publish wkv-01 wkv-02 wkv-03 wkv-04
./provisioning/workenv-desk-publish --status
```

Each slot gets host port `6400 + N`, so `wkv-03` is always
`http://<vm-host>:6403/vnc.html`. Lima forwards a guest port to the host only when
nothing else has claimed it, so four guests all serving 6080 collide and only the
first is reachable — hence a port per slot. The forward is supervised and redials
when it drops; it is not a launchd job, because a host reboot destroys the Lima
guests too and a forward that outlived it would point at nothing.

## Access control

**There is no VNC password, on any route.** `Xvnc` runs with `-SecurityTypes None`.
The protection is entirely positional:

- The framebuffer binds loopback inside the guest (`-localhost`) and no other
  interface, so it is never reachable by accident.
- Every route in is therefore explicit: `tailscale serve` inside the guest, or
  `workenv-desk-forward` on the VM host binding the host's tailnet address.
- The tailnet ACL is the whole access control. The live grant is
  `{src:["autogroup:member"], dst:["*"], ip:["*"]}`, so **anyone on the tailnet can
  open and drive these desktops**.

**Correction to an earlier version of this section.** It claimed Serve populates no
identity headers. That is backwards. The docs say identity headers are not populated for
traffic *originating from* tagged devices — the guest is the *destination*. A reviewer on
a user-owned device **does** receive `Tailscale-User-Login`. Only tagged clients (other
guests, CI) are anonymous, and `--accept-app-caps` covers those.

That is an acceptable trade for ephemeral demo guests on a personal tailnet. It is
not acceptable for anything holding real credentials, and nothing here should be
read as making it so.

## Measured: 4354s -> 7s, with the full toolchain

The baked image closes the comparison. This is a complete workenv guest, not a bare
Ubuntu: cloned from `workenv-base`, which carries nix, devenv, the `tools.json` closure,
`apoc`, `nib` and the desktop stack.

```
APFS clone done        t+1s
ip assigned            t+5s
SSH READY              t+7s
TOOLCHAIN READY        t+7s

nix 2.35.2   devenv 2.3.0+e0781f7   apoc 0.6.0
codex-cli 0.153.4   claude 2.1.267 (Claude Code)
```

Against the 4354 s clean-room `workenv-up` baseline, that is **622x**. The provisioning
did not get faster; it moved out of the spawn path and happens once per image tag. Image
cost: 16 GB on disk, and APFS clones share blocks, so the second guest is nearly free.

### The bake found a real defect

The first bake produced an image missing `apoc`, `node`, `ripgrep`, `gh` and `uv`. One
cause: `rustc` was SIGKILLed compiling the `apoc` lib crate, and every later stage never
ran. The guest had **7914 MB** and so sat on the safe side of the provisioner's
`MEM_MB -lt 6000` test, taking neither the swapfile nor `-j1` — and still ran out.

The threshold is now gone. Swap is added whenever there is none, and build jobs are
budgeted at roughly 4 GB each and capped at `nproc`. A constant someone must remember to
raise is not a guard.

Two more traps worth recording:

- **Cloud images run `apt` at first boot.** Starting the provisioner into that lock fails
  stage 1 instantly with `Could not get lock /var/lib/dpkg/lock-frontend`, which looks
  nothing like "you started too early". Wait for the lock before provisioning.
- **`rsync` will not create intermediate directories.** Syncing to `~/src/workenv` on a
  guest with no `~/src` fails, and every later stage then fails for reasons that appear
  unrelated.

## Measured: 7 seconds to ssh-ready (bare image)

The premise of the whole re-platform is that spawning an environment should be seconds, not
an hour. Measured on the controller Mac (64 GB / 14 cpu) running `orchard dev` with
Tart 2.32.1 and Orchard 0.56.1, from a cached `ghcr.io/cirruslabs/ubuntu` image:

```
create returned      t+0s
status=running       t+6s
SSH READY            t+7s
guest: aarch64, 2 cpu, 1951 MB
```

**Read the comparison honestly.** The 4,354 s clean-room figure elsewhere in this file is
`workenv-up` on Lima doing create + seed + provision, including compiling `apoc` and `nib`
from source in the guest. The 7 s figure is create-to-ssh from a cached image with none of
that. They measure different things — and that *is* the point: the re-platform moves
provisioning out of the per-spawn path and into a one-time image bake. Spawn becomes an
APFS clone of an already-provisioned disk.

The remaining work to make the comparison exact is baking the workenv toolchain into the
image; the spawn cost does not change when you do, only the image tag does.

### The scheduler actually enforces resources

Three VMs were requested against an idle worker advertising:

```
org.cirruslabs.logical-cores: 14
org.cirruslabs.memory-mib:    65536
org.cirruslabs.tart-vms:      2
```

Each VM consumes `tart-vms: 1`, so the third stayed `pending` with no assigned worker until
capacity was freed. **This is the single defect that most justifies the migration**:
`provisioning/workenv-lima` accepts `--system`, `--cpus`, `--memory-gb` and `--disk-gb` and
ignores every one of them, handing back a fixed-size guest regardless. Orchard refuses to
place a VM it cannot fit.

The `tart-vms: 2` default is worth noting as a density trade-off — Lima currently runs four
guests on `box-03`. It is a default, not a hard cap: `orchard worker run --resources` sets it.

## Enrolling another Mac as a worker

`workenv-worker-enroll <ssh-target> [name] [slots]` installs tart and orchard on a Mac and
registers it with the controller, so an environment can be placed there. Verified across
two Macs:

```
box-03                   cores=  8 mem=  8192MiB slots=1
desk-01.lan.example   cores= 14 mem= 65536MiB slots=2
fleet totals: 22 cores, 73728 MiB, 3 slots
```

Four traps, each of which fails in a way that does not name its cause:

- **A tagged worker cannot dial a user-owned controller.** The default tailnet grant is
  `{src:["autogroup:member"], dst:["*"], ip:["*"]}`, and a tagged machine is not a member,
  so TCP simply times out. Note `tailscale ping` still succeeds — it is a disco ping and
  does not traverse the ACL, so it is not evidence that a connection will work. The script
  works around it with a reverse ssh tunnel in the direction that *is* permitted. **The
  real fix is a grant of `tag:<worker>` → controller on the Orchard port**, which is a
  policy change and is deliberately left to a human.
- **Copying a signed Mach-O invalidates its signature.** The binary is then SIGKILLed on
  exec with no output but `Killed: 9`. Download on the target instead.
- **Even downloaded, `spctl` rejects the orchard release** as "the code is valid but does
  not seem to be an app", despite a valid Cirrus Labs Developer ID. `codesign --force
  --sign -` makes it runnable.
- **Workers need a bootstrap token**, and there is no service account by default. Without
  one the worker exits immediately with `no bootstrap token was provided`.

## Expiry: reap works from cluster state, never from receipts

Orchard has no TTL, so expiry is the one piece of lifecycle that stays ours. `reap` lists
live guests, compares `createdAt` against a lease, and deletes what has expired.

It reads **cluster state, never a receipt**, and that is the point. The create fingerprint
covers the whole environment and host, so a rebuild invalidates it; a sweeper that needed
a receipt would stop working exactly when guests start leaking. This is what today's
`workenv-lima cmd_reap` cannot do — it returns a hardcoded `"tailnet_removed": []`.

Three safety properties, each covered by a test that fails when the property is removed:

- **`name_prefix` is required by the schema, not defaulted.** An omitted prefix would
  otherwise mean "every guest in the cluster is mine to delete". Dispatch refuses the call
  before the adapter runs.
- **An unreadable `createdAt` is skipped and reported, never reaped.** Treating an
  unparseable date as infinitely old would let one malformed record take the fleet.
- **`dry_run` reports without removing**, so a lease change can be checked before it bites.

Verified live: dry run listed the expired guest and left it running; a sweep with a
non-matching prefix touched nothing; the real sweep removed it.

**A field-name trap worth recording**: the controller spells this timestamp `createdAt`
while `scheduled_at` and `started_at` beside it are snake_case. Reading `created_at`
returns empty, which is indistinguishable from a guest with no age — so every guest looks
un-reapable and nothing ever expires.

## Three undocumented Orchard API requirements

Its CLI fills these in; a body written straight against `POST /v1/vms` does not, and
every one of them produces the same symptom: the guest sits `pending` forever with an
**empty status message**, on a cluster with ample free capacity. None is diagnosable from
the response.

| Field | What happens without it |
|---|---|
| `resources: {"org.cirruslabs.tart-vms": 1}` | Never scheduled. The API does not default the slot a guest occupies. |
| `os` | Defaults to `darwin`. A Linux image with `os: darwin` is placed on nothing. |
| `labels` | **Must be omitted unless you mean it.** |

The third is the trap. **Labels are scheduling constraints, not metadata**: a labelled
guest only lands on a worker carrying the same label. Attaching bookkeeping — an
environment name, a lease deadline — makes the guest unschedulable. Workers carry no
labels by default, so *any* label is disqualifying.

That reframes labels usefully rather than as a hazard: they are the mechanism for pinning
a guest to a chosen machine. `workenv.orchard` therefore attaches none of its own and
passes through only what the caller asks for.

The lease needs no label anyway. A guest is named for its environment and the controller
records `created_at`, so a sweeper has everything it needs from live cluster state plus
the lease declared on the binding — which is also the property that lets teardown work
after a rebuild has invalidated the create receipt.

Measured once correct: **create to running in 2-4 s**, destroy removes, and a second
destroy converges to `ready` rather than failing.

## The image/spawn boundary: toolchain is baked, credentials are not

Moving provisioning into a golden image raises a question the Lima path never had
to answer, because it provisioned each guest individually: **what goes in the image?**

The boundary is credentials.

- **Baked into the image:** nix, devenv, the `tools.json` closure, `apoc`, `nib`, the
  desktop stack. Everything reproducible from this repository.
- **Seeded at spawn:** `~/.claude/.credentials.json`, `~/.codex/auth.json`,
  `~/.grok/auth.json`, and the skills trees. Everything specific to the person.

This is not a preference. A Tart image is *cloned* for every guest and can be pushed to
an OCI registry, so a credential baked into it is a credential in every clone and in the
registry. `provisioning/workenv-seed` already streams these over ssh at 0600 and prints
only byte counts; it stays a per-spawn step and is deliberately **not** part of the bake.

The practical consequence is that the bake runs `workenv-provision` directly against
rsynced sources rather than running `workenv-seed` first, even though `workenv-seed` is
what normally delivers those sources. Two things that were one step become two, and the
split is along the line that matters.

## Measured: KasmVNC replaces the hand-rolled stack

Verified on `wkv-02` (Ubuntu 24.04 aarch64, 2 GB, 2 vCPU). These numbers are not published
anywhere and were the gate on adopting KasmVNC over the current TigerVNC + noVNC +
websockify stack.

| check | result |
|---|---|
| `kasmvncserver_noble_1.5.0_arm64.deb` install (`--no-install-recommends`) | resolves, 92 packages, no unmet dependency |
| **`Xvnc` RSS** | **71–77 MB** |
| openbox RSS | 19 MB |
| `MemAvailable` with desktop up | **1624 MB of 1959 MB** |
| CPU, idle with a rendering client | 0.1% of one core |
| framebuffer changing (two `scrot` hashes, 2 s apart) | differ — rendering |
| auth: no credentials / with credentials | **401 / 200** |

That is *less* memory than the stack it replaces, while also absorbing websockify, noVNC,
TLS and a real user store. The `401` is the point: it retires `-SecurityTypes None`.

Traps found bringing it up, none of them documented upstream:

- **The snakeoil cert does not exist on a minimal guest.** `require_ssl: true` is the
  default and the server refuses to start with
  `/etc/ssl/private/ssl-cert-snakeoil.key: certificate file doesn't exist`. Generating it
  with `make-ssl-cert` leaves it `root:ssl-cert 0640`, and adding the user to `ssl-cert`
  does not take effect until a new login session. For an ephemeral guest the simpler answer
  is a user-owned self-signed cert in `~/.vnc/` named in `kasmvnc.yaml`.
- **`logging.level` must be an integer.** `level: info` fails with
  `must be an integer`, and the server exits.
- **The binary is named `Xvnc`, not `Xkasmvnc`** — the same name TigerVNC uses. A
  `pgrep`-based health check cannot tell the two apart, which is one more reason the
  lifecycle belongs in a systemd unit rather than in pattern matching.
- **KasmVNC and TigerVNC contend for `/usr/bin/Xvnc` through `update-alternatives`.**
  Installing `kasmvncserver` silently switches the alternative, so a script that calls
  `Xvnc ... -SecurityTypes None` starts getting KasmVNC and fails on flags it does not
  accept. They can coexist — but only one is `Xvnc` at a time, so **you cannot A/B the
  two stacks on one guest**; switch with `update-alternatives --set Xvnc`, or use
  separate guests. Purging `kasmvncserver` restores `Xvnc` to `Xtigervnc` automatically.
- **Stale X locks survive a killed server.** After switching servers, `/tmp/.X1-lock` and
  `/tmp/.X11-unix/X1` must be removed or the next `Xvnc` dies with
  `vncExtInit: failed to bind socket: Address already in use (98)` — an error that names
  a socket and says nothing about the lock file actually responsible.

## Measured: previewing a web app needs no proxy at all

Both failure classes reproduced on `wkv-02`, then fixed with one command and no new code.

**(i) Dev servers bind loopback.** A server on `127.0.0.1:5173`: reached over the guest's
own external address (`192.168.0.2`) → **connection refused**; over loopback → **200**.
The server is fine; the binding is the whole problem.

**(ii) Frameworks reject unfamiliar `Host` headers** (Vite `allowedHosts`, Next
`allowedDevOrigins`, Rails `config.hosts`). Tailscale Serve does **not** help here — it
preserves the original Host.

**The fix is `ssh -L`, and the proof is the bytes.** With
`ssh -N -L 15173:127.0.0.1:5173 <guest>` and `nc -l 127.0.0.1 5173` capturing raw input
inside the guest, what arrives is:

```
GET / HTTP/1.1
Host: localhost:15173
```

`localhost` is permitted unconditionally by every framework allowlist, so (ii) is sidestepped
*by construction* rather than by configuration. `workenv-desk-forward` was doing exactly
this; it needed 117 lines only because it ran on the Mac mini multiplexing four guests onto
one loopback. From the reviewer's own machine it is one command.

## Reaching a scheduled guest: `workenv.orchard connect`

Every other connection in this repo needs `target.address`. A scheduled guest cannot have
one: placement is chosen after the manifest is written, and the provider response never
feeds back into it. So a guest could be created and then not reached at all.

`connect` closes that. It answers from the request alone -- no cluster read -- with argv that
tunnels by name through the controller:

```
orchard ssh vm <guest> [<command>]
```

Verified live against a guest scheduled onto `box-03`, with no address anywhere in the
manifest or the response: exit 0 and `Linux ubuntu 7.0.0-30-generic ... aarch64`.

Two details that are not obvious:

- **The command is ONE argument, not spread.** `orchard ssh vm` takes at most two
  positionals. Spreading `uname -a` both overflows that and lets the command's own flags be
  parsed as orchard's, failing with `unknown shorthand flag: 'a'` -- which names neither
  orchard nor uname.
- **No cluster read on purpose.** Reaching in has to keep working while the controller is
  briefly unreachable, which is exactly when someone is trying to get in and look.

### The transport: running work inside a guest that has no address

`connect` returns argv a human can use. `workenv-core` needs something else to
place a *target-located* extension (`identity`, `clipboard`): it calls the
host's transport with `execute`, one exact argv, and reads back
`{exit_code, stdout, stderr}`. Every other transport dials `target.address`, so
before `workenv.orchard` gained `execute`, an Orchard-backed host could be
created and then could not run an integration at all.

The outer hop is `orchard ssh vm <name>`. Probed against a live guest before
any of it was written:

| property | measured |
|---|---|
| stdout cleanliness | exactly the child's bytes (`4d41524b4552`), no banner |
| stdin | passes through (`wc -c` returned 11) |
| exit code | **not propagated** -- `exit 7` surfaces as `1` |
| stderr | prefixed with orchard's `no credentials specified or found...` |

The last two decide the design. The child's result is not read off the orchard
process at all: a wrapper script inside the guest captures the child's own
streams and status and prints one JSON object on stdout, which is the stream
measured to be clean. Verified end to end -- a child that writes to both
streams, reads stdin and exits 7 comes back as exit_code 7, `err-here\n`, and
`out-here\npiped-stdin-payload`, with the orchard banner gone.

The script travels base64-encoded, because that alphabet contains no shell
metacharacter and so cannot be re-split by the guest's shell.

### `orchard port-forward vm` is broken; there is no `preview` operation

A port-forwarding counterpart was written, verified against its own tests, and then
**deleted** rather than shipped. `orchard port-forward vm <guest> <local>:<remote>` binds its
local listener and prints `forwarding 127.0.0.1:19488 -> <guest>:8080...`, then fails every
data transfer:

```
failed to forward port: ... failed to read frame header: EOF
```

Reproduced on **both workers**, against a guest whose server answers 200 to itself, across
three retries, while `orchard ssh` to that same guest works. An adapter operation returning
`ready` with that argv would report success and hand the caller a dead port.

The correct mechanism for a future preview feature is Orchard's **`endpoints`**: the worker
binds the listener itself and reports the port it actually bound back as
`observedEndpoints[].workerPort`. That is read-back rather than assumption, and it does not
route bytes through the broken `port-forward` gRPC stream. `ssh -L` (above) remains the
zero-code path in the meantime.

## The fleet root

`presets/personal.nix` holds the hosts, but nothing imported it, and the
repository's own `devenv.nix` enables no adapters -- so running the CLI from the
repo root reported no environments and the shipped fleet was unreachable from
the shipped CLI. `fleet/` is the join:

```sh
workenv --root fleet environment list
workenv --root fleet environment up wkv-fast
```

`wkv-fast` is the address-free host: `workenv.orchard` is both the provider that
creates the guest and the transport that reaches it. Nothing in the manifest
names a machine, so the environment lands on whichever worker has capacity.

Three defects had to be fixed before that root would load at all, each of which
only appears once a manifest has real content in it:

- **A manifest slower than 30s could never be loaded.** `ApocExecutor::execute`
  caps its first wait at 30s inside the code-mode runner, then reports the
  result. A fleet manifest evaluates in 55s (measured), so it came back with no
  exit code and surfaced as `devenv manifest evaluation did not complete
  successfully` with an *empty* stderr -- an error naming neither the cause nor
  the timeout. It now keeps observing the execution APoC already accepted until
  the caller's own budget is spent.
- **An unavailable tool took down the whole shell.** `modules/tools.nix`
  resolves a tool the host cannot run to a placeholder carrying the *tool's*
  `meta.platforms`, so nixpkgs' `check-meta` refuses to evaluate it and every
  command fails with `Refusing to evaluate package
  'apoc-unsupported-on-aarch64-darwin'`. The module already computed
  `availableNames` and simply never used it; `packages` now uses the filter.
- **`workenv.clipboard` cannot be enabled on an Apple Silicon controller.** It
  is an xclip/xsel integration declared `x86_64-linux` only, and enabling it put
  an unbuildable package in the devenv shell. This fleet is Macs only, so it is
  off.

## Supervision: the cluster was three unsupervised processes

Before `workenv-controller-install`, the fleet was held together by an
`orchard dev` (pid parented to launchd), a hand-started worker on `box-03`, and a
single `ssh -N` reverse tunnel -- none supervised, none surviving a reboot, and
`~/Library/LaunchAgents` empty on both machines. Three things were wrong with
the controller specifically, all measured rather than assumed:

- `orchard dev --help` says "for development purposes" and bundles a worker into
  the controller process.
- its `--data-dir` defaults to the **relative** path `.dev-data`, so the cluster
  database landed wherever the process was started. It was found under a
  per-session scratchpad in `/private/tmp`, which is reaped -- every worker
  registration and service account with it.
- it bound `*:6120` (confirmed with `lsof` against the live process) while
  answering to **default `admin:admin` credentials**, on a tailnet whose grant is
  `dst:["*"]`. That is the fleet's whole control plane offered to every device
  on it.

`workenv-controller-install` replaces it with a launchd agent running a real
controller, an absolute data dir under `$HOME`, and `--listen 127.0.0.1:6120`.
It refuses to load an agent whose listener is not loopback, and verifies after
starting that nothing is listening on all interfaces -- the `--insecure-no-tls`
that keeps the existing context URL working is only safe while that holds.

### Two things supervision only taught by being tested

Both were found by killing the process and watching, not by reading the code.

- **A 401 is the readiness signal, not a failure.** `orchard dev` answered with
  default `admin:admin`. A real controller mints a random `bootstrap-admin`
  account on first start and answers 401 to an unauthenticated probe -- so the
  install script's original `curl -f` readiness check reported a correctly
  secured, fully working controller as one that never came up. Readiness is
  "the port speaks HTTP" (200/401/403), and the 401 is the deliverable.
- **The worker needs its token on every start.** `orchard worker run` has no
  persisted-credential flag. Supplying the token once at enrolment, on the
  theory that it created a durable identity, produced a supervised worker that
  crash-looped on `no bootstrap token was provided` **while
  `orchard list workers` still showed the worker present** -- what the
  controller had seen was the one-shot enrolment process, which then exited. The
  token now lives in a 0600 file read on stdin at each start, never in argv and
  never in the plist.

Proven by killing each of the three and watching launchd bring it back with a
new pid: controller 11187 -> 24829, worker 45594 -> 45858, tunnel -> 24904.

Also note the first boot writes the generated token to the controller log, so
that log is a credential at rest from the start; the install script now creates
`~/Library/Logs/workenv` and `~/.orchard` as 0700.

### Credentials: the adapter cannot use the manifest

Securing the controller broke every adapter operation with `401 Unauthorized`,
because `adapters/orchard` talked to the API with no credentials at all and had
only ever been tested against a wide-open dev controller. The protocol forbids
secrets in `config` or `input` -- a manifest is committed -- so the token
reaches the adapter another way: a two-line file, account then token, at
`~/.orchard/workenv-credentials` (0600), overridable by
`WORKENV_ORCHARD_CREDENTIALS`, or by `WORKENV_ORCHARD_ACCOUNT` /
`WORKENV_ORCHARD_TOKEN` for CI.

Two lines rather than reading orchard's own YAML: a generated token can contain
`": "`, and a line-oriented YAML reader would hand the controller a silently
truncated secret, which fails as a wrong password rather than as a parsing bug.
A real YAML parser would be a new dependency for six lines of config. Measured:
HTTP Basic with the service account name and token answers 200.

The adapter's account is **compute-scoped, never the bootstrap admin**, and the
install script creates it. Both halves of that were learned by breaking it:

> Rotating the bootstrap account by deleting and recreating it **locked admin
> operations out of the controller entirely**. The bootstrap account is minted
> only when NO service accounts exist, so with a compute-scoped account still
> present nothing re-mints it, and no remaining account could create one. The
> fleet kept working -- the adapter and worker were unaffected -- but there was
> no recovery short of moving the data directory aside and re-enrolling. The
> script now creates the scoped account *from* the bootstrap account and never
> deletes the bootstrap account.

Three smaller things the script had to stop doing, each of which produced a
confident wrong answer rather than an error:

- `head -c -1` is a GNU extension; on macOS it fails with "illegal byte count",
  which made the credential check report a working credential as a 401.
- reading the bootstrap account with `head -1` takes the *first* line in the
  log, so a re-run after a rotation read a stale, redacted entry and wrote
  credentials that could not authenticate. It reads the last one now.
- `tr -dc ... < /dev/urandom | head -c 48` aborts the whole script with status 2
  and no message, because `head` closing the pipe sends `tr` SIGPIPE and
  `set -o pipefail` turns that into a failure. Token generation uses `openssl`.

## Verified end to end

Against the supervised, authenticated cluster, through the shipped CLI, with no
address anywhere in the manifest:

```
workenv --root fleet extension call workenv.orchard create --environment wkv-fast
  -> status ready, guest "wkv-fast" running, assigned worker "box-03"
```

and then work run inside that guest through the transport: a child writing both
streams, reading stdin and exiting 5 came back as `exit_code: 5`,
`stdout: "Linux aarch64\nadmin\ncarried-through-the-transport"`, `stderr: ""`.
Placement was chosen by the scheduler; nothing in the manifest names a machine.

## Known: every adapter call costs ~100s of devenv shell

`crates/workenv-core/src/adapter.rs` wraps any `/nix/store` adapter in
`devenv shell -- <path>`. Measured on a warm tree:

| invocation | time | result |
|---|---:|---|
| `devenv shell -- <adapter>` | **100,000 ms** | correct |
| `<adapter>` directly | **39 ms** | identical |

The executable is already an absolute store path and needs no shell to run. An
`environment up` with a provider and two integrations is three adapter calls, so
roughly five minutes passes before any work starts -- which is why
`extension inspect` (manifest only) returns instantly while `extension call`
looks like it has hung. Several verification runs in this session were written
off as failures for exactly this reason.

It is **not** changed here on purpose. The wrapper plausibly exists so adapters
inherit the devenv shell's PATH, and `herdr`, `tailscale` and `bootstrap` all
shell out to tools that shell provides. Changing how all nine adapters are
invoked while only one or two can be exercised from this machine (exedev needs
exe.dev, tailscale needs the tailnet) would trade a measured 2,500x win for an
unmeasured risk of breaking adapters silently. Whoever takes it should confirm,
per adapter, what it actually needs on PATH.

## Known: `adapters/project` is built, accepted, and enabled by nothing

Unlike `adapters/pool`, which was deleted because nothing referenced it,
`adapters/project` works -- git history carries "Record live project up and down
acceptance" -- but `project.enable` appears only in
`.state/project-lifecycle/controller/devenv.nix`, a working-state manifest,
never in `presets/personal.nix`. It should be wired into the shipped preset, not
removed.

## Traps this encodes

Each of these cost real time to find; the scripts handle them so you don't rediscover them.

- **`trusted-users` must be set before installing devenv.** A fresh Nix install leaves it
  unset, so `--accept-flake-config` silently ignores devenv's cachix and Nix compiles
  `cachix-api` from source. Setting it plus the substituters took the devenv install from
  an hours-long Haskell build to **45 seconds**.
- **A C toolchain is required.** Rust cannot link without `cc`; `proc-macro2`'s build
  script fails with a bare "No such file or directory".
- **Small guests OOM building apoc.** rustc is SIGKILLed compiling the final crate in
  3 GB. The provisioner adds 8G of swap and drops to `-j1` below 6 GB of RAM.
- **`target` may be a symlink** into a host cache directory; rsync brings it across and
  cargo then fails with "Not a directory".
- **apoc needs `extensions/`** at compile time via `include_str!`, even though it looks
  like a runtime-only directory.
- **Credentials never appear in output.** `workenv-seed` streams them over ssh, including
  Claude's, which lives in the macOS Keychain (`Claude Code-credentials`) and must land as
  `~/.claude/.credentials.json` on Linux.
- **Bulk is excluded deliberately.** `~/.agents/artifacts` is 33 GB and
  `~/.codex/logs_2.sqlite` is 1.8 GB; only skills, agent definitions, instruction files
  and configs are seeded.
- **macOS truncates the argv that `pkill -f` matches against.** A pattern anchored on
  the tail of a long `ssh -L …` invocation matches nothing, which is indistinguishable
  from a process that was already stopped. `workenv-desk-forward` kills by listening
  port via `lsof -t` instead. Found by running it — the forwards outlived their `pkill`.
- **There is no `setsid` on macOS.** `nohup` plus redirected stdin is what actually
  detaches a process from a closing ssh session there; `setsid` fails and silently
  takes the whole launch with it.
- **Epiphany's `restore-session-policy` has no `'never'`.** The enum is `'always'` or
  `'crashed'`; anything else fails with "value is outside of the valid range". Paired
  with `2>/dev/null || true` that error vanished and the browser went on restoring
  every previous tab, failed loads included, while the script reported success.
- **Two `cat` readers on one ssh stdin gives the second an empty file.** Piping a
  script through a host to a guest with `cat > file && limactl shell … 'cat > file'`
  writes zero bytes in the guest, and the empty script then runs and exits 0. Compare
  byte counts after any such copy.
- **A successful request does not prove a counter is counting.** The old check made one
  request and passed on a frozen server. Two samples with a strict increase is the
  cheapest check that can actually fail.

## Clean-room result

Destroyed the guest, wiped host state, and ran `workenv-up wkv-01` against nothing:

```
workenv-up complete in 4354s   (~73 min, unattended)
```

Most of that is two source builds on 2 cores (apoc ~20 min, nib ~45 min) plus a 13 GB
nix closure. A host with a prebuilt golden image skips all of it.

That run caught two real gaps that hand-provisioning had hidden: `herdr` was missing from
the devenv package list, and `code-review-graph` had only ever been installed by hand.
Both are now in the provisioner, and a second idempotent run closed them in minutes.

It also proved the ssh fix: the guest was destroyed and recreated, Lima assigned a new
port, and `ssh wkv-01` kept working with no configuration change.

## Verified on

`box-03` (Mac mini M2, 8 GB), guest `wkv-01` — Ubuntu 24.04 aarch64, 2 vCPU / 3 GB / 30 GB:

```
nix 2.35.2   devenv 2.3.0+e0781f7 (aarch64-linux)   apoc 0.6.0   nib 0.3.2
codex-cli 0.153.4   claude 2.1.267   herdr 0.9.0   grok 1.0.13   pi 0.84.2   devsql 0.5.0
code-review-graph 2.3.2
node v22.23.2   npx 10.9.8   ripgrep 15.2.0   fd 10.5.0   gh 2.100.0   uv 0.12.5

claude -p  -> AUTH_OK              (real API call)
codex exec -> CODEX_AUTH_OK        (real API call, 35,717 tokens)
apoc execution command echo -> outcome: passed
npx -y @modelcontextprotocol/server-everything -> launches (MCP path works)

.claude/skills 469   .codex/skills 70   .agents/skills 312   .claude/commands 8
```

## Tools come from `tools.json`, not from curl

The five pinned agent CLIs (`devsql`, `codex`, `claude`, `grok`, `pi`) are built by
`modules/tools.nix` from `tools.json` and materialised through `devenv`, then symlinked
into `~/.local/bin`. There is no parallel download path: one pinned, hash-verified
definition serves every architecture.

`tools.json` entries now carry a `by_system` map:

```json
"by_system": {
  "x86_64-linux":  { "url": "…x86_64…", "hash": "sha256-…", "member": "…" },
  "aarch64-linux": { "url": "…aarch64…", "hash": "sha256-…", "member": "…" }
}
```

`modules/tools.nix` merges the entry for the evaluating system over the base spec, and
derives `meta.platforms` from the declared keys. A tool is classified as pinned or local
by whether *any* variant declares the field, so an unsupported architecture surfaces as
an unsupported package rather than an unknown one.

Verified on both architectures from the nix store:

| | aarch64-linux (`wkv-01`) | x86_64-linux (`workenv-01`) |
|---|---|---|
| devsql | 0.5.0, ELF ARM aarch64 | 0.5.0, ELF x86-64 |
| codex | 0.153.4, ELF ARM aarch64 | 0.153.4, ELF x86-64 |
| claude | 2.1.267, ELF ARM aarch64 | 2.1.263, ELF x86-64 |
| grok | 1.0.13, ELF ARM aarch64 | 1.0.13, ELF x86-64 |
| pi | 0.84.2 | 0.84.2 |

## Built from source (no upstream arm64 release)

- **apoc** — needs `extensions/` at compile time and 8G of swap on a 3 GB guest.
- **nib** — ships no `linux-aarch64` release and no `Cargo.lock` (so no `--locked`). Links
  pipewire, EGL, gbm and xkbcommon-x11 even headless; the provisioner installs those.

## Not portable as-is

- **`work`** is *not* macOS-specific — it is a Node CLI. But it needs 22 dependencies
  including private `@work/*` workspace packages and `incur`, i.e. its 1.2 GB
  monorepo. Reaching it over http from the controller is the better answer than
  installing it in every ephemeral guest.
- **`tmppr`** is the same shape: a local monorepo with 68 dependencies and no built
  `dist`, unpublished to npm.
- **`todoist` and `uidotsh`** are `http` MCP transports and need nothing installed.
- **`obs` and `testnode`** launch via `npx`, which works.
