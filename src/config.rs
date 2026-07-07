use crate::ui_colors::ColorPalette;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, collections::BTreeSet, env, fs, path::PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub config_dir: PathBuf,
    pub data_dir: PathBuf,
    pub model: String,
    pub provider_model: String,
    pub provider_name: String,
    pub provider: ProviderConfig,
    pub providers: BTreeMap<String, ProviderDefinition>,
    pub models: BTreeMap<String, ModelDefinition>,
    pub offline: bool,
    pub max_context_tokens: usize,
    pub base_max_context_tokens: usize,
    pub max_tool_rounds: usize,
    pub thinking: ThinkingLevel,
    pub mcp_enabled: bool,
    pub mcp_server_allow: Option<Vec<String>>,
    pub color_mode: ColorMode,
    pub colors: ColorPalette,
    pub diff_mode: DiffMode,
    pub safety: SafetyLevel,
    pub tools_allow: Option<Vec<String>>,
    pub tools_deny: Vec<String>,
    pub tool_selection: Option<ToolSelection>,
    pub mcp_servers: Vec<McpServerConfig>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ToolSelection {
    None,
    List(Vec<String>),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServerConfig {
    pub name: String,
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThinkingLevel {
    Off,
    Minimal,
    Low,
    Medium,
    High,
    Xhigh,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ColorMode {
    Auto,
    On,
    Off,
}

impl ColorMode {
    pub fn parse(value: &str) -> Result<Self> {
        match value {
            "auto" => Ok(Self::Auto),
            "on" => Ok(Self::On),
            "off" => Ok(Self::Off),
            other => anyhow::bail!("unsupported color mode: {other}"),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::On => "on",
            Self::Off => "off",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiffMode {
    Unified,
    Compact,
    Full,
    Words,
    SideBySide,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SafetyLevel {
    Low,
    Medium,
    High,
}

impl SafetyLevel {
    pub fn parse(value: &str) -> Result<Self> {
        match value {
            "low" => Ok(Self::Low),
            "medium" | "med" => Ok(Self::Medium),
            "high" => Ok(Self::High),
            other => {
                anyhow::bail!("unsupported safety level: {other}; expected low, medium, or high")
            }
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
        }
    }
}

impl DiffMode {
    pub fn parse(value: &str) -> Result<Self> {
        match value {
            "unified" => Ok(Self::Unified),
            "compact" => Ok(Self::Compact),
            "full" => Ok(Self::Full),
            "words" | "word" => Ok(Self::Words),
            "side_by_side" | "side-by-side" | "side" | "split" => Ok(Self::SideBySide),
            other => anyhow::bail!("unsupported diff mode: {other}"),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Unified => "unified",
            Self::Compact => "compact",
            Self::Full => "full",
            Self::Words => "words",
            Self::SideBySide => "side_by_side",
        }
    }
}

impl ThinkingLevel {
    pub fn parse(value: &str) -> Result<Self> {
        match value {
            "off" => Ok(Self::Off),
            "minimal" => Ok(Self::Minimal),
            "low" => Ok(Self::Low),
            "medium" => Ok(Self::Medium),
            "high" => Ok(Self::High),
            "xhigh" => Ok(Self::Xhigh),
            other => anyhow::bail!("unsupported thinking level: {other}"),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Minimal => "minimal",
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::Xhigh => "xhigh",
        }
    }

    pub fn as_openai(self) -> Option<&'static str> {
        match self {
            Self::Off => None,
            Self::Minimal => Some("minimal"),
            Self::Low => Some("low"),
            Self::Medium => Some("medium"),
            Self::High => Some("high"),
            Self::Xhigh => Some("high"),
        }
    }

    pub fn as_codex(self) -> Option<&'static str> {
        match self {
            Self::Off => None,
            Self::Minimal => Some("low"),
            Self::Low => Some("low"),
            Self::Medium => Some("medium"),
            Self::High => Some("high"),
            Self::Xhigh => Some("xhigh"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ProviderConfig {
    Fake,
    OpenAiCompat {
        api_key_env: Option<String>,
        base_url: String,
        streaming: bool,
        stream_usage: bool,
    },
    OpenAiCodex {
        base_url: String,
        auth_path: PathBuf,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderDefinition {
    #[serde(rename = "type")]
    pub kind: String,
    pub base_url: Option<String>,
    pub api_key_env: Option<String>,
    pub default_model: Option<String>,
    pub streaming: Option<bool>,
    pub stream_usage: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelDefinition {
    pub provider: Option<String>,
    pub actual_model: Option<String>,
    pub max_context_tokens: Option<usize>,
}

#[derive(Debug, Deserialize, Default)]
struct FileConfig {
    model: Option<String>,
    provider: Option<String>,
    max_context_tokens: Option<usize>,
    max_tool_rounds: Option<usize>,
    thinking: Option<String>,
    mcp_enabled: Option<bool>,
    color: Option<String>,
    diff_mode: Option<String>,
    safety: Option<String>,
    tools: Option<FileToolsConfig>,
    mcp: Option<FileMcpConfig>,
    #[serde(default)]
    providers: BTreeMap<String, ProviderDefinition>,
    #[serde(default)]
    models: BTreeMap<String, ModelDefinition>,
}

#[derive(Debug, Deserialize, Default)]
struct FileToolsConfig {
    allow: Option<Vec<String>>,
    #[serde(default)]
    deny: Vec<String>,
}

#[derive(Debug, Deserialize, Default)]
struct FileMcpConfig {
    #[serde(default)]
    servers: Vec<McpServerConfig>,
}

impl Config {
    pub fn load() -> Result<Self> {
        let config_dir = match env::var_os("FERRUM_CONFIG_DIR") {
            Some(path) => PathBuf::from(path),
            None => home_dir()?.join(".config/ferrum"),
        };
        let data_dir = match env::var_os("FERRUM_DATA_DIR") {
            Some(path) => PathBuf::from(path),
            None => match env::var_os("XDG_DATA_HOME") {
                Some(path) => PathBuf::from(path).join("ferrum"),
                None => home_dir()?.join(".local/share/ferrum"),
            },
        };
        Self::load_from_dirs(config_dir, data_dir)
    }

    #[cfg(test)]
    fn load_from_dir(config_dir: PathBuf) -> Result<Self> {
        Self::load_from_dirs(config_dir.clone(), config_dir)
    }

    fn load_from_dirs(config_dir: PathBuf, data_dir: PathBuf) -> Result<Self> {
        let file = config_dir.join("config.toml");
        let file_config: FileConfig = if file.exists() {
            let text = fs::read_to_string(&file)
                .with_context(|| format!("failed to read {}", file.display()))?;
            toml::from_str(&text).with_context(|| format!("failed to parse {}", file.display()))?
        } else {
            FileConfig::default()
        };

        let offline = env::var("FERRUM_OFFLINE")
            .is_ok_and(|value| value == "1" || value.eq_ignore_ascii_case("true"));
        let providers = file_config.providers;
        let models = file_config.models;
        let mut provider_name = if offline {
            "fake".to_string()
        } else {
            file_config.provider.unwrap_or_else(|| "fake".to_string())
        };

        let selected_model = file_config
            .model
            .or_else(|| {
                providers
                    .get(&provider_name)
                    .and_then(|definition| definition.default_model.clone())
            })
            .unwrap_or_else(|| "fake".to_string());
        if let Some(model_provider) = models
            .get(&selected_model)
            .and_then(|definition| definition.provider.as_ref())
        {
            if offline && model_provider != "fake" {
                anyhow::bail!(
                    "model {selected_model} requires provider {model_provider}, but FERRUM_OFFLINE is set"
                );
            }
            provider_name = model_provider.clone();
        }
        let provider = resolve_provider(&provider_name, &providers, &config_dir)?;
        let provider_model = provider_model_for(&selected_model, &models);
        let max_context_tokens = models
            .get(&selected_model)
            .and_then(|definition| definition.max_context_tokens)
            .or(file_config.max_context_tokens)
            .unwrap_or(256_000);
        let tools_config = file_config.tools.unwrap_or_default();
        let tools_allow = tools_config
            .allow
            .map(validate_tool_name_list)
            .transpose()?;
        let tools_deny = validate_tool_name_list(tools_config.deny)?;

        let colors = ColorPalette::load(&config_dir)?;

        crate::ui_colors::seed_palettes(&config_dir);

        Ok(Self {
            config_dir,
            data_dir,
            model: selected_model,
            provider_model,
            provider_name,
            provider,
            providers,
            models,
            offline,
            max_context_tokens,
            base_max_context_tokens: file_config.max_context_tokens.unwrap_or(256_000),
            max_tool_rounds: env::var("FERRUM_MAX_TOOL_ROUNDS")
                .ok()
                .and_then(|value| value.parse::<usize>().ok())
                .or(file_config.max_tool_rounds)
                .unwrap_or(0)
                .min(256),
            thinking: file_config
                .thinking
                .as_deref()
                .map(ThinkingLevel::parse)
                .transpose()?
                .unwrap_or(ThinkingLevel::Off),
            mcp_enabled: file_config.mcp_enabled.unwrap_or(true),
            mcp_server_allow: None,
            color_mode: file_config
                .color
                .as_deref()
                .map(ColorMode::parse)
                .transpose()?
                .unwrap_or(ColorMode::Auto),
            colors,
            diff_mode: file_config
                .diff_mode
                .as_deref()
                .map(DiffMode::parse)
                .transpose()?
                .unwrap_or(DiffMode::Unified),
            safety: file_config
                .safety
                .as_deref()
                .map(SafetyLevel::parse)
                .transpose()?
                .unwrap_or(SafetyLevel::Medium),
            tools_allow,
            tools_deny,
            tool_selection: None,
            mcp_servers: file_config.mcp.map(|mcp| mcp.servers).unwrap_or_default(),
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub fn apply_cli_overrides(
        &mut self,
        provider: Option<&str>,
        model: Option<&str>,
        thinking: Option<&str>,
        safety: Option<&str>,
        mcp_enabled: Option<bool>,
        mcp_server_allow: Option<Vec<String>>,
        tools: Option<ToolSelection>,
    ) -> Result<()> {
        if let Some(provider) = provider {
            self.set_provider(provider)?;
        }
        if let Some(model) = model {
            self.set_model(model)?;
        }
        if let Some(thinking) = thinking {
            self.thinking = ThinkingLevel::parse(thinking)?;
        }
        if let Some(safety) = safety {
            self.safety = SafetyLevel::parse(safety)?;
        }
        if let Some(mcp_enabled) = mcp_enabled {
            self.mcp_enabled = mcp_enabled;
        }
        if let Some(mcp_server_allow) = mcp_server_allow {
            self.mcp_enabled = true;
            self.set_mcp_server_allow(mcp_server_allow)?;
        }
        if let Some(tools) = tools {
            self.tool_selection = Some(match tools {
                ToolSelection::None => ToolSelection::None,
                ToolSelection::List(names) => ToolSelection::List(validate_tool_name_list(names)?),
            });
        }
        Ok(())
    }

    pub fn set_provider(&mut self, provider: &str) -> Result<()> {
        if self.offline && provider != "fake" {
            anyhow::bail!("cannot override provider to {provider} while FERRUM_OFFLINE is set");
        }
        self.provider = resolve_provider(provider, &self.providers, &self.config_dir)?;
        self.provider_name = provider.to_string();
        if let Some(default_model) = self
            .providers
            .get(provider)
            .and_then(|definition| definition.default_model.clone())
        {
            self.apply_model_name(default_model)?;
        }
        Ok(())
    }

    pub fn set_model(&mut self, model: &str) -> Result<()> {
        self.apply_model_name(model.to_string())
    }

    fn apply_model_name(&mut self, model: String) -> Result<()> {
        if let Some(model_provider) = self
            .models
            .get(&model)
            .and_then(|definition| definition.provider.clone())
        {
            self.set_provider(&model_provider)?;
        }
        self.provider_model = provider_model_for(&model, &self.models);
        self.max_context_tokens = self
            .models
            .get(&model)
            .and_then(|definition| definition.max_context_tokens)
            .unwrap_or(self.base_max_context_tokens);
        self.model = model;
        Ok(())
    }

    pub fn sessions_dir(&self) -> PathBuf {
        self.data_dir.join("sessions")
    }

    pub fn history_path(&self) -> PathBuf {
        self.data_dir.join("history.txt")
    }

    pub fn auth_path(&self) -> PathBuf {
        self.config_dir.join("auth.json")
    }

    fn set_mcp_server_allow(&mut self, servers: Vec<String>) -> Result<()> {
        let servers = validate_mcp_server_name_list(servers)?;
        let configured = self
            .mcp_servers
            .iter()
            .map(|server| server.name.as_str())
            .collect::<BTreeSet<_>>();
        for server in &servers {
            if !configured.contains(server.as_str()) {
                anyhow::bail!("unknown MCP server requested by --mcp: {server}");
            }
        }
        self.mcp_server_allow = Some(servers);
        Ok(())
    }
}

fn validate_mcp_server_name_list(values: Vec<String>) -> Result<Vec<String>> {
    let mut seen = BTreeSet::new();
    let mut normalized = Vec::new();
    for raw in values {
        let name = raw.trim();
        if name.is_empty() {
            anyhow::bail!("MCP server lists must not contain empty entries");
        }
        if !seen.insert(name.to_string()) {
            anyhow::bail!("duplicate MCP server in --mcp list: {name}");
        }
        normalized.push(name.to_string());
    }
    Ok(normalized)
}

fn provider_model_for(model: &str, models: &BTreeMap<String, ModelDefinition>) -> String {
    models
        .get(model)
        .and_then(|definition| definition.actual_model.clone())
        .unwrap_or_else(|| model.to_string())
}

fn validate_tool_name_list(values: Vec<String>) -> Result<Vec<String>> {
    let mut seen = BTreeSet::new();
    let mut normalized = Vec::new();
    for raw in values {
        let name = raw.trim();
        if name.is_empty() {
            anyhow::bail!("tool lists must not contain empty entries");
        }
        if !seen.insert(name.to_string()) {
            anyhow::bail!("duplicate tool name in tool list: {name}");
        }
        normalized.push(name.to_string());
    }
    Ok(normalized)
}

fn default_true() -> bool {
    true
}

fn resolve_provider(
    name: &str,
    providers: &BTreeMap<String, ProviderDefinition>,
    config_dir: &std::path::Path,
) -> Result<ProviderConfig> {
    if let Some(definition) = providers.get(name) {
        return provider_from_definition(name, definition, config_dir);
    }
    legacy_provider_from_name(name, config_dir)
}

fn provider_from_definition(
    name: &str,
    definition: &ProviderDefinition,
    config_dir: &std::path::Path,
) -> Result<ProviderConfig> {
    match definition.kind.as_str() {
        "fake" => Ok(ProviderConfig::Fake),
        "openai-compatible" => Ok(ProviderConfig::OpenAiCompat {
            api_key_env: definition.api_key_env.clone(),
            base_url: definition
                .base_url
                .clone()
                .with_context(|| format!("providers.{name}.base_url is required"))?,
            streaming: definition.streaming.unwrap_or(true),
            stream_usage: definition.stream_usage.unwrap_or(true),
        }),
        "openai-codex" => Ok(ProviderConfig::OpenAiCodex {
            base_url: definition
                .base_url
                .clone()
                .unwrap_or_else(|| "https://chatgpt.com/backend-api".to_string()),
            auth_path: config_dir.join("auth.json"),
        }),
        other => anyhow::bail!("unsupported provider type for {name}: {other}"),
    }
}

fn legacy_provider_from_name(name: &str, config_dir: &std::path::Path) -> Result<ProviderConfig> {
    match name {
        "fake" => Ok(ProviderConfig::Fake),
        "openai" | "openai-compatible" => Ok(ProviderConfig::OpenAiCompat {
            api_key_env: Some("OPENAI_API_KEY".to_string()),
            base_url: env::var("OPENAI_BASE_URL")
                .unwrap_or_else(|_| "https://api.openai.com/v1".to_string()),
            streaming: true,
            stream_usage: true,
        }),
        "openai-codex" => Ok(ProviderConfig::OpenAiCodex {
            base_url: env::var("OPENAI_CODEX_BASE_URL")
                .unwrap_or_else(|_| "https://chatgpt.com/backend-api".to_string()),
            auth_path: config_dir.join("auth.json"),
        }),
        other => anyhow::bail!("unsupported provider: {other}"),
    }
}

fn home_dir() -> Result<PathBuf> {
    env::var_os("HOME")
        .map(PathBuf::from)
        .context("HOME is not set")
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn mcp_enabled_defaults_true() {
        let dir = TempDir::new().unwrap();
        let config = Config::load_from_dir(dir.path().to_path_buf()).unwrap();
        assert!(config.mcp_enabled);
    }

    #[test]
    fn mcp_enabled_can_be_disabled_in_config() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("config.toml"), "mcp_enabled = false\n").unwrap();
        let config = Config::load_from_dir(dir.path().to_path_buf()).unwrap();
        assert!(!config.mcp_enabled);
    }

    #[test]
    fn cli_mcp_override_sets_runtime_config() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("config.toml"), "mcp_enabled = false\n").unwrap();
        let mut config = Config::load_from_dir(dir.path().to_path_buf()).unwrap();
        assert!(!config.mcp_enabled);
        config
            .apply_cli_overrides(None, None, None, None, Some(true), None, None)
            .unwrap();
        assert!(config.mcp_enabled);
        config
            .apply_cli_overrides(None, None, None, None, Some(false), None, None)
            .unwrap();
        assert!(!config.mcp_enabled);
    }

    #[test]
    fn parses_safety_config_and_cli_override() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("config.toml"), "safety = \"low\"\n").unwrap();
        let mut config = Config::load_from_dir(dir.path().to_path_buf()).unwrap();
        assert_eq!(config.safety, SafetyLevel::Low);

        config
            .apply_cli_overrides(None, None, None, Some("high"), None, None, None)
            .unwrap();
        assert_eq!(config.safety, SafetyLevel::High);
    }

    #[test]
    fn parses_tools_config_and_cli_selection() {
        let dir = TempDir::new().unwrap();
        fs::write(
            dir.path().join("config.toml"),
            r#"
[tools]
allow = ["read", "grep"]
deny = ["bash"]
"#,
        )
        .unwrap();
        let mut config = Config::load_from_dir(dir.path().to_path_buf()).unwrap();
        assert_eq!(
            config.tools_allow,
            Some(vec!["read".to_string(), "grep".to_string()])
        );
        assert_eq!(config.tools_deny, vec!["bash".to_string()]);

        config
            .apply_cli_overrides(
                None,
                None,
                None,
                None,
                None,
                None,
                Some(ToolSelection::List(vec![
                    "read".to_string(),
                    "grep".to_string(),
                ])),
            )
            .unwrap();
        assert_eq!(
            config.tool_selection,
            Some(ToolSelection::List(vec![
                "read".to_string(),
                "grep".to_string()
            ]))
        );
    }

    #[test]
    fn parses_no_tools_cli_selection() {
        let dir = TempDir::new().unwrap();
        let mut config = Config::load_from_dir(dir.path().to_path_buf()).unwrap();
        config
            .apply_cli_overrides(
                None,
                None,
                None,
                None,
                None,
                None,
                Some(ToolSelection::None),
            )
            .unwrap();
        assert_eq!(config.tool_selection, Some(ToolSelection::None));
    }

    #[test]
    fn cli_mcp_server_allow_filters_configured_servers() {
        let dir = TempDir::new().unwrap();
        fs::write(
            dir.path().join("config.toml"),
            r#"
[[mcp.servers]]
name = "chrome-devtools"
command = "node"

[[mcp.servers]]
name = "web-search"
command = "node"
"#,
        )
        .unwrap();
        let mut config = Config::load_from_dir(dir.path().to_path_buf()).unwrap();
        config
            .apply_cli_overrides(
                None,
                None,
                None,
                None,
                Some(true),
                Some(vec!["web-search".to_string()]),
                None,
            )
            .unwrap();
        assert!(config.mcp_enabled);
        assert_eq!(
            config.mcp_server_allow,
            Some(vec!["web-search".to_string()])
        );
    }

    #[test]
    fn cli_mcp_server_allow_rejects_unknown_server() {
        let dir = TempDir::new().unwrap();
        let mut config = Config::load_from_dir(dir.path().to_path_buf()).unwrap();
        let error = config
            .apply_cli_overrides(
                None,
                None,
                None,
                None,
                Some(true),
                Some(vec!["missing".to_string()]),
                None,
            )
            .unwrap_err();
        assert!(error.to_string().contains("unknown MCP server"));
    }

    #[test]
    fn uses_provider_default_model_when_model_is_missing() {
        let dir = TempDir::new().unwrap();
        fs::write(
            dir.path().join("config.toml"),
            r#"
provider = "llama-local"

[providers.llama-local]
type = "openai-compatible"
base_url = "http://localhost:8080/v1"
api_key_env = "LLAMA_API_KEY"
default_model = "gemma-4-E4B-it-Q8_0"
"#,
        )
        .unwrap();
        let config = Config::load_from_dir(dir.path().to_path_buf()).unwrap();
        assert_eq!(config.provider_name, "llama-local");
        assert_eq!(config.model, "gemma-4-E4B-it-Q8_0");
        assert_eq!(config.provider_model, "gemma-4-E4B-it-Q8_0");
    }

    #[test]
    fn top_level_model_overrides_provider_default_model() {
        let dir = TempDir::new().unwrap();
        fs::write(
            dir.path().join("config.toml"),
            r#"
provider = "llama-local"
model = "explicit-model"

[providers.llama-local]
type = "openai-compatible"
base_url = "http://localhost:8080/v1"
api_key_env = "LLAMA_API_KEY"
default_model = "default-model"
"#,
        )
        .unwrap();
        let config = Config::load_from_dir(dir.path().to_path_buf()).unwrap();
        assert_eq!(config.model, "explicit-model");
        assert_eq!(config.provider_model, "explicit-model");
    }

    #[test]
    fn model_definition_overrides_context_and_actual_model() {
        let dir = TempDir::new().unwrap();
        fs::write(
            dir.path().join("config.toml"),
            r#"
provider = "openai-codex"
model = "gpt-test-small"
max_context_tokens = 256000

[providers.openai-codex]
type = "openai-codex"
default_model = "gpt-5.5"

[models.gpt-test-small]
actual_model = "gpt-5.5"
max_context_tokens = 400
"#,
        )
        .unwrap();
        let config = Config::load_from_dir(dir.path().to_path_buf()).unwrap();
        assert_eq!(config.provider_name, "openai-codex");
        assert_eq!(config.model, "gpt-test-small");
        assert_eq!(config.provider_model, "gpt-5.5");
        assert_eq!(config.max_context_tokens, 400);
    }

    #[test]
    fn model_definition_can_select_provider() {
        let dir = TempDir::new().unwrap();
        fs::write(
            dir.path().join("config.toml"),
            r#"
provider = "fake"
model = "mini"

[providers.minimax]
type = "openai-compatible"
base_url = "https://api.minimax.io/v1"
api_key_env = "MINIMAX_API_KEY"
default_model = "MiniMax-M2"

[models.mini]
provider = "minimax"
actual_model = "MiniMax-M2"
max_context_tokens = 100000
"#,
        )
        .unwrap();
        let config = Config::load_from_dir(dir.path().to_path_buf()).unwrap();
        assert_eq!(config.provider_name, "minimax");
        assert_eq!(config.model, "mini");
        assert_eq!(config.provider_model, "MiniMax-M2");
        assert_eq!(config.max_context_tokens, 100000);
    }

    #[test]
    fn model_switch_resets_context_budget_to_base_when_no_override() {
        let dir = TempDir::new().unwrap();
        fs::write(
            dir.path().join("config.toml"),
            r#"
provider = "openai-codex"
model = "small"
max_context_tokens = 1000

[providers.openai-codex]
type = "openai-codex"
default_model = "plain"

[models.small]
actual_model = "gpt-5.5"
max_context_tokens = 400
"#,
        )
        .unwrap();
        let mut config = Config::load_from_dir(dir.path().to_path_buf()).unwrap();
        assert_eq!(config.max_context_tokens, 400);
        config.set_model("plain").unwrap();
        assert_eq!(config.max_context_tokens, 1000);
    }

    #[test]
    fn quoted_model_names_can_contain_dots_and_hyphens() {
        let dir = TempDir::new().unwrap();
        fs::write(
            dir.path().join("config.toml"),
            r#"
provider = "openai-codex"
model = "gpt-5.5-small-context"

[providers.openai-codex]
type = "openai-codex"
default_model = "gpt-5.5"

[models."gpt-5.5-small-context"]
actual_model = "gpt-5.5"
max_context_tokens = 400
"#,
        )
        .unwrap();
        let config = Config::load_from_dir(dir.path().to_path_buf()).unwrap();
        assert_eq!(config.model, "gpt-5.5-small-context");
        assert_eq!(config.provider_model, "gpt-5.5");
        assert_eq!(config.max_context_tokens, 400);
    }
}
