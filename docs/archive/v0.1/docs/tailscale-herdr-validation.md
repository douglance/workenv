# Tailscale and main Herdr validation

Observed on September 8, 2026. This record is for the fleet operator and
distinguishes connectivity, visible agent state, and subscription readiness.

## Fleet connections

All six nodes enrolled in `example.ts.net` with `tag:workenv`. Tailscale SSH
from the Mac succeeded for each node. Their Herdr 0.9.0 servers use the named
`workenv` session; the main Mac Herdr has an enabled connection for each.

| Worker | Tailscale IP | Herdr SSH target |
| --- | --- | --- |
| workenv-01 | 100.64.0.11 | exedev@workenv-01.example.ts.net |
| workenv-02 | 100.64.0.12 | exedev@workenv-02.example.ts.net |
| workenv-03 | 100.64.0.13 | exedev@workenv-03.example.ts.net |
| workenv-04 | 100.64.0.14 | exedev@workenv-04.example.ts.net |
| workenv-05 | 100.64.0.15 | exedev@workenv-05.example.ts.net |
| workenv-06 | 100.64.0.16 | exedev@workenv-06.example.ts.net |

The saved policy permits the owner to SSH as `exedev` to tagged workers.
Personal devices retain their previous network access. Tagged workers do not
receive the former wildcard initiation grant. The policy editor validated the
owner-access and worker-isolation tests. A live worker-01 probe timed out when
connecting to Mac `100.64.0.10:22` and worker-02 `100.64.0.12:22`.
This proves those two tested paths, not every possible destination or port.

Evidence: connection inventory
`exec_0001788909905837_000000000000016f`; live isolation execution
`01a08366-6e0b-7863-a8fd-a26544eb3cf1`.

## Visible workspaces and agents

The main Herdr custom sidebar uses Space Attention metadata. Remote workspace
rows were blank until the plugin was installed on every remote server.
The bundled copy supports Linux and adds an explicit refresh action because
Herdr does not run startup hooks when a plugin is linked into a running server.
All six refresh logs completed with `status=succeeded`, `exit_code=0`.
The actual Ghostty accessibility view showed all six connected machine
headings with visible workspace labels at 23:41 UTC.

A separate `fleet-check` workspace on worker-01 (`w2:p1`) appeared in the main
Herdr agent list at 23:44 UTC. The pre-existing Codex pane `w1:p1` was preserved.
The check exposed an incomplete Codex package: the CLI binary had been copied
without `codex-code-mode-host` or its resources. The Nix derivation now installs
the complete pinned vendor runtime. All six workers rebuilt it and verified
the host, bundled zsh, bubblewrap, and ripgrep are executable.

Evidence: refresh logs `exec_0001788910921622_000000000000018b`;
package verification `exec_0001788911175289_0000000000000197`.

After relaunching only the check agent, its command runner executed `hostname`
and returned `workenv-01`. The deployed bootstrap directory has no README or
fleet manifest, so the agent correctly reported those requested files absent.
Its completed terminal remains available under `workenv-01 > fleet-check`.
Command-output evidence: `01a0836c-1e1f-7fa3-9b9c-33a45c6949c2`.

## Authentication limits

Codex subscription authentication was present on worker-01. Workers 02 through
06 still require their own Codex login. Claude subscription authentication was
absent on all six. No subscription cache was cloned and no API billing fallback
was enabled. Existing Codex processes must be restarted to use the repaired
runtime; their running sessions were not forcibly replaced.

## Source verification

The Rust suite and Clippy passed after the SSH-host and sidebar refresh
changes. The refresh tests use the observed Herdr JSON envelopes and reject
success-looking logs with nonzero exit codes. Enrollment tests passed: 14 tests.
Space Attention's 24 tests passed separately on each Linux worker.

Rust suite: `01a0836a-4ecd-7142-84fa-9770fc9d85f9`.
Clippy: `01a0836a-7fc9-7d82-a343-61c82f93d05d`.
Enrollment tests: `01a08368-43fb-7100-b459-ed0c5002665e`.

The final release build passed (`01a0836b-5410-7a91-9476-91acdf5446e7`).
The installed CLI at `~/.local/bin/workenv` has SHA-256
`39d6bd17c96af268e25b4178d2e3bb570c8a06698aa438a9c5156dc629b37262`.
Its `in 1 --print --json` output selected the enabled Tailscale machine profile
and `workenv` session (`01a0836c-83cd-7372-8538-05efdb3c41c2`).
