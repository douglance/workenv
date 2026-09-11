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

## Measured: 7 seconds to ssh-ready

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
