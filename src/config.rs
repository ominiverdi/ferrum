use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::{env, fs, path::PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub config_dir: PathBuf,
    pub model: String,
    pub provider: ProviderConfig,
    pub offline: bool,
    pub max_context_tokens: usize,
    pub thinking: ThinkingLevel,
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

#[derive(Debug, Deserialize, Default)]
struct FileConfig {
    model: Option<String>,
    provider: Option<String>,
    max_context_tokens: Option<usize>,
    thinking: Option<String>,
    mcp: Option<FileMcpConfig>,
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
        let provider_name = if offline {
            "fake".to_string()
        } else {
            file_config.provider.unwrap_or_else(|| "fake".to_string())
        };

        let provider = provider_from_name(&provider_name, &config_dir)?;

        Ok(Self {
            config_dir,
            model: file_config.model.unwrap_or_else(|| "fake".to_string()),
            provider,
            offline,
            max_context_tokens: file_config.max_context_tokens.unwrap_or(256_000),
            thinking: file_config
                .thinking
                .as_deref()
                .map(ThinkingLevel::parse)
                .transpose()?
                .unwrap_or(ThinkingLevel::Off),
            mcp_servers: file_config.mcp.map(|mcp| mcp.servers).unwrap_or_default(),
        })
    }

    pub fn apply_cli_overrides(
        &mut self,
        provider: Option<&str>,
        model: Option<&str>,
        thinking: Option<&str>,
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
        Ok(())
    }

    pub fn set_provider(&mut self, provider: &str) -> Result<()> {
        if self.offline && provider != "fake" {
            anyhow::bail!("cannot override provider to {provider} while FERRUM_OFFLINE is set");
        }
        self.provider = provider_from_name(provider, &self.config_dir)?;
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

fn provider_from_name(name: &str, config_dir: &std::path::Path) -> Result<ProviderConfig> {
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
        "llama" => Ok(ProviderConfig::OpenAiCompat {
            api_key_env: "LLAMA_API_KEY".to_string(),
            base_url: env::var("LLAMA_BASE_URL")
                .unwrap_or_else(|_| "http://localhost:8080/v1".to_string()),
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
