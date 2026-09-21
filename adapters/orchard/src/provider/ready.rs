//! Whether a running guest has finished its first-boot setup.
//!
//! Orchard reports `running` as soon as Tart boots the guest, while the pool's
//! startup script still has minutes of Nix installation ahead of it. Create used
//! to return at that point, so `apply` and the identity seed reached a guest
//! with no devenv on it -- and the pool's own comment claimed the provider
//! waited for the setup to finish, which was true of exe.dev and false here.
//! The setup script's last act is to write the provisioned marker, so a guest
//! is ready when that file exists, the same rule exe.dev follows.
use std::cell::Cell;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use serde_json::json;
use workenv_platform::{ExecutionSpec, Executor};
use workenv_protocol::PROVISIONED_MARKER;

use super::create::{Created, Spec};
use super::execute::{carrier_argv, interpret, script};

/// The whole create, running wait and setup wait together, must finish inside
/// the controller's 300 s direct-invocation timeout with room to answer. A
/// create killed mid-wait leaves a receipt that claims nothing about a guest
/// that does exist, which is worse than an honest `pending`.
pub(super) const CREATE_BUDGET_SECS: u64 = 270;
/// The longest one marker probe may take.
pub(super) const PROBE_TIMEOUT_SECS: u64 = 20;
/// Seconds between marker probes.
const PROBE_POLL_SECS: u64 = 10;

/// Time, injected so tests neither sleep nor read the wall clock.
pub(super) trait Clock {
    /// Wait this many seconds.
    fn sleep(&self, seconds: u64);
    /// Seconds since this clock started.
    fn elapsed(&self) -> u64;
}

/// The real clock, started when a create begins.
pub(super) struct WallClock(Instant);

impl WallClock {
    pub(super) fn start() -> Self {
        Self(Instant::now())
    }
}

impl Clock for WallClock {
    fn sleep(&self, seconds: u64) {
        std::thread::sleep(std::time::Duration::from_secs(seconds));
    }

    fn elapsed(&self) -> u64 {
        self.0.elapsed().as_secs()
    }
}

/// Wait for the setup to finish, unless there is none to wait for.
///
/// A guest with no startup script has nothing to provision, and gating it on a
/// file nobody writes would turn "no setup needed" into the slowest case. A
/// create that was already pending is passed through untouched: the machine is
/// not up, so its setup cannot be either.
pub(super) fn await_provisioned(
    created: Created,
    spec: &Spec,
    clock: &dyn Clock,
    provisioned: &dyn Fn(&str) -> bool,
) -> Created {
    let guest = match &created {
        Created::Pending(..) => return created,
        Created::Existing(guest) | Created::Running(guest) => guest.clone(),
    };
    if spec.body.get("startup_script").is_none() {
        return created;
    }
    loop {
        // Checked before probing, because a probe itself can take its whole
        // timeout, and starting one that cannot finish inside the budget is how
        // the adapter would be killed mid-wait.
        if clock.elapsed() + PROBE_TIMEOUT_SECS > CREATE_BUDGET_SECS {
            let reason = format!(
                "guest {} is running but its setup has not finished after {}s; \
                 {PROVISIONED_MARKER} is absent. Run up again to keep waiting.",
                spec.name,
                clock.elapsed()
            );
            return Created::Pending(guest, reason);
        }
        if provisioned(&spec.name) {
            return created;
        }
        clock.sleep(PROBE_POLL_SECS);
    }
}

/// Asks a guest whether the provisioned marker exists, over `orchard ssh`.
///
/// An unreachable guest reads as not provisioned, deliberately: the probe uses
/// the same path every later step uses, so a guest the probe cannot reach is a
/// guest those steps could not reach either.
pub(super) struct MarkerProbe<'a> {
    executor: &'a dyn Executor,
    attempts: Cell<u32>,
}

impl<'a> MarkerProbe<'a> {
    pub(super) fn new(executor: &'a dyn Executor) -> Self {
        Self {
            executor,
            attempts: Cell::new(0),
        }
    }

    pub(super) fn present(&self, guest: &str) -> bool {
        let argv = ["test", "-e", PROVISIONED_MARKER].map(str::to_owned);
        let carried = carrier_argv(guest, &script(&argv, "/", ""));
        self.executor
            .execute(ExecutionSpec {
                executable: carried[0].clone(),
                arg: carried[1..].to_vec(),
                cwd: None,
                stdin: None,
                timeout_ms: PROBE_TIMEOUT_SECS * 1000,
                idempotency_key: self.key(guest),
                purpose: format!("Check whether Orchard guest {guest} finished its setup."),
            })
            .ok()
            .and_then(|output| interpret(&output.stdout).ok())
            .is_some_and(|result| result["exit_code"] == json!(0))
    }

    /// A key no earlier probe used. `APoC` replays the original receipt for a
    /// repeated key, so a probe keyed only by the request would answer "absent"
    /// forever after the first miss -- including on the second `up`, which is
    /// the one meant to finish the wait.
    fn key(&self, guest: &str) -> String {
        let attempt = self.attempts.get();
        self.attempts.set(attempt + 1);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_nanos());
        format!("workenv-orchard-provisioned:{guest}:{nanos}:{attempt}")
    }
}

#[cfg(test)]
// Test-only, and only these: a readiness test that cannot unwrap its own
// recorded probes says less than one that panics loudly when none ran.
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
#[path = "ready_tests.rs"]
mod tests;
