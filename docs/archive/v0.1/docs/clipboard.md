# Headless clipboard process

`devenv/clipboard.nix` declares the worker-side clipboard processes for `ssh-clipboard` 0.2.11. Root `devenv.nix` imports it. Start `devenv up` through an APoC-owned execution to run the services.

The module runs two devenv processes:

- `clipboard-xvfb`: starts `Xvfb :99 -screen 0 1024x768x24 -nolisten tcp` and is ready when the private X11 socket exists.
- `ssh-clipboard`: waits for `clipboard-xvfb`, sets `DISPLAY=:99`, creates `.state/clipboard/config/config.json` on first run, and starts `ssh-clipboard daemon`.

The generated worker config is intentionally local-only:

```json
{"version":1,"node_id":"<stable generated UUID>","node_name":"<hostname>","peers":[],"max_bytes":268435456,"poll_interval_ms":75,"headless_x11":true}
```

Keep `peers` empty on the worker. The Mac should initiate the peer connection to `exedev@workenv-01.exe.xyz`; ssh-clipboard uses that single SSH stream bidirectionally, so the VM does not need an SSH route back to the Mac.

Auto-update is disabled in the devenv process with `SSH_CLIPBOARD_DISABLE_AUTO_UPDATE=1`. Use `ssh-clipboard update --check` for read-only version checks, then make any rollout/update plan explicitly. Upstream documents that normal daemons can gossip desired versions and converge peers, so do not enable automatic update behavior silently.

Tailscale enrollment is still a separate gate. Until `workenv-01` is enrolled and reachable on the intended tailnet path, acceptance is limited to local headless X11 daemon readiness and isolated worker clipboard smoke tests.

References:

- https://ssh-clipboard.standardagents.ai/
- https://github.com/standardagents/ssh-clipboard/blob/main/docs/headless-linux.md
- https://github.com/standardagents/ssh-clipboard/blob/main/docs/architecture.md
