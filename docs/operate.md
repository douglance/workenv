# Set up an environment with Workenv 0.2

Use this procedure to prepare a declared environment and enter its devenv shell.
The controller needs Workenv, APoC, and devenv on PATH. SSH targets also need a
trusted host key and working noninteractive authentication. The bootstrap
integration prepares target prerequisites before target shell setup.

For exe.dev, configure the registered SSH identity for both `exe.dev` and the
declared machine hostname. Nix's OpenSSH can select a local SSH certificate that
the macOS client does not select. If the account uses the plain key and one
client unexpectedly asks you to register, set `IdentityFile`, `IdentitiesOnly
yes`, and `CertificateFile none` for those hosts in your SSH configuration.
Verify the [published exe.dev host fingerprint](https://exe.dev/docs/faq/host-key)
before trusting a new hostname.

Run the commands from the directory containing your `devenv.nix`, or supply
`--root /path/to/configuration`. In these examples, `dev` is a declared environment
name. Replace it with a name from `environment list`.

## Spin a project up and down

Declare the provider, bootstrap, project, and Herdr integrations in your
configuration. The project integration requires `config.repository` and accepts
an optional `config.ref`. Set the environment's `directory` to the editable
checkout and `source` to its devenv configuration. Use the same Herdr binding in
`integrations` and `connection` so Workenv records its registration for cleanup.

1. Run `workenv environment plan dev --format json` to inspect the declared host,
   project directory, and setup stages.
2. Run `workenv environment up dev --idempotency-key dev-up-1 --format json`.
   It provisions the resource, bootstraps prerequisites, prepares the checkout,
   applies devenv, starts Herdr, and registers the machine on the controller.
3. If the result is `pending`, repeat the same command and key until it completes.
   A successful result has `ok = true` and status `ready` or `changed`.
4. Run `workenv environment connect dev` to enter the project through Herdr.
5. When finished, run
   `workenv environment down dev --idempotency-key dev-down-1 --format json`.
   This deletes the owned disposable machine, including its checkout, and removes
   its recorded Herdr registration. Save project changes before running it.

Use new up and down keys for the next machine incarnation. Reusing a completed
key replays its result. Disconnecting from Herdr leaves the machine running.

Bootstrap `seed_tools` can reference a target-local `path`, a download `url`, or
a `controller_path`. Each entry requires a safe binary `name` and its `sha256`.
For SSH targets, Workenv verifies controller files before streaming them to a
private target cache and verifies their bytes again before installation. Seed
APoC and the project adapter before the first clone; target `prepare` runs before
the project's Nix environment exists. Keep credentials out of seed files.

For a private repository without target-side Git credentials, create a Git bundle
from the desired committed branch on the controller. Stage it with bootstrap's
`controller_path` and SHA-256 check, then set the project integration's
`config.clone_from` to the installed bundle path, such as
`/usr/local/bin/project.bundle` for a seed named `project.bundle`. Bootstrap uses
`config.link_dir` for this directory and defaults to `/usr/local/bin`. Keep
`config.repository` set to the permanent repository URL. Workenv uses the bundle only for the initial
clone and retains that URL as `origin`. Uncommitted controller changes are not
included. Future authenticated fetches and pushes require a target login.

## Apply configuration on an existing host

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

Apply prepares the target directory and declared project checkout, realizes its selected devenv shell and
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

The exe.dev module defaults every environment bound to its provider to
`ephemeral = true` and rejects persistent declarations. Create exe.dev machines
when needed and destroy them explicitly when finished.

`environment destroy` requires `ephemeral = true` and a matching creation receipt
that establishes resource ownership. It refuses adopted resources and existing
hosts without an owned disposable provider resource. Disconnecting never destroys
an environment.

```sh
workenv environment destroy dev --idempotency-key dev-destroy-1 --format json
```

Review the named environment before running this command. Provider destruction
removes the backing resource and its files.

After the provider confirms deletion, Workenv runs each declared integration's
`cleanup` operation on the controller. Herdr cleanup uses the saved profile ID,
SSH target, and session. Tailscale cleanup uses the device ID recorded during
setup. Cleanup receipts are scoped to the provider's create-to-destroy interval.
An old receipt cannot select registrations recorded for a replacement machine.

Register a Herdr connection through Workenv so its identity is recorded for
cleanup. The environment must include the Herdr integration binding.

```sh
workenv extension call workenv.herdr register --environment dev \
  --input '{}' --idempotency-key dev-herdr-register-1 --format json
```

For Tailscale, set the integration's `config.api_oauth_file` to a controller-local
JSON file containing `client_id` and `client_secret`, with file mode `0600`.
The OAuth client needs the `devices:core` scope. Keep that file outside the Nix
store and version control. An enrollment-only credential cannot delete devices.

All cleanup integrations are attempted even when another reports a failure.
Destroy reports `failed` or `pending` until every required cleanup completes.
After resolving a known cleanup failure, retry the same destroy key; Workenv
replays the completed provider deletion and retries incomplete cleanup stages.
This command also handles a previously owned VM that is already absent. There
is no background scan for machines deleted outside Workenv.
