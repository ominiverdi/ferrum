use super::Provider;
use crate::{
    agent::{
        messages::{Message, Role},
        tools::ToolDefinition,
    },
    config::ThinkingLevel,
};
use anyhow::Result;
use std::{future::Future, pin::Pin};

pub struct FakeProvider;

impl Provider for FakeProvider {
    fn complete<'a>(
        &'a self,
        _model: &'a str,
        messages: &'a [Message],
        _tools: &'a [ToolDefinition],
        _thinking: ThinkingLevel,
    ) -> Pin<Box<dyn Future<Output = Result<Message>> + Send + 'a>> {
        Box::pin(async move {
            let last_user = messages
                .iter()
                .rev()
                .find(|message| matches!(message.role, Role::User))
                .map(Message::text_content)
                .unwrap_or_default();
            Ok(Message::text(
                Role::Assistant,
                format!("fake provider response: {last_user}\n"),
            ))
        })
    }
}
