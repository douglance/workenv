# Workenv

Workenv prepares development environments on local machines, existing SSH
hosts, and optional infrastructure providers. It is a thin Rust wrapper over
upstream devenv, with the same command graph exposed through the CLI and MCP.

Devenv owns packages, version pins, shells, and services. Workenv coordinates
explicit provisioning, bootstrap, access configuration, and setup diagnostics.

```text
devenv configuration
  +-- packages, pins, profiles, and services
  +-- workenv.hosts and workenv.environments
  `-- workenv.extensions
        `-- versioned JSON -> independent native Rust adapters

workenv CLI / MCP
  `-- generic controller -> APoC setup executions and receipts
```

## Commands

Run from a directory containing `devenv.nix`, or pass `--root PATH`.
The controller requires devenv and APoC on PATH. The explicit bootstrap command
also accepts an exported manifest for a machine that does not yet have devenv.

| Command | Purpose |
| --- | --- |
| `environment list` | List declared environments |
| `environment status NAME` | Inspect readiness |
| `environment plan NAME` | Inspect proposed setup operations |
| `environment up NAME` | Provision, prepare the project, apply devenv, and register access |
| `environment down NAME` | Destroy an owned ephemeral resource and remove access registrations |
| `environment create NAME` | Provision a declared resource |
| `environment apply NAME` | Apply devenv and enabled setup integrations |
| `environment connect NAME` | Enter the configured shell or access client |
| `environment destroy NAME` | Destroy an explicitly owned disposable resource |
| `extension list/inspect/check/call` | Discover and invoke declared adapters |
| `bootstrap NAME` | Prepare prerequisites through the configured adapter |
| `doctor` | Inspect controller configuration and prerequisites |
| `migrate` | Generate reviewable Nix from legacy configuration |

Every mutation requires an explicit idempotency key. Use the same key when
observing an interrupted request. A pending result does not establish failure
or authorize a second provider resource.

```sh
workenv environment list --format json
workenv environment plan dev --format json
workenv environment up dev --idempotency-key dev-up-1 --format json
workenv environment connect dev
workenv environment down dev --idempotency-key dev-down-1 --format json
workenv extension inspect example.independent --format json
workenv extension call example.independent inspect --environment dev --input '{}'
workenv --root /path/to/config --mcp
```

Interactive `connect` hands the terminal to the configured client. MCP,
redirected output, explicit output formatting, and `--print true` return a
connection descriptor.

MCP uses progressive discovery: `search_tools` lists command capabilities,
`get_tool_details` returns the selected schema, and `call_read_tool` or
`call_write_tool` invokes it. See [the setup procedure](docs/operate.md).

## Configuration

Import [modules/base.nix](modules/base.nix) into a devenv configuration to
declare hosts, environments, and extension contracts. Import other modules
only when their integration is needed. The base module enables no access or
provider integration implicitly. [presets/](presets/) contains optional
compositions.

Devenv evaluates `workenv.manifest` for the controller. Handwritten Nix remains
authoritative; Workenv does not edit it during setup. Pin local or remote module
inputs using `devenv.yaml` and `devenv.lock`. Package overrides use Nix packages
or release URLs with explicit hashes.

An environment names its host, target directory, configuration source, devenv
profiles, integrations, and optional connection adapter. Static environments
persist across connections. `ephemeral = true` permits explicit destruction
only when a provider receipt proves that Workenv owns the resource.

The optional project adapter prepares an editable Git checkout before devenv
starts. Herdr's apply operation starts a compatible session whose panes enter
the project's devenv shell; `up` then registers that session on the controller.
Include the same Herdr binding in `integrations` and `connection` so `down` can
remove its exact registration. See [the setup procedure](docs/operate.md).

## Extensions

The core knows the versioned manifest and JSON protocol. Provider names and
integration implementation details belong in adapters. Normal tool installation
requires only a Nix package declaration.

An adapter receives one JSON request on stdin and writes one JSON response to
stdout. Metadata declares its executable, platform support, operations, schemas,
execution location, and mutation behavior. Diagnostic output belongs on stderr.

[examples/external-extension](examples/external-extension/) is a standalone
Cargo workspace with no Workenv dependency. Its Nix module demonstrates how to
register an independently packaged executable through the public contract.
See [the extension reference](docs/extensions.md) for fields, execution location,
response statuses, and update behavior.

## Migration and preservation

Version 0.2 removes the worker/task command surface. Task claims, reservations,
task worktrees, command workflows, collection, and release are no longer part
of Workenv. Existing credentials, checkouts, sessions, collections, and legacy
state remain in place. Migration generates a proposal and adopts existing
provider resources; it does not replace them.
The retained `fleet.json` is migration input only; commands read the devenv
manifest. Historical operating instructions are under `docs/archive/v0.1/`.

Keep credentials outside Git, the Nix store, command arguments, protocol
messages, and receipts. Identity integrations use scoped configuration and
explicit login. Personal environments remain separate from work credentials,
customer data, and production access.

## Development

The virtual Cargo workspace pins Rust 1.97.1 and shares dependencies and lint
policy. Build the CLI and adapters with `cargo build --workspace --locked`.
The devenv Rust module packages the same workspace for self-hosting.

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-targets --all-features
RUSTDOCFLAGS='-D warnings' cargo doc --workspace --all-features --no-deps
cargo run -p xtask -- check
```

Source, Nix, installed-artifact, and physical-machine evidence are distinct.
[The acceptance record](docs/rewrite-validation.md) identifies artifact hashes,
durable execution IDs, verified behavior, and remaining rollout requirements.
