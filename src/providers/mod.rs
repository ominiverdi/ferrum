mod fake;
mod openai;

use crate::{
    agent::{messages::Message, tools::ToolDefinition},
    config::{ProviderConfig, ThinkingLevel},
};
use anyhow::Result;
use std::future::Future;
use std::pin::Pin;

pub trait Provider: Send + Sync {
    fn complete<'a>(
        &'a self,
        model: &'a str,
        messages: &'a [Message],
        tools: &'a [ToolDefinition],
        thinking: ThinkingLevel,
    ) -> Pin<Box<dyn Future<Output = Result<Message>> + Send + 'a>>;
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
