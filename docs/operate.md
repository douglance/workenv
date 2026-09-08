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

## Claim and run work

```sh
workenv claim incurs fix-parser --worker 2
workenv run fix-parser -- cargo test
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
arguments. A disconnected controller does not make the worker available.

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

## Retries and automation

MCP and noninteractive CLI calls must supply an idempotency key:

```sh
workenv claim incurs fix-parser --idempotency-key claim-fix-parser-v1 --json
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
