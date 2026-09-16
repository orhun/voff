//! Versioned, secret-free voff configuration.
use serde::{Deserialize, Serialize};
use std::{
    collections::HashSet,
    fs,
    path::{Component, Path, PathBuf},
};
use thiserror::Error;

pub const CONFIG_VERSION: u16 = 1;

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Config {
    pub config_version: u16,
    pub workspace: WorkspaceConfig,
    pub runtime: RuntimeConfig,
    pub provider: ProviderConfig,
    /// Named providers used by heterogeneous formations. Empty keeps the
    /// legacy single-provider configuration fully compatible.
    #[serde(default)]
    pub providers: Vec<NamedProviderConfig>,
    #[serde(default)]
    pub cost: CostConfig,
    pub agent: AgentConfig,
    #[serde(default)]
    pub formation: Option<FormationConfig>,
    #[serde(default)]
    pub agents: Vec<FormationAgentConfig>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct CostConfig {
    /// User-supplied currency units per one million input tokens.
    pub input_per_million: Option<f64>,
    /// User-supplied currency units per one million output tokens.
    pub output_per_million: Option<f64>,
    #[serde(default = "default_currency")]
    pub currency: String,
}

impl Default for CostConfig {
    fn default() -> Self {
        Self {
            input_per_million: None,
            output_per_million: None,
            currency: default_currency(),
        }
    }
}

fn default_currency() -> String {
    "USD".into()
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct WorkspaceConfig {
    pub root: PathBuf,
    pub state_file: PathBuf,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct RuntimeConfig {
    pub default_timeout_seconds: u64,
    pub event_channel_capacity: usize,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ProviderConfig {
    Mock,
    Ollama {
        base_url: String,
        model: String,
    },
    Openai {
        model: String,
        #[serde(default = "default_openai_key_env")]
        api_key_env: String,
    },
}
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct NamedProviderConfig {
    pub id: String,
    #[serde(flatten)]
    pub config: ProviderConfig,
}
fn default_openai_key_env() -> String {
    "OPENAI_API_KEY".into()
}
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct AgentConfig {
    pub name: String,
    pub role: String,
    pub system_prompt: String,
    pub tools: Vec<String>,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct FormationConfig {
    pub name: String,
    pub entry_agent: String,
    #[serde(default = "default_parallel")]
    pub max_parallel_tasks: usize,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct FormationAgentConfig {
    pub id: String,
    pub name: String,
    pub role: String,
    pub system_prompt: String,
    pub tools: Vec<String>,
    #[serde(default)]
    pub provider: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub delegation: DelegationConfig,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct DelegationConfig {
    #[serde(default)]
    pub allowed_targets: Vec<String>,
    #[serde(default = "default_depth")]
    pub max_depth: u16,
    #[serde(default = "default_children")]
    pub max_children: u16,
}
impl Default for DelegationConfig {
    fn default() -> Self {
        Self {
            allowed_targets: vec![],
            max_depth: default_depth(),
            max_children: default_children(),
        }
    }
}
const fn default_parallel() -> usize {
    2
}
const fn default_depth() -> u16 {
    2
}
const fn default_children() -> u16 {
    5
}

impl Default for Config {
    fn default() -> Self {
        Self {
            config_version: CONFIG_VERSION,
            workspace: WorkspaceConfig {
                root: ".".into(),
                state_file: ".voff/voff.db".into(),
            },
            runtime: RuntimeConfig {
                default_timeout_seconds: 1800,
                event_channel_capacity: 128,
            },
            provider: ProviderConfig::Mock,
            providers: vec![],
            cost: CostConfig::default(),
            agent: AgentConfig {
                name: "Solo Agent".into(),
                role: "software_developer".into(),
                system_prompt: "Follow repository rules and return verifiable results.".into(),
                tools: vec!["file.read".into(), "search.text".into()],
            },
            formation: None,
            agents: vec![],
        }
    }
}

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("cannot read config {path}: {source}")]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("invalid TOML in {path}: {source}")]
    Parse {
        path: PathBuf,
        source: toml::de::Error,
    },
    #[error("unsupported config_version {0}; expected 1")]
    Version(u16),
    #[error("{0} must be greater than zero")]
    NonPositive(&'static str),
    #[error("workspace state_file must be relative and cannot contain '..'")]
    UnsafeStatePath,
    #[error("unsupported or unsafe tool: {0}")]
    UnsupportedTool(String),
    #[error("Ollama base_url must use http://127.0.0.1 or http://localhost in Phase 1")]
    UnsafeOllamaEndpoint,
    #[error("agent field {0} cannot be empty")]
    EmptyAgentField(&'static str),
    #[error("formation entry agent was not found: {0}")]
    EntryAgentNotFound(String),
    #[error("duplicate formation agent id: {0}")]
    DuplicateAgent(String),
    #[error("duplicate provider id: {0}")]
    DuplicateProvider(String),
    #[error("provider was not found: {0}")]
    ProviderNotFound(String),
    #[error("delegation target was not found: {0}")]
    TargetNotFound(String),
    #[error("formation and agents must be configured together")]
    IncompleteFormation,
}

impl Config {
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        let text = fs::read_to_string(path).map_err(|source| ConfigError::Read {
            path: path.to_path_buf(),
            source,
        })?;
        let config = toml::from_str(&text).map_err(|source| ConfigError::Parse {
            path: path.to_path_buf(),
            source,
        })?;
        Self::validate(config)
    }
    pub fn validate(config: Self) -> Result<Self, ConfigError> {
        if config.config_version != CONFIG_VERSION {
            return Err(ConfigError::Version(config.config_version));
        }
        if config.runtime.default_timeout_seconds == 0 {
            return Err(ConfigError::NonPositive("default_timeout_seconds"));
        }
        if config.runtime.event_channel_capacity == 0 {
            return Err(ConfigError::NonPositive("event_channel_capacity"));
        }
        for rate in [
            config.cost.input_per_million,
            config.cost.output_per_million,
        ]
        .into_iter()
        .flatten()
        {
            if !rate.is_finite() || rate < 0.0 {
                return Err(ConfigError::NonPositive("cost rate"));
            }
        }
        if config.cost.currency.trim().is_empty() {
            return Err(ConfigError::EmptyAgentField("cost.currency"));
        }
        if config.workspace.state_file.is_absolute()
            || config
                .workspace
                .state_file
                .components()
                .any(|c| matches!(c, Component::ParentDir))
        {
            return Err(ConfigError::UnsafeStatePath);
        }
        if config.agent.name.trim().is_empty() {
            return Err(ConfigError::EmptyAgentField("name"));
        }
        if config.agent.role.trim().is_empty() {
            return Err(ConfigError::EmptyAgentField("role"));
        }
        let allowed = HashSet::from([
            "file.read",
            "search.text",
            "file.patch",
            "process.test",
            "task.delegate",
            "review.request",
        ]);
        if let Some(tool) = config
            .agent
            .tools
            .iter()
            .find(|t| !allowed.contains(t.as_str()))
        {
            return Err(ConfigError::UnsupportedTool(tool.clone()));
        }
        validate_provider(&config.provider)?;
        let mut provider_ids = HashSet::new();
        for provider in &config.providers {
            if provider.id.trim().is_empty() {
                return Err(ConfigError::EmptyAgentField("provider.id"));
            }
            if !provider_ids.insert(provider.id.as_str()) {
                return Err(ConfigError::DuplicateProvider(provider.id.clone()));
            }
            validate_provider(&provider.config)?;
        }
        match (&config.formation, config.agents.is_empty()) {
            (None, true) => {}
            (Some(formation), false) => {
                if formation.name.trim().is_empty() || formation.max_parallel_tasks == 0 {
                    return Err(ConfigError::EmptyAgentField("formation"));
                }
                let mut ids = HashSet::new();
                for agent in &config.agents {
                    if agent.id.trim().is_empty()
                        || agent.name.trim().is_empty()
                        || agent.role.trim().is_empty()
                    {
                        return Err(ConfigError::EmptyAgentField("formation agent"));
                    }
                    if !ids.insert(agent.id.as_str()) {
                        return Err(ConfigError::DuplicateAgent(agent.id.clone()));
                    }
                    if let Some(tool) = agent.tools.iter().find(|t| !allowed.contains(t.as_str())) {
                        return Err(ConfigError::UnsupportedTool(tool.clone()));
                    }
                    if let Some(provider) = &agent.provider
                        && ((!config.providers.is_empty()
                            && !provider_ids.contains(provider.as_str()))
                            || (config.providers.is_empty()
                                && provider != provider_kind(&config.provider)))
                    {
                        return Err(ConfigError::ProviderNotFound(provider.clone()));
                    }
                    if agent
                        .model
                        .as_ref()
                        .is_some_and(|model| model.trim().is_empty())
                    {
                        return Err(ConfigError::EmptyAgentField("formation agent model"));
                    }
                }
                if !ids.contains(formation.entry_agent.as_str()) {
                    return Err(ConfigError::EntryAgentNotFound(
                        formation.entry_agent.clone(),
                    ));
                }
                for target in config
                    .agents
                    .iter()
                    .flat_map(|a| &a.delegation.allowed_targets)
                {
                    if !ids.contains(target.as_str()) {
                        return Err(ConfigError::TargetNotFound(target.clone()));
                    }
                }
            }
            _ => return Err(ConfigError::IncompleteFormation),
        }
        Ok(config)
    }
    #[must_use]
    pub fn state_path(&self) -> PathBuf {
        self.workspace.root.join(&self.workspace.state_file)
    }
}

fn validate_provider(provider: &ProviderConfig) -> Result<(), ConfigError> {
    if let ProviderConfig::Ollama { base_url, model } = provider {
        if model.trim().is_empty() {
            return Err(ConfigError::EmptyAgentField("provider.model"));
        }
        if !(base_url.starts_with("http://127.0.0.1:") || base_url.starts_with("http://localhost:"))
        {
            return Err(ConfigError::UnsafeOllamaEndpoint);
        }
    }
    if let ProviderConfig::Openai { model, api_key_env } = provider {
        if model.trim().is_empty() {
            return Err(ConfigError::EmptyAgentField("provider.model"));
        }
        if api_key_env.trim().is_empty()
            || !api_key_env
                .chars()
                .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
        {
            return Err(ConfigError::EmptyAgentField("provider.api_key_env"));
        }
    }
    Ok(())
}

fn provider_kind(provider: &ProviderConfig) -> &'static str {
    match provider {
        ProviderConfig::Mock => "mock",
        ProviderConfig::Ollama { .. } => "ollama",
        ProviderConfig::Openai { .. } => "openai",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn default_round_trips() {
        let encoded = toml::to_string(&Config::default()).unwrap();
        let parsed: Config = toml::from_str(&encoded).unwrap();
        assert_eq!(parsed.config_version, 1);
    }
    #[test]
    fn rejects_version_and_unsafe_paths() {
        let c = Config {
            config_version: 2,
            ..Config::default()
        };
        assert!(matches!(Config::validate(c), Err(ConfigError::Version(2))));
        let mut c = Config::default();
        c.workspace.state_file = "../outside.db".into();
        assert!(matches!(
            Config::validate(c),
            Err(ConfigError::UnsafeStatePath)
        ));
    }
    #[test]
    fn rejects_remote_ollama_and_arbitrary_shell() {
        let c = Config {
            provider: ProviderConfig::Ollama {
                base_url: "https://remote.example".into(),
                model: "x".into(),
            },
            ..Config::default()
        };
        assert!(matches!(
            Config::validate(c),
            Err(ConfigError::UnsafeOllamaEndpoint)
        ));
        let mut c = Config::default();
        c.agent.tools.push("process.shell".into());
        assert!(matches!(
            Config::validate(c),
            Err(ConfigError::UnsupportedTool(_))
        ));
    }
    #[test]
    fn accepts_phase2_scoped_tools() {
        let mut c = Config::default();
        c.agent
            .tools
            .extend(["file.patch".into(), "process.test".into()]);
        assert!(Config::validate(c).is_ok());
    }
    #[test]
    fn validates_openai_secret_reference_and_cost_rates() {
        let c = Config {
            provider: ProviderConfig::Openai {
                model: "gpt-5-mini".into(),
                api_key_env: "OPENAI_API_KEY".into(),
            },
            cost: CostConfig {
                input_per_million: Some(1.0),
                output_per_million: Some(2.0),
                currency: "USD".into(),
            },
            ..Config::default()
        };
        assert!(Config::validate(c).is_ok());

        let c = Config {
            provider: ProviderConfig::Openai {
                model: "gpt-5-mini".into(),
                api_key_env: "actual-secret-value".into(),
            },
            ..Config::default()
        };
        assert!(matches!(
            Config::validate(c),
            Err(ConfigError::EmptyAgentField("provider.api_key_env"))
        ));
    }
    #[test]
    fn validates_named_provider_routes_for_formation_agents() {
        let c = Config {
            providers: vec![
                NamedProviderConfig {
                    id: "local".into(),
                    config: ProviderConfig::Ollama {
                        base_url: "http://127.0.0.1:11434".into(),
                        model: "qwen3:4b".into(),
                    },
                },
                NamedProviderConfig {
                    id: "cloud".into(),
                    config: ProviderConfig::Openai {
                        model: "gpt-5-mini".into(),
                        api_key_env: "OPENAI_API_KEY".into(),
                    },
                },
            ],
            formation: Some(FormationConfig {
                name: "Mixed".into(),
                entry_agent: "lead".into(),
                max_parallel_tasks: 2,
            }),
            agents: vec![FormationAgentConfig {
                id: "lead".into(),
                name: "Lead".into(),
                role: "lead".into(),
                system_prompt: "delegate".into(),
                tools: vec![],
                provider: Some("cloud".into()),
                model: Some("gpt-5-mini".into()),
                delegation: DelegationConfig::default(),
            }],
            ..Config::default()
        };
        assert!(Config::validate(c).is_ok());
    }
    #[test]
    fn validates_multi_agent_formation() {
        let c = Config {
            formation: Some(FormationConfig {
                name: "Team".into(),
                entry_agent: "lead".into(),
                max_parallel_tasks: 2,
            }),
            agents: vec![
                FormationAgentConfig {
                    id: "lead".into(),
                    name: "Lead".into(),
                    role: "technical_lead".into(),
                    system_prompt: "Delegate bounded work.".into(),
                    tools: vec!["task.delegate".into()],
                    provider: None,
                    model: None,
                    delegation: DelegationConfig {
                        allowed_targets: vec!["worker".into()],
                        ..DelegationConfig::default()
                    },
                },
                FormationAgentConfig {
                    id: "worker".into(),
                    name: "Worker".into(),
                    role: "developer".into(),
                    system_prompt: "Implement and test.".into(),
                    tools: vec!["file.patch".into(), "process.test".into()],
                    provider: None,
                    model: None,
                    delegation: DelegationConfig::default(),
                },
            ],
            ..Config::default()
        };
        assert!(Config::validate(c).is_ok());
    }
    #[test]
    fn rejects_unknown_delegation_target() {
        let c = Config {
            formation: Some(FormationConfig {
                name: "Team".into(),
                entry_agent: "lead".into(),
                max_parallel_tasks: 2,
            }),
            agents: vec![FormationAgentConfig {
                id: "lead".into(),
                name: "Lead".into(),
                role: "lead".into(),
                system_prompt: "x".into(),
                tools: vec![],
                provider: None,
                model: None,
                delegation: DelegationConfig {
                    allowed_targets: vec!["missing".into()],
                    ..DelegationConfig::default()
                },
            }],
            ..Config::default()
        };
        assert!(matches!(
            Config::validate(c),
            Err(ConfigError::TargetNotFound(_))
        ));
    }
}
