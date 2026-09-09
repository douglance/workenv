# Workenv 0.2 acceptance record

This record is for maintainers deciding whether to roll out the environment-only
rewrite. Evidence was collected on September 9, 2026 (UTC). Implementation gates
and the existing-Linux canary pass. Full rollout remains blocked by the local Mac
bootstrap and dedicated disposable-provider acceptance scenarios.

## Scope and preservation

The rewrite replaces the old controller with a Rust workspace over upstream
devenv. The CLI and MCP expose 14 setup commands. Task claims, reservations,
task worktrees, execution workflows, collection, and release are removed.
Historical documentation and pilot records are under `archive/v0.1/`.

The existing six provider machines, their active installations, checkouts,
sessions, credentials, and legacy state were not replaced. Linux acceptance used
separate build and canary directories on `workenv-02.exe.xyz`. No global binary
or PATH installation was changed. `fleet.json` remains migration input only.

## Final Linux artifact

| Field | Value |
| --- | --- |
| Package | `/nix/store/qn2ni3sfgbk17gkw883fakqyjkhc0i7l-workenv-0.2.0` |
| CLI SHA-256 | `50ca9f5681f40ee12148929d4aca95dfa25243ec0029c6ea3079315af2b2fb59` |
| Filtered source | `/nix/store/2sm1d92f148yi8m0xf8gn44c0fw63my0-source` |
| Source NAR hash | `sha256-aIzYFU9HLrQn3fyKba7E3H7E0amMeU0vO/5XUggItCU=` |
| Source NAR size | 514960 bytes |
| Nix build | `01a085ae-e767-7641-b99e-d3264803b6d5` |
| Artifact validation | `01a085b2-9ec4-7b93-b90b-999afd715866` |
| Source hash capture | `01a085b3-74c5-7ea2-93d5-56809f1f2552` |

The package contains the CLI and all seven adapter executables. This is an
isolated Nix installation, not a fleet-wide promotion. The source filter excludes
documentation, example modules, runtime state, and generated build outputs.

## Quality gates

The local ARM64 Mac run passed 93 tests. Three additional tests are opt-in and
have separate execution evidence below. Formatting, Clippy with warnings denied,
Rust documentation with warnings denied, and the custom source/dependency policy
all passed.

| Gate | Durable APoC execution |
| --- | --- |
| Full workspace formatting | `01a085b0-8d4e-7f40-8aab-34a5d3420a47` |
| Full workspace Clippy | `01a085b0-8f74-7820-b63a-56e4e16c8fb0` |
| Full workspace tests | `01a085b0-9940-7242-8888-21ca1b73bf35` |
| Full workspace documentation | `01a085b0-b48a-7101-b9f6-7f423e9aacba` |
| Source and dependency policy | `01a085b0-ddae-7b33-b150-b0b9da8ad546` |
| Linux `workenv-check` through devenv | `01a085b9-9d3a-7212-826c-141e390b7fca` |
| Nix formatting | `01a085b9-831e-7033-8c08-0ec836758b35` |
| Standalone native adapter through APoC | `01a0859b-e54d-77d2-837b-5001f9e8986f` |
| Native CLI extension lifecycle fixture | `01a085ac-5552-7b52-a56f-aa8e87d4f0d5` |
| Evaluated migration manifest accepted by core | `01a0858f-53c2-7142-b31a-73bc0874e736` |

The native CLI lifecycle fixture uses a temporary manifest-evaluation fixture.
It is core/APoC evidence. The actual Nix lifecycle below separately verifies the
upstream configuration and packaging behavior.

## Live Linux setup

The final canary configuration is at
`/home/exedev/workenv-rewrite-canary-final-20260909/configuration`, with its
environment at the adjacent `environment` directory.

| Scenario | Observed result | APoC execution |
| --- | --- | --- |
| Fresh final configuration | Controller adapter package automatically realized; bootstrap ready; environment changed | `01a085b7-656e-7c71-8422-8a9b3c6f6f5c` |
| Final status | Applied configuration and package profile ready | `01a085b9-a5fb-7103-8099-e55ab7f129ab` |
| Fresh repeat apply | Every stage ready; same profile and derivation | `01a085b9-a600-70d2-9dea-0c2bb7355b8c` |
| Final artifact in a fresh SSH shell | Exact final binary SHA-256, correct cwd/profile, tools available | `01a085bd-bb4e-7251-9895-f62412cf18b1` |
| Final artifact after reconnect | Same binary hash and preservation file | `01a085bd-c70f-7563-b629-f0c71bb2a370` |
| Unapplied source change | `pending` with `devenv_configuration_changed` | `01a085bf-753e-7a01-bd2f-bf2c2000f612` |
| Source restored | Returned to `ready` | `01a085c0-061a-7b72-a1ed-8610a0f1d60e` |
| Fresh SSH shell in initial canary | Correct cwd/root; Workenv 0.2.0, Git, jq, and ripgrep available | `01a085ae-8bd6-7783-82f1-0566b82fdfbb` |
| Second SSH connection | Static directory and preservation file intact | `01a085b0-07d6-73a0-975c-1e40859b8d20` |
| Actual MCP session | Initialization, progressive discovery of 14 setup commands, successful `environment_list` | `01a085b2-151d-74d0-bbb7-bb2025956781` |
| Native SSH adapter | Exact stdin/stdout through real SSH | `01a08591-741e-7ca3-8c50-f2fa57544b80` |

The controller and target environment in this canary both run on the existing
Linux machine; fresh SSH sessions reach that machine from the Mac. The native SSH
adapter has separate live transport evidence. Nix 2.35.2 and devenv
2.3.0+e0781f7 were already installed, so bootstrap verified their readiness.

Upstream `--from path:SOURCE` preserved distinct cwd, `DEVENV_ROOT`, and
`DEVENV_DOTFILE` for two directories sharing one source. Executions
`01a0859d-a77f-7893-898e-2d715401832d` and
`01a0859d-f823-70e3-97f1-8073b874529a` establish that isolation. The live probe
also caught and removed unsupported `devenv eval --json` arguments.

## Actual Nix extension lifecycle

The standalone example was built outside the Workenv Cargo workspace. Its
configuration imported the public module and pinned devenv inputs. The CLI hash
remained `41f325ef8e23457ce8f9247a9797797092821d99b2307c4f3b9b65edd69d1e72`
through this experiment; this is the earlier canary binary, before the final
readiness and automatic controller-package-realization fixes.

| Step | Observed result | APoC execution |
| --- | --- | --- |
| Import and discover v1 | Version 1.0.0 declared and listed | `01a085b3-3702-7130-8372-5c56f50cc019` |
| Check and invoke v1 | Contract valid; native v1 executable returned ready | `01a085b3-6022-79b3-a81c-b054262585b8`, `01a085b3-85e5-72b3-bc50-082121348e77` |
| Disable | Extension absent from manifest and CLI | `01a085b3-dafb-7311-a0f8-0db54adb861a` |
| Update | Different native Nix store executable returned version 2.0.0 | `01a085b4-2666-7aa3-aaf8-a8c877426624` |
| Roll back | Original v1 executable and manifest restored; CLI and lock hashes unchanged | `01a085b4-9d11-7880-afbf-ca54c05869ac` |

The v1 and v2 packages were
`/nix/store/a034nfhxf3cp47rarrxxzd4746w57y0d-workenv-external-example-1.0.0`
and `/nix/store/7gvd1i12x2v1vi8lccmn6k3q236p6lvq-workenv-external-example-2.0.0`.

## Open acceptance requirements

| Requirement | Blocking evidence | Next action |
| --- | --- | --- |
| Existing local Mac Nix/devenv environment | `/nix` absent; noninteractive sudo requires administrator interaction | Complete the verified Nix installer step, then run the Mac canary |
| Dedicated exe.dev create/configure/connect/destroy | All 16 CPUs and 64 GB memory allocated across six existing VMs | Authorize capacity from an existing VM or provide another available allocation |
| Remaining-machine migration and promotion | Both required canaries have not passed | Keep active installations in place until Mac/provider acceptance passes |
| Four-target release artifacts | Build matrix configured; hosted results recorded separately | Run and verify Linux/macOS x86-64/ARM64 builds |

The bootstrap installer prepared for the Mac is Nix 2.35.2 from the official
release URL. Its verified SHA-256 is
`9adda97297d9e8ab360df95c729eabff4f4f93d6db091953c3a68f29e3fb130c`.
No existing VM was resized or deleted to make room for acceptance, and no plan
upgrade was purchased.
