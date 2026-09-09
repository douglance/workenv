# Worker profile validation

Validated on September 9, 2026 with two temporary profiles on worker 06.
The fixture used the real Nix environment, direnv, APoC daemon, and Herdr.

## Runtime evidence

| Check | Result |
| --- | --- |
| Two APoC child processes select separate profiles | Passed; different GitHub and Codex config paths |
| Child command retains its original directory | Passed; read the expected marker from that directory |
| Arguments containing spaces survive wrapping | Passed |
| Inherited synthetic GitHub token is removed | Passed |
| Herdr terminal inherits the server's selected profile | Passed without wrapping the terminal probe |
| Temporary Herdr server stops after validation | Passed; session reported `not_running` |

The two APoC probe execution IDs on worker 06 were
`01a083ad-bc8e-7643-8842-9937051e21a8` and
`01a083ad-bc8e-7643-8842-995ce5950287`.
The temporary Herdr execution was
`01a083ad-bcab-77a1-ac2d-d1c2c07c14bf`, using session
`profile-proof-0059`. Controller readback executions were
`01a083ae-8f08-78b0-9ec7-83bedcbea125` for the APoC probes,
`01a083b0-4446-7893-9524-0c6e0a580de0` for the terminal probe, and
`01a083b1-ea39-7491-8772-2675539dc1a4` for stopped-session verification.

## Verification boundaries

Rust tests cover the CLI/MCP interface, profile metadata, scheduler selection,
task binding, wrapped command arguments, and worker lifecycle guards. Python
tests exercise real direnv environment loading and bootstrap profile handling.
The final run passed 69 Rust tests and 161 Python tests. Clippy with warnings
denied and the locked release build passed. Readiness regressions include a
healthy Herdr server whose live execution carries a different profile.

The live fixture did not assign profiles to the fleet, transfer credentials,
or log in to GitHub, Codex, or Claude. Account authentication and a complete
profile assignment on an idle fleet worker remain separate acceptance steps.
Existing worker sessions and fleet assignments were preserved.
The two temporary profile directories and the worker-side fixture were removed
after stopped-session verification.
