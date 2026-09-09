# Use separate worker profiles

A profile selects account configuration for a worker. It follows the same
directory-based pattern as the Mac's `lv`, `work`, and `oc` direnv setup:
separate tool config paths, explicit Git identity, and removal of inherited
credentials that could select another account.

Profiles contain identity metadata. Login credentials stay on each worker,
outside the repository. Assigning the same profile name to two workers does
not copy their credentials; each worker needs its own login.

## Create, assign, and log in

1. Create a profile on the controller. Replace these example identity values:

   ```sh
   workenv profile create personal \
     --github-login YOUR_GITHUB_USERNAME \
     --git-name "Your Name" \
     --git-email "YOUR_VERIFIED_GITHUB_EMAIL"
   ```

   This writes `profiles/personal.json`. It does not assign workers or initiate
   login. Omit `--github-login` if you only want scoped config directories;
   Workenv then does not enforce an expected GitHub username.

2. Finish and release tasks on the target worker, close its Herdr terminals,
   and assign the profile:

   ```sh
   workenv profile assign 2 personal
   ```

   Active tasks, panes, agents, builds, or uncertain runtime state block the
   change. Assignment prepares profile directories and restarts only the empty
   owned Herdr runtime. The worker remains in the main Herdr sidebar.

3. Start login in a profile-scoped Herdr workspace:

   ```sh
   workenv profile login 2 github
   workenv profile login 2 codex
   workenv profile login 2 claude
   ```

   Select the login workspace under worker 02 in Herdr and complete the
   provider's browser step. Run only the login commands for tools you use.
   The generated Git config already uses `gh auth git-credential` for HTTPS
   GitHub remotes. No copied token or controller credential is required.

4. Check the assignment and GitHub identity:

   ```sh
   workenv profile list
   workenv profile status 2
   ```

   Status distinguishes prepared directories from a matching GitHub login.
   It does not print credential contents or claim Codex/Claude login succeeded.

5. Request the profile when claiming work:

   ```sh
   workenv claim PROJECT TASK --profile personal
   ```

   Add `--worker 2` to require that worker as well. The scheduler selects a
   worker with the requested profile and class. GitHub HEAD lookup and fetch
   use that worker's account. The task records the profile name and definition
   digest; later operations refuse a changed profile.

Noninteractive CLI and MCP mutations require `--idempotency-key`. Reuse a key
to retrieve its saved result; use a new key for a new operation.

## Configuration and paths

Profile definitions are small, nonsecret JSON files:

```json
{
  "schema_version": 1,
  "name": "personal",
  "github_login": "example-user",
  "git_name": "Example User",
  "git_email": "example@example.invalid"
}
```

`anthropic_profile` is an optional named profile for the Anthropic CLI. It is
separate from Claude Code's scoped configuration directory. Unknown fields,
credential fields, unsafe names, and control characters are rejected.

`workers[].profile` in `fleet.json` records the assignment. A project can set
`worker_profile` to require an identity profile automatically. Existing project
`profile: "rust"` and `profile: "web"` values describe tool intent and do not
select a credential identity.

Each worker stores a profile under
`~/.config/workenv/profiles/<name>/`:

| Setting | Profile-relative path |
| --- | --- |
| `GH_CONFIG_DIR` | `.gh/` |
| `GIT_CONFIG_GLOBAL` | `.gitconfig` |
| `CODEX_HOME` | `.codex/` |
| `CLAUDE_CONFIG_DIR` | `.claude/` |
| `KUBECONFIG` | `.kube/config` |
| Wrangler's `XDG_CONFIG_HOME` | `.config/` |
| Wrangler command wrapper | `.bin/wrangler` |

The profile directory is private to the worker user. Its generated `.envrc`
is loaded with `direnv exec` inside the actual command process, including
APoC task children and the Herdr server. Commands keep their original working
directory. The global `HOME` and `XDG_CONFIG_HOME` stay unchanged so Herdr
and APoC retain their normal runtime state.

Inherited GitHub, OpenAI, Anthropic, and Cloudflare token overrides are removed.
Git author/committer environment overrides are removed as well. The Wrangler
wrapper supports an optional worker-local `.cloudflare.env` inside the profile
directory, matching the existing Mac pattern. Edit that file on the worker;
keep actual credentials out of CLI arguments and tracked files.

## Changing a profile

Create a new name when changing an existing profile definition. Workenv does
not overwrite a different definition or silently move existing logins. Assign
the new name once the worker is idle, then log in within the new profile.

Generated runtime files are validated before use. Missing or modified profile
files cause an error instead of falling back to a global account. Login stores
and the Kubernetes config remain editable. Profile setup does not copy the
Mac's `lv`, `work`, or `oc` credential stores; those directories are prior art
for the mechanism. The fleet's existing personal-project scope still applies.

Profiles select tool configuration. Processes running as the same Linux user
can still access that user's other profile directories. Separate workers
provide the VM boundary.

See [profile validation](profile-validation.md) for runtime evidence and the
remaining authentication and fleet-assignment acceptance steps.
