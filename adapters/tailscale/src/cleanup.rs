use std::{fs, path::PathBuf};

use anyhow::{Context, Result, bail};
use serde_json::{Map, Value, json};
use workenv_platform::Executor;
use workenv_protocol::{AdapterRequest, AdapterResponse, ResponseStatus};

use crate::util::{optional_string, response};

#[path = "cleanup_api.rs"]
mod api;

use api::{ApiClient, TailscaleApi};

pub(crate) fn run(request: &AdapterRequest, _runner: &impl Executor) -> Result<AdapterResponse> {
    let Some(device_id) = previous_device_id(request) else {
        return Ok(failed(
            request,
            "tailscale_cleanup_blocked",
            json!({"error":"cleanup requires a previous Tailscale device ID"}),
        ));
    };
    let credentials = match Credentials::from_request(request) {
        Ok(credentials) => credentials,
        Err(error) => {
            return Ok(failed(
                request,
                "tailscale_cleanup_blocked",
                json!({"error":error.to_string()}),
            ));
        }
    };
    let api = TailscaleApi::new()?;
    run_with_api(request, &api, &credentials, &device_id)
}

fn run_with_api(
    request: &AdapterRequest,
    api: &impl ApiClient,
    credentials: &Credentials,
    device_id: &str,
) -> Result<AdapterResponse> {
    let token = match access_token(request, api, credentials)? {
        Token::Ready(token) => token,
        Token::Failed(response) => return Ok(response),
    };
    delete_device(request, api, device_id, &token)
}

fn access_token(
    request: &AdapterRequest,
    api: &impl ApiClient,
    credentials: &Credentials,
) -> Result<Token> {
    let response = api.access_token(credentials)?;
    if response.status != 200 {
        return Ok(Token::Failed(failed(
            request,
            "tailscale_cleanup_token_failed",
            json!({"http_status":response.status}),
        )));
    }
    let token = serde_json::from_str::<Value>(&response.body)
        .ok()
        .and_then(|value| string_field(&value, "access_token"))
        .context("Tailscale token response did not include access_token")?;
    Ok(Token::Ready(token))
}

fn delete_device(
    request: &AdapterRequest,
    api: &impl ApiClient,
    device_id: &str,
    token: &str,
) -> Result<AdapterResponse> {
    match api.delete_device(device_id, token)?.status {
        200 | 204 => Ok(cleaned(request, device_id, ResponseStatus::Changed)),
        404 => Ok(cleaned(request, device_id, ResponseStatus::Ready)),
        202 => verify_accepted_delete(request, api, device_id, token),
        code => Ok(failed(
            request,
            "tailscale_cleanup_failed",
            json!({"device_id":device_id,"http_status":code}),
        )),
    }
}

fn verify_accepted_delete(
    request: &AdapterRequest,
    api: &impl ApiClient,
    device_id: &str,
    token: &str,
) -> Result<AdapterResponse> {
    match api.get_device(device_id, token)?.status {
        404 => Ok(cleaned(request, device_id, ResponseStatus::Changed)),
        200 | 202 => Ok(failed(
            request,
            "tailscale_cleanup_unverified",
            json!({"device_id":device_id,"delete_http_status":202,"get_http_status":200}),
        )),
        code => Ok(failed(
            request,
            "tailscale_cleanup_failed",
            json!({"device_id":device_id,"http_status":code}),
        )),
    }
}

fn cleaned(request: &AdapterRequest, device_id: &str, status: ResponseStatus) -> AdapterResponse {
    let mut data = Map::new();
    data.insert("status".to_string(), json!("tailscale_cleaned"));
    data.insert("device_id".to_string(), json!(device_id));
    response(request, status, data)
}

fn failed(request: &AdapterRequest, status: &str, extra: Value) -> AdapterResponse {
    let mut data = Map::new();
    data.insert("status".to_string(), json!(status));
    if let Value::Object(map) = extra {
        data.extend(map);
    }
    response(request, ResponseStatus::Failed, data)
}

fn previous_device_id(request: &AdapterRequest) -> Option<String> {
    previous_values(request).into_iter().find_map(|value| {
        value
            .get("device_id")
            .or_else(|| value.get("tailscale_device_id"))
            .and_then(Value::as_str)
            .or_else(|| value.pointer("/tailscale/Self/ID").and_then(Value::as_str))
            .map(ToOwned::to_owned)
    })
}

fn previous_values(request: &AdapterRequest) -> Vec<&Value> {
    let mut values: Vec<&Value> = [
        request.previous.as_ref(),
        request
            .previous
            .as_ref()
            .and_then(|value| value.get("data")),
        request.input.get("apply"),
        request.input.get("receipt"),
    ]
    .into_iter()
    .flatten()
    .collect();
    if let Some(receipts) = request
        .input
        .get("integration_receipts")
        .and_then(Value::as_array)
    {
        values.extend(
            receipts
                .iter()
                .filter_map(|receipt| receipt.pointer("/response/data")),
        );
    }
    values
}

struct Credentials {
    client_id: String,
    client_secret: String,
}

impl Credentials {
    fn from_request(request: &AdapterRequest) -> Result<Self> {
        if let Some(path) = optional_string(&request.config, "api_oauth_file") {
            return Self::from_file(PathBuf::from(path));
        }
        Ok(Self {
            client_id: credential(request, "api_oauth_client_id")?,
            client_secret: credential(request, "api_oauth_client_secret")?,
        })
    }

    fn from_file(path: PathBuf) -> Result<Self> {
        let value: Value =
            serde_json::from_str(&read_owner_file(path)?).context("credential file is not JSON")?;
        Ok(Self {
            client_id: validate_credential(
                string_field(&value, "client_id").context("credential file has no client_id")?,
            )?,
            client_secret: validate_credential(
                string_field(&value, "client_secret")
                    .context("credential file has no client_secret")?,
            )?,
        })
    }
}

fn credential(request: &AdapterRequest, key: &str) -> Result<String> {
    if let Some(name) = optional_string(&request.config, &format!("{key}_env")) {
        return validate_credential(
            std::env::var(&name).with_context(|| format!("{name} is not set"))?,
        );
    }
    if let Some(path) = optional_string(&request.config, &format!("{key}_file")) {
        return read_secret_file(PathBuf::from(path));
    }
    bail!("{key}_env or {key}_file is required");
}

fn read_secret_file(path: PathBuf) -> Result<String> {
    validate_credential(read_owner_file(path)?.trim().to_string())
}

fn read_owner_file(path: PathBuf) -> Result<String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(&path)?.permissions().mode() & 0o777;
        if mode != 0o600 {
            bail!("credential file must be mode 0600");
        }
    }
    Ok(fs::read_to_string(path)?)
}

fn validate_credential(value: String) -> Result<String> {
    if value.is_empty() || value.chars().any(char::is_whitespace) {
        bail!("credential has invalid format");
    }
    Ok(value)
}

fn string_field(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
}

enum Token {
    Ready(String),
    Failed(AdapterResponse),
}

#[cfg(test)]
#[path = "cleanup_tests.rs"]
mod cleanup_tests;
