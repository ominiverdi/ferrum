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

const DEFAULT_CODEX_CLIENT_VERSION: &str = "0.135.0";

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
    ) -> Pin<Box<dyn Future<Output = Result<Message>> + Send + 'a>>;

    fn complete_streaming<'a>(
        &'a self,
        model: &'a str,
        messages: &'a [Message],
        tools: &'a [ToolDefinition],
        thinking: ThinkingLevel,
        _on_event: &'a mut (dyn FnMut(StreamEvent) + Send),
        _cancelled: Option<Arc<AtomicBool>>,
    ) -> Pin<Box<dyn Future<Output = Result<Message>> + Send + 'a>> {
        self.complete(model, messages, tools, thinking)
    }
}

#[derive(Debug)]
pub enum ModelList {
    Live { source: String, models: Vec<String> },
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

pub async fn list_models(config: &ProviderConfig) -> Result<ModelList> {
    match config {
        ProviderConfig::Fake => Ok(ModelList::Live {
            source: "fake".to_string(),
            models: vec!["fake".to_string()],
        }),
        ProviderConfig::OpenAiCodex {
            base_url,
            auth_path,
        } => {
            let api_key = openai_codex::get_api_key_from_path(auth_path.clone())
                .await?
                .context("OpenAI Codex auth not found; run `ferrum login openai`")?;
            let account_id = openai_codex::extract_account_id(&api_key)?;
            let url = codex_models_url(base_url);
            let response = Client::builder()
                .timeout(Duration::from_secs(20))
                .build()?
                .get(&url)
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
            Ok(ModelList::Live {
                source: url,
                models,
            })
        }
        ProviderConfig::OpenAiCompat {
            api_key_env,
            base_url,
        } => {
            let api_key = std::env::var(api_key_env)
                .with_context(|| format!("{} is not set", api_key_env))?;
            let url = format!("{}/models", base_url.trim_end_matches('/'));
            let response = Client::builder()
                .timeout(Duration::from_secs(20))
                .build()?
                .get(&url)
                .bearer_auth(api_key)
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
            })
        }
    }
}

fn codex_models_url(base_url: &str) -> String {
    let normalized = base_url.trim_end_matches('/');
    let client_version = std::env::var("FERRUM_CODEX_CLIENT_VERSION")
        .unwrap_or_else(|_| DEFAULT_CODEX_CLIENT_VERSION.to_string());
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
        } => Box::new(openai::OpenAiCompatProvider::new(
            api_key_env.clone(),
            base_url.clone(),
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
