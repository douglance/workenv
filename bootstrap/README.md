# Bootstrap

`bootstrap/bootstrap.sh` prepares an Ubuntu 24.04 x86_64 worker host for the shared repository `devenv`.

Run it with sudo on the worker:

```bash
sudo bootstrap/bootstrap.sh --json --prefix /opt/workenv
```

The bootstrap script installs only host prerequisites, pinned Nix, pinned `devenv`, the `workenv-herdr-bootstrap.service` unit, and enables an existing `tailscaled.service` when systemd already knows about it. It does not install Herdr, APoC, DevSQL, Codex, Claude, Grok, Pi, Nib, or ssh-clipboard directly; those shared commands come from the root `devenv` configuration.

Use health mode to inspect only bootstrap prerequisites:

```bash
bootstrap/bootstrap.sh --health-only --json --prefix /opt/workenv
```

Shared tool health is checked separately through `remote/tool_health.py` from inside the shared `devenv` environment.

The boot service installs `/opt/workenv/bin/workenv-herdr-bootstrap` and enables `workenv-herdr-bootstrap.service`. The helper is responsible for starting the Herdr server through APoC and the shared `devenv` tools at boot. Bootstrap does not start Herdr during install, does not enroll Tailscale, does not write credential values, and does not sync home directories.

Build the audited APoC source archive only when refreshing the artifact that the shared `devenv` consumes:

```bash
bootstrap/build-apoc.sh \
  --json \
  --source-archive /path/to/incoming/apoc-913b1e9.tar.gz \
  --source-sha256 c91295cd18822bc4755f138ff5ffec2cc47bf81cc65e802e46f28b2cd86775e3 \
  --prefix /opt/workenv \
  --jobs 2
```

The build script writes the fleet archive and `.sha256` sidecar under `/opt/workenv/artifacts/apoc`.
