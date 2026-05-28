use crate::config::Config;
use anyhow::{Context, Result};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use rand::{RngCore, rngs::OsRng};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, OpenOptions},
    io::Write,
    os::unix::fs::OpenOptionsExt,
    path::PathBuf,
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};
use url::Url;

const CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
const AUTHORIZE_URL: &str = "https://auth.openai.com/oauth/authorize";
const TOKEN_URL: &str = "https://auth.openai.com/oauth/token";
const REDIRECT_URI: &str = "http://localhost:1455/auth/callback";
const SCOPE: &str = "openid profile email offline_access";
const JWT_CLAIM_PATH: &str = "https://api.openai.com/auth";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenAiCodexCredential {
    pub r#type: String,
    pub access: String,
    pub refresh: String,
    pub expires: u128,
    #[serde(rename = "accountId")]
    pub account_id: String,
}

#[derive(Debug, Deserialize)]
struct TokenResponse {
    access_token: String,
    refresh_token: String,
    expires_in: u64,
}

pub async fn login(config: &Config) -> Result<()> {
    let pkce = generate_pkce();
    let state = random_hex(16);
    let url = authorization_url(&pkce.challenge, &state)?;

    let listener = TcpListener::bind("127.0.0.1:1455")
        .await
        .context("failed to bind OAuth callback server on 127.0.0.1:1455")?;

    println!("OpenAI login URL:\n{url}\n");
    println!("Complete login in the browser. If it does not open, paste the URL manually.");
    let _ = Command::new("xdg-open").arg(url.as_str()).spawn();

    let code = wait_for_callback(listener, &state).await?;
    let token = exchange_authorization_code(&code, &pkce.verifier).await?;
    let account_id = extract_account_id(&token.access_token)?;

    let credential = OpenAiCodexCredential {
        r#type: "oauth".to_string(),
        access: token.access_token,
        refresh: token.refresh_token,
        expires: now_ms() + u128::from(token.expires_in) * 1000,
        account_id,
    };
    save(config.auth_path(), &credential)?;
    println!(
        "OpenAI Codex authentication saved to {}",
        config.auth_path().display()
    );
    Ok(())
}

pub async fn get_api_key_from_path(path: PathBuf) -> Result<Option<String>> {
    let mut credential = match load(path.clone())? {
        Some(credential) => credential,
        None => return Ok(None),
    };
    if now_ms() >= credential.expires {
        credential = refresh(&credential.refresh).await?;
        save(path, &credential)?;
    }
    Ok(Some(credential.access))
}

pub fn extract_account_id(access_token: &str) -> Result<String> {
    let payload = access_token
        .split('.')
        .nth(1)
        .context("invalid OpenAI Codex token")?;
    let decoded = URL_SAFE_NO_PAD
        .decode(payload)
        .context("failed to decode OpenAI Codex token payload")?;
    let json: serde_json::Value =
        serde_json::from_slice(&decoded).context("failed to parse OpenAI Codex token payload")?;
    json.get(JWT_CLAIM_PATH)
        .and_then(|claim| claim.get("chatgpt_account_id"))
        .and_then(|value| value.as_str())
        .map(str::to_string)
        .context("failed to extract accountId from OpenAI Codex token")
}

pub fn load(path: PathBuf) -> Result<Option<OpenAiCodexCredential>> {
    if !path.exists() {
        return Ok(None);
    }
    let text =
        fs::read_to_string(&path).with_context(|| format!("failed to read {}", path.display()))?;
    let json: serde_json::Value =
        serde_json::from_str(&text).context("failed to parse auth storage")?;
    Ok(json
        .get("openai-codex")
        .cloned()
        .map(serde_json::from_value)
        .transpose()
        .context("failed to parse OpenAI Codex credential")?)
}

fn save(path: PathBuf, credential: &OpenAiCodexCredential) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    let mut json = if path.exists() {
        let text = fs::read_to_string(&path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        serde_json::from_str::<serde_json::Value>(&text).unwrap_or_else(|_| serde_json::json!({}))
    } else {
        serde_json::json!({})
    };
    json["openai-codex"] = serde_json::to_value(credential)?;
    let text = serde_json::to_string_pretty(&json)?;
    let mut file = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .mode(0o600)
        .open(&path)
        .with_context(|| format!("failed to write {}", path.display()))?;
    file.write_all(text.as_bytes())?;
    Ok(())
}

async fn refresh(refresh_token: &str) -> Result<OpenAiCodexCredential> {
    let response = Client::new()
        .post(TOKEN_URL)
        .form(&[
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token),
            ("client_id", CLIENT_ID),
        ])
        .send()
        .await
        .context("OpenAI Codex token refresh failed")?;
    let status = response.status();
    let text = response.text().await?;
    if !status.is_success() {
        anyhow::bail!("OpenAI Codex token refresh returned {status}: {text}");
    }
    let token: TokenResponse =
        serde_json::from_str(&text).context("failed to parse token refresh response")?;
    Ok(OpenAiCodexCredential {
        r#type: "oauth".to_string(),
        account_id: extract_account_id(&token.access_token)?,
        access: token.access_token,
        refresh: token.refresh_token,
        expires: now_ms() + u128::from(token.expires_in) * 1000,
    })
}

async fn exchange_authorization_code(code: &str, verifier: &str) -> Result<TokenResponse> {
    let response = Client::new()
        .post(TOKEN_URL)
        .form(&[
            ("grant_type", "authorization_code"),
            ("client_id", CLIENT_ID),
            ("code", code),
            ("code_verifier", verifier),
            ("redirect_uri", REDIRECT_URI),
        ])
        .send()
        .await
        .context("OpenAI Codex token exchange failed")?;
    let status = response.status();
    let text = response.text().await?;
    if !status.is_success() {
        anyhow::bail!("OpenAI Codex token exchange returned {status}: {text}");
    }
    serde_json::from_str(&text).context("failed to parse token exchange response")
}

async fn wait_for_callback(listener: TcpListener, state: &str) -> Result<String> {
    let (mut stream, _) = listener.accept().await?;
    let mut buffer = vec![0; 8192];
    let n = stream.read(&mut buffer).await?;
    let request = String::from_utf8_lossy(&buffer[..n]);
    let path = request
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .context("invalid OAuth callback request")?;
    let url = Url::parse(&format!("http://localhost{path}"))?;
    let result = if url.path() != "/auth/callback" {
        Err(anyhow::anyhow!("callback route not found"))
    } else if url
        .query_pairs()
        .find(|(key, _)| key == "state")
        .map(|(_, value)| value.into_owned())
        != Some(state.to_string())
    {
        Err(anyhow::anyhow!("state mismatch"))
    } else {
        url.query_pairs()
            .find(|(key, _)| key == "code")
            .map(|(_, value)| value.into_owned())
            .context("missing authorization code")
    };

    let html = match &result {
        Ok(_) => oauth_success_html("OpenAI authentication completed. You can close this window."),
        Err(error) => oauth_error_html(&error.to_string()),
    };
    let response = format!(
        "HTTP/1.1 200 OK\r\ncontent-type: text/html; charset=utf-8\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
        html.len(),
        html
    );
    stream.write_all(response.as_bytes()).await?;
    result
}

struct Pkce {
    verifier: String,
    challenge: String,
}

fn generate_pkce() -> Pkce {
    let verifier = URL_SAFE_NO_PAD.encode(random_bytes(32));
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    Pkce {
        verifier,
        challenge,
    }
}

fn authorization_url(challenge: &str, state: &str) -> Result<Url> {
    let mut url = Url::parse(AUTHORIZE_URL)?;
    url.query_pairs_mut()
        .append_pair("response_type", "code")
        .append_pair("client_id", CLIENT_ID)
        .append_pair("redirect_uri", REDIRECT_URI)
        .append_pair("scope", SCOPE)
        .append_pair("code_challenge", challenge)
        .append_pair("code_challenge_method", "S256")
        .append_pair("state", state)
        .append_pair("id_token_add_organizations", "true")
        .append_pair("codex_cli_simplified_flow", "true")
        .append_pair("originator", "ferrum");
    Ok(url)
}

fn random_hex(bytes: usize) -> String {
    random_bytes(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn random_bytes(len: usize) -> Vec<u8> {
    let mut bytes = vec![0; len];
    OsRng.fill_bytes(&mut bytes);
    bytes
}

fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis()
}

fn escape_html(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

fn oauth_success_html(message: &str) -> String {
    oauth_page(
        "Authentication successful",
        "Authentication successful",
        message,
        None,
    )
}

fn oauth_error_html(message: &str) -> String {
    oauth_page(
        "Authentication failed",
        "Authentication failed",
        message,
        None,
    )
}

fn oauth_page(title: &str, heading: &str, message: &str, details: Option<&str>) -> String {
    let details = details
        .map(|value| format!("<div class=\"details\">{}</div>", escape_html(value)))
        .unwrap_or_default();
    format!(
        r#"<!doctype html>
<html lang="en"><head><meta charset="utf-8"/><meta name="viewport" content="width=device-width, initial-scale=1"/>
<title>{}</title><style>:root{{--text:#fafafa;--text-dim:#a1a1aa;--page-bg:#09090b;--font-sans:ui-sans-serif,system-ui,-apple-system,BlinkMacSystemFont,"Segoe UI",Roboto,"Helvetica Neue",Arial,"Noto Sans",sans-serif;--font-mono:ui-monospace,SFMono-Regular,Menlo,Monaco,Consolas,"Liberation Mono","Courier New",monospace}}*{{box-sizing:border-box}}html{{color-scheme:dark}}body{{margin:0;min-height:100vh;display:flex;align-items:center;justify-content:center;padding:24px;background:var(--page-bg);color:var(--text);font-family:var(--font-sans);text-align:center}}main{{width:100%;max-width:560px;display:flex;flex-direction:column;align-items:center;justify-content:center}}h1{{margin:0 0 10px;font-size:28px;line-height:1.15;font-weight:650;color:var(--text)}}p{{margin:0;line-height:1.7;color:var(--text-dim);font-size:15px}}.details{{margin-top:16px;font-family:var(--font-mono);font-size:13px;color:var(--text-dim);white-space:pre-wrap;word-break:break-word}}</style></head>
<body><main><h1>{}</h1><p>{}</p>{}</main></body></html>"#,
        escape_html(title),
        escape_html(heading),
        escape_html(message),
        details
    )
}
