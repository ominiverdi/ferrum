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

const DEFAULT_BASH_TIMEOUT_SECONDS: u64 = 30;
const MAX_BASH_TIMEOUT_SECONDS: u64 = 600;

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
                    "path": { "type": "string" },
                    "limit": { "type": "integer", "minimum": 1, "maximum": 10000 }
                },
                "required": ["path"],
                "additionalProperties": false
            }),
        },
        ToolDefinition {
            name: "bash".to_string(),
            description: "Run focused bash commands in the current working directory with a timeout. Prefer find/grep/ls tools for broad filesystem exploration; if using shell find/grep, exclude .git, target, and node_modules. Use nohup with redirected logs for background jobs or commands that should outlive the tool call."
                .to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "command": { "type": "string" },
                    "timeout_seconds": { "type": "integer", "minimum": 1, "maximum": MAX_BASH_TIMEOUT_SECONDS }
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
            description: "Search file contents under a path with optional glob filtering, case-insensitive/literal matching, context lines, and match limits. Includes hidden config directories, respects ignore files, and skips noisy directories such as .git, target, and node_modules. Uses ripgrep if available.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "pattern": { "type": "string" },
                    "path": { "type": "string" },
                    "glob": { "type": "string", "description": "Filter files by glob pattern, e.g. '*.rs' or '**/*.service'" },
                    "ignore_case": { "type": "boolean" },
                    "literal": { "type": "boolean", "description": "Treat pattern as a literal string" },
                    "context": { "type": "integer", "minimum": 0, "maximum": 20 },
                    "limit": { "type": "integer", "minimum": 1, "maximum": 10000 }
                },
                "required": ["pattern", "path"],
                "additionalProperties": false
            }),
        },
        ToolDefinition {
            name: "find".to_string(),
            description: "Find files by glob pattern and/or legacy filename substring/extension filters. Includes hidden config directories, respects ignore files, and skips noisy directories such as .git, target, and node_modules.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string" },
                    "pattern": { "type": "string", "description": "Glob pattern, e.g. '*.rs', '**/*.service', or 'src/**/*.rs'" },
                    "name": { "type": "string", "description": "Legacy filename substring filter" },
                    "extension": { "type": "string", "description": "Legacy extension filter, with or without leading dot" },
                    "limit": { "type": "integer", "minimum": 1, "maximum": 10000 }
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
            let limit = input
                .get("limit")
                .and_then(|v| v.as_u64())
                .map(|v| v as usize);
            ls::list(&path::resolve_to_cwd(path, cwd)?, limit)
        }
        "bash" => {
            let command = required_str(input, "command")?;
            let timeout = input
                .get("timeout_seconds")
                .and_then(|v| v.as_u64())
                .unwrap_or(DEFAULT_BASH_TIMEOUT_SECONDS);
            if timeout > MAX_BASH_TIMEOUT_SECONDS {
                anyhow::bail!(
                    "bash timeout_seconds must be <= {MAX_BASH_TIMEOUT_SECONDS}, got {timeout}"
                );
            }
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
            let options = grep::GrepOptions {
                glob: input.get("glob").and_then(|v| v.as_str()),
                ignore_case: input
                    .get("ignore_case")
                    .or_else(|| input.get("ignoreCase"))
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false),
                literal: input
                    .get("literal")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false),
                context: input
                    .get("context")
                    .and_then(|v| v.as_u64())
                    .map(|v| v as usize),
                limit: input
                    .get("limit")
                    .and_then(|v| v.as_u64())
                    .map(|v| v as usize),
            };
            grep::grep(pattern, &path::resolve_to_cwd(path, cwd)?, options)
        }
        "find" => {
            let path = required_str(input, "path")?;
            let options = find::FindOptions {
                pattern: input.get("pattern").and_then(|v| v.as_str()),
                name: input.get("name").and_then(|v| v.as_str()),
                extension: input.get("extension").and_then(|v| v.as_str()),
                limit: input
                    .get("limit")
                    .and_then(|v| v.as_u64())
                    .map(|v| v as usize),
            };
            find::find(&path::resolve_to_cwd(path, cwd)?, options)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bash_schema_allows_ten_minute_timeout() {
        let bash = definitions()
            .into_iter()
            .find(|tool| tool.name == "bash")
            .unwrap();

        assert_eq!(
            bash.input_schema["properties"]["timeout_seconds"]["maximum"],
            MAX_BASH_TIMEOUT_SECONDS
        );
    }

    #[tokio::test]
    async fn bash_rejects_timeout_above_limit() {
        let temp = tempfile::tempdir().unwrap();
        let input = serde_json::json!({
            "command": "true",
            "timeout_seconds": MAX_BASH_TIMEOUT_SECONDS + 1,
        });

        let error = execute("bash", &input, temp.path()).await.unwrap_err();

        assert!(
            error
                .to_string()
                .contains("bash timeout_seconds must be <=")
        );
    }
}
