use super::Provider;
use crate::{
    agent::{
        messages::{ContentBlock, Message, Role},
        tools::ToolDefinition,
    },
    auth::openai_codex,
    config::ThinkingLevel,
};
use anyhow::{Context, Result};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::{env, future::Future, path::PathBuf, pin::Pin};

pub struct OpenAiCompatProvider {
    api_key_env: String,
    base_url: String,
    client: Client,
}

impl OpenAiCompatProvider {
    pub fn new(api_key_env: String, base_url: String) -> Self {
        Self {
            api_key_env,
            base_url: base_url.trim_end_matches('/').to_string(),
            client: Client::new(),
        }
    }
}

impl Provider for OpenAiCompatProvider {
    fn complete<'a>(
        &'a self,
        model: &'a str,
        messages: &'a [Message],
        _tools: &'a [ToolDefinition],
        thinking: ThinkingLevel,
    ) -> Pin<Box<dyn Future<Output = Result<Message>> + Send + 'a>> {
        Box::pin(async move {
            let api_key = env::var(&self.api_key_env).unwrap_or_default();
            let request = ChatRequest {
                model,
                messages: messages.iter().map(ChatMessage::from_message).collect(),
                tools: openai_tools(_tools),
                tool_choice: if _tools.is_empty() {
                    None
                } else {
                    Some("auto")
                },
                reasoning_effort: thinking.as_openai(),
                stream: false,
            };

            let response = self
                .client
                .post(format!("{}/chat/completions", self.base_url))
                .bearer_auth(api_key)
                .json(&request)
                .send()
                .await
                .context("OpenAI-compatible request failed")?;

            let status = response.status();
            let text = response
                .text()
                .await
                .context("failed to read provider response")?;
            if !status.is_success() {
                anyhow::bail!("OpenAI-compatible provider returned {status}: {text}");
            }

            let body: ChatResponse = serde_json::from_str(&text)
                .with_context(|| format!("failed to parse provider response: {text}"))?;
            let message = body.choices.into_iter().next().map(|choice| choice.message);
            Ok(chat_response_to_message(message))
        })
    }
}

pub struct OpenAiCodexProvider {
    base_url: String,
    auth_path: PathBuf,
    client: Client,
}

impl OpenAiCodexProvider {
    pub fn new(base_url: String, auth_path: PathBuf) -> Self {
        Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            auth_path,
            client: Client::new(),
        }
    }

    fn responses_url(&self) -> String {
        if self.base_url.ends_with("/codex/responses") {
            self.base_url.clone()
        } else if self.base_url.ends_with("/codex") {
            format!("{}/responses", self.base_url)
        } else {
            format!("{}/codex/responses", self.base_url)
        }
    }
}

impl Provider for OpenAiCodexProvider {
    fn complete<'a>(
        &'a self,
        model: &'a str,
        messages: &'a [Message],
        _tools: &'a [ToolDefinition],
        thinking: ThinkingLevel,
    ) -> Pin<Box<dyn Future<Output = Result<Message>> + Send + 'a>> {
        Box::pin(async move {
            let api_key = openai_codex::get_api_key_from_path(self.auth_path.clone())
                .await?
                .context("OpenAI Codex auth not found; run `ferrum login openai`")?;
            let account_id = openai_codex::extract_account_id(&api_key)?;
            let instructions = codex_instructions(messages);
            let request = CodexResponsesRequest {
                model,
                store: false,
                stream: true,
                instructions: &instructions,
                input: codex_inputs(messages),
                text: CodexText { verbosity: "low" },
                tools: codex_tools(_tools),
                tool_choice: if _tools.is_empty() {
                    None
                } else {
                    Some("auto")
                },
                reasoning: thinking.as_codex().map(|effort| CodexReasoning {
                    effort,
                    summary: "auto",
                }),
                parallel_tool_calls: !_tools.is_empty(),
            };

            let response = self
                .client
                .post(self.responses_url())
                .bearer_auth(api_key)
                .header("chatgpt-account-id", account_id)
                .header("originator", "ferrum")
                .header("OpenAI-Beta", "responses=experimental")
                .header("content-type", "application/json")
                .json(&request)
                .send()
                .await
                .context("OpenAI Codex request failed")?;
            let status = response.status();
            let text = response
                .text()
                .await
                .context("failed to read OpenAI Codex response")?;
            if !status.is_success() {
                anyhow::bail!("OpenAI Codex returned {status}: {text}");
            }
            Ok(extract_sse_responses_message(&text).unwrap_or_else(|| {
                let content = serde_json::from_str::<serde_json::Value>(&text)
                    .ok()
                    .and_then(|body| extract_responses_text(&body))
                    .unwrap_or_default();
                Message::text(Role::Assistant, content)
            }))
        })
    }
}

#[derive(Debug, Serialize)]
struct ChatRequest<'a> {
    model: &'a str,
    messages: Vec<ChatMessage>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tools: Vec<OpenAiTool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_choice: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reasoning_effort: Option<&'static str>,
    stream: bool,
}

#[derive(Debug, Serialize)]
struct ChatMessage {
    role: &'static str,
    content: serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_call_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_calls: Option<Vec<ChatToolCall>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reasoning_content: Option<&'static str>,
}

#[derive(Debug, Serialize)]
struct ChatToolCall {
    id: String,
    #[serde(rename = "type")]
    kind: &'static str,
    function: ChatToolFunction,
}

#[derive(Debug, Serialize)]
struct ChatToolFunction {
    name: String,
    arguments: String,
}

impl ChatMessage {
    fn from_message(message: &Message) -> Self {
        let role = match message.role {
            Role::System => "system",
            Role::User => "user",
            Role::Assistant => "assistant",
            Role::Tool => "tool",
        };
        let has_images = message
            .content
            .iter()
            .any(|block| matches!(block, ContentBlock::Image { .. }));
        let text = message
            .content
            .iter()
            .filter_map(|block| match block {
                ContentBlock::Text { text } => Some(text.as_str()),
                ContentBlock::ToolResult { content, .. } => Some(content.as_str()),
                ContentBlock::ToolUse { .. } | ContentBlock::Image { .. } => None,
            })
            .collect::<Vec<_>>()
            .join("");
        let content = if has_images {
            serde_json::Value::Array(
                message
                    .content
                    .iter()
                    .filter_map(|block| match block {
                        ContentBlock::Text { text } if !text.is_empty() => {
                            Some(serde_json::json!({
                                "type": "text",
                                "text": text,
                            }))
                        }
                        ContentBlock::Image {
                            mime_type,
                            data_base64,
                            ..
                        } => Some(serde_json::json!({
                            "type": "image_url",
                            "image_url": {
                                "url": format!("data:{mime_type};base64,{data_base64}")
                            }
                        })),
                        ContentBlock::ToolResult { content, .. } if !content.is_empty() => {
                            Some(serde_json::json!({"type": "text", "text": content}))
                        }
                        _ => None,
                    })
                    .collect(),
            )
        } else {
            serde_json::Value::String(text)
        };
        let tool_call_id = message.content.iter().find_map(|block| match block {
            ContentBlock::ToolResult { tool_use_id, .. } => Some(tool_use_id.clone()),
            _ => None,
        });
        let tool_calls: Vec<_> = message
            .content
            .iter()
            .filter_map(|block| match block {
                ContentBlock::ToolUse { id, name, input } => Some(ChatToolCall {
                    id: id.clone(),
                    kind: "function",
                    function: ChatToolFunction {
                        name: name.clone(),
                        arguments: input.to_string(),
                    },
                }),
                _ => None,
            })
            .collect();
        let has_tool_calls = !tool_calls.is_empty();
        Self {
            role,
            content,
            tool_call_id,
            tool_calls: has_tool_calls.then_some(tool_calls),
            reasoning_content: (role == "assistant" && has_tool_calls).then_some(""),
        }
    }
}

#[derive(Debug, Deserialize)]
struct ChatResponse {
    choices: Vec<ChatChoice>,
}

#[derive(Debug, Deserialize)]
struct ChatChoice {
    message: ChatChoiceMessage,
}

#[derive(Debug, Deserialize)]
struct ChatChoiceMessage {
    content: Option<String>,
    tool_calls: Option<Vec<ChatResponseToolCall>>,
}

#[derive(Debug, Deserialize)]
struct ChatResponseToolCall {
    id: String,
    function: ChatResponseToolFunction,
}

#[derive(Debug, Deserialize)]
struct ChatResponseToolFunction {
    name: String,
    arguments: String,
}

#[derive(Debug, Serialize)]
struct OpenAiTool {
    #[serde(rename = "type")]
    kind: &'static str,
    function: OpenAiToolFunction,
}

#[derive(Debug, Serialize)]
struct OpenAiToolFunction {
    name: String,
    description: String,
    parameters: serde_json::Value,
}

fn openai_tools(tools: &[ToolDefinition]) -> Vec<OpenAiTool> {
    tools
        .iter()
        .map(|tool| OpenAiTool {
            kind: "function",
            function: OpenAiToolFunction {
                name: tool.name.clone(),
                description: tool.description.clone(),
                parameters: tool.input_schema.clone(),
            },
        })
        .collect()
}

fn chat_response_to_message(message: Option<ChatChoiceMessage>) -> Message {
    let Some(message) = message else {
        return Message::text(Role::Assistant, "");
    };
    let mut content = Vec::new();
    if let Some(text) = message.content.filter(|text| !text.is_empty()) {
        content.push(ContentBlock::Text { text });
    }
    for call in message.tool_calls.unwrap_or_default() {
        let input =
            serde_json::from_str(&call.function.arguments).unwrap_or(serde_json::Value::Null);
        content.push(ContentBlock::ToolUse {
            id: call.id,
            name: call.function.name,
            input,
        });
    }
    Message {
        role: Role::Assistant,
        content,
    }
}

#[derive(Debug, Serialize)]
struct CodexResponsesRequest<'a> {
    model: &'a str,
    store: bool,
    stream: bool,
    instructions: &'a str,
    input: Vec<ResponsesInput>,
    text: CodexText<'a>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tools: Vec<CodexTool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_choice: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reasoning: Option<CodexReasoning<'a>>,
    parallel_tool_calls: bool,
}

#[derive(Debug, Serialize)]
struct CodexReasoning<'a> {
    effort: &'a str,
    summary: &'a str,
}

#[derive(Debug, Serialize)]
struct CodexTool {
    #[serde(rename = "type")]
    kind: &'static str,
    name: String,
    description: String,
    parameters: serde_json::Value,
    strict: Option<bool>,
}

fn codex_tools(tools: &[ToolDefinition]) -> Vec<CodexTool> {
    tools
        .iter()
        .map(|tool| CodexTool {
            kind: "function",
            name: tool.name.clone(),
            description: tool.description.clone(),
            parameters: tool.input_schema.clone(),
            strict: None,
        })
        .collect()
}

#[derive(Debug, Serialize)]
struct CodexText<'a> {
    verbosity: &'a str,
}

#[derive(Debug, Serialize)]
#[serde(untagged)]
enum ResponsesInput {
    Message {
        role: &'static str,
        content: serde_json::Value,
    },
    FunctionCall {
        #[serde(rename = "type")]
        kind: &'static str,
        id: String,
        call_id: String,
        name: String,
        arguments: String,
    },
    FunctionCallOutput {
        #[serde(rename = "type")]
        kind: &'static str,
        call_id: String,
        output: String,
    },
}

fn codex_instructions(messages: &[Message]) -> String {
    let mut instructions = String::from("You are a helpful coding assistant.");
    for message in messages {
        if matches!(message.role, Role::System) {
            let text = message.text_content();
            if !text.trim().is_empty() {
                instructions.push_str("\n\n");
                instructions.push_str(&text);
            }
        }
    }
    instructions
}

fn codex_message_content(message: &Message) -> serde_json::Value {
    let has_images = message
        .content
        .iter()
        .any(|block| matches!(block, ContentBlock::Image { .. }));
    if !has_images {
        return serde_json::Value::String(message.text_content());
    }
    serde_json::Value::Array(
        message
            .content
            .iter()
            .filter_map(|block| match block {
                ContentBlock::Text { text } if !text.is_empty() => Some(serde_json::json!({
                    "type": "input_text",
                    "text": text,
                })),
                ContentBlock::Image {
                    mime_type,
                    data_base64,
                    ..
                } => Some(serde_json::json!({
                    "type": "input_image",
                    "image_url": format!("data:{mime_type};base64,{data_base64}"),
                })),
                _ => None,
            })
            .collect(),
    )
}

fn codex_inputs(messages: &[Message]) -> Vec<ResponsesInput> {
    let mut inputs = Vec::new();
    for message in messages {
        if matches!(message.role, Role::System) {
            continue;
        }
        let mut emitted_special = false;
        for block in &message.content {
            match block {
                ContentBlock::ToolResult {
                    tool_use_id,
                    content,
                    ..
                } => {
                    inputs.push(ResponsesInput::FunctionCallOutput {
                        kind: "function_call_output",
                        call_id: tool_use_id
                            .split('|')
                            .next()
                            .unwrap_or(tool_use_id)
                            .to_string(),
                        output: content.clone(),
                    });
                    emitted_special = true;
                }
                ContentBlock::ToolUse { id, name, input } => {
                    let call_id = id.split('|').next().unwrap_or(id).to_string();
                    let item_id = id
                        .split('|')
                        .nth(1)
                        .map(str::to_string)
                        .unwrap_or_else(|| format!("fc_{call_id}"));
                    inputs.push(ResponsesInput::FunctionCall {
                        kind: "function_call",
                        id: item_id,
                        call_id,
                        name: name.clone(),
                        arguments: input.to_string(),
                    });
                    emitted_special = true;
                }
                ContentBlock::Text { .. } | ContentBlock::Image { .. } => {}
            }
        }
        if !emitted_special {
            let role = match message.role {
                Role::System => "system",
                Role::User => "user",
                Role::Assistant => "assistant",
                Role::Tool => "user",
            };
            inputs.push(ResponsesInput::Message {
                role,
                content: codex_message_content(message),
            });
        }
    }
    inputs
}

fn extract_sse_responses_message(text: &str) -> Option<Message> {
    let mut output = String::new();
    let mut tool_calls = Vec::new();
    let mut current_call: Option<(String, String, String, String)> = None;

    for line in text.lines() {
        let Some(data) = line.strip_prefix("data: ") else {
            continue;
        };
        if data == "[DONE]" {
            break;
        }
        let Ok(event) = serde_json::from_str::<serde_json::Value>(data) else {
            continue;
        };
        match event.get("type").and_then(|value| value.as_str()) {
            Some("response.output_text.delta") => {
                if let Some(delta) = event.get("delta").and_then(|value| value.as_str()) {
                    output.push_str(delta);
                }
            }
            Some("response.output_item.added") => {
                if let Some(item) = event.get("item") {
                    if item.get("type").and_then(|value| value.as_str()) == Some("function_call") {
                        current_call = Some((
                            item.get("call_id")
                                .and_then(|v| v.as_str())
                                .unwrap_or("call")
                                .to_string(),
                            item.get("id")
                                .and_then(|v| v.as_str())
                                .unwrap_or("fc_call")
                                .to_string(),
                            item.get("name")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string(),
                            item.get("arguments")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string(),
                        ));
                    }
                }
            }
            Some("response.function_call_arguments.delta") => {
                if let Some((_, _, _, args)) = &mut current_call {
                    if let Some(delta) = event.get("delta").and_then(|value| value.as_str()) {
                        args.push_str(delta);
                    }
                }
            }
            Some("response.function_call_arguments.done") => {
                if let Some((_, _, _, args)) = &mut current_call {
                    if let Some(done) = event.get("arguments").and_then(|value| value.as_str()) {
                        *args = done.to_string();
                    }
                }
            }
            Some("response.output_item.done") => {
                if let Some(item) = event.get("item") {
                    if item.get("type").and_then(|value| value.as_str()) == Some("function_call") {
                        let call_id = item
                            .get("call_id")
                            .and_then(|v| v.as_str())
                            .unwrap_or("call")
                            .to_string();
                        let item_id = item
                            .get("id")
                            .and_then(|v| v.as_str())
                            .unwrap_or("fc_call")
                            .to_string();
                        let name = item
                            .get("name")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        let args = item
                            .get("arguments")
                            .and_then(|v| v.as_str())
                            .unwrap_or("{}")
                            .to_string();
                        tool_calls.push((call_id, item_id, name, args));
                        current_call = None;
                    }
                }
            }
            _ => {}
        }
    }

    if let Some(call) = current_call {
        tool_calls.push(call);
    }

    let mut content = Vec::new();
    if !output.is_empty() {
        content.push(ContentBlock::Text { text: output });
    }
    for (call_id, item_id, name, args) in tool_calls {
        let input = serde_json::from_str(&args).unwrap_or(serde_json::Value::Null);
        content.push(ContentBlock::ToolUse {
            id: format!("{call_id}|{item_id}"),
            name,
            input,
        });
    }
    (!content.is_empty()).then_some(Message {
        role: Role::Assistant,
        content,
    })
}

fn extract_responses_text(body: &serde_json::Value) -> Option<String> {
    if let Some(text) = body.get("output_text").and_then(|value| value.as_str()) {
        return Some(text.to_string());
    }
    let mut parts = Vec::new();
    for item in body.get("output")?.as_array()? {
        for content in item
            .get("content")
            .and_then(|value| value.as_array())
            .into_iter()
            .flatten()
        {
            if let Some(text) = content.get("text").and_then(|value| value.as_str()) {
                parts.push(text);
            }
        }
    }
    Some(parts.join(""))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_normalized_text_message() {
        let message = Message::text(Role::User, "hello");
        let chat = ChatMessage::from_message(&message);
        assert_eq!(chat.role, "user");
        assert_eq!(chat.content, "hello");
    }
}
