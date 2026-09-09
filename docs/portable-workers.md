# Portable workers

Portable workers let Workenv use an existing local or SSH Mac/Linux host
instead of the legacy exe.dev provider path. The controller still owns the
command graph, APoC owns executions, Herdr owns terminals, and the Python
workspace helper owns task claim, collection, and release state.

## Prerequisites

Install these on each native host before `workenv up`, `workenv install`, or
task execution:

- `python3`
- `git`
- `cargo`
- `apoc`
- `herdr`

SSH hosts must accept noninteractive SSH as `USER@HOST` with `BatchMode=yes`
and `StrictHostKeyChecking=yes`. The configured host root must be an absolute
path. Workenv may also run a host with `--tools devenv` when a shared devenv
executable is available.

## Register a host and worker

```sh
workenv host add laptop --local true --idempotency-key host-laptop-v1 --format json
workenv host add linux-box --ssh builder@linux.example --directory /srv/workenv --idempotency-key host-linux-box-v1 --format json
workenv worker add linux-build --host linux-box --class general --cpus 8 --memory-gb 32 --disk-gb 250 --lifetime static --idempotency-key worker-linux-build-v1 --format json
workenv worker add scratch --host linux-box --lifetime ephemeral --idempotency-key worker-scratch-v1 --format json
```

`host add` requires exactly one transport: `--local true` or `--ssh USER@HOST`.
It probes the host with `python3` and records `transport`, absolute `root`,
`tools`, and `platform` in `fleet.json`. The response includes observed tool paths. If `--directory`
is omitted, the host root is the probed user's `~/.local/share/workenv`.

`worker add` requires an existing host. It defaults `--class` to `general`,
`--cpus` to `2`, `--memory-gb` to `8`, `--disk-gb` to `50`, and `--lifetime`
to `static`. The default worker root is `HOST_ROOT/workers/NAME`; pass
`--directory` for another absolute root. The command updates `fleet.json` but
does not contact the worker.

Names use lowercase letters, digits, and hyphens. Numeric legacy selectors
still resolve to `workenv-NN`, so portable worker names need at least one
letter.

## Install the native CLI on a worker

```sh
workenv install linux-build --source /Users/operator/Developer/src/workenv --idempotency-key install-linux-build-v1 --format json
```

`install` syncs the selected source checkout to the worker environment root,
writes local-controller metadata, and starts a remote APoC build shaped like
this:

```sh
apoc execution start python3 \
  --purpose "Build and install native workenv CLI for WORKER." \
  --idempotency-key KEY:cli:WORKER:build \
  --cwd HOST_ROOT \
  --timeout-ms 600000 \
  --expect-exit-code 0 \
  --verbosity trace \
  --label workenv.component=cli-install \
  --label workenv.worker=WORKER \
  --label workenv.worker.name=WORKER \
  --telemetry \
  --format json \
  -- HOST_ROOT/remote/build_workenv.py --root HOST_ROOT
```

The build helper runs `cargo build --release --locked` with
`CARGO_TARGET_DIR=HOST_ROOT/.state/rust-target`, verifies that the source did
not change during the build, installs the binary into `~/.local/bin/workenv`,
and writes the worker's Workenv config. If a matching install receipt and
installed binary already exist, the helper reports `skipped`.

`install` may return `pending` with the APoC execution ID after the first
30-second wait slice. Repeat the same command and key to observe that build.
A request for changed source waits for any earlier build on the same host
environment to finish, then builds the requested source. A completed request
replays its saved result. Use a new key to install later changes.

Installation configures `~/.local/bin` in Bash or Zsh startup files and exposes
the build environment's APoC executable there when no user entry exists.
Existing user APoC entries are preserved. Reinstalling a matching binary skips
Cargo while checking the CLI configuration and command availability again.

## Static and ephemeral workers

Static workers use stable paths under their worker root:

- worktrees: `WORKER_ROOT/worktrees/TASK_ID`
- service state: `WORKER_ROOT/state/services/TASK_ID`
- collections: `WORKER_ROOT/collections/TASK_ID/COLLECTION_ID`

Ephemeral workers keep the host environment root stable and claim each task
under `WORKER_ROOT/allocations/ALLOCATION_ID/`. Release removes only that
allocation after collection verification. The controller must send
`runtime_quiescent=true` for ephemeral release cleanup. If cleanup fails, the
workspace stays busy with `cleanup_required`, and replaying the same release
request retries cleanup instead of deleting a new allocation.

Collections, ownership records, and retry receipts remain under the stable
worker root after release. Allocations share the host OS and user account.
The configured CPU, memory, and disk values describe scheduling capacity;
they do not impose process resource limits or VM isolation.

## Status and telemetry

`workenv status` runs one health helper per worker and summarizes separate
signals:

- `workspace`: task availability from the workspace helper
- `tools`: required native or devenv tools
- `herdr`: session readiness
- `auth`: Codex and Claude subscription readiness
- `tailscale`: tailnet readiness when applicable
- `cli`: installed `workenv` path and SHA-256 when available
- `disk`: total, used, and free bytes at the nearest existing root path

Use `workenv status WORKER --details true --format json` for the full probe.
Use `workenv run TASK --telemetry true -- COMMAND ...` to ask remote APoC to
retain CPU and memory samples for that execution family.

The shared devenv environment includes pytest and Node for Workenv's own test
suites. Tests that inspect previously captured pilot bundles and repositories
report an explicit skip when those ignored local artifacts are absent from a
fresh checkout.

## Validation status

The [self-hosting validation](self-hosting-validation.md) records current live
Mac/Linux lifecycle tests, automatic CLI installation on all seven machines,
worker test runs, self-installation, and resource measurements. Earlier live
validation covers the native CLI and six-worker exe.dev rollout path in
[native CLI validation](native-validation.md),
[Tailscale and Herdr validation](tailscale-herdr-validation.md), and
[rollout acceptance](acceptance.md).
