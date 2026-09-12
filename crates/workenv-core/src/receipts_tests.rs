use std::{sync::Mutex, time::Duration};

use anyhow::Context as _;

use tempfile::TempDir;
use workenv_protocol::{PROTOCOL_VERSION, ResponseStatus};

use super::*;

#[test]
fn pending_receipt_is_observed_with_previous_instead_of_replayed() -> Result<()> {
    let temp = TempDir::new()?;
    let store = ReceiptStore::new(temp.path());
    let id = identity();
    let seen_previous = Mutex::new(false);
    let pending = response("request-1", ResponseStatus::Pending);
    store.apply(&id, "request-1", "fingerprint", |_| Ok(pending))?;
    let observed = store.apply(&id, "request-1", "fingerprint", |previous| {
        let previous = previous.context("pending receipt was not passed back")?;
        *seen_previous.lock().map_err(lock_error)? = previous["status"] == "pending";
        Ok(response("request-1", ResponseStatus::Changed))
    })?;
    assert_eq!(observed.status, ResponseStatus::Changed);
    assert!(*seen_previous.lock().map_err(lock_error)?);
    Ok(())
}

#[test]
fn latest_response_for_returns_pending_receipt() -> Result<()> {
    let temp = TempDir::new()?;
    let store = ReceiptStore::new(temp.path());
    let id = identity();
    store.apply(&id, "request-1", "fingerprint", |_| {
        Ok(response("request-1", ResponseStatus::Pending))
    })?;
    assert_eq!(
        store
            .latest_recorded_response_for(&id, "fingerprint")?
            .context("missing latest response")?
            .response
            .status,
        ResponseStatus::Pending
    );
    Ok(())
}

#[test]
fn latest_response_for_returns_matching_fingerprint() -> Result<()> {
    let temp = TempDir::new()?;
    let store = ReceiptStore::new(temp.path());
    let id = identity();
    store.apply(&id, "request-1", "fingerprint", |_| {
        Ok(response("request-1", ResponseStatus::Changed))
    })?;
    let response = store
        .latest_recorded_response_for(&id, "fingerprint")?
        .context("missing latest response")?
        .response;
    assert_eq!(response.request_id, "request-1");
    Ok(())
}

#[test]
fn latest_response_for_rejects_newest_matching_identity_fingerprint_mismatch() -> Result<()> {
    let temp = TempDir::new()?;
    let store = ReceiptStore::new(temp.path());
    let id = identity();
    store.apply(&id, "request-1", "old-fingerprint", |_| {
        Ok(response("request-1", ResponseStatus::Changed))
    })?;
    std::thread::sleep(Duration::from_millis(10));
    store.apply(&id, "request-2", "new-fingerprint", |_| {
        Ok(response("request-2", ResponseStatus::Changed))
    })?;
    let error = store
        .latest_recorded_response_for(&id, "old-fingerprint")
        .err()
        .context("fingerprint mismatch unexpectedly succeeded")?;
    assert!(error.to_string().contains("fingerprint"));
    Ok(())
}

#[test]
fn a_failed_retry_keeps_the_execution_it_already_started() -> Result<()> {
    // A pending create records an execution_id. Retrying it used to overwrite the
    // receipt with a response-less marker *before* calling the adapter, so an
    // adapter error on the retry erased the execution_id and resource_id for good:
    // every later retry bailed "operation outcome is uncertain" and destroy bailed
    // "latest matching receipt has no response", while the resource stayed live.
    let temp = TempDir::new()?;
    let store = ReceiptStore::new(temp.path());
    let id = identity();
    store.apply(&id, "request-1", "fingerprint", |_| {
        Ok(response("request-1", ResponseStatus::Pending))
    })?;
    let failed = store.apply(&id, "request-1", "fingerprint", |_| {
        bail!("adapter exploded")
    });
    assert!(failed.is_err(), "the retry was supposed to fail");

    // The recorded pending response, and its execution id, must have survived.
    let recorded = store
        .latest_recorded_response_for(&id, "fingerprint")?
        .context("the failed retry erased the recorded response")?
        .response;
    assert_eq!(recorded.status, ResponseStatus::Pending);
    assert_eq!(recorded.execution_id.as_deref(), Some("exec-1"));

    // And the key is still usable: the next attempt is handed the previous
    // response so it can observe that execution rather than launching another.
    let seen = Mutex::new(None);
    let resumed = store.apply(&id, "request-1", "fingerprint", |previous| {
        *seen.lock().map_err(lock_error)? = previous;
        Ok(response("request-1", ResponseStatus::Changed))
    })?;
    assert_eq!(resumed.status, ResponseStatus::Changed);
    let previous = seen.lock().map_err(lock_error)?.clone();
    let previous = previous.context("previous response was not passed back")?;
    assert_eq!(previous["execution_id"], json!("exec-1"));
    Ok(())
}

#[test]
fn a_first_attempt_that_crashes_still_refuses_a_blind_retry() -> Result<()> {
    // The other half of the same marker: with no previous response to carry, the
    // record written before the adapter runs is still response-less, so an
    // interrupted first attempt cannot be retried as though nothing was launched.
    let temp = TempDir::new()?;
    let store = ReceiptStore::new(temp.path());
    let id = identity();
    let crashed = store.apply(&id, "request-1", "fingerprint", |_| {
        bail!("killed before any response was recorded")
    });
    assert!(crashed.is_err());
    let error = store
        .apply(&id, "request-1", "fingerprint", |_| {
            Ok(response("request-1", ResponseStatus::Changed))
        })
        .err()
        .context("a blind retry after a crash was allowed")?;
    assert!(
        error.to_string().contains("uncertain"),
        "unexpected error: {error}"
    );
    Ok(())
}

#[test]
fn an_outcomeless_attempt_does_not_hide_an_earlier_answer() -> Result<()> {
    // Receipts are walked newest-first and matched on identity alone, so a failed
    // create under a *new* key shadowed the completed create under the old one --
    // permanently blocking destroy of a resource that really existed.
    let temp = TempDir::new()?;
    let store = ReceiptStore::new(temp.path());
    let id = identity();
    store.apply(&id, "request-1", "fingerprint", |_| {
        Ok(response("request-1", ResponseStatus::Changed))
    })?;
    std::thread::sleep(Duration::from_millis(10));
    let failed = store.apply(&id, "request-2", "fingerprint", |_| {
        bail!("second attempt never produced a response")
    });
    assert!(failed.is_err());

    let recorded = store
        .latest_recorded_response_for(&id, "fingerprint")?
        .context("the outcomeless attempt hid the completed one")?
        .response;
    assert_eq!(recorded.request_id, "request-1");
    assert_eq!(recorded.status, ResponseStatus::Changed);
    Ok(())
}

#[test]
fn two_requests_do_not_share_one_lock() -> Result<()> {
    // The lock used to be a single store-wide `receipts.lock` held across the
    // adapter call, so an `up` of one environment waited on an unrelated `up` for
    // the whole of its provider call.
    let temp = TempDir::new()?;
    let store = ReceiptStore::new(temp.path());
    assert_ne!(store.lock_path("request-1"), store.lock_path("request-2"));
    store.apply(&identity(), "request-1", "fingerprint", |_| {
        Ok(response("request-1", ResponseStatus::Changed))
    })?;
    assert!(store.lock_path("request-1").exists());
    assert!(
        !temp
            .path()
            .join(".state/workenv-core/receipts/receipts.lock")
            .exists(),
        "the store-wide lock is still being taken"
    );
    Ok(())
}

fn identity() -> ReceiptIdentity {
    ReceiptIdentity {
        environment: "dev".to_owned(),
        extension: "provider".to_owned(),
        operation: "create".to_owned(),
    }
}

fn response(request_id: &str, status: ResponseStatus) -> AdapterResponse {
    AdapterResponse {
        protocol_version: PROTOCOL_VERSION,
        request_id: request_id.to_owned(),
        status,
        data: json!({"ok": status == ResponseStatus::Changed}),
        error: None,
        execution_id: (status == ResponseStatus::Pending).then(|| "exec-1".to_owned()),
    }
}

fn lock_error<T>(_: std::sync::PoisonError<T>) -> anyhow::Error {
    anyhow::anyhow!("test lock poisoned")
}
