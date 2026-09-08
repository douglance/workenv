# First-time authentication

The fleet uses personal subscription authentication. Routine task commands must
report missing or expired authentication instead of switching to API billing.

## Tailscale

Sign in to the existing `example.ts.net` tailnet's admin console. Inspect the
current policy before merging `enrollment/policy_fragment.json`. The fragment
adds the workenv tag and controller access; it cannot restrict worker access
already granted by a broader existing rule. Validate the resulting policy and
its worker isolation tests before enrollment.

In Settings > Trust credentials, create a credential with Auth Keys read/write
permission limited to `tag:workenv`. Store its client ID and secret on
`desk-01` in the ignored file
`.state/tailscale-oauth.local.json`, with owner-only mode `0600`:

```json
{"client_id":"<client ID>","client_secret":"<client secret>"}
```

Do not put actual credentials in command arguments, this document, Git, or task
artifacts. `enrollment/enroll.py` reads the credential file and sends only a
short-lived one-use key to the selected worker over SSH stdin. The OAuth secret
stays on the controller. An uncertain key-creation result requires inspection;
repeating the same request does not mint another key.

Enrollment request:

```json
{"operation":"enroll","request_id":"enroll-workenv-01-v1","worker":"workenv-01"}
```

Run the enrollment module through an APoC-owned execution, passing the base64
encoding of this non-secret request through `--request-base64`, along with
`--fleet fleet.json`, `--state-dir .state/enrollment`, and
`--credentials-file .state/tailscale-oauth.local.json`.

The driver verifies an existing node's name and tailnet before considering it
enrolled. It never forces reauthentication of an existing identity.

References: [OAuth clients](https://tailscale.com/docs/features/oauth-clients),
[auth keys](https://tailscale.com/docs/features/access-control/auth-keys), and
[tailscale up](https://tailscale.com/docs/reference/tailscale-cli/up).

## Coding subscriptions

Each persistent worker needs its own login. Use `codex login --device-auth` for
Codex and the subscription login offered by Claude Code. Complete the provider's
browser step, then verify with `codex login status` and `claude auth status`.
Record status and execution IDs, not tokens.

Keep these login files in the worker's normal home configuration directories,
outside Git and bootstrap artifacts. Do not copy or synchronize rotating auth
caches among workers. If a provider requests interactive reauthentication, leave
the task assigned and report that blocker.

References: [Codex authentication](https://learn.chatgpt.com/docs/auth) and
[Claude Code authentication](https://code.claude.com/docs/en/authentication).

## Nib

The shared devenv wraps Nib to load its credential at runtime from
`~/.config/workenv/nib-token`. The file must belong to the worker user and have
mode `0600`. It is never added to the Nix store. An explicit `NIB_AUTH_TOKEN`
environment variable takes precedence.

`enrollment/nib_auth.py` can transfer the existing Mac Nib session to one declared
worker using SSH stdin. It verifies the source credential, preserves a different
existing worker credential, and writes a receipt containing no token. After
transfer, verify `devenv shell -- nib auth status --format json` on the worker.

Grok and Pi are installed by devenv. Configure their provider authentication
separately; installation does not establish a subscription or authorize API
charges.
