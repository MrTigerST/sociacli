use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize)]
struct AuthBody<'a> {
    username: &'a str,
    password: &'a str,
}

#[derive(Debug, Deserialize)]
pub struct AuthResp {
    pub id: String,
    pub username: String,
    pub token: String,
}

pub async fn register(server: &str, username: &str, password: &str) -> Result<AuthResp> {
    auth(server, "/register", username, password).await
}

pub async fn login(server: &str, username: &str, password: &str) -> Result<AuthResp> {
    auth(server, "/login", username, password).await
}

async fn auth(server: &str, path: &str, username: &str, password: &str) -> Result<AuthResp> {
    let url = format!("{}{}", server.trim_end_matches('/'), path);
    let client = reqwest::Client::new();
    let resp = client
        .post(&url)
        .json(&AuthBody { username, password })
        .send()
        .await?;
    let status = resp.status();
    let text = resp.text().await?;
    if !status.is_success() {
        return Err(anyhow!("{} -> {}: {}", path, status, text));
    }
    Ok(serde_json::from_str(&text)?)
}
