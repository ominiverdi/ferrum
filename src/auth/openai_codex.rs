use crate::{config::Config, persistence::ExclusiveFileLock};
use anyhow::{Context, Result};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use futures_util::StreamExt;
use rand::{RngCore, rngs::OsRng};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::PathBuf,
    process::Command,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};
use url::Url;
use uuid::Uuid;

const CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
const AUTHORIZE_URL: &str = "https://auth.openai.com/oauth/authorize";
const TOKEN_URL: &str = "https://auth.openai.com/oauth/token";
const REDIRECT_HOST: &str = "127.0.0.1";
const OAUTH_CALLBACK_PORTS: [u16; 2] = [1455, 1457];
const OAUTH_CALLBACK_TIMEOUT: Duration = Duration::from_secs(5 * 60);
const OAUTH_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const OAUTH_CONNECTION_TIMEOUT: Duration = Duration::from_secs(10);
const OAUTH_SINGLE_CALLBACK_TIMEOUT: Duration = Duration::from_secs(5);
const OAUTH_MAX_CALLBACK_REQUESTS: usize = 16;
const OAUTH_MAX_REQUEST_BYTES: usize = 8 * 1024;
const OAUTH_MAX_RESPONSE_BYTES: usize = 1024 * 1024;
const MAX_AUTH_STORAGE_BYTES: usize = 1024 * 1024;
const REFRESH_EARLY_MS: u128 = 5 * 60 * 1000;
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
    #[serde(default)]
    refresh_token: Option<String>,
    expires_in: u64,
}

pub async fn login(config: &Config) -> Result<()> {
    let pkce = generate_pkce();
    let state = random_hex(16);
    let (listener, redirect_uri) = bind_callback_listener().await?;
    let url = authorization_url(&pkce.challenge, &state, &redirect_uri)?;

    println!("OpenAI login URL:\n{url}\n");
    println!("Complete login in the browser. If it does not open, paste the URL manually.");
    let _ = Command::new("xdg-open").arg(url.as_str()).spawn();

    let code = wait_for_callback(listener, &state).await?;
    let token = exchange_authorization_code(&code, &pkce.verifier, &redirect_uri).await?;
    let account_id = extract_account_id(&token.access_token)?;

    let credential = OpenAiCodexCredential {
        r#type: "oauth".to_string(),
        access: token.access_token,
        refresh: token
            .refresh_token
            .context("OpenAI token response omitted refresh_token")?,
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
    let credential = match load(path.clone())? {
        Some(credential) => credential,
        None => return Ok(None),
    };
    if !credential_needs_refresh(&credential) {
        return Ok(Some(credential.access));
    }

    let _lock = lock_auth_storage_async(path.clone()).await?;
    let credential = match load_unlocked(&path)? {
        Some(credential) => credential,
        None => return Ok(None),
    };
    if !credential_needs_refresh(&credential) {
        return Ok(Some(credential.access));
    }
    let refreshed = refresh(&credential).await?;
    save_unlocked(&path, &refreshed)?;
    Ok(Some(refreshed.access))
}

pub async fn refresh_after_rejection(
    path: PathBuf,
    rejected_access_token: &str,
) -> Result<Option<String>> {
    let _lock = lock_auth_storage_async(path.clone()).await?;
    let credential = match load_unlocked(&path)? {
        Some(credential) => credential,
        None => return Ok(None),
    };
    if credential.access != rejected_access_token {
        return Ok(Some(credential.access));
    }
    let refreshed = refresh(&credential).await?;
    save_unlocked(&path, &refreshed)?;
    Ok(Some(refreshed.access))
}

fn credential_needs_refresh(credential: &OpenAiCodexCredential) -> bool {
    now_ms().saturating_add(REFRESH_EARLY_MS) >= credential.expires
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
    load_unlocked(&path)
}

fn load_unlocked(path: &std::path::Path) -> Result<Option<OpenAiCodexCredential>> {
    if !path.exists() {
        return Ok(None);
    }
    let json = read_auth_storage(path)?;
    let object = json
        .as_object()
        .context("failed to parse auth storage: root must be a JSON object")?;
    object
        .get("openai-codex")
        .cloned()
        .map(serde_json::from_value)
        .transpose()
        .context("failed to parse OpenAI Codex credential")
}

pub fn save(path: PathBuf, credential: &OpenAiCodexCredential) -> Result<()> {
    prepare_auth_parent(&path)?;
    let _lock = lock_auth_storage(&path)?;
    save_unlocked(&path, credential)
}

fn read_auth_storage(path: &std::path::Path) -> Result<serde_json::Value> {
    let file = File::open(path).with_context(|| format!("failed to read {}", path.display()))?;
    let mut bytes = Vec::new();
    file.take((MAX_AUTH_STORAGE_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .with_context(|| format!("failed to read {}", path.display()))?;
    if bytes.len() > MAX_AUTH_STORAGE_BYTES {
        anyhow::bail!(
            "auth storage is too large: > {MAX_AUTH_STORAGE_BYTES} bytes ({})",
            path.display()
        );
    }
    serde_json::from_slice(&bytes).context("failed to parse auth storage")
}

fn prepare_auth_parent(path: &std::path::Path) -> Result<PathBuf> {
    let parent = path
        .parent()
        .map(std::path::Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    fs::create_dir_all(&parent)
        .with_context(|| format!("failed to create {}", parent.display()))?;
    tighten_dir_permissions(&parent);
    Ok(parent)
}

fn auth_lock_path(path: &std::path::Path) -> PathBuf {
    let parent = path.parent().unwrap_or_else(|| std::path::Path::new("."));
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("auth.json");
    parent.join(format!(".{file_name}.lock"))
}

fn lock_auth_storage(path: &std::path::Path) -> Result<ExclusiveFileLock> {
    prepare_auth_parent(path)?;
    ExclusiveFileLock::acquire(&auth_lock_path(path))
}

async fn lock_auth_storage_async(path: PathBuf) -> Result<ExclusiveFileLock> {
    tokio::task::spawn_blocking(move || lock_auth_storage(&path))
        .await
        .context("auth lock task failed")?
}

fn save_unlocked(path: &std::path::Path, credential: &OpenAiCodexCredential) -> Result<()> {
    let parent = prepare_auth_parent(path)?;
    tighten_file_permissions(path);
    let mut json = if path.exists() {
        let value = read_auth_storage(path)?;
        if !value.is_object() {
            anyhow::bail!("failed to parse auth storage: root must be a JSON object");
        }
        value
    } else {
        serde_json::json!({})
    };
    json["openai-codex"] = serde_json::to_value(credential)?;
    let text = serde_json::to_vec_pretty(&json)?;
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("auth.json");

    let (temp_path, mut file) = (0..16)
        .find_map(|_| {
            let candidate = parent.join(format!(".{file_name}.{}.tmp", Uuid::new_v4()));
            match OpenOptions::new()
                .create_new(true)
                .write(true)
                .mode(0o600)
                .open(&candidate)
            {
                Ok(file) => Some(Ok((candidate, file))),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => None,
                Err(error) => Some(Err(error)),
            }
        })
        .transpose()?
        .context("failed to allocate random auth temporary file")?;
    let mut temp_guard = AuthTempFile::new(temp_path.clone());
    file.write_all(&text)
        .with_context(|| format!("failed to write {}", temp_path.display()))?;
    file.sync_all()
        .with_context(|| format!("failed to sync {}", temp_path.display()))?;
    drop(file);
    fs::rename(&temp_path, path)
        .with_context(|| format!("failed to replace {}", path.display()))?;
    temp_guard.disarm();
    tighten_file_permissions(path);
    File::open(&parent)
        .with_context(|| format!("failed to open {}", parent.display()))?
        .sync_all()
        .with_context(|| format!("failed to sync {}", parent.display()))?;
    Ok(())
}

struct AuthTempFile {
    path: Option<PathBuf>,
}

impl AuthTempFile {
    fn new(path: PathBuf) -> Self {
        Self { path: Some(path) }
    }

    fn disarm(&mut self) {
        self.path = None;
    }
}

impl Drop for AuthTempFile {
    fn drop(&mut self) {
        if let Some(path) = self.path.take() {
            let _ = fs::remove_file(path);
        }
    }
}

fn tighten_dir_permissions(path: &std::path::Path) {
    if let Ok(metadata) = fs::metadata(path) {
        let mut permissions = metadata.permissions();
        if permissions.mode() & 0o077 != 0 {
            permissions.set_mode(0o700);
            let _ = fs::set_permissions(path, permissions);
        }
    }
}

fn tighten_file_permissions(path: &std::path::Path) {
    if let Ok(metadata) = fs::metadata(path) {
        let mut permissions = metadata.permissions();
        if permissions.mode() & 0o177 != 0 {
            permissions.set_mode(0o600);
            let _ = fs::set_permissions(path, permissions);
        }
    }
}

fn oauth_client() -> Result<Client> {
    Client::builder()
        .connect_timeout(OAUTH_CONNECTION_TIMEOUT)
        .timeout(OAUTH_REQUEST_TIMEOUT)
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .context("failed to build OAuth HTTP client")
}

async fn refresh(credential: &OpenAiCodexCredential) -> Result<OpenAiCodexCredential> {
    let response = oauth_client()?
        .post(TOKEN_URL)
        .form(&[
            ("grant_type", "refresh_token"),
            ("refresh_token", credential.refresh.as_str()),
            ("client_id", CLIENT_ID),
        ])
        .send()
        .await
        .context("OpenAI Codex token refresh failed")?;
    let status = response.status();
    let text = bounded_response_text(response, "token refresh response").await?;
    if !status.is_success() {
        anyhow::bail!("OpenAI Codex token refresh returned {status}: {text}");
    }
    let token: TokenResponse =
        serde_json::from_str(&text).context("failed to parse token refresh response")?;
    credential_from_token(token, Some(&credential.refresh))
}

fn credential_from_token(
    token: TokenResponse,
    existing_refresh_token: Option<&str>,
) -> Result<OpenAiCodexCredential> {
    Ok(OpenAiCodexCredential {
        r#type: "oauth".to_string(),
        account_id: extract_account_id(&token.access_token)?,
        access: token.access_token,
        refresh: token
            .refresh_token
            .or_else(|| existing_refresh_token.map(str::to_string))
            .context("OpenAI token response omitted refresh_token")?,
        expires: now_ms() + u128::from(token.expires_in) * 1000,
    })
}

async fn exchange_authorization_code(
    code: &str,
    verifier: &str,
    redirect_uri: &str,
) -> Result<TokenResponse> {
    let response = oauth_client()?
        .post(TOKEN_URL)
        .form(&[
            ("grant_type", "authorization_code"),
            ("client_id", CLIENT_ID),
            ("code", code),
            ("code_verifier", verifier),
            ("redirect_uri", redirect_uri),
        ])
        .send()
        .await
        .context("OpenAI Codex token exchange failed")?;
    let status = response.status();
    let text = bounded_response_text(response, "token exchange response").await?;
    if !status.is_success() {
        anyhow::bail!("OpenAI Codex token exchange returned {status}: {text}");
    }
    serde_json::from_str(&text).context("failed to parse token exchange response")
}

async fn bounded_response_text(response: reqwest::Response, label: &str) -> Result<String> {
    let mut stream = response.bytes_stream();
    let mut bytes = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.with_context(|| format!("failed to read {label}"))?;
        if bytes.len().saturating_add(chunk.len()) > OAUTH_MAX_RESPONSE_BYTES {
            anyhow::bail!("{label} exceeded {OAUTH_MAX_RESPONSE_BYTES} bytes");
        }
        bytes.extend_from_slice(&chunk);
    }
    String::from_utf8(bytes).with_context(|| format!("{label} was not valid UTF-8"))
}

async fn bind_callback_listener() -> Result<(TcpListener, String)> {
    let mut errors = Vec::new();
    for port in OAUTH_CALLBACK_PORTS {
        let address = format!("{REDIRECT_HOST}:{port}");
        match TcpListener::bind(&address).await {
            Ok(listener) => {
                return Ok((listener, format!("http://localhost:{port}/auth/callback")));
            }
            Err(error) => errors.push(format!("{address}: {error}")),
        }
    }
    anyhow::bail!(
        "failed to bind an OAuth callback server on registered ports {}: {}",
        OAUTH_CALLBACK_PORTS
            .iter()
            .map(u16::to_string)
            .collect::<Vec<_>>()
            .join(", "),
        errors.join("; ")
    )
}

async fn wait_for_callback(listener: TcpListener, state: &str) -> Result<String> {
    tokio::time::timeout(OAUTH_CALLBACK_TIMEOUT, async {
        for _ in 0..OAUTH_MAX_CALLBACK_REQUESTS {
            let (mut stream, _) = listener.accept().await?;
            let request = match read_callback_request(&mut stream).await {
                Ok(request) => request,
                Err(error) => {
                    write_callback_response(&mut stream, &Err(anyhow::anyhow!(error.to_string())))
                        .await?;
                    continue;
                }
            };
            let result = parse_callback_request(&request, state);
            write_callback_response(&mut stream, &result).await?;
            match result {
                Ok(Some(code)) => return Ok(code),
                Ok(None) => continue,
                Err(error) => return Err(error),
            }
        }
        anyhow::bail!("too many invalid OAuth callback requests")
    })
    .await
    .context("timed out waiting for OAuth callback")?
}

async fn read_callback_request(stream: &mut tokio::net::TcpStream) -> Result<Vec<u8>> {
    tokio::time::timeout(OAUTH_SINGLE_CALLBACK_TIMEOUT, async {
        let mut request = Vec::new();
        let mut chunk = [0u8; 1024];
        loop {
            let n = stream.read(&mut chunk).await?;
            if n == 0 {
                anyhow::bail!("OAuth callback connection closed before request headers completed");
            }
            if request.len().saturating_add(n) > OAUTH_MAX_REQUEST_BYTES {
                anyhow::bail!("OAuth callback request exceeded {OAUTH_MAX_REQUEST_BYTES} bytes");
            }
            request.extend_from_slice(&chunk[..n]);
            if request.windows(4).any(|window| window == b"\r\n\r\n")
                || request.windows(2).any(|window| window == b"\n\n")
            {
                return Ok(request);
            }
        }
    })
    .await
    .context("OAuth callback request timed out")?
}

fn parse_callback_request(request: &[u8], state: &str) -> Result<Option<String>> {
    let request =
        std::str::from_utf8(request).context("invalid OAuth callback request encoding")?;
    let mut fields = request
        .lines()
        .next()
        .context("invalid OAuth callback request")?
        .split_whitespace();
    if fields.next() != Some("GET") {
        return Ok(None);
    }
    let path = fields.next().context("invalid OAuth callback request")?;
    let url = Url::parse(&format!("http://localhost{path}"))?;
    if url.path() != "/auth/callback" {
        return Ok(None);
    }
    let callback_state = url
        .query_pairs()
        .find(|(key, _)| key == "state")
        .map(|(_, value)| value.into_owned());
    if callback_state.as_deref() != Some(state) {
        return Ok(None);
    }
    if let Some(error) = url
        .query_pairs()
        .find(|(key, _)| key == "error")
        .map(|(_, value)| value.into_owned())
    {
        anyhow::bail!("OAuth authorization failed: {error}");
    }
    Ok(Some(
        url.query_pairs()
            .find(|(key, _)| key == "code")
            .map(|(_, value)| value.into_owned())
            .context("missing authorization code")?,
    ))
}

async fn write_callback_response(
    stream: &mut tokio::net::TcpStream,
    result: &Result<Option<String>>,
) -> Result<()> {
    let html = match result {
        Ok(Some(_)) => {
            oauth_success_html("OpenAI authentication completed. You can close this window.")
        }
        Ok(None) => oauth_error_html("Invalid OAuth callback request. Login is still waiting."),
        Err(error) => oauth_error_html(&error.to_string()),
    };
    let response = format!(
        "HTTP/1.1 200 OK\r\ncontent-type: text/html; charset=utf-8\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
        html.len(),
        html
    );
    stream.write_all(response.as_bytes()).await?;
    Ok(())
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

fn authorization_url(challenge: &str, state: &str, redirect_uri: &str) -> Result<Url> {
    let mut url = Url::parse(AUTHORIZE_URL)?;
    url.query_pairs_mut()
        .append_pair("response_type", "code")
        .append_pair("client_id", CLIENT_ID)
        .append_pair("redirect_uri", redirect_uri)
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn save_tightens_existing_auth_file_permissions() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("auth.json");
        fs::write(&path, "{}").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        let credential = test_credential();
        save(path.clone(), &credential).unwrap();
        let mode = fs::metadata(path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }

    #[test]
    fn save_replaces_auth_file_without_leaving_temp_file() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("auth.json");
        let credential = test_credential();
        save(path.clone(), &credential).unwrap();
        let text = fs::read_to_string(&path).unwrap();
        assert!(text.contains("openai-codex"));
        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        let leftover_temp = fs::read_dir(temp.path())
            .unwrap()
            .filter_map(Result::ok)
            .any(|entry| entry.file_name().to_string_lossy().contains(".tmp"));
        assert!(!leftover_temp);
    }

    #[test]
    fn malformed_auth_storage_is_preserved() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("auth.json");
        let original = b"{not-json";
        fs::write(&path, original).unwrap();

        let error = save(path.clone(), &test_credential()).unwrap_err();

        assert!(error.to_string().contains("failed to parse auth storage"));
        assert_eq!(fs::read(path).unwrap(), original);
    }

    #[test]
    fn auth_save_process_child() {
        let Ok(path) = std::env::var("FERRUM_AUTH_STRESS_PATH") else {
            return;
        };
        let writer = std::env::var("FERRUM_AUTH_STRESS_WRITER").unwrap();
        let mut credential = test_credential();
        credential.access = format!("access-{writer}");
        save(PathBuf::from(path), &credential).unwrap();
    }

    #[test]
    fn concurrent_auth_saves_preserve_unrelated_entries() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("auth.json");
        fs::write(&path, r#"{"other-provider":{"token":"keep"}}"#).unwrap();
        let executable = std::env::current_exe().unwrap();
        let mut children = Vec::new();
        for writer in 0..8 {
            children.push(
                std::process::Command::new(&executable)
                    .arg("--exact")
                    .arg("auth::openai_codex::tests::auth_save_process_child")
                    .arg("--nocapture")
                    .env("FERRUM_AUTH_STRESS_PATH", &path)
                    .env("FERRUM_AUTH_STRESS_WRITER", writer.to_string())
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .spawn()
                    .unwrap(),
            );
        }
        for mut child in children {
            assert!(child.wait().unwrap().success());
        }

        let json: serde_json::Value = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
        assert_eq!(json["other-provider"]["token"], "keep");
        assert!(
            json["openai-codex"]["access"]
                .as_str()
                .unwrap()
                .starts_with("access-")
        );
    }

    #[test]
    fn refresh_response_may_omit_rotated_refresh_token() {
        let credential = credential_from_token(
            TokenResponse {
                access_token: test_access_token(),
                refresh_token: None,
                expires_in: 60,
            },
            Some("existing-refresh"),
        )
        .unwrap();

        assert_eq!(credential.refresh, "existing-refresh");
    }

    #[tokio::test]
    async fn rejected_stale_access_token_reuses_concurrently_refreshed_credential() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("auth.json");
        let mut credential = test_credential();
        credential.access = "new-access".to_string();
        save(path.clone(), &credential).unwrap();

        let access = refresh_after_rejection(path, "old-access")
            .await
            .unwrap()
            .unwrap();

        assert_eq!(access, "new-access");
    }

    #[test]
    fn callback_parser_ignores_wrong_route_and_state() {
        assert_eq!(
            parse_callback_request(b"GET /favicon.ico HTTP/1.1\r\n\r\n", "expected").unwrap(),
            None
        );
        assert_eq!(
            parse_callback_request(
                b"GET /auth/callback?state=wrong&code=no HTTP/1.1\r\n\r\n",
                "expected"
            )
            .unwrap(),
            None
        );
    }

    #[tokio::test]
    async fn callback_waits_through_unrelated_and_fragmented_requests() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let waiter = tokio::spawn(async move { wait_for_callback(listener, "expected").await });

        let mut unrelated = tokio::net::TcpStream::connect(address).await.unwrap();
        unrelated
            .write_all(b"GET /favicon.ico HTTP/1.1\r\nHost: localhost\r\n\r\n")
            .await
            .unwrap();
        let mut response = Vec::new();
        unrelated.read_to_end(&mut response).await.unwrap();
        assert!(
            String::from_utf8(response)
                .unwrap()
                .contains("still waiting")
        );

        let mut valid = tokio::net::TcpStream::connect(address).await.unwrap();
        valid
            .write_all(b"GET /auth/callback?state=expected&")
            .await
            .unwrap();
        valid
            .write_all(b"code=accepted HTTP/1.1\r\nHost: localhost\r\n\r\n")
            .await
            .unwrap();
        let mut response = Vec::new();
        valid.read_to_end(&mut response).await.unwrap();
        assert!(String::from_utf8(response).unwrap().contains("completed"));

        assert_eq!(waiter.await.unwrap().unwrap(), "accepted");
    }

    fn test_access_token() -> String {
        let payload = serde_json::json!({
            JWT_CLAIM_PATH: {"chatgpt_account_id": "acct"}
        });
        format!(
            "header.{}.signature",
            URL_SAFE_NO_PAD.encode(serde_json::to_vec(&payload).unwrap())
        )
    }

    fn test_credential() -> OpenAiCodexCredential {
        OpenAiCodexCredential {
            r#type: "oauth".to_string(),
            access: "access".to_string(),
            refresh: "refresh".to_string(),
            expires: 1,
            account_id: "acct".to_string(),
        }
    }
}
