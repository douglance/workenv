# Set up an environment with Workenv 0.2

Use this procedure to prepare a declared environment and enter its devenv shell.
The controller needs Workenv, APoC, and devenv on PATH. SSH targets also need a
trusted host key and working noninteractive authentication. The bootstrap
integration prepares target prerequisites before target shell setup.

Run the commands from the directory containing your `devenv.nix`, or supply
`--root /path/to/configuration`. In these examples, `dev` is a declared environment
name. Replace it with a name from `environment list`.

## Apply configuration

1. Run `workenv environment list --format json` to confirm the target host,
   directory, source, and integrations.
2. Run `workenv environment plan dev --format json` to inspect the setup stages.
3. If the environment needs a new provider resource, run
   `workenv environment create dev --idempotency-key dev-create-1 --format json`.
   Existing hosts do not need resource creation.
4. Run `workenv environment apply dev --idempotency-key dev-setup-1 --format json`.
   Success is `changed` or `ready`; `pending` does not prove completion.
5. Run `workenv environment status dev --format json` to inspect the applied
   configuration and enabled integration diagnostics.

Apply prepares the target directory, realizes its selected devenv shell and
profiles, and invokes configured setup integrations. The bootstrap stage runs
first when a controller bootstrap integration is declared. Devenv owns packages
and services throughout this process.

A fresh apply with a new key should report `ready` when the setup is unchanged.
Reuse an existing key only to observe or resume that same request. Use a new key
after changing configuration or package pins. Completed receipts replay the
recorded result; they do not reevaluate a new setup intent.

## Enter and leave

Run `workenv environment connect dev` from an interactive terminal. Workenv hands
the terminal to the configured shell or access client. Exiting that client leaves
the static environment and its files in place.

For automation, use `workenv environment connect dev --print true --format json`.
MCP and redirected output also return a connection descriptor. Its `argv` and
`cwd` describe how to enter the environment; they are not proof that entry ran.

Manage declared services with upstream devenv from the environment. Workenv has
no task runner, service supervisor, connection-based cleanup, or inactivity timer.

## Recover an interrupted setup

Inspect the response and its `execution_id`. APoC retains the setup execution:

```sh
apoc execution attach EXECUTION_ID --purpose 'Inspect interrupted setup' --format json
```

Replace `EXECUTION_ID` with the reported ID. If the operation is still running,
repeat the original Workenv command with the same idempotency key. Workenv
observes retained work instead of starting a duplicate provider mutation.

If a receipt records an uncertain operation without a response or execution ID,
inspect the receipt and provider state before issuing another mutation. Receipts
live under `.state/workenv-core/receipts` in the controller configuration root.
Do not delete them to bypass an ownership or uncertainty check.

For a known bootstrap prerequisite failure such as `privilege_required`, satisfy
the reported prerequisite and retry. Initial Nix installation on macOS can need
an administrator interaction. Bootstrap does not request or record a password.

## Bootstrap before devenv is available

The explicit `bootstrap` command accepts `--manifest /path/to/manifest.json`.
This seed is the decoded `workenv.manifestJSON` exported from a prepared devenv
configuration. Its controller adapter executable must refer to a verified native
artifact available on the machine running the command. Normal commands always
evaluate the authoritative devenv configuration.

```sh
workenv --root /path/to/configuration bootstrap dev \
  --manifest /path/to/manifest.json --idempotency-key dev-bootstrap-1 --format json
```

## Migrate an existing installation

Keep the existing `fleet.json`, profile metadata, state, and credentials in place.
`workenv migrate --format json` generates a proposal without changing them. To
write the proposal to a new file, run:

```sh
workenv migrate --output migrated-environments.nix \
  --idempotency-key migrate-environments-1 --format json
```

Review the reported warnings, platform selections, target source paths, and
integration bindings before importing that file from `devenv.nix`. Migration
preserves separate environment directories and sessions and marks existing
provider machines for adoption. Credentials are not copied into Nix or receipts.
Keep the previous binary and configuration available until canary setup succeeds.

## Destroy a disposable provider resource

`environment destroy` requires `ephemeral = true` and a matching creation receipt
that establishes resource ownership. It refuses adopted resources and existing
hosts without an owned disposable provider resource. Disconnecting never destroys
an environment.

```sh
workenv environment destroy dev --idempotency-key dev-destroy-1 --format json
```

Review the named environment before running this command. Provider destruction
removes the backing resource and its files.
