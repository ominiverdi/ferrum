use super::messages::ContentBlock;
use serde::{Deserialize, Serialize};

#[derive(Debug)]
pub struct ToolOutput {
    pub content: String,
    pub images: Vec<ContentBlock>,
}

impl From<String> for ToolOutput {
    fn from(content: String) -> Self {
        Self {
            content,
            images: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub input_schema: serde_json::Value,
}
