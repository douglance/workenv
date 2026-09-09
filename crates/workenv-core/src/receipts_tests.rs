use std::{sync::Mutex, time::Duration};

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
            .latest_response_for(&id, "fingerprint")?
            .context("missing latest response")?
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
        .latest_response_for(&id, "fingerprint")?
        .context("missing latest response")?;
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
        .latest_response_for(&id, "old-fingerprint")
        .err()
        .context("fingerprint mismatch unexpectedly succeeded")?;
    assert!(error.to_string().contains("fingerprint"));
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
