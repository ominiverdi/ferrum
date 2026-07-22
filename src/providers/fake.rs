use super::{Provider, ProviderResponse, StreamEvent};
use crate::{
    agent::{
        messages::{ContentBlock, Message, Role},
        tools::ToolDefinition,
    },
    config::ThinkingLevel,
};
use anyhow::Result;
use std::{
    future::Future,
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

pub struct FakeProvider;

impl Provider for FakeProvider {
    fn complete<'a>(
        &'a self,
        _model: &'a str,
        messages: &'a [Message],
        tools: &'a [ToolDefinition],
        _thinking: ThinkingLevel,
    ) -> Pin<Box<dyn Future<Output = Result<ProviderResponse>> + Send + 'a>> {
        Box::pin(async move {
            if let Ok(script) = std::env::var("FERRUM_FAKE_SCRIPT") {
                return Ok(ProviderResponse::message(scripted_response(
                    &script,
                    messages,
                    tools.is_empty(),
                )));
            }
            if messages.iter().any(|message| {
                matches!(message.role, Role::System)
                    && message
                        .text_content()
                        .contains("You are a context summarization assistant")
            }) {
                return Ok(ProviderResponse::message(Message::text(
                    Role::Assistant,
                    "fake compaction summary\n",
                )));
            }
            let last_user = messages
                .iter()
                .rev()
                .find(|message| matches!(message.role, Role::User))
                .map(Message::text_content)
                .unwrap_or_default();
            #[cfg(test)]
            if last_user == "__ferrum_test_event_stream__" {
                return Ok(ProviderResponse::message(Message {
                    role: Role::Assistant,
                    content: vec![
                        ContentBlock::Thinking {
                            text: "event thought".to_string(),
                            signature: None,
                        },
                        ContentBlock::Text {
                            text: "event text\n".to_string(),
                        },
                    ],
                    usage: None,
                }));
            }
            #[cfg(test)]
            if last_user == "__ferrum_test_repeat_read__" {
                return Ok(ProviderResponse::message(repeat_read_response(
                    messages,
                    tools.is_empty(),
                )));
            }
            #[cfg(test)]
            if last_user == "__ferrum_test_single_read__" {
                return Ok(ProviderResponse::message(single_read_response(messages)));
            }
            Ok(ProviderResponse::message(Message::text(
                Role::Assistant,
                format!("fake provider response: {last_user}\n"),
            )))
        })
    }
    fn complete_streaming<'a>(
        &'a self,
        model: &'a str,
        messages: &'a [Message],
        tools: &'a [ToolDefinition],
        thinking: ThinkingLevel,
        on_event: &'a mut (dyn FnMut(StreamEvent) + Send),
        cancelled: Option<Arc<AtomicBool>>,
    ) -> Pin<Box<dyn Future<Output = Result<ProviderResponse>> + Send + 'a>> {
        Box::pin(async move {
            if std::env::var("FERRUM_FAKE_SCRIPT").as_deref() == Ok("wait_cancel")
                && messages
                    .iter()
                    .rev()
                    .find(|message| matches!(message.role, Role::User))
                    .is_some_and(|message| message.text_content() == "wait")
            {
                loop {
                    if cancelled
                        .as_ref()
                        .is_some_and(|flag| flag.load(Ordering::Relaxed))
                    {
                        anyhow::bail!("aborted");
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                }
            }
            if cancelled
                .as_ref()
                .is_some_and(|flag| flag.load(Ordering::Relaxed))
            {
                anyhow::bail!("aborted");
            }
            #[cfg(test)]
            if messages.iter().any(|message| {
                matches!(message.role, Role::User)
                    && message.text_content() == "__ferrum_test_wait_cancel__"
            }) {
                loop {
                    if cancelled
                        .as_ref()
                        .is_some_and(|flag| flag.load(Ordering::Relaxed))
                    {
                        anyhow::bail!("aborted");
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                }
            }
            let response = self.complete(model, messages, tools, thinking).await?;
            if cancelled
                .as_ref()
                .is_some_and(|flag| flag.load(Ordering::Relaxed))
            {
                anyhow::bail!("aborted");
            }
            for block in &response.message.content {
                match block {
                    ContentBlock::Thinking { text, .. } if !text.is_empty() => {
                        on_event(StreamEvent::ThinkingDelta(text.clone()));
                    }
                    ContentBlock::Text { text } if !text.is_empty() => {
                        on_event(StreamEvent::TextDelta(text.clone()));
                    }
                    _ => {}
                }
            }
            Ok(response)
        })
    }
}

fn scripted_response(script: &str, messages: &[Message], final_response: bool) -> Message {
    match script {
        "repeat_read" => repeat_read_response(messages, final_response),
        "single_read" => single_read_response(messages),
        "cancel_bash" => cancel_bash_response(messages),
        "thought" => Message {
            role: Role::Assistant,
            content: vec![
                ContentBlock::Thinking {
                    text: "visible thought summary<!-- hidden -->".to_string(),
                    signature: None,
                },
                ContentBlock::Text {
                    text: "thought complete\n".to_string(),
                },
            ],
            usage: None,
        },
        "missing_read" => missing_read_response(messages, final_response),
        "mixed_write_read" => mixed_write_read_response(messages),
        "edit_preview" => edit_preview_response(messages),
        "history_search_read" => history_search_read_response(messages),
        "mcp_echo" => mcp_echo_response(messages),
        "permission_write" => permission_write_response(messages, "permission.txt"),
        "permission_outside" => permission_write_response(messages, "../outside.txt"),
        "permission_protected" => permission_write_response(messages, "/etc/passwd"),
        "permission_bash_denied" => permission_bash_response(messages, "sh -c 'echo denied'"),
        _ => Message::text(Role::Assistant, format!("unknown fake script: {script}\n")),
    }
}

fn permission_bash_response(messages: &[Message], command: &str) -> Message {
    let completed = messages.iter().any(|message| {
        message.content.iter().any(|block| {
            matches!(block, ContentBlock::ToolResult { tool_use_id, .. } if tool_use_id == "fake-permission-bash")
        })
    });
    if completed {
        return Message::text(Role::Assistant, "permission bash complete\n");
    }
    Message {
        role: Role::Assistant,
        content: vec![ContentBlock::ToolUse {
            id: "fake-permission-bash".to_string(),
            name: "bash".to_string(),
            input: serde_json::json!({"command": command}),
        }],
        usage: None,
    }
}

fn permission_write_response(messages: &[Message], path: &str) -> Message {
    let completed = messages.iter().any(|message| {
        message.content.iter().any(|block| {
            matches!(block, ContentBlock::ToolResult { tool_use_id, .. } if tool_use_id == "fake-permission-write")
        })
    });
    if completed {
        return Message::text(Role::Assistant, "permission write complete\n");
    }
    Message {
        role: Role::Assistant,
        content: vec![ContentBlock::ToolUse {
            id: "fake-permission-write".to_string(),
            name: "write".to_string(),
            input: serde_json::json!({"path": path, "content": "approved\n"}),
        }],
        usage: None,
    }
}

fn mcp_echo_response(messages: &[Message]) -> Message {
    let mut result = None;
    for message in messages.iter().rev() {
        if matches!(message.role, Role::User) {
            break;
        }
        result = message.content.iter().find_map(|block| match block {
            ContentBlock::ToolResult {
                tool_use_id,
                content,
                ..
            } if tool_use_id == "fake-mcp-echo" => Some(content.clone()),
            _ => None,
        });
        if result.is_some() {
            break;
        }
    }
    if let Some(result) = result {
        return Message::text(Role::Assistant, format!("MCP result: {result}\n"));
    }
    Message {
        role: Role::Assistant,
        content: vec![ContentBlock::ToolUse {
            id: "fake-mcp-echo".to_string(),
            name: "mcp__client__echo".to_string(),
            input: serde_json::json!({"text": "hello"}),
        }],
        usage: None,
    }
}

fn history_search_read_response(messages: &[Message]) -> Message {
    let tool_results = messages
        .iter()
        .flat_map(|message| &message.content)
        .filter_map(|block| match block {
            ContentBlock::ToolResult { content, .. } => Some(content.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>();

    if tool_results.len() >= 2 {
        let saw_marker = tool_results
            .iter()
            .any(|content| content.contains("unique-history-marker"));
        return Message::text(
            Role::Assistant,
            if saw_marker {
                "history tools observed marker\n"
            } else {
                "history tools did not observe marker\n"
            },
        );
    }

    if tool_results.len() == 1 {
        return Message {
            role: Role::Assistant,
            content: vec![ContentBlock::ToolUse {
                id: "fake-history-read".to_string(),
                name: "history_read".to_string(),
                input: serde_json::json!({"offset": 1, "limit": 20}),
            }],
            usage: None,
        };
    }

    Message {
        role: Role::Assistant,
        content: vec![ContentBlock::ToolUse {
            id: "fake-history-search".to_string(),
            name: "history_search".to_string(),
            input: serde_json::json!({"query": "unique-history-marker", "literal": true}),
        }],
        usage: None,
    }
}

fn edit_preview_response(messages: &[Message]) -> Message {
    let saw_edit = messages.iter().any(|message| {
        message.content.iter().any(|block| {
            matches!(block, ContentBlock::ToolResult { tool_use_id, .. } if tool_use_id == "fake-edit-preview")
        })
    });
    if saw_edit {
        return Message::text(Role::Assistant, "edit preview complete\n");
    }
    Message {
        role: Role::Assistant,
        content: vec![ContentBlock::ToolUse {
            id: "fake-edit-preview".to_string(),
            name: "edit".to_string(),
            input: serde_json::json!({
                "path": "sample.txt",
                "edits": [{
                    "old_text": "alpha beta gamma\nkeep this line\nremove this sentence\n",
                    "new_text": "alpha beta delta\nkeep this line\nadd this better sentence\n"
                }]
            }),
        }],
        usage: None,
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
        usage: None,
    }
}

fn missing_read_response(messages: &[Message], final_response: bool) -> Message {
    let tool_results = messages
        .iter()
        .flat_map(|message| &message.content)
        .filter(|block| matches!(block, ContentBlock::ToolResult { .. }))
        .count();

    if tool_results >= 8 || final_response {
        return Message::text(Role::Assistant, "final after missing read loop guard\n");
    }

    Message {
        role: Role::Assistant,
        content: vec![ContentBlock::ToolUse {
            id: format!("fake-missing-read-{tool_results}"),
            name: "read".to_string(),
            input: serde_json::json!({"path": format!("missing-{tool_results}.txt")}),
        }],
        usage: None,
    }
}

fn cancel_bash_response(messages: &[Message]) -> Message {
    if messages.iter().any(|message| {
        message
            .content
            .iter()
            .any(|block| matches!(block, ContentBlock::ToolResult { .. }))
    }) {
        return Message::text(Role::Assistant, "cancelled bash complete\n");
    }
    Message {
        role: Role::Assistant,
        content: vec![ContentBlock::ToolUse {
            id: "fake-cancel-bash".to_string(),
            name: "bash".to_string(),
            input: serde_json::json!({"command": "sleep 30", "timeout_seconds": 60}),
        }],
        usage: None,
    }
}

fn single_read_response(messages: &[Message]) -> Message {
    let tool_result = messages
        .iter()
        .flat_map(|message| &message.content)
        .find_map(|block| match block {
            ContentBlock::ToolResult { content, .. } => Some(content),
            _ => None,
        });
    if let Some(content) = tool_result {
        return Message::text(
            Role::Assistant,
            format!("single read complete: {content}\n"),
        );
    }
    Message {
        role: Role::Assistant,
        content: vec![ContentBlock::ToolUse {
            id: "fake-single-read".to_string(),
            name: "read".to_string(),
            input: serde_json::json!({"path": "relative.txt"}),
        }],
        usage: None,
    }
}

fn repeat_read_response(messages: &[Message], final_response: bool) -> Message {
    let tool_results = messages
        .iter()
        .flat_map(|message| &message.content)
        .filter(|block| matches!(block, ContentBlock::ToolResult { .. }))
        .count();

    if tool_results >= 7 || final_response {
        return Message::text(Role::Assistant, "final after repeated read loop guard\n");
    }

    Message {
        role: Role::Assistant,
        content: vec![ContentBlock::ToolUse {
            id: format!("fake-read-{tool_results}"),
            name: "read".to_string(),
            input: serde_json::json!({"path": "loop.txt"}),
        }],
        usage: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repeat_read_script_emits_tool_call() {
        let message = repeat_read_response(&[], false);
        assert!(matches!(
            message.content.first(),
            Some(ContentBlock::ToolUse { name, .. }) if name == "read"
        ));
    }

    #[test]
    fn missing_read_script_emits_tool_call() {
        let message = missing_read_response(&[], false);
        assert!(matches!(
            message.content.first(),
            Some(ContentBlock::ToolUse { name, input, .. })
                if name == "read" && input["path"] == "missing-0.txt"
        ));
    }

    #[test]
    fn loop_scripts_use_tool_exposure_for_final_response() {
        let repeated = repeat_read_response(&[], true);
        let missing = missing_read_response(&[], true);

        assert_eq!(
            repeated.text_content(),
            "final after repeated read loop guard\n"
        );
        assert_eq!(
            missing.text_content(),
            "final after missing read loop guard\n"
        );
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
