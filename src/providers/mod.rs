mod fake;
mod openai;

use crate::{
    agent::{messages::Message, tools::ToolDefinition},
    auth::openai_codex,
    config::{ProviderConfig, ThinkingLevel},
};
use anyhow::{Context, Result};
use reqwest::Client;
use serde::Deserialize;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, atomic::AtomicBool};
use std::time::Duration;

const DEFAULT_CODEX_CLIENT_VERSION: &str = "0.144.0";
const LATEST_CODEX_RELEASE_URL: &str = "https://api.github.com/repos/openai/codex/releases/latest";

#[derive(Debug, Clone)]
pub enum StreamEvent {
    ThinkingDelta(String),
    TextDelta(String),
}

pub trait Provider: Send + Sync {
    fn complete<'a>(
        &'a self,
        model: &'a str,
        messages: &'a [Message],
        tools: &'a [ToolDefinition],
        thinking: ThinkingLevel,
    ) -> Pin<Box<dyn Future<Output = Result<ProviderResponse>> + Send + 'a>>;

    fn complete_streaming<'a>(
        &'a self,
        model: &'a str,
        messages: &'a [Message],
        tools: &'a [ToolDefinition],
        thinking: ThinkingLevel,
        _on_event: &'a mut (dyn FnMut(StreamEvent) + Send),
        _cancelled: Option<Arc<AtomicBool>>,
    ) -> Pin<Box<dyn Future<Output = Result<ProviderResponse>> + Send + 'a>> {
        self.complete(model, messages, tools, thinking)
    }
}

pub use crate::agent::messages::TokenUsage;

#[derive(Debug, Clone, PartialEq)]
pub struct ProviderResponse {
    pub message: Message,
    pub usage: Option<TokenUsage>,
}

impl ProviderResponse {
    pub fn message(message: Message) -> Self {
        Self {
            message,
            usage: None,
        }
    }
}

#[derive(Debug)]
pub enum ModelList {
    Live {
        source: String,
        models: Vec<String>,
        notices: Vec<String>,
    },
}

#[derive(Debug, Deserialize)]
struct ModelsResponse {
    data: Vec<ModelInfo>,
}

#[derive(Debug, Deserialize)]
struct ModelInfo {
    id: String,
}

#[derive(Debug, Deserialize)]
struct CodexModelsResponse {
    models: Vec<CodexModelInfo>,
}

#[derive(Debug, Deserialize)]
struct CodexModelInfo {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    slug: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GitHubRelease {
    #[serde(default)]
    name: Option<String>,
    tag_name: String,
}

pub async fn list_models(config: &ProviderConfig) -> Result<ModelList> {
    match config {
        ProviderConfig::Fake => Ok(ModelList::Live {
            source: "fake".to_string(),
            models: vec!["fake".to_string()],
            notices: Vec::new(),
        }),
        ProviderConfig::OpenAiCodex {
            base_url,
            auth_path,
        } => {
            let api_key = openai_codex::get_api_key_from_path(auth_path.clone())
                .await?
                .context("OpenAI Codex auth not found; run `ferrum login openai`")?;
            let account_id = openai_codex::extract_account_id(&api_key)?;
            let client = Client::builder().timeout(Duration::from_secs(20)).build()?;
            let (client_version, mut notices) = match std::env::var("FERRUM_CODEX_CLIENT_VERSION") {
                Ok(version) => {
                    let version = validate_codex_client_version(&version)?;
                    (
                        version.clone(),
                        vec![format!("Codex client version override: {version}")],
                    )
                }
                Err(_) => match latest_codex_client_version(&client).await {
                    Ok(version) => (
                        version.clone(),
                        vec![format!(
                            "latest Codex client version from {LATEST_CODEX_RELEASE_URL}: {version}"
                        )],
                    ),
                    Err(error) => (
                        DEFAULT_CODEX_CLIENT_VERSION.to_string(),
                        vec![format!(
                            "latest Codex client version lookup failed: {error:#}; using tested fallback {DEFAULT_CODEX_CLIENT_VERSION}"
                        )],
                    ),
                },
            };

            let mut url = codex_models_url(base_url, &client_version);
            let models = match fetch_codex_models(&client, &url, &api_key, &account_id).await {
                Ok(models) => models,
                Err(error) if client_version != DEFAULT_CODEX_CLIENT_VERSION => {
                    notices.push(format!(
                        "Codex model listing with client version {client_version} failed: {error:#}; retrying with tested fallback {DEFAULT_CODEX_CLIENT_VERSION}"
                    ));
                    url = codex_models_url(base_url, DEFAULT_CODEX_CLIENT_VERSION);
                    fetch_codex_models(&client, &url, &api_key, &account_id).await?
                }
                Err(error) => return Err(error),
            };
            Ok(ModelList::Live {
                source: url,
                models,
                notices,
            })
        }
        ProviderConfig::OpenAiCompat {
            api_key_env,
            base_url,
            ..
        } => {
            let url = format!("{}/models", base_url.trim_end_matches('/'));
            let mut request = Client::builder()
                .timeout(Duration::from_secs(20))
                .build()?
                .get(&url);
            if let Some(api_key_env) = api_key_env {
                let api_key = std::env::var(api_key_env)
                    .with_context(|| format!("{} is not set", api_key_env))?;
                request = request.bearer_auth(api_key);
            }
            let response = request
                .send()
                .await
                .with_context(|| format!("failed to GET {url}"))?;
            if !response.status().is_success() {
                let status = response.status();
                let body = response.text().await.unwrap_or_default();
                anyhow::bail!("model listing failed: {status}: {body}");
            }
            let mut models = response.json::<ModelsResponse>().await?.data;
            models.sort_by(|a, b| a.id.cmp(&b.id));
            Ok(ModelList::Live {
                source: url,
                models: models.into_iter().map(|model| model.id).collect(),
                notices: Vec::new(),
            })
        }
    }
}

async fn latest_codex_client_version(client: &Client) -> Result<String> {
    let response = client
        .get(LATEST_CODEX_RELEASE_URL)
        .header(
            "user-agent",
            format!("ferrum/{}", env!("CARGO_PKG_VERSION")),
        )
        .send()
        .await
        .with_context(|| format!("failed to GET {LATEST_CODEX_RELEASE_URL}"))?
        .error_for_status()
        .context("latest Codex release lookup failed")?;
    let release = response
        .json::<GitHubRelease>()
        .await
        .context("failed to decode latest Codex release")?;
    let candidate = release
        .name
        .as_deref()
        .filter(|name| !name.trim().is_empty())
        .unwrap_or(&release.tag_name);
    validate_codex_client_version(candidate)
}

fn validate_codex_client_version(version: &str) -> Result<String> {
    let version = version
        .trim()
        .strip_prefix("rust-v")
        .unwrap_or(version.trim());
    let valid = !version.is_empty()
        && version
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || ".-+".contains(character));
    if !valid {
        anyhow::bail!("invalid Codex client version: {version:?}");
    }
    Ok(version.to_string())
}

async fn fetch_codex_models(
    client: &Client,
    url: &str,
    api_key: &str,
    account_id: &str,
) -> Result<Vec<String>> {
    let response = client
        .get(url)
        .bearer_auth(api_key)
        .header("chatgpt-account-id", account_id)
        .header("originator", "ferrum")
        .header("version", env!("CARGO_PKG_VERSION"))
        .send()
        .await
        .with_context(|| format!("failed to GET {url}"))?;
    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        anyhow::bail!("OpenAI Codex model listing failed: {status}: {body}");
    }
    let mut models = response
        .json::<CodexModelsResponse>()
        .await?
        .models
        .into_iter()
        .filter_map(|model| model.slug.or(model.id))
        .collect::<Vec<_>>();
    models.sort();
    models.dedup();
    Ok(models)
}

fn codex_models_url(base_url: &str, client_version: &str) -> String {
    let normalized = base_url.trim_end_matches('/');
    let base = if normalized.ends_with("/codex/responses") {
        normalized.trim_end_matches("/responses")
    } else if normalized.ends_with("/codex/models") {
        normalized.trim_end_matches("/models")
    } else if normalized.ends_with("/codex") {
        normalized
    } else {
        return format!("{normalized}/codex/models?client_version={client_version}");
    };
    format!("{base}/models?client_version={client_version}")
}

pub fn from_config(config: &ProviderConfig) -> Box<dyn Provider> {
    match config {
        ProviderConfig::Fake => Box::new(fake::FakeProvider),
        ProviderConfig::OpenAiCompat {
            api_key_env,
            base_url,
            streaming,
            stream_usage,
        } => Box::new(openai::OpenAiCompatProvider::new(
            api_key_env.clone(),
            base_url.clone(),
            *streaming,
            *stream_usage,
        )),
        ProviderConfig::OpenAiCodex {
            base_url,
            auth_path,
        } => Box::new(openai::OpenAiCodexProvider::new(
            base_url.clone(),
            auth_path.clone(),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_codex_release_name_and_tag_versions() {
        assert_eq!(validate_codex_client_version("0.144.0").unwrap(), "0.144.0");
        assert_eq!(
            validate_codex_client_version("rust-v0.145.0").unwrap(),
            "0.145.0"
        );
    }

    #[test]
    fn rejects_unsafe_codex_client_versions() {
        assert!(validate_codex_client_version("").is_err());
        assert!(validate_codex_client_version("0.144.0&extra=1").is_err());
    }

    #[test]
    fn builds_codex_models_url_for_supported_base_urls() {
        let version = "0.144.0";
        assert_eq!(
            codex_models_url("https://chatgpt.com/backend-api", version),
            "https://chatgpt.com/backend-api/codex/models?client_version=0.144.0"
        );
        assert_eq!(
            codex_models_url("https://example.test/codex/responses", version),
            "https://example.test/codex/models?client_version=0.144.0"
        );
        assert_eq!(
            codex_models_url("https://example.test/codex/models", version),
            "https://example.test/codex/models?client_version=0.144.0"
        );
    }
}
