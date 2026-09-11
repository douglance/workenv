use std::sync::Mutex;

use anyhow::Result;
use serde_json::json;
use workenv_protocol::{AdapterRequest, PROTOCOL_VERSION, ResponseStatus, Target};

use super::{
    Credentials,
    api::{ApiClient, ApiResponse, TailscaleApi},
    previous_device_id, run_with_api,
};

#[test]
fn cleanup_deletes_exact_previous_device_id_with_controller_oauth() -> Result<()> {
    let api = FakeApi::new(vec![token(), empty(204)]);
    let result = run_with_api(&request(), &api, &credentials(), "node-1")?;
    assert_eq!(result.status, ResponseStatus::Changed);
    assert_eq!(result.data["device_id"], "node-1");
    let calls = api
        .calls
        .lock()
        .map_err(|_| anyhow::anyhow!("mutex poisoned"))?;
    assert_eq!(calls.as_slice(), ["token", "delete:node-1:access-token"]);
    Ok(())
}

#[test]
fn cleanup_surfaces_api_scope_failure() -> Result<()> {
    let api = FakeApi::new(vec![token(), empty(403)]);
    let result = run_with_api(&request(), &api, &credentials(), "node-1")?;
    assert_eq!(result.status, ResponseStatus::Failed);
    assert_eq!(result.data["status"], "tailscale_cleanup_failed");
    assert_eq!(result.data["http_status"], 403);
    Ok(())
}

#[test]
fn accepted_delete_fails_unverified_when_get_still_finds_device() -> Result<()> {
    let api = FakeApi::new(vec![token(), empty(202), empty(200)]);
    let result = run_with_api(&request(), &api, &credentials(), "node-1")?;
    assert_eq!(result.status, ResponseStatus::Failed);
    assert_eq!(result.data["status"], "tailscale_cleanup_unverified");
    assert_eq!(result.data["delete_http_status"], 202);
    assert_eq!(result.data["get_http_status"], 200);
    Ok(())
}

#[test]
fn accepted_delete_completes_when_get_proves_absence() -> Result<()> {
    let api = FakeApi::new(vec![token(), empty(202), empty(404)]);
    let result = run_with_api(&request(), &api, &credentials(), "node-1")?;
    assert_eq!(result.status, ResponseStatus::Changed);
    assert_eq!(result.data["status"], "tailscale_cleaned");
    Ok(())
}

#[test]
fn previous_device_id_reads_core_integration_receipt_shape() {
    let mut request = request();
    request.input = json!({"integration_receipts":[
        {"extension":"workenv.tailscale","response":{"data":{"device_id":"node-1"}}}
    ]});
    assert_eq!(previous_device_id(&request).as_deref(), Some("node-1"));
}

#[test]
fn cleanup_refuses_without_previous_device_id() -> Result<()> {
    let result = super::run(&request(), &NoopExecutor)?;
    assert_eq!(result.status, ResponseStatus::Failed);
    assert_eq!(result.data["status"], "tailscale_cleanup_blocked");
    Ok(())
}

#[test]
fn device_id_is_encoded_as_one_url_path_segment() -> Result<()> {
    let api = TailscaleApi::new()?;
    let url = api.url(&["device", "node/one?x#y"])?;
    assert_eq!(
        url.as_str(),
        "https://api.tailscale.com/api/v2/device/node%2Fone%3Fx%23y"
    );
    Ok(())
}

fn request() -> AdapterRequest {
    AdapterRequest {
        protocol_version: PROTOCOL_VERSION,
        request_id: "request-1".to_string(),
        extension: "tailscale".to_string(),
        operation: "cleanup".to_string(),
        target: Target {
            environment: "dev".to_string(),
            host: "workenv-01".to_string(),
            address: Some("exedev@workenv-01.exe.xyz".to_string()),
            directory: ".".into(),
            system: "x86_64-linux".to_string(),
            source: ".".to_string(),
            profiles: Vec::new(),
        },
        config: json!({}),
        input: json!({}),
        previous: None,
    }
}

fn credentials() -> Credentials {
    Credentials {
        client_id: "client-id".to_string(),
        client_secret: "client-secret".to_string(),
    }
}

fn token() -> ApiResponse {
    ApiResponse {
        status: 200,
        body: "{\"access_token\":\"access-token\"}".to_string(),
    }
}

fn empty(status: u16) -> ApiResponse {
    ApiResponse {
        status,
        body: String::new(),
    }
}

struct FakeApi {
    calls: Mutex<Vec<String>>,
    responses: Mutex<Vec<ApiResponse>>,
}

impl FakeApi {
    fn new(responses: Vec<ApiResponse>) -> Self {
        Self {
            calls: Mutex::new(Vec::new()),
            responses: Mutex::new(responses),
        }
    }

    fn next(&self) -> Result<ApiResponse> {
        Ok(self
            .responses
            .lock()
            .map_err(|_| anyhow::anyhow!("mutex poisoned"))?
            .remove(0))
    }
}

impl ApiClient for FakeApi {
    fn access_token(&self, credentials: &Credentials) -> Result<ApiResponse> {
        assert_eq!(credentials.client_id, "client-id");
        assert_eq!(credentials.client_secret, "client-secret");
        self.calls
            .lock()
            .map_err(|_| anyhow::anyhow!("mutex poisoned"))?
            .push("token".to_string());
        self.next()
    }

    fn delete_device(&self, device_id: &str, token: &str) -> Result<ApiResponse> {
        self.calls
            .lock()
            .map_err(|_| anyhow::anyhow!("mutex poisoned"))?
            .push(format!("delete:{device_id}:{token}"));
        self.next()
    }

    fn get_device(&self, device_id: &str, token: &str) -> Result<ApiResponse> {
        self.calls
            .lock()
            .map_err(|_| anyhow::anyhow!("mutex poisoned"))?
            .push(format!("get:{device_id}:{token}"));
        self.next()
    }
}

struct NoopExecutor;

impl workenv_platform::Executor for NoopExecutor {
    fn execute(
        &self,
        _spec: workenv_platform::ExecutionSpec,
    ) -> Result<workenv_platform::ExecutionOutput> {
        unreachable!("cleanup must reject before executing")
    }
}
