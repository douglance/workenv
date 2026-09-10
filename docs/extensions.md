# Extend Workenv 0.2

This reference is for developers adding a devenv module or native setup adapter.
The public wire protocol is version 1. The source contracts are
[manifest.rs](../crates/workenv-protocol/src/manifest.rs) and
[adapter.rs](../crates/workenv-protocol/src/adapter.rs).

Use a Nix package declaration for ordinary tools. Add a native adapter when an
integration needs an explicit host, account, or provider operation. Nix evaluation
and shell activation must not create machines, delete resources, enroll accounts,
or start interactive login.

## Register an adapter

Import [the base module](../modules/base.nix) and the extension module in your
devenv configuration. The base module enables no integrations automatically.
The standalone [external example](../examples/external-extension/module.nix)
shows a native executable packaged without a Workenv Rust dependency.

Each `workenv.extensions.<id>` declaration supplies these fields:

| Field | Meaning |
| --- | --- |
| `version` | Version of the selected adapter package |
| `protocol_version` | Wire version; currently `1` |
| `executable` | Executable path resolved by Nix |
| `location` | `controller` or `target` |
| `systems` | Supported Nix systems; an empty list accepts every system |
| `operations` | Operation descriptions, schemas, and behavior metadata |

Each operation declares `description`, `mutating`, `internal`, `input_schema`,
and `output_schema`. Optional `location` overrides the extension's execution
location for that operation. For example, Herdr's `register` runs on the
controller while its target configuration operations run on the target.
Input and output schemas describe the operation's `input`
and response `data`, respectively. Internal transport operations are unavailable
through public `extension call`.

A controller adapter packaged in `/nix/store` runs through the controller's
devenv shell, which realizes the package and supplies its runtime tools. Native
paths run directly so a bootstrap seed can work before Nix is installed.
A target adapter runs
inside the target's selected devenv shell, using the executable's file name.
The target configuration therefore must install that adapter package. This keeps
controller-specific Nix store paths out of remote execution.

## Exchange one request and response

The controller writes one JSON request on stdin. The adapter writes exactly one
JSON response on stdout and sends diagnostics to stderr. The Rust `serve` helper
limits requests to 4 MiB and rejects incompatible protocol versions before calling
the operation handler. Top-level protocol objects reject unknown fields.

| Request field | Meaning |
| --- | --- |
| `protocol_version` | Wire version |
| `request_id` | Stable mutation identity or fresh observation identity |
| `extension`, `operation` | Declared adapter and operation names |
| `target` | Environment, host, address, directory, system, source, and profiles |
| `config` | Non-secret settings from the configured binding |
| `input` | Schema-validated operation arguments |
| `previous` | Prior response or resource receipt, or `null` |

The response echoes `protocol_version` and `request_id`, then supplies `status`,
`data`, optional `error`, and optional `execution_id`.

| Status | Contract |
| --- | --- |
| `ready` | Verified successful completion without a change |
| `changed` | Verified successful completion with a change |
| `pending` | Completion is ongoing or uncertain |
| `failed` | Known failure |
| `unsupported` | Operation or platform is unsupported |

For an ongoing operation, return its durable execution ID. Observe that operation
on retry instead of repeating its mutation. A timeout alone does not prove that
an operation failed. The core checks response identity, protocol version, and
the declared output schema before accepting the response.

Adapters are trusted programs running with normal user privileges. Keep secret
values out of manifests, requests, responses, arguments, and receipts. Pass
references to an appropriate credential store instead.

## Clean up a destroyed environment

`environment up` runs provider `create`, environment `apply`, then each declared
integration's `register` operation. `environment down` uses the same destruction
and cleanup path as `environment destroy`. Failed or pending stages prevent a
successful lifecycle result.

During `apply`, declared `prepare` operations run after prerequisite bootstrap
and directory creation, before devenv realization. A target `prepare` runs
outside devenv: local targets use the declared executable directly; remote
targets resolve its basename on the target's PATH. Bootstrap must install that
native adapter first. This lets a project adapter clone the repository before
its devenv configuration exists. Other target operations run inside devenv.

An integration can declare a mutating `cleanup` operation. Workenv invokes it on
the controller after the provider confirms destruction, including when the
previously owned provider resource is already absent. Declare
`location = "controller"` for operations that must run after the target is gone.

Cleanup input contains `provider_create`, `provider_destroy`, and
`integration_receipts`. Each integration receipt includes its `operation` and
`response`; only receipts recorded during that provider lifecycle are included.
Record exact registration IDs in successful setup responses. Do not identify
resources for deletion by hostname alone.

Return `ready` when an exact registration is already absent, `changed` when its
deletion is verified, and `failed` when credentials or other prerequisites need
repair. Return `pending` only with a durable execution ID that can be observed.
Incomplete cleanup prevents a successful destroy result. Independent cleanup
stages still run when another stage returns a failure.

## Change an extension

Declare a local input or pin a remote module/package through `devenv.yaml` and
`devenv.lock`. For the external example, `exampleExtension.package` and
`exampleExtension.version` select its implementation. Setting
`exampleExtension.enable = false` removes its manifest declaration and package.
Restoring the previous configuration and lockfile restores the prior selection.

Use `workenv extension list`, `inspect`, and `check` to verify the evaluated
configuration. Use `extension call` for a declared operation. Workenv does not
maintain another package registry, extension lockfile, or update service.
