# Operate a worker

Use the native `workenv` command on the controller. Herdr owns interactive
agent terminals, APoC owns build/test executions, and devenv owns project
services.

## Prepare and enter

```sh
workenv status
workenv up 2
workenv in 2
```

`up` reserves the worker before provisioning or changing its environment.
It refuses occupied or unknown workers, syncs the pinned sources and tools,
starts Herdr, and registers its SSH connection. Missing login state is reported
separately from installed tools and running services.

The default Herdr detach shortcut is Ctrl+b, then q. `workenv out` prints
these instructions; it does not terminate the remote session.
`workenv in 2 --print` returns connection details without opening a TUI.
MCP always returns connection details.

The main Herdr sidebar also connects to all six registered machines through
Tailscale. Expand `workenv-01` through `workenv-06`, then select a workspace
under that machine. Remote agent states feed the same sidebar used for local
agents. `workenv in` opens a separate client onto the same persistent session.

`fleet.json` sets each worker's `ssh_host` to its tailnet DNS name. Omitting
that field selects provider SSH. Bootstrap and enrollment always use provider
SSH so they can prepare a machine before Tailscale is ready.

For portable Mac/Linux hosts, register the host before adding workers:

```sh
workenv host add mac-mini --local true --idempotency-key host-mac-mini-v1 --format json
workenv host add linux-box --ssh builder@linux.example --directory /srv/workenv --idempotency-key host-linux-box-v1 --format json
workenv worker add linux-scratch --host linux-box --lifetime ephemeral --idempotency-key worker-linux-scratch-v1 --format json
```

`host add` probes the target with `python3` and records `transport`, absolute
host root, `tools`, `platform`, and observed tool paths in `fleet.json`.
`worker add` writes worker metadata only; it does not contact the worker.
Portable workers use the host root plus `workers/NAME` unless `--directory`
sets an absolute worker root.

Portable native hosts need `python3`, `git`, `cargo`, `apoc`, and `herdr`.
SSH hosts must allow noninteractive SSH with strict host-key checking. Local
hosts run commands directly. SSH hosts wrap commands in `ssh`; native-tool
hosts prepend the host root's `bin` directory plus `~/.local/bin` to PATH.
See [portable workers](portable-workers.md).

## Claim and run work

```sh
workenv claim incurs fix-parser --worker 2
workenv run fix-parser --telemetry true -- cargo test
workenv status fix-parser
```

The project must be declared in `fleet.json`. Omit `--worker` to
select an available worker of the project's class. The claim records an exact
revision, repository, task, branch, worktree, session, and reservation.

For unpublished source:

```sh
workenv claim incurs local-fix --source-bundle /path/to/source.bundle --revision FULL_SHA
```

The controller verifies and uploads the local bundle into the selected worker's
incoming directory. It does not copy a home directory or credential cache.

`run` starts an exact argv command under remote APoC, records its execution
ID, and returns. Put program flags after `--` so they remain program
arguments. `--telemetry true` asks APoC to sample CPU and memory for the
execution family. A disconnected controller does not make the worker available.

`status` summarizes the worker probe by keeping disk, installed CLI, tool,
Herdr, Tailscale, and auth results distinct. Use `status --details true` when
you need raw health sections and timings.

## Services and completion

```sh
workenv services fix-parser
workenv down fix-parser
```

`services` runs `devenv up` from the task worktree with ownership
labels. `down <task>` stops only owned services, checks remaining activity,
collects source and evidence, verifies the downloaded bytes, and releases the
reservation. The worktree and collection remain on disk.

`workenv collect <task>` collects without releasing. An explicit
`workenv release <task>` requires a verified collection and unchanged
source. Staged changes, unstaged changes, untracked files, and unpublished
commits are preserved. Dirty submodules and unknown execution state prevent
release.

`workenv down 2` addresses the worker instead: it stops only an idle
owned Herdr runtime and leaves the VM and disk intact. Live tasks or panes
prevent the operation. `workenv up 2` starts it again.

Static workers keep task worktrees and service state under the worker root
after release. Ephemeral workers claim under `allocations/<allocation_id>/`;
release removes only that allocation after collection verification and an
explicit `runtime_quiescent=true` acknowledgement from the controller. If
cleanup fails, the worker remains busy in `cleanup_required` until the same
release request succeeds.

## Retries and automation

MCP and noninteractive CLI calls must supply an idempotency key:

```sh
workenv claim incurs fix-parser --idempotency-key claim-fix-parser-v1 --format json
```

A key binds to one command and its arguments. Repeating it returns its saved
result, including pending results. It does not rerun a possibly accepted
mutation. Terminal calls generate and print a key automatically.

When an operation reports pending or unknown state, inspect the returned
execution ID and run a fresh `workenv status`. A request interrupted before
saving its result is not automatically repeated. Issue a new request only
after resolving the observed state. An expired lease alone is never proof
that a worker is idle.

Use `workenv doctor` for provider and worker checks. Credentials and
enrollment steps are in [authentication setup](credentials.md).

## Install the native CLI

```sh
workenv install linux-scratch --source /Users/operator/Developer/src/workenv --idempotency-key install-linux-scratch-v1 --format json
```

`install` selects one worker or, with no worker argument, every configured
worker. It syncs the source checkout, writes local-controller metadata on the
worker, starts `remote/build_workenv.py` through remote APoC with telemetry,
and waits one 30-second slice. A completed build installs `workenv` under the
worker user's `~/.local/bin` and writes the worker's config root.

If the build is still running or unknown, `install` returns `pending` with the
remote execution ID and saves a checkpoint under `.state/controller`. A later
install call observes that checkpoint before syncing sources or starting another
build. Final install receipt behavior is still under review with the installer
implementation; check `src/install.rs`, `remote/build_workenv.py`, and
`remote/install_workenv.py` before documenting a stronger retry guarantee.
