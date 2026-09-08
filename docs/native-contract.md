# Native controller contract

`workenv` is a Rust binary using Incurs 0.5.3. CLI and MCP commands share
handlers, schemas, validation, and persisted mutation receipts.

The controller uses APoC for executions, reservations, and sessions. Herdr owns
interactive terminals, devenv owns project services, and the remote Python
workspace helper owns Git claims and collection verification. Credentials
remain in separate enrollment helpers and private files.

Every mutation binds a request key to its command and arguments. Replays return
the saved result. An interrupted request with unknown outcome requires fresh
inspection. Read probes always use new execution keys.

Claims reserve one configured worker and bind a full revision, repository, task,
worktree, branch, and runtime identity. Collection checks both recorded APoC
executions and the full Herdr pane and agent inventories, including activity
opened manually. Missing directory provenance blocks collection. Release
requires verified downloaded artifacts and an unchanged source fingerprint.

Worker maintenance uses the same reservation namespace as claims. It inspects
all runtime inventory pages and refuses unknown activity before changing the
worker. Worker down stops only its owned idle Herdr runtime and preserves the
VM, disk, worktrees, and collections. Task down stops owned services, collects,
and releases the task. `out` reports the Herdr detach shortcut; `in` opens the
worker session in an interactive terminal and returns a connection descriptor
over MCP.

The installed binary discovers configuration through an explicit root,
WORKENV_ROOT, an ancestor fleet.json, or ~/.config/workenv/config.json. MCP
retains its startup root for all requests. Shared remote tools and subscription
or tailnet authentication are reported separately.
