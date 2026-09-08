# Rollout acceptance

Record the exact source revision, command, APoC execution ID, timestamps,
environment, and artifact paths for each result. Do not mark a row accepted from
configuration alone.

| Gate | Required evidence |
| --- | --- |
| Worker bootstrap | Actual CPU/RAM/disk; pinned tool versions; successful repeat install; unchanged authenticated Tailscale identity |
| Tailnet access | Six unique persistent tagged nodes; controller SSH to exedev; policy validation; workers cannot initiate unintended Mac or worker access |
| Herdr registration | Matching 0.9.0 versions; six saved machines and `workenv` sessions; existing `dlance` profile preserved |
| Subscription authentication | Each worker's Codex and Claude subscription status and a small successful agent invocation; no metered fallback |
| Incurs pilot | Exact revision; repository-required Rust gates; source and execution collection |
| Groktris pilot | Exact snapshot; Node 26.6.0; pnpm 10.33.0; matching Playwright browser and melint; full check; browser and PWA gates |
| Private preview | Actual HTTPS app response; browser navigation and WebSocket behavior; local database/service state |
| Worker ownership | Duplicate claims refused; disconnect and expired reservation cause inspection; task and Herdr IDs remain scoped to machine/session |
| Collection | Staged/unstaged changes, untracked files, unpublished commits and evidence restored from collected artifacts; changed source blocks release |
| Reconnection | Disconnect controller transport while task runs; reconnect to the same Herdr session and execution |
| Reboot | Identify which layout restores and which processes resume or stop; preserve source and receipts |
| Authentication failure | Expired/missing auth produces an explicit blocker; no API billing fallback |
| Throughput | Same revisions on Mac/cloud; cold preparation, warm Rust/JS/browser timings, peak memory and load; six representative concurrent tasks |

## Timing protocol

Capture OS, architecture, CPU allocation, RAM, tool versions and background load
before the run. Record dependency preparation separately from execution. Repeat
the same command and exact revision for warm measurements. Preserve raw timing
and memory evidence. Report architecture differences and any source/configuration
differences that prevent a direct comparison.

For the concurrency measurement, start six independently owned tasks on the six
workers. Compare total completed work per elapsed minute and each task's elapsed
time against its isolated baseline. Do not infer equivalent CPU throughput from
the Mac's core count and the provider's vCPU count.

## Recovery protocol

Use a dedicated test task. Record machine, session, pane/agent ID, task revision,
and APoC execution IDs before disconnecting. Verify progress after reconnecting.
Collect a known source change and a known evidence artifact, then validate their
checksums on the controller. Change the source again and verify release is
refused until another collection is acknowledged.

Reboot only the selected worker after preserving the test task. Inspect durable
execution outcomes and restored Herdr layout separately. A restored terminal
layout does not prove process continuity.
