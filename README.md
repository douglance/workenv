# Workenv

A native Rust CLI and MCP server for six persistent personal development
workers on exe.dev. Incurs 0.5.3 exposes the same command handlers to people
and agents.

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
`status` to address the fleet. `status --details` includes the
full probe evidence.

`down 2` stops an idle worker's owned runtime and preserves its VM and
disk. exe.dev has no suspend operation. Active tasks, live panes or agents,
and uncertain ownership prevent worker maintenance.

## Tasks

```sh
workenv claim incurs fix-parser
workenv run fix-parser -- cargo test
workenv status fix-parser
workenv services fix-parser
workenv down fix-parser
```

A claim resolves the repository's current HEAD to an exact commit, or accepts
`--revision <full-sha>`. `--source-bundle <local-file>` uploads and
verifies an unpublished Git bundle. `run` returns the remote APoC
execution ID; it does not wait for the build to finish.

`services` starts the task's devenv processes. `down <task>` stops
owned services, collects and verifies the source changes, and releases the
worker. Active agents/builds and unknown state remain blockers. Explicit
`collect` and `release` commands are also available.

See [operating instructions](docs/operate.md) and
[authentication setup](docs/credentials.md).

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
workenv up 2 --idempotency-key prepare-02-v1 --json
```

Configuration comes from `--root`, the MCP server's configured root,
`WORKENV_ROOT`, an ancestor directory containing `fleet.json`, or
`~/.config/workenv/config.json`:

```json
{"root":"/Users/operator/Developer/src/workenv"}
```

Build from source with `cargo build --release --locked`. Keep the installed
binary outside disposable build directories. The controller requires APoC,
SSH, Git, and Herdr on PATH.

## Runtime ownership

```text
workenv (Rust / Incurs)
  +-- CLI and MCP: shared handlers and schemas
  +-- APoC: controller reservations and durable build/test executions
  +-- exe.dev: six persistent Linux VMs
  |     +-- Nix / devenv: shared tools and project services
  |     +-- Herdr 0.9.0: sessions and agent terminals
  |     `-- remote workspace helper: exact Git claims and verified collection
  `-- Mac: Xcode, signing, devices, native UI and GPU checks
```

The fleet allocates 16 vCPU and 64 GB RAM: four 2-vCPU/8-GB workers and two
4-vCPU/16-GB workers. `fleet.json` is desired configuration, not live
inventory. Runtime receipts and collected artifacts are under the ignored
`.state/` directory.

The shared devenv includes Herdr, Tailscale, Claude, Codex, Grok, Pi, APoC,
DevSQL, Git, lazygit, Nib, and ssh-clipboard. Versions and local binary artifacts
are pinned in `tools.json`. Authentication remains outside the Nix store,
Git, command arguments, and collected source.

Provider SSH keeps workers accessible while Tailscale enrollment is pending.
Tailscale, Nib, and coding subscription readiness are reported separately.
No API billing fallback is enabled automatically. Personal projects only;
Work repositories, customer data, and production credentials stay outside this
fleet.

See [native CLI validation](docs/native-validation.md),
[rollout acceptance](docs/acceptance.md) and the recorded
[Mac](pilots/mac-results.json) and [Linux](pilots/linux-results.json) pilot
results. Tool installation does not establish coding subscription authentication
or acceptance of an application's own tests.
