//! Which controller failures are worth repeating.
use anyhow::{Result, anyhow};
use std::cell::Cell;

use super::client::with_read_retries;

/// The message reqwest produces when a request outruns its budget.
fn timeout() -> anyhow::Error {
    anyhow!("operation timed out").context("requesting http://127.0.0.1:6120/v1/workers")
}

#[test]
fn a_read_that_times_out_then_answers_is_retried_until_it_answers() {
    // Measured on this fleet: the first request to an idle controller exceeds the
    // 15 s budget and the next one answers instantly. With a single attempt, every
    // first observation after an idle period failed.
    let attempts = Cell::new(0);
    let answer: Result<&str> = with_read_retries(|| {
        attempts.set(attempts.get() + 1);
        if attempts.get() < 3 {
            Err(timeout())
        } else {
            Ok("workers")
        }
    });
    assert_eq!(
        answer.expect("the retry never reached an answer"),
        "workers"
    );
    assert_eq!(attempts.get(), 3);
}

#[test]
fn a_read_that_only_ever_times_out_gives_up_rather_than_looping() {
    let attempts = Cell::new(0);
    let answer: Result<&str> = with_read_retries(|| {
        attempts.set(attempts.get() + 1);
        Err(timeout())
    });
    assert!(answer.is_err());
    // One initial attempt plus READ_RETRIES.
    assert_eq!(attempts.get(), 3, "the retry bound is not being enforced");
}

#[test]
fn an_answer_that_is_not_a_timeout_is_never_repeated() {
    // A 401, a 404 or a malformed body is an answer. Repeating it cannot improve it,
    // and retrying would turn one clear failure into three slow ones.
    for message in [
        "401 Unauthorized",
        "did not answer JSON",
        "connection refused",
    ] {
        let attempts = Cell::new(0);
        let answer: Result<&str> = with_read_retries(|| {
            attempts.set(attempts.get() + 1);
            Err(anyhow!("{message}"))
        });
        assert!(answer.is_err());
        assert_eq!(
            attempts.get(),
            1,
            "{message:?} was retried but is an answer"
        );
    }
}

#[test]
fn a_read_that_answers_immediately_costs_one_attempt() {
    let attempts = Cell::new(0);
    let answer: Result<&str> = with_read_retries(|| {
        attempts.set(attempts.get() + 1);
        Ok("workers")
    });
    assert_eq!(answer.expect("a healthy read failed"), "workers");
    assert_eq!(attempts.get(), 1);
}
