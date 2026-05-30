use super::Provider;
use crate::{
    agent::{
        messages::{ContentBlock, Message, Role},
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
            if let Ok(script) = std::env::var("FERRUM_FAKE_SCRIPT") {
                return Ok(scripted_response(&script, messages));
            }
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

fn scripted_response(script: &str, messages: &[Message]) -> Message {
    match script {
        "repeat_read" => repeat_read_response(messages),
        "missing_read" => missing_read_response(messages),
        "mixed_write_read" => mixed_write_read_response(messages),
        _ => Message::text(Role::Assistant, format!("unknown fake script: {script}\n")),
    }
}

fn mixed_write_read_response(messages: &[Message]) -> Message {
    let tool_results = messages
        .iter()
        .flat_map(|message| &message.content)
        .filter(|block| matches!(block, ContentBlock::ToolResult { .. }))
        .count();

    if tool_results >= 2 {
        let saw_ready = messages.iter().any(|message| {
            message.content.iter().any(|block| match block {
                ContentBlock::ToolResult { content, .. } => {
                    content.contains("ready from mixed batch")
                }
                _ => false,
            })
        });
        return Message::text(
            Role::Assistant,
            if saw_ready {
                "mixed batch read observed ready from mixed batch\n"
            } else {
                "mixed batch read did not observe ready content\n"
            },
        );
    }

    Message {
        role: Role::Assistant,
        content: vec![
            ContentBlock::ToolUse {
                id: "fake-write-generated".to_string(),
                name: "write".to_string(),
                input: serde_json::json!({
                    "path": "generated.txt",
                    "content": "ready from mixed batch\n"
                }),
            },
            ContentBlock::ToolUse {
                id: "fake-read-generated".to_string(),
                name: "read".to_string(),
                input: serde_json::json!({"path": "generated.txt"}),
            },
        ],
    }
}

fn missing_read_response(messages: &[Message]) -> Message {
    let tool_results = messages
        .iter()
        .flat_map(|message| &message.content)
        .filter(|block| matches!(block, ContentBlock::ToolResult { .. }))
        .count();

    if tool_results >= 8
        || messages.iter().any(|message| {
            message
                .text_content()
                .contains("Adaptive loop guard stopped tool use")
        })
    {
        return Message::text(Role::Assistant, "final after missing read loop guard\n");
    }

    Message {
        role: Role::Assistant,
        content: vec![ContentBlock::ToolUse {
            id: format!("fake-missing-read-{tool_results}"),
            name: "read".to_string(),
            input: serde_json::json!({"path": format!("missing-{tool_results}.txt")}),
        }],
    }
}

fn repeat_read_response(messages: &[Message]) -> Message {
    let tool_results = messages
        .iter()
        .flat_map(|message| &message.content)
        .filter(|block| matches!(block, ContentBlock::ToolResult { .. }))
        .count();

    if tool_results >= 7
        || messages.iter().any(|message| {
            message
                .text_content()
                .contains("Adaptive loop guard stopped tool use")
        })
    {
        return Message::text(Role::Assistant, "final after repeated read loop guard\n");
    }

    Message {
        role: Role::Assistant,
        content: vec![ContentBlock::ToolUse {
            id: format!("fake-read-{tool_results}"),
            name: "read".to_string(),
            input: serde_json::json!({"path": "loop.txt"}),
        }],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repeat_read_script_emits_tool_call() {
        let message = repeat_read_response(&[]);
        assert!(matches!(
            message.content.first(),
            Some(ContentBlock::ToolUse { name, .. }) if name == "read"
        ));
    }

    #[test]
    fn missing_read_script_emits_tool_call() {
        let message = missing_read_response(&[]);
        assert!(matches!(
            message.content.first(),
            Some(ContentBlock::ToolUse { name, input, .. })
                if name == "read" && input["path"] == "missing-0.txt"
        ));
    }

    #[test]
    fn mixed_write_read_script_emits_mixed_batch() {
        let message = mixed_write_read_response(&[]);
        assert_eq!(message.content.len(), 2);
        assert!(matches!(
            message.content.first(),
            Some(ContentBlock::ToolUse { name, .. }) if name == "write"
        ));
        assert!(matches!(
            message.content.get(1),
            Some(ContentBlock::ToolUse { name, .. }) if name == "read"
        ));
    }
}
