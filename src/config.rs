use crate::ui_colors::ColorPalette;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    collections::BTreeSet,
    env, fs,
    path::{Path, PathBuf},
};

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
    pub readable_roots: Option<Vec<PathBuf>>,
    pub writable_roots: Vec<PathBuf>,
    pub allow_external_global_skill_symlinks: bool,
    pub inherit_global_skills: bool,
    pub skills_allow: Option<Vec<String>>,
    pub skills_deny: Vec<String>,
    pub tool_selection: Option<ToolSelection>,
    pub mcp_servers: Vec<McpServerConfig>,
    pub mcp_server_deny: Vec<String>,
    pub project_config_path: Option<PathBuf>,
    pub project_safety_floor: Option<SafetyLevel>,
    pub project_mcp_disabled: bool,
    pub project_mcp_allow: Option<Vec<String>>,
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
    #[serde(default)]
    pub env: Vec<String>,
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
    #[serde(default)]
    pub allow_insecure_http: bool,
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
    skills: Option<FileSkillsConfig>,
    tools: Option<FileToolsConfig>,
    mcp: Option<FileMcpConfig>,
    #[serde(default)]
    providers: BTreeMap<String, ProviderDefinition>,
    #[serde(default)]
    models: BTreeMap<String, ModelDefinition>,
}

#[derive(Debug, Deserialize, Default)]
struct FileSkillsConfig {
    #[serde(default)]
    allow_external_global_symlinks: bool,
}

#[derive(Debug, Deserialize, Default)]
struct FileToolsConfig {
    allow: Option<Vec<String>>,
    #[serde(default)]
    deny: Vec<String>,
    readable_roots: Option<Vec<PathBuf>>,
    #[serde(default = "default_writable_roots")]
    writable_roots: Vec<PathBuf>,
}

fn default_writable_roots() -> Vec<PathBuf> {
    vec![PathBuf::from(".")]
}

#[derive(Debug, Deserialize, Default)]
struct FileMcpConfig {
    #[serde(default)]
    servers: Vec<McpServerConfig>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct ProjectConfig {
    safety: Option<String>,
    max_tool_rounds: Option<usize>,
    tools: Option<ProjectToolsConfig>,
    skills: Option<ProjectSkillsConfig>,
    mcp: Option<ProjectMcpConfig>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct ProjectToolsConfig {
    allow: Option<Vec<String>>,
    #[serde(default)]
    deny: Vec<String>,
    readable_roots: Option<Vec<PathBuf>>,
    writable_roots: Option<Vec<PathBuf>>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProjectSkillsConfig {
    #[serde(default = "default_true")]
    inherit_global: bool,
    allow: Option<Vec<String>>,
    #[serde(default)]
    deny: Vec<String>,
}

impl Default for ProjectSkillsConfig {
    fn default() -> Self {
        Self {
            inherit_global: true,
            allow: None,
            deny: Vec::new(),
        }
    }
}

#[derive(Debug, Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct ProjectMcpConfig {
    enabled: Option<bool>,
    allow: Option<Vec<String>>,
    #[serde(default)]
    deny: Vec<String>,
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
        let skills_config = file_config.skills.unwrap_or_default();
        let tools_config = file_config.tools.unwrap_or_default();
        let tools_allow = tools_config
            .allow
            .map(validate_tool_name_list)
            .transpose()?;
        let tools_deny = validate_tool_name_list(tools_config.deny)?;
        let readable_roots = tools_config
            .readable_roots
            .map(|roots| validate_root_list(roots, "readable_roots"))
            .transpose()?;
        let writable_roots = if tools_config.writable_roots.is_empty() {
            default_writable_roots()
        } else {
            validate_root_list(tools_config.writable_roots, "writable_roots")?
        };

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
            readable_roots,
            writable_roots,
            allow_external_global_skill_symlinks: skills_config.allow_external_global_symlinks,
            inherit_global_skills: true,
            skills_allow: None,
            skills_deny: Vec::new(),
            tool_selection: None,
            mcp_servers: file_config.mcp.map(|mcp| mcp.servers).unwrap_or_default(),
            mcp_server_deny: Vec::new(),
            project_config_path: None,
            project_safety_floor: None,
            project_mcp_disabled: false,
            project_mcp_allow: None,
        })
    }

    pub fn for_cwd(&self, cwd: &Path) -> Result<Self> {
        let mut candidate = self.clone();
        candidate.apply_project_config(cwd)?;
        Ok(candidate)
    }

    pub fn apply_project_config(&mut self, cwd: &Path) -> Result<()> {
        let cwd = cwd
            .canonicalize()
            .with_context(|| format!("failed to resolve project cwd {}", cwd.display()))?;
        if !cwd.is_dir() {
            anyhow::bail!("project cwd is not a directory: {}", cwd.display());
        }
        let Some(path) = find_project_config(&cwd) else {
            return Ok(());
        };
        let metadata =
            fs::metadata(&path).with_context(|| format!("failed to inspect {}", path.display()))?;
        const MAX_PROJECT_CONFIG_BYTES: u64 = 1024 * 1024;
        if metadata.len() > MAX_PROJECT_CONFIG_BYTES {
            anyhow::bail!(
                "project config exceeds {MAX_PROJECT_CONFIG_BYTES} bytes: {}",
                path.display()
            );
        }
        let text = fs::read_to_string(&path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        let project: ProjectConfig =
            toml::from_str(&text).with_context(|| format!("failed to parse {}", path.display()))?;
        let mut candidate = self.clone();
        candidate.apply_project_policy(project, &cwd, &path)?;
        *self = candidate;
        Ok(())
    }

    fn apply_project_policy(
        &mut self,
        project: ProjectConfig,
        cwd: &Path,
        path: &Path,
    ) -> Result<()> {
        if let Some(safety) = project.safety.as_deref() {
            let safety = SafetyLevel::parse(safety)?;
            self.safety = stricter_safety(self.safety, safety);
            self.project_safety_floor = Some(self.safety);
        }
        if let Some(limit) = project.max_tool_rounds {
            if limit == 0 {
                anyhow::bail!("project max_tool_rounds must be greater than zero");
            }
            self.max_tool_rounds = narrower_round_limit(self.max_tool_rounds, limit);
        }
        if let Some(tools) = project.tools {
            if let Some(allow) = tools.allow {
                let allow = validate_tool_name_list(allow)?;
                self.tools_allow = intersect_optional_allow(self.tools_allow.take(), allow);
            }
            merge_unique(&mut self.tools_deny, validate_tool_name_list(tools.deny)?);
            if let Some(roots) = tools.readable_roots {
                let roots = validate_root_list(roots, "readable_roots")?;
                if let Some(global) = &self.readable_roots {
                    ensure_roots_narrow(global, &roots, cwd, "readable_roots")?;
                }
                self.readable_roots = Some(roots);
            }
            if let Some(roots) = tools.writable_roots {
                let roots = validate_root_list(roots, "writable_roots")?;
                ensure_roots_narrow(&self.writable_roots, &roots, cwd, "writable_roots")?;
                self.writable_roots = roots;
            }
        }
        if let Some(skills) = project.skills {
            self.inherit_global_skills &= skills.inherit_global;
            if let Some(allow) = skills.allow {
                let allow = validate_named_list(allow, "skill")?;
                self.skills_allow = intersect_optional_allow(self.skills_allow.take(), allow);
            }
            merge_unique(
                &mut self.skills_deny,
                validate_named_list(skills.deny, "skill")?,
            );
        }
        if let Some(mcp) = project.mcp {
            if mcp.enabled == Some(false) {
                self.mcp_enabled = false;
                self.project_mcp_disabled = true;
            }
            if let Some(allow) = mcp.allow {
                let allow = validate_mcp_server_name_list(allow)?;
                self.project_mcp_allow = Some(allow.clone());
                let configured_allow = allow
                    .into_iter()
                    .filter(|name| self.mcp_servers.iter().any(|server| server.name == *name))
                    .collect();
                self.mcp_server_allow =
                    intersect_optional_allow(self.mcp_server_allow.take(), configured_allow);
            }
            let deny = validate_mcp_server_name_list(mcp.deny)?;
            merge_unique(&mut self.mcp_server_deny, deny);
        }
        self.project_config_path = Some(path.to_path_buf());
        Ok(())
    }

    pub fn enforce_project_constraints(&mut self) {
        if let Some(floor) = self.project_safety_floor {
            self.safety = stricter_safety(self.safety, floor);
        }
        if self.project_mcp_disabled {
            self.mcp_enabled = false;
        }
    }

    pub fn constrained_safety(&self, requested: SafetyLevel) -> SafetyLevel {
        self.project_safety_floor
            .map_or(requested, |floor| stricter_safety(requested, floor))
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
        let mut candidate = self.clone();
        if let Some(provider) = provider {
            candidate.set_provider(provider)?;
        }
        if let Some(model) = model {
            candidate.set_model(model)?;
        }
        if let Some(thinking) = thinking {
            candidate.thinking = ThinkingLevel::parse(thinking)?;
        }
        if let Some(safety) = safety {
            candidate.safety = SafetyLevel::parse(safety)?;
        }
        if let Some(mcp_enabled) = mcp_enabled {
            candidate.mcp_enabled = mcp_enabled;
        }
        if let Some(mcp_server_allow) = mcp_server_allow {
            candidate.mcp_enabled = true;
            candidate.set_mcp_server_allow(mcp_server_allow)?;
        }
        if let Some(tools) = tools {
            candidate.tool_selection = Some(match tools {
                ToolSelection::None => ToolSelection::None,
                ToolSelection::List(names) => ToolSelection::List(validate_tool_name_list(names)?),
            });
        }
        candidate.enforce_project_constraints();
        *self = candidate;
        Ok(())
    }

    pub fn set_provider(&mut self, provider: &str) -> Result<()> {
        let mut candidate = self.clone();
        let mut visited_providers = BTreeSet::new();
        let mut visited_models = BTreeSet::new();
        candidate.set_provider_inner(provider, &mut visited_providers, &mut visited_models)?;
        *self = candidate;
        Ok(())
    }

    fn set_provider_inner(
        &mut self,
        provider: &str,
        visited_providers: &mut BTreeSet<String>,
        visited_models: &mut BTreeSet<String>,
    ) -> Result<()> {
        if !visited_providers.insert(provider.to_string()) {
            anyhow::bail!("provider/model configuration cycle involving provider `{provider}`");
        }
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
            self.apply_model_name_inner(default_model, visited_providers, visited_models)?;
        }
        Ok(())
    }

    pub fn set_model(&mut self, model: &str) -> Result<()> {
        let mut candidate = self.clone();
        let mut visited_providers = BTreeSet::new();
        let mut visited_models = BTreeSet::new();
        candidate.apply_model_name_inner(
            model.to_string(),
            &mut visited_providers,
            &mut visited_models,
        )?;
        *self = candidate;
        Ok(())
    }

    fn apply_model_name_inner(
        &mut self,
        model: String,
        visited_providers: &mut BTreeSet<String>,
        visited_models: &mut BTreeSet<String>,
    ) -> Result<()> {
        if !visited_models.insert(model.clone()) {
            anyhow::bail!("provider/model configuration cycle involving model `{model}`");
        }
        if let Some(model_provider) = self
            .models
            .get(&model)
            .and_then(|definition| definition.provider.clone())
        {
            self.set_provider_inner(&model_provider, visited_providers, visited_models)?;
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
        self.ensure_known_mcp_servers(&servers, "--mcp list")?;
        self.mcp_server_allow = intersect_optional_allow(self.mcp_server_allow.take(), servers);
        Ok(())
    }

    fn ensure_known_mcp_servers(&self, servers: &[String], source: &str) -> Result<()> {
        let configured = self
            .mcp_servers
            .iter()
            .map(|server| server.name.as_str())
            .collect::<BTreeSet<_>>();
        for server in servers {
            if !configured.contains(server.as_str()) {
                anyhow::bail!("unknown MCP server in {source}: {server}");
            }
        }
        Ok(())
    }
}

fn find_project_config(cwd: &Path) -> Option<PathBuf> {
    cwd.ancestors()
        .map(|dir| dir.join(".ferrum/config.toml"))
        .find(|path| path.is_file())
}

fn stricter_safety(left: SafetyLevel, right: SafetyLevel) -> SafetyLevel {
    use SafetyLevel::{High, Low, Medium};
    match (left, right) {
        (High, _) | (_, High) => High,
        (Medium, _) | (_, Medium) => Medium,
        (Low, Low) => Low,
    }
}

fn narrower_round_limit(current: usize, project: usize) -> usize {
    if current == 0 {
        project
    } else {
        current.min(project)
    }
}

fn intersect_optional_allow(
    current: Option<Vec<String>>,
    proposed: Vec<String>,
) -> Option<Vec<String>> {
    let Some(current) = current else {
        return Some(proposed);
    };
    let proposed = proposed.into_iter().collect::<BTreeSet<_>>();
    Some(
        current
            .into_iter()
            .filter(|name| proposed.contains(name))
            .collect(),
    )
}

fn merge_unique(target: &mut Vec<String>, values: Vec<String>) {
    let mut seen = target.iter().cloned().collect::<BTreeSet<_>>();
    for value in values {
        if seen.insert(value.clone()) {
            target.push(value);
        }
    }
}

fn validate_named_list(values: Vec<String>, kind: &str) -> Result<Vec<String>> {
    let mut seen = BTreeSet::new();
    let mut normalized = Vec::new();
    for raw in values {
        let name = raw.trim();
        if name.is_empty() {
            anyhow::bail!("{kind} lists must not contain empty entries");
        }
        if !seen.insert(name.to_string()) {
            anyhow::bail!("duplicate {kind} name in {kind} list: {name}");
        }
        normalized.push(name.to_string());
    }
    Ok(normalized)
}

fn validate_root_list(values: Vec<PathBuf>, key: &str) -> Result<Vec<PathBuf>> {
    if values.is_empty() {
        anyhow::bail!("{key} must not be empty");
    }
    let mut seen = BTreeSet::new();
    for value in &values {
        if value.as_os_str().is_empty() {
            anyhow::bail!("{key} must not contain empty paths");
        }
        if !seen.insert(value.clone()) {
            anyhow::bail!("{key} contains duplicate path: {}", value.display());
        }
    }
    Ok(values)
}

fn ensure_roots_narrow(
    current: &[PathBuf],
    proposed: &[PathBuf],
    cwd: &Path,
    key: &str,
) -> Result<()> {
    let current = crate::tools::write_policy::canonical_roots(cwd, current)?;
    let proposed_resolved = crate::tools::write_policy::canonical_roots(cwd, proposed)?;
    for (raw, resolved) in proposed.iter().zip(proposed_resolved) {
        if !current.iter().any(|root| resolved.starts_with(root)) {
            anyhow::bail!(
                "project {key} entry {} broadens the global roots",
                raw.display()
            );
        }
    }
    Ok(())
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
        "openai-compatible" => {
            let base_url = definition
                .base_url
                .clone()
                .with_context(|| format!("providers.{name}.base_url is required"))?;
            validate_provider_base_url(
                name,
                &base_url,
                definition.api_key_env.is_some(),
                definition.allow_insecure_http,
            )?;
            Ok(ProviderConfig::OpenAiCompat {
                api_key_env: definition.api_key_env.clone(),
                base_url,
                streaming: definition.streaming.unwrap_or(true),
                stream_usage: definition.stream_usage.unwrap_or(true),
            })
        }
        "openai-codex" => {
            let base_url = definition
                .base_url
                .clone()
                .unwrap_or_else(|| "https://chatgpt.com/backend-api".to_string());
            validate_provider_base_url(name, &base_url, true, definition.allow_insecure_http)?;
            Ok(ProviderConfig::OpenAiCodex {
                base_url,
                auth_path: config_dir.join("auth.json"),
            })
        }
        other => anyhow::bail!("unsupported provider type for {name}: {other}"),
    }
}

fn legacy_provider_from_name(name: &str, config_dir: &std::path::Path) -> Result<ProviderConfig> {
    match name {
        "fake" => Ok(ProviderConfig::Fake),
        "openai" | "openai-compatible" => {
            let base_url = env::var("OPENAI_BASE_URL")
                .unwrap_or_else(|_| "https://api.openai.com/v1".to_string());
            validate_provider_base_url(name, &base_url, true, false)?;
            Ok(ProviderConfig::OpenAiCompat {
                api_key_env: Some("OPENAI_API_KEY".to_string()),
                base_url,
                streaming: true,
                stream_usage: true,
            })
        }
        "openai-codex" => {
            let base_url = env::var("OPENAI_CODEX_BASE_URL")
                .unwrap_or_else(|_| "https://chatgpt.com/backend-api".to_string());
            validate_provider_base_url(name, &base_url, true, false)?;
            Ok(ProviderConfig::OpenAiCodex {
                base_url,
                auth_path: config_dir.join("auth.json"),
            })
        }
        other => anyhow::bail!("unsupported provider: {other}"),
    }
}

fn validate_provider_base_url(
    name: &str,
    base_url: &str,
    authenticated: bool,
    allow_insecure_http: bool,
) -> Result<()> {
    let url = url::Url::parse(base_url)
        .with_context(|| format!("provider {name} has an invalid base_url"))?;
    if !matches!(url.scheme(), "http" | "https") {
        anyhow::bail!("provider {name} base_url must use http or https");
    }
    if url.host().is_none() {
        anyhow::bail!("provider {name} base_url must include a host");
    }
    let authenticated = authenticated || !url.username().is_empty() || url.password().is_some();
    if url.scheme() == "http"
        && authenticated
        && !allow_insecure_http
        && !url_host_is_loopback(&url)
    {
        anyhow::bail!(
            "provider {name} would send credentials over non-loopback cleartext HTTP; use HTTPS or set allow_insecure_http = true explicitly"
        );
    }
    Ok(())
}

fn url_host_is_loopback(url: &url::Url) -> bool {
    match url.host() {
        Some(url::Host::Domain(host)) => host.eq_ignore_ascii_case("localhost"),
        Some(url::Host::Ipv4(address)) => address.is_loopback(),
        Some(url::Host::Ipv6(address)) => address.is_loopback(),
        None => false,
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
    fn project_config_applies_only_restrictive_runtime_policy() {
        let config_dir = TempDir::new().unwrap();
        fs::write(
            config_dir.path().join("config.toml"),
            r#"
safety = "low"
max_tool_rounds = 20

[tools]
allow = ["read", "grep", "bash"]
deny = ["write"]
readable_roots = ["."]
writable_roots = ["."]

[[mcp.servers]]
name = "time"
command = "true"

[[mcp.servers]]
name = "chrome"
command = "true"
"#,
        )
        .unwrap();
        let workspace = TempDir::new().unwrap();
        let cwd = workspace.path().join("threads/one");
        fs::create_dir_all(cwd.join("downloads")).unwrap();
        fs::create_dir_all(workspace.path().join(".ferrum")).unwrap();
        fs::write(
            workspace.path().join(".ferrum/config.toml"),
            r#"
safety = "high"
max_tool_rounds = 8

[tools]
allow = ["read", "grep"]
deny = ["bash", "edit"]
readable_roots = ["."]
writable_roots = ["downloads"]

[skills]
inherit_global = false
allow = ["bridge-help"]
deny = ["other"]

[mcp]
enabled = false
allow = ["time", "client-extra"]
deny = ["chrome"]
"#,
        )
        .unwrap();

        let config = Config::load_from_dir(config_dir.path().to_path_buf())
            .unwrap()
            .for_cwd(&cwd)
            .unwrap();

        assert_eq!(config.safety, SafetyLevel::High);
        assert_eq!(config.project_safety_floor, Some(SafetyLevel::High));
        assert_eq!(config.max_tool_rounds, 8);
        assert_eq!(config.tools_allow, Some(vec!["read".into(), "grep".into()]));
        assert_eq!(config.tools_deny, vec!["write", "bash", "edit"]);
        assert_eq!(config.readable_roots, Some(vec![PathBuf::from(".")]));
        assert_eq!(config.writable_roots, vec![PathBuf::from("downloads")]);
        assert!(!config.inherit_global_skills);
        assert_eq!(config.skills_allow, Some(vec!["bridge-help".into()]));
        assert_eq!(config.skills_deny, vec!["other"]);
        assert!(!config.mcp_enabled);
        assert!(config.project_mcp_disabled);
        assert_eq!(config.mcp_server_allow, Some(vec!["time".into()]));
        assert_eq!(
            config.project_mcp_allow,
            Some(vec!["time".into(), "client-extra".into()])
        );
        assert_eq!(config.mcp_server_deny, vec!["chrome"]);
        assert_eq!(
            config.project_config_path,
            Some(workspace.path().join(".ferrum/config.toml"))
        );
    }

    #[test]
    fn project_config_cannot_change_provider_or_broaden_roots() {
        let config_dir = TempDir::new().unwrap();
        fs::write(
            config_dir.path().join("config.toml"),
            "[tools]\nwritable_roots = [\"allowed\"]\n",
        )
        .unwrap();
        let workspace = TempDir::new().unwrap();
        let cwd = workspace.path().join("work");
        fs::create_dir_all(cwd.join("allowed")).unwrap();
        fs::create_dir_all(workspace.path().join(".ferrum")).unwrap();
        fs::write(
            workspace.path().join(".ferrum/config.toml"),
            "provider = \"fake\"\n",
        )
        .unwrap();
        let config = Config::load_from_dir(config_dir.path().to_path_buf()).unwrap();
        let error = config.for_cwd(&cwd).unwrap_err();
        assert!(format!("{error:#}").contains("unknown field `provider`"));

        fs::write(
            workspace.path().join(".ferrum/config.toml"),
            "[tools]\nwritable_roots = [\"..\"]\n",
        )
        .unwrap();
        let error = config.for_cwd(&cwd).unwrap_err();
        assert!(error.to_string().contains("broadens the global roots"));
    }

    #[test]
    fn cli_cannot_weaken_project_safety_or_reenable_project_disabled_mcp() {
        let config_dir = TempDir::new().unwrap();
        let workspace = TempDir::new().unwrap();
        fs::create_dir_all(workspace.path().join(".ferrum")).unwrap();
        fs::write(
            workspace.path().join(".ferrum/config.toml"),
            "safety = \"high\"\n[mcp]\nenabled = false\n",
        )
        .unwrap();
        let mut config = Config::load_from_dir(config_dir.path().to_path_buf())
            .unwrap()
            .for_cwd(workspace.path())
            .unwrap();

        config
            .apply_cli_overrides(None, None, None, Some("low"), Some(true), None, None)
            .unwrap();

        assert_eq!(config.safety, SafetyLevel::High);
        assert!(!config.mcp_enabled);
    }

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
    fn external_global_skill_symlinks_require_config_opt_in() {
        let dir = TempDir::new().unwrap();
        let default_config = Config::load_from_dir(dir.path().to_path_buf()).unwrap();
        assert!(!default_config.allow_external_global_skill_symlinks);

        fs::write(
            dir.path().join("config.toml"),
            "[skills]\nallow_external_global_symlinks = true\n",
        )
        .unwrap();
        let opted_in = Config::load_from_dir(dir.path().to_path_buf()).unwrap();
        assert!(opted_in.allow_external_global_skill_symlinks);
    }

    #[test]
    fn parses_safety_config_and_cli_override() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("config.toml"), "safety = \"low\"\n").unwrap();
        let mut config = Config::load_from_dir(dir.path().to_path_buf()).unwrap();
        assert_eq!(config.safety, SafetyLevel::Low);
        assert_eq!(config.writable_roots, vec![PathBuf::from(".")]);

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
writable_roots = [".", "/tmp/ferrum-output"]
"#,
        )
        .unwrap();
        let mut config = Config::load_from_dir(dir.path().to_path_buf()).unwrap();
        assert_eq!(
            config.tools_allow,
            Some(vec!["read".to_string(), "grep".to_string()])
        );
        assert_eq!(config.tools_deny, vec!["bash".to_string()]);
        assert_eq!(
            config.writable_roots,
            vec![PathBuf::from("."), PathBuf::from("/tmp/ferrum-output")]
        );

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
    fn rejects_authenticated_remote_cleartext_provider_by_default() {
        let definition = ProviderDefinition {
            kind: "openai-compatible".to_string(),
            base_url: Some("http://example.com/v1".to_string()),
            api_key_env: Some("EXAMPLE_API_KEY".to_string()),
            default_model: None,
            streaming: None,
            stream_usage: None,
            allow_insecure_http: false,
        };
        let error = provider_from_definition(
            "remote",
            &definition,
            std::path::Path::new("/tmp/ferrum-config-test"),
        )
        .unwrap_err();
        assert!(error.to_string().contains("cleartext HTTP"));
    }

    #[test]
    fn permits_explicit_or_loopback_authenticated_cleartext_provider() {
        let mut definition = ProviderDefinition {
            kind: "openai-compatible".to_string(),
            base_url: Some("http://127.0.0.1:8080/v1".to_string()),
            api_key_env: Some("EXAMPLE_API_KEY".to_string()),
            default_model: None,
            streaming: None,
            stream_usage: None,
            allow_insecure_http: false,
        };
        assert!(
            provider_from_definition(
                "local",
                &definition,
                std::path::Path::new("/tmp/ferrum-config-test"),
            )
            .is_ok()
        );

        definition.base_url = Some("http://example.com/v1".to_string());
        definition.allow_insecure_http = true;
        assert!(
            provider_from_definition(
                "remote",
                &definition,
                std::path::Path::new("/tmp/ferrum-config-test"),
            )
            .is_ok()
        );
    }

    #[test]
    fn permits_authless_remote_cleartext_provider() {
        validate_provider_base_url("authless", "http://example.com/v1", false, false).unwrap();
        assert!(
            validate_provider_base_url(
                "url-auth",
                "http://user:password@example.com/v1",
                false,
                false,
            )
            .is_err()
        );
    }

    #[test]
    fn mcp_server_environment_is_an_explicit_allowlist() {
        let dir = TempDir::new().unwrap();
        fs::write(
            dir.path().join("config.toml"),
            r#"
[[mcp.servers]]
name = "example"
command = "example-server"
env = ["PATH", "HOME"]
"#,
        )
        .unwrap();
        let config = Config::load_from_dir(dir.path().to_path_buf()).unwrap();
        assert_eq!(config.mcp_servers[0].env, vec!["PATH", "HOME"]);
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
    fn provider_default_model_self_cycle_errors() {
        let dir = TempDir::new().unwrap();
        fs::write(
            dir.path().join("config.toml"),
            r#"
provider = "fake"

[providers.loop]
type = "openai-compatible"
base_url = "http://localhost:8080/v1"
default_model = "loop-model"

[models.loop-model]
provider = "loop"
"#,
        )
        .unwrap();
        let mut config = Config::load_from_dir(dir.path().to_path_buf()).unwrap();
        let before = (
            config.provider_name.clone(),
            config.model.clone(),
            config.provider_model.clone(),
            config.max_context_tokens,
        );
        let error = config.set_provider("loop").unwrap_err();
        assert!(error.to_string().contains("configuration cycle"));
        assert_eq!(
            before,
            (
                config.provider_name.clone(),
                config.model.clone(),
                config.provider_model.clone(),
                config.max_context_tokens,
            )
        );
    }

    #[test]
    fn provider_model_cross_cycle_errors() {
        let dir = TempDir::new().unwrap();
        fs::write(
            dir.path().join("config.toml"),
            r#"
provider = "fake"

[providers.a]
type = "openai-compatible"
base_url = "http://localhost:8080/v1"
default_model = "model-b"

[providers.b]
type = "openai-compatible"
base_url = "http://localhost:8081/v1"
default_model = "model-a"

[models.model-a]
provider = "a"

[models.model-b]
provider = "b"
"#,
        )
        .unwrap();
        let mut config = Config::load_from_dir(dir.path().to_path_buf()).unwrap();
        let before = (
            config.provider_name.clone(),
            config.model.clone(),
            config.provider_model.clone(),
            config.max_context_tokens,
        );
        let error = config.set_provider("a").unwrap_err();
        assert!(error.to_string().contains("configuration cycle"));
        assert_eq!(
            before,
            (
                config.provider_name.clone(),
                config.model.clone(),
                config.provider_model.clone(),
                config.max_context_tokens,
            )
        );
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
