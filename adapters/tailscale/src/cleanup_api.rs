use std::time::Duration;

use anyhow::Result;
use reqwest::{Url, blocking::Client, redirect::Policy};

use super::Credentials;

const API_BASE_URL: &str = "https://api.tailscale.com/api/v2";

pub(super) trait ApiClient {
    fn access_token(&self, credentials: &Credentials) -> Result<ApiResponse>;
    fn delete_device(&self, device_id: &str, token: &str) -> Result<ApiResponse>;
    fn get_device(&self, device_id: &str, token: &str) -> Result<ApiResponse>;
}

pub(super) struct TailscaleApi {
    base: Url,
    client: Client,
}

impl TailscaleApi {
    pub(super) fn new() -> Result<Self> {
        Ok(Self {
            base: Url::parse(API_BASE_URL)?,
            client: Client::builder()
                .timeout(Duration::from_mins(1))
                .redirect(Policy::none())
                .build()?,
        })
    }

    pub(super) fn url(&self, segments: &[&str]) -> Result<Url> {
        let mut url = self.base.clone();
        url.path_segments_mut()
            .map_err(|()| anyhow::anyhow!("Tailscale API base URL cannot be a base"))?
            .extend(segments);
        Ok(url)
    }
}

impl ApiClient for TailscaleApi {
    fn access_token(&self, credentials: &Credentials) -> Result<ApiResponse> {
        let response = self
            .client
            .post(self.url(&["oauth", "token"])?)
            .form(&[
                ("grant_type", "client_credentials"),
                ("scope", "devices:core"),
                ("client_id", credentials.client_id.as_str()),
                ("client_secret", credentials.client_secret.as_str()),
            ])
            .send()?;
        api_response(response)
    }

    fn delete_device(&self, device_id: &str, token: &str) -> Result<ApiResponse> {
        let response = self
            .client
            .delete(self.url(&["device", device_id])?)
            .bearer_auth(token)
            .send()?;
        api_response(response)
    }

    fn get_device(&self, device_id: &str, token: &str) -> Result<ApiResponse> {
        let response = self
            .client
            .get(self.url(&["device", device_id])?)
            .bearer_auth(token)
            .send()?;
        api_response(response)
    }
}

fn api_response(response: reqwest::blocking::Response) -> Result<ApiResponse> {
    let status = response.status().as_u16();
    let body = response.text()?;
    Ok(ApiResponse { status, body })
}

pub(super) struct ApiResponse {
    pub(super) status: u16,
    pub(super) body: String,
}
