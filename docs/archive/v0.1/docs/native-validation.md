# Native workenv validation

Verified on September 8, 2026, on the Mac controller and workenv-02.

The installed release is `/Users/operator/.local/bin/workenv`, version 0.1.0,
built with Incurs 0.5.3. Its SHA-256 is
`5b44befb979b5ef78216b748f71bf71bfd4c08a58eeca4ec02a1b65d680637d5`.
The saved configuration points to this checkout. The binary resolves on PATH
and MCP was exercised from `/tmp`.

| Check | Result | APoC execution |
| --- | --- | --- |
| Rust tests, all targets | 40 passed | 01a08327-25ee-7a31-9bd3-c762a3c91c38 |
| Clippy, all targets, warnings denied | Passed | 01a08327-fdea-7560-9389-956f3d95a1ed |
| Locked release build | Passed | 01a08327-26bc-7dc2-805c-17245770b009 |
| Install release outside build cache | Passed | 01a08327-f35e-7e60-b6b9-52c4504eda37 |
| Claim unpublished source bundle on worker 2 | Exact revision verified | 01a08307-055c-7f93-a0e3-c48a1b43bfa9 |
| Run command in claimed worktree | Passed; wrote collection proof | 01a08309-563d-7353-89df-6735d04ab1cb |
| Start devenv service | Process startup verified in remote logs | 01a0831c-a328-7350-a2e0-5216d77be0b9 |
| Task down, collection, and release | Passed; reservation released | 01a08327-2d87-7853-980e-5c15dd3e3062 |
| Installed MCP initialization and worker connection | Passed without optional boolean arguments | 01a08328-413a-7d10-8ae0-bd12aba8837f |
| MCP mutation without request key | Rejected before mutation | 01a08328-413a-7d10-8ae0-bd12aba8837f |
| Worker down with an open Herdr pane | Correctly refused; session preserved | 01a08329-429c-7e93-a106-348e85730235 |

The smoke task was `native-lifecycle-2157`, based on exact fixture revision
`3f68c4a29e5ac006f4b1af02d384072cc63d9924`. Its final collection digest is
`b873ab224d3f2ec0bf6117f03cd8f9b3806ca54d80cebb854d04913a7a033bee`.
The downloaded untracked archive restored `collected-proof.txt` with exact
contents `native workenv collection proof\n`; its SHA-256 is
`ce83de6780f029be16cf928cd48337d9b87a8c82197c2408e8cdfe2ab140094d`.
The changed devenv file and untracked lock/config files were also preserved.
The task record is released and the worker reservation is released. Remote
worktree and local collection remain available under the smoke fixture.

The service had already finished by the successful task down request. Owned
service cancellation is covered by the native regression suite; this live run
establishes service startup and successful collection after it ended. Worker
stop/start completion was not exercised because the existing Herdr pane was
preserved. The shutdown refusal is a verified protection, not a stopped VM.

MCP also verified that an explicit startup root controls where mutation
receipts are saved. The main controller received no receipt from that isolated
MCP test. An omitted key is rejected, and replay tests ensure pending or
interrupted operations are not silently repeated.

Live validation found and corrected Herdr inventory flag handling and omitted
MCP boolean defaults. Regression coverage also protects unrecorded Herdr
writers, unknown directory provenance, corrupt task ownership, expired
reservations, and complete paginated worker runtime inspection.

Tailscale enrollment and Codex/Claude subscription authentication remain
separate rollout gates. Provider SSH remains usable. This native tool
validation does not establish full fleet throughput or application pilot
acceptance; see acceptance.md and the recorded pilot results.
