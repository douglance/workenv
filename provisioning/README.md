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
  open and drive these desktops**, and the guest cannot tell which device connected
  (Serve does not populate identity headers for tagged devices).

That is an acceptable trade for ephemeral demo guests on a personal tailnet. It is
not acceptable for anything holding real credentials, and nothing here should be
read as making it so.

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
