pub mod bash;
pub mod edit;
pub mod find;
pub mod grep;
pub mod ls;
pub mod path;
pub mod read;
pub mod write;

use crate::agent::tools::ToolDefinition;
use anyhow::Result;
use serde_json::json;
use std::{path::Path, time::Duration};

pub fn definitions() -> Vec<ToolDefinition> {
    vec![
        ToolDefinition {
            name: "read".to_string(),
            description: "Read a text file with optional 1-based line offset and line limit."
                .to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string" },
                    "offset": { "type": "integer", "minimum": 1 },
                    "limit": { "type": "integer", "minimum": 1 }
                },
                "required": ["path"],
                "additionalProperties": false
            }),
        },
        ToolDefinition {
            name: "ls".to_string(),
            description: "List directory contents.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string" }
                },
                "required": ["path"],
                "additionalProperties": false
            }),
        },
        ToolDefinition {
            name: "bash".to_string(),
            description: "Run a bash command in the current working directory with a timeout."
                .to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "command": { "type": "string" },
                    "timeout_seconds": { "type": "integer", "minimum": 1, "maximum": 120 }
                },
                "required": ["command"],
                "additionalProperties": false
            }),
        },
        ToolDefinition {
            name: "write".to_string(),
            description: "Create or overwrite a text file. Creates parent directories.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string" },
                    "content": { "type": "string" }
                },
                "required": ["path", "content"],
                "additionalProperties": false
            }),
        },
        ToolDefinition {
            name: "edit".to_string(),
            description: "Apply exact text replacements to a file. Each old_text must match exactly once and edits must not overlap. Preserves BOM and existing LF/CRLF line endings.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string" },
                    "edits": {
                        "type": "array",
                        "minItems": 1,
                        "items": {
                            "type": "object",
                            "properties": {
                                "old_text": { "type": "string" },
                                "new_text": { "type": "string" }
                            },
                            "required": ["old_text", "new_text"],
                            "additionalProperties": false
                        }
                    }
                },
                "required": ["path", "edits"],
                "additionalProperties": false
            }),
        },
        ToolDefinition {
            name: "grep".to_string(),
            description: "Search file contents under a path. Uses ripgrep if available.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "pattern": { "type": "string" },
                    "path": { "type": "string" }
                },
                "required": ["pattern", "path"],
                "additionalProperties": false
            }),
        },
        ToolDefinition {
            name: "find".to_string(),
            description: "Find files by filename substring and/or extension under a path.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string" },
                    "name": { "type": "string" },
                    "extension": { "type": "string" }
                },
                "required": ["path"],
                "additionalProperties": false
            }),
        },
    ]
}

pub async fn execute(name: &str, input: &serde_json::Value, cwd: &Path) -> Result<String> {
    match name {
        "read" => {
            let path = required_str(input, "path")?;
            let offset = input.get("offset").and_then(|v| v.as_u64()).unwrap_or(1) as usize;
            let limit = input
                .get("limit")
                .and_then(|v| v.as_u64())
                .map(|v| v as usize);
            read::read_text(&path::resolve_to_cwd(path, cwd)?, offset, limit)
        }
        "ls" => {
            let path = input.get("path").and_then(|v| v.as_str()).unwrap_or(".");
            ls::list(&path::resolve_to_cwd(path, cwd)?)
        }
        "bash" => {
            let command = required_str(input, "command")?;
            let timeout = input
                .get("timeout_seconds")
                .and_then(|v| v.as_u64())
                .unwrap_or(30);
            let output = bash::run(command, cwd, Duration::from_secs(timeout)).await?;
            Ok(format!(
                "status: {:?}\ntimed_out: {}\nstdout:\n{}\nstderr:\n{}",
                output.status, output.timed_out, output.stdout, output.stderr
            ))
        }
        "write" => {
            let path = required_str(input, "path")?;
            let content = required_str(input, "content")?;
            write::write_text(&path::resolve_to_cwd(path, cwd)?, content)
        }
        "edit" => {
            let path = required_str(input, "path")?;
            let edits_value = input
                .get("edits")
                .ok_or_else(|| anyhow::anyhow!("missing required field: edits"))?;
            let edits: Vec<edit::EditSpec> = serde_json::from_value(edits_value.clone())?;
            edit::replace_exact(&path::resolve_to_cwd(path, cwd)?, &edits)
        }
        "grep" => {
            let pattern = required_str(input, "pattern")?;
            let path = required_str(input, "path")?;
            grep::grep(pattern, &path::resolve_to_cwd(path, cwd)?)
        }
        "find" => {
            let path = required_str(input, "path")?;
            let name = input.get("name").and_then(|v| v.as_str());
            let extension = input.get("extension").and_then(|v| v.as_str());
            find::find(&path::resolve_to_cwd(path, cwd)?, name, extension)
        }
        other => anyhow::bail!("unknown tool: {other}"),
    }
}

fn required_str<'a>(input: &'a serde_json::Value, key: &str) -> Result<&'a str> {
    input
        .get(key)
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("missing required string field: {key}"))
}
