# Self-hosting and portable worker validation

Verified on September 9, 2026, between 03:35 and 04:20 UTC. Workenv built,
tested, updated, collected, and released its own source on actual workers.

## Installed fleet

Fresh shell probes verified executable discovery, the installed binary's
SHA-256 against its installation receipt, and the receipt's Rust source digest
against the source present on each of seven physical machines.

All installations matched this digest of Cargo manifests, Cargo.lock, and
Rust source:

```text
sha256:6f76a9b6ae9c642d5bd3900efa5afcac167a735a0a3e7733632fb9cd85dcdb24
```

| Machine | Installed binary SHA-256 |
| --- | --- |
| desk-01, macOS | `0000000000000000000000000000000000000000000000000000000000000000` |
| workenv-01, Linux | `63d756e4e91a2e75a641a50d0641a0046568a2475a46c3bf981e9d34d1b8d377` |
| workenv-02 through workenv-06, Linux | `74682ea659ca893593d3ccec93e0f99102beb3c7803db0f0a1180f4cfdf58b3b` |

The installed Mac CLI completed a fresh ten-worker status check in 36.4
seconds. All ten configured workers reported `available`, with their CLIs
present and no runtime blockers. Agent subscription authentication is reported
separately; runtime availability does not establish agent sign-in readiness.

Status execution: `01a08463-9852-7f33-9fda-042857ed49c2` on desk-01.

## Worker lifecycle proof

| Mode | Worker and result |
| --- | --- |
| macOS, local, static | `mac-static-selftest-0348`: Rust and Python suites ran through `workenv run`; fixes were applied there; collection and release passed. |
| macOS, local, ephemeral | `mac-ephemeral-proof-0359`: generated output and a tracked edit were collected; release removed the allocation and retained collection and ownership storage. |
| Linux, local self-controller, static | `selfhost-linux-0406` on workenv-02: its installed CLI claimed source, applied fixes, ran both suites, installed the CLI built from its task checkout, then collected and released the task. |
| Linux, generic SSH, ephemeral | `linux-ephemeral-proof-0405` on workenv-06: generated output and a tracked edit were collected; release removed the allocation and retained collection and ownership storage. |

The generic SSH `linux-static` and `linux-ephemeral` sessions were also started
and registered independently on the same existing host. They coexist with its
legacy exe.dev worker session. macOS SSH transport has source-level coverage;
the live macOS lifecycle above used local transport.

Final source checks on the Mac controller passed 105 Rust tests and 248 Python
tests. The Linux worker passed 105 Rust tests and 245 Python tests, with three
explicit skips for ignored pilot artifacts absent from a fresh checkout. The
Mac worker passed its earlier 103-test Rust snapshot and 245 Python tests with
the same three skips; the subsequent collection fix passed the real Mac
ephemeral lifecycle and the final controller suite.

The worker-discovered fixes cover bounded active execution inventories,
separate Herdr sessions on one host, local devenv working directories, stable
ephemeral collection paths, and declared test environment prerequisites.

## Self-install and resource evidence

The Linux worker executed:

```sh
workenv install self \
  --source /home/exedev/workenv/workers/self/worktrees/selfhost-linux-0406 \
  --idempotency-key selfhost-worker-self-install-0412 --format json
```

The first response retained the running build ID. Repeating the command with
the same key returned the completed installation. The build and atomic install
took 45.236 seconds. The subsequent Python suite ran successfully through the
newly installed CLI in the updated shared environment.

| Evidence on workenv-02 | APoC execution |
| --- | --- |
| Rust test run | `01a0845a-83d1-77a3-a5b4-58e31d5c304a` |
| Successful Python rerun | `01a08460-a3dd-7322-9ad8-bb7289aa307b` |
| Build and self-install | `01a0845e-e242-7e31-bfef-8d855151cec2` |

The Rust run retained 545 telemetry samples over 152.6 seconds, with no dropped
samples. APoC reported a peak aggregate CPU value of 425%, aggregate memory of
11,741,331,456 bytes, and 21 processes. These are sampled execution-family
measurements; aggregate process memory can include shared pages. They are not
hard resource limits or measurements of isolated VM capacity.

The self-hosted task's verified collection remains on workenv-02 at:

```text
/home/exedev/workenv/.state/controller/collections/self/selfhost-linux-0406/9a2ed0662e007ec50dc01ba5c1ac20090bf11cf06a37eeb3139ebbf94698b820
```

The Mac controller retained both ephemeral collections and the static Mac
task's fixes under `.state/controller/collections/`. Static worktrees remain
available after release. Ephemeral allocation directories were checked absent
after release; their collections remain outside those allocations.
