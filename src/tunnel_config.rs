use std::str::FromStr;

use anyhow::{Ok, Result};
use base64::{engine::general_purpose::STANDARD as STANDARD_BASE64, Engine};
use reqwest::Client as ReqwestClient;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Clone, Serialize, Deserialize)]
pub struct TunnelConfig {
    pub account_tag: String,
    pub tunnel_secret: Vec<u8>,
    pub tunnel_id: Vec<u8>,
    pub hostname: String,
}

impl TunnelConfig {
    pub async fn try_cloudflare() -> Result<Self> {
        let create_tunnel_response = ReqwestClient::new()
            .post("https://api.trycloudflare.com/tunnel")
            .send()
            .await?
            .json::<CreateTunnelResponse>()
            .await?;
        Ok(Self {
            account_tag: create_tunnel_response.result.account_tag,
            tunnel_secret: STANDARD_BASE64.decode(&create_tunnel_response.result.secret)?,
            tunnel_id: Uuid::from_str(&create_tunnel_response.result.id)?
                .as_bytes()
                .to_vec(),
            hostname: create_tunnel_response.result.hostname,
        })
    }
}

#[derive(Deserialize)]
struct CreateTunnelResponse {
    result: CreateTunnelResult,
}

#[derive(Deserialize)]
struct CreateTunnelResult {
    account_tag: String,
    secret: String,
    id: String,
    hostname: String,
}
