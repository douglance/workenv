# Workenv

A native Rust CLI and MCP server for development workers on local, SSH, and
exe.dev machines. Incurs 0.5.3 exposes the same command handlers to people and
agents.

## Everyday use

Run these from any directory after installation:

```sh
workenv status
workenv up 2
workenv in 2
# Ctrl+b, then q leaves Herdr while remote work keeps running.
workenv down 2
```

Worker numbers, padded numbers, and names are equivalent: `2`,
`02`, and `workenv-02`. Omit the worker from `up` or
`status` to address the fleet. `status --details true` includes the
full probe evidence.

`status` runs one worker health probe and summarizes workspace state, task
IDs, tool readiness, Herdr readiness, Tailscale, subscription authentication,
installed CLI hash, disk bytes, and probe time as separate fields.

`down 2` stops an idle worker's owned runtime and preserves its VM and
disk. exe.dev has no suspend operation. Active tasks, live panes or agents,
and uncertain ownership prevent worker maintenance.

## Tasks

```sh
workenv claim incurs fix-parser
workenv run fix-parser --telemetry true -- cargo test
workenv status fix-parser
workenv services fix-parser
workenv down fix-parser
```

A claim resolves the repository's current HEAD to an exact commit, or accepts
`--revision <full-sha>`. `--source-bundle <local-file>` uploads and
verifies an unpublished Git bundle. `run` returns the remote APoC
execution ID; it does not wait for the build to finish. `--telemetry true`
adds CPU and memory sampling to the APoC execution.

`services` starts the task's devenv processes. `down <task>` stops
owned services, collects and verifies the source changes, and releases the
worker. Active agents/builds and unknown state remain blockers. Explicit
`collect` and `release` commands are also available.

See [operating instructions](docs/operate.md) and
[authentication setup](docs/credentials.md).

## Portable workers

Register an existing Mac or Linux machine as a host, then add workers on that
host. Host and worker names use lowercase letters, digits, and hyphens.

```sh
workenv host add laptop --local true --idempotency-key host-laptop-v1 --format json
workenv host add linux-box --ssh builder@linux.example --directory /srv/workenv --idempotency-key host-linux-box-v1 --format json
workenv worker add scratch --host linux-box --lifetime ephemeral --idempotency-key worker-scratch-v1 --format json
workenv install scratch --source /Users/operator/Developer/src/workenv --idempotency-key install-scratch-v1 --format json
```

Portable hosts need `python3`, `git`, `cargo`, `apoc`, and `herdr` on PATH for
native tools. SSH hosts must accept noninteractive SSH with `BatchMode=yes` and
strict host-key checking. `--tools devenv` is still supported when the host has
a shared devenv executable.

Static workers keep task worktrees under their worker root. Ephemeral workers
use per-task allocation directories and delete only that allocation after
collection, unchanged-source verification, and a `runtime_quiescent=true`
release acknowledgement from the controller. See [portable workers](docs/portable-workers.md).

## Worker profiles

Use named profiles for separate GitHub accounts, Git authors, and tool login
directories. Definitions contain no credentials; each worker logs in locally.

```sh
workenv profile create personal --github-login YOUR_GITHUB_USERNAME
workenv profile assign 2 personal
workenv profile login 2 github
workenv profile status 2
workenv claim PROJECT TASK --profile personal
```

Assignment refuses active work. Herdr terminals and background task commands
use the same profile environment. See [worker profiles](docs/profiles.md) for
Git identity, other tool logins, and directory mappings.

## CLI and MCP

```sh
workenv --mcp
```

MCP uses Incurs' progressive tool discovery. Discover a command's schema, then
call it through the read or write tool as appropriate. Every mutation includes
an idempotency-key option in its command schema.

MCP and noninteractive CLI calls require a stable key. Terminal calls generate
and print one. Reusing a key returns the saved result without repeating an
accepted operation. The same key cannot be used for different arguments.
An interrupted request with an unknown outcome requires inspection.

```sh
workenv up 2 --idempotency-key prepare-02-v1 --format json
```

Configuration comes from `--root`, the MCP server's configured root,
`WORKENV_ROOT`, an ancestor directory containing `fleet.json`, or
`~/.config/workenv/config.json`:

```json
{"root":"/Users/operator/Developer/src/workenv"}
```

Build from source with `cargo build --release --locked`. To install the native
CLI onto configured workers from this checkout, run `workenv install [worker]
--source PATH --idempotency-key KEY --format json`. The installer syncs the
source, configures a self-hosting local controller on the worker, starts a
remote APoC build with telemetry, and writes the installed binary outside the
build target directory.

If install returns `pending`, reuse the same request key to inspect the saved
result after the remote build advances. While a build checkpoint is active, a
later install call observes that checkpoint before syncing sources or starting
another build. The install receipt and retry behavior are still under active
review; check the current source before documenting an unsupported retry path.

## Runtime ownership

```text
workenv (Rust / Incurs)
  +-- CLI and MCP: shared handlers and schemas
  +-- APoC: controller reservations and durable build/test executions
  +-- portable hosts: local or SSH Mac/Linux machines
  |     +-- native PATH tools or a shared devenv environment
  |     +-- Herdr 0.9.0: sessions and agent terminals
  |     `-- remote workspace helper: exact Git claims and verified collection
  +-- exe.dev: legacy persistent Linux VM provider
  `-- Mac controller: Xcode, signing, devices, native UI and GPU checks
```

The current hosted fleet allocates 16 vCPU and 64 GB RAM: four 2-vCPU/8-GB
workers and two 4-vCPU/16-GB workers. `fleet.json` is desired configuration,
not live inventory. Runtime receipts and collected artifacts are under the
ignored `.state/` directory.

The shared devenv includes Herdr, Tailscale, Claude, Codex, Grok, Pi, APoC,
DevSQL, Git, lazygit, Nib, and ssh-clipboard. Versions and local binary artifacts
are pinned in `tools.json`. Authentication remains outside the Nix store,
Git, command arguments, and collected source.

The fleet uses Tailscale SSH for task commands and Herdr connections. Each
worker's `ssh_host` selects its tailnet DNS name. Provider SSH remains the
explicit bootstrap and enrollment transport; task mutations do not silently
retry through a different host.

All six workers are registered in the main Herdr sidebar. Expand a worker and
select its workspace to use its remote agent terminals. The bundled Space
Attention plugin supplies workspace labels and agent state colors, including
an initial refresh when `workenv up` links it into an existing session.

Tailscale, Nib, and coding subscription readiness are reported separately.
No API billing fallback is enabled automatically. Personal projects only;
Work repositories, customer data, and production credentials stay outside this
fleet.

See [native CLI validation](docs/native-validation.md),
[Tailscale and Herdr validation](docs/tailscale-herdr-validation.md),
[rollout acceptance](docs/acceptance.md) and the recorded
[Mac](pilots/mac-results.json) and [Linux](pilots/linux-results.json) pilot
results. Tool installation does not establish coding subscription authentication
or acceptance of an application's own tests. The portable Mac/Linux host
support is source- and regression-verified in this checkout; live multi-platform
fleet proof is still a rollout gate.
