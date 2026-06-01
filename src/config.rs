use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, env, fs, path::PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub config_dir: PathBuf,
    pub model: String,
    pub provider_name: String,
    pub provider: ProviderConfig,
    pub providers: BTreeMap<String, ProviderDefinition>,
    pub offline: bool,
    pub max_context_tokens: usize,
    pub max_tool_rounds: usize,
    pub thinking: ThinkingLevel,
    pub mcp_enabled: bool,
    pub diff_mode: DiffMode,
    pub mcp_servers: Vec<McpServerConfig>,
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
pub enum DiffMode {
    Unified,
    Compact,
    Full,
    Words,
    SideBySide,
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
        api_key_env: String,
        base_url: String,
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
}

#[derive(Debug, Deserialize, Default)]
struct FileConfig {
    model: Option<String>,
    provider: Option<String>,
    max_context_tokens: Option<usize>,
    max_tool_rounds: Option<usize>,
    thinking: Option<String>,
    mcp_enabled: Option<bool>,
    diff_mode: Option<String>,
    mcp: Option<FileMcpConfig>,
    #[serde(default)]
    providers: BTreeMap<String, ProviderDefinition>,
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
        Self::load_from_dir(config_dir)
    }

    fn load_from_dir(config_dir: PathBuf) -> Result<Self> {
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
        let provider_name = if offline {
            "fake".to_string()
        } else {
            file_config.provider.unwrap_or_else(|| "fake".to_string())
        };

        let provider = resolve_provider(&provider_name, &providers, &config_dir)?;
        let model = file_config
            .model
            .or_else(|| {
                providers
                    .get(&provider_name)
                    .and_then(|definition| definition.default_model.clone())
            })
            .unwrap_or_else(|| "fake".to_string());

        Ok(Self {
            config_dir,
            model,
            provider_name,
            provider,
            providers,
            offline,
            max_context_tokens: file_config.max_context_tokens.unwrap_or(256_000),
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
            diff_mode: file_config
                .diff_mode
                .as_deref()
                .map(DiffMode::parse)
                .transpose()?
                .unwrap_or(DiffMode::Unified),
            mcp_servers: file_config.mcp.map(|mcp| mcp.servers).unwrap_or_default(),
        })
    }

    pub fn apply_cli_overrides(
        &mut self,
        provider: Option<&str>,
        model: Option<&str>,
        thinking: Option<&str>,
        mcp_enabled: Option<bool>,
    ) -> Result<()> {
        if let Some(provider) = provider {
            self.set_provider(provider)?;
        }
        if let Some(model) = model {
            self.model = model.to_string();
        }
        if let Some(thinking) = thinking {
            self.thinking = ThinkingLevel::parse(thinking)?;
        }
        if let Some(mcp_enabled) = mcp_enabled {
            self.mcp_enabled = mcp_enabled;
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
            .and_then(|definition| definition.default_model.as_ref())
        {
            self.model = default_model.clone();
        }
        Ok(())
    }

    pub fn sessions_dir(&self) -> PathBuf {
        self.config_dir.join("sessions")
    }

    pub fn auth_path(&self) -> PathBuf {
        self.config_dir.join("auth.json")
    }
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
            api_key_env: definition
                .api_key_env
                .clone()
                .with_context(|| format!("providers.{name}.api_key_env is required"))?,
            base_url: definition
                .base_url
                .clone()
                .with_context(|| format!("providers.{name}.base_url is required"))?,
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
            api_key_env: "OPENAI_API_KEY".to_string(),
            base_url: env::var("OPENAI_BASE_URL")
                .unwrap_or_else(|_| "https://api.openai.com/v1".to_string()),
        }),
        "minimax" => Ok(ProviderConfig::OpenAiCompat {
            api_key_env: "MINIMAX_API_KEY".to_string(),
            base_url: env::var("MINIMAX_BASE_URL")
                .unwrap_or_else(|_| "https://api.minimax.io/v1".to_string()),
        }),
        "opencode-go" => Ok(ProviderConfig::OpenAiCompat {
            api_key_env: env::var("OPENCODE_GO_API_KEY_ENV")
                .unwrap_or_else(|_| "OPENCODE_API_KEY".to_string()),
            base_url: env::var("OPENCODE_GO_BASE_URL")
                .unwrap_or_else(|_| "https://opencode.ai/zen/go/v1".to_string()),
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
            .apply_cli_overrides(None, None, None, Some(true))
            .unwrap();
        assert!(config.mcp_enabled);
        config
            .apply_cli_overrides(None, None, None, Some(false))
            .unwrap();
        assert!(!config.mcp_enabled);
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
    }
}
