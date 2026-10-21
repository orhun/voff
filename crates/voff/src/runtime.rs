//! In-memory team settings assembled from the setup builder.
use std::{collections::HashSet, path::PathBuf};
use thiserror::Error;

pub use voff_core::MAX_OUTPUT_TOKENS;

#[derive(Clone, Debug)]
pub struct RuntimeTeam {
    pub workspace: WorkspaceConfig,
    pub runtime: RuntimeConfig,
    pub provider: ProviderConfig,
    pub providers: Vec<NamedProviderConfig>,
    pub agent: AgentConfig,
    pub formation: Option<FormationConfig>,
    pub agents: Vec<FormationAgentConfig>,
}

#[derive(Clone, Debug)]
pub struct WorkspaceConfig {
    pub root: PathBuf,
}

#[derive(Clone, Debug)]
pub struct RuntimeConfig {
    pub default_timeout_seconds: u64,
    pub event_channel_capacity: usize,
    pub automatic_review: bool,
}

#[derive(Clone, Debug)]
pub enum ProviderConfig {
    Mock,
    Ollama { base_url: String, model: String },
    Openai { model: String, api_key_env: String },
    CodexCli { model: String },
    ClaudeCli { model: String },
}

#[derive(Clone, Debug)]
pub struct NamedProviderConfig {
    pub id: String,
    pub config: ProviderConfig,
}

#[derive(Clone, Debug)]
pub struct AgentConfig {
    pub name: String,
    pub role: String,
    pub system_prompt: String,
    pub tools: Vec<String>,
    pub max_output_tokens: u64,
}

#[derive(Clone, Debug)]
pub struct FormationConfig {
    pub name: String,
    pub entry_agent: String,
    pub max_parallel_tasks: usize,
}

#[derive(Clone, Debug)]
pub struct FormationAgentConfig {
    pub id: String,
    pub name: String,
    pub role: String,
    pub system_prompt: String,
    pub tools: Vec<String>,
    pub provider: Option<String>,
    pub model: Option<String>,
    pub max_output_tokens: u64,
    pub delegation: DelegationConfig,
}

#[derive(Clone, Debug, Default)]
pub struct DelegationConfig {
    pub allowed_targets: Vec<String>,
    pub max_depth: u16,
    pub max_children: u16,
}

#[derive(Debug, Error)]
pub enum RuntimeError {
    #[error("invalid team setting: {0}")]
    Invalid(&'static str),
    #[error("unknown provider route: {0}")]
    Provider(String),
    #[error("unknown delegation target: {0}")]
    Target(String),
}

impl Default for RuntimeTeam {
    fn default() -> Self {
        Self {
            workspace: WorkspaceConfig { root: ".".into() },
            runtime: RuntimeConfig {
                default_timeout_seconds: 1800,
                event_channel_capacity: 128,
                automatic_review: true,
            },
            provider: ProviderConfig::Mock,
            providers: Vec::new(),
            agent: AgentConfig {
                name: "Team Lead".into(),
                role: "technical_lead".into(),
                system_prompt: String::new(),
                tools: Vec::new(),
                max_output_tokens: voff_core::DEFAULT_MAX_OUTPUT_TOKENS,
            },
            formation: None,
            agents: Vec::new(),
        }
    }
}

impl RuntimeTeam {
    pub fn validate(team: Self) -> Result<Self, RuntimeError> {
        if !team.workspace.root.is_dir() {
            return Err(RuntimeError::Invalid("workspace does not exist"));
        }
        if team.agent.name.trim().is_empty() || team.agent.role.trim().is_empty() {
            return Err(RuntimeError::Invalid("lead name or role is empty"));
        }
        if team.runtime.default_timeout_seconds == 0 || team.runtime.event_channel_capacity == 0 {
            return Err(RuntimeError::Invalid("runtime limits must be positive"));
        }
        validate_agent(&team.agent.tools, team.agent.max_output_tokens)?;
        validate_provider(&team.provider)?;
        let mut providers = HashSet::new();
        for provider in &team.providers {
            if provider.id.trim().is_empty() || !providers.insert(provider.id.as_str()) {
                return Err(RuntimeError::Invalid(
                    "provider route is empty or duplicated",
                ));
            }
            validate_provider(&provider.config)?;
        }
        if let Some(formation) = &team.formation {
            if formation.name.trim().is_empty() || formation.max_parallel_tasks == 0 {
                return Err(RuntimeError::Invalid("invalid formation"));
            }
            let mut ids = HashSet::new();
            for agent in &team.agents {
                if agent.id.trim().is_empty()
                    || agent.name.trim().is_empty()
                    || agent.role.trim().is_empty()
                    || !ids.insert(agent.id.as_str())
                {
                    return Err(RuntimeError::Invalid("invalid or duplicate team member"));
                }
                validate_agent(&agent.tools, agent.max_output_tokens)?;
                if let Some(route) = &agent.provider
                    && !providers.contains(route.as_str())
                {
                    return Err(RuntimeError::Provider(route.clone()));
                }
                if agent
                    .model
                    .as_ref()
                    .is_some_and(|model| model.trim().is_empty())
                {
                    return Err(RuntimeError::Invalid("model is empty"));
                }
            }
            if !ids.contains(formation.entry_agent.as_str()) {
                return Err(RuntimeError::Invalid("team lead is missing"));
            }
            for target in team
                .agents
                .iter()
                .flat_map(|agent| &agent.delegation.allowed_targets)
            {
                if !ids.contains(target.as_str()) {
                    return Err(RuntimeError::Target(target.clone()));
                }
            }
        } else if !team.agents.is_empty() || !team.providers.is_empty() {
            return Err(RuntimeError::Invalid("formation is missing"));
        }
        Ok(team)
    }
}

fn validate_agent(tools: &[String], max_output_tokens: u64) -> Result<(), RuntimeError> {
    if !(1..=MAX_OUTPUT_TOKENS).contains(&max_output_tokens) {
        return Err(RuntimeError::Invalid("output token limit is out of range"));
    }
    if tools.iter().any(|tool| {
        !matches!(
            tool.as_str(),
            "file.read"
                | "search.text"
                | "file.patch"
                | "process.test"
                | "task.delegate"
                | "review.request"
        )
    }) {
        return Err(RuntimeError::Invalid("unknown tool"));
    }
    Ok(())
}

fn validate_provider(provider: &ProviderConfig) -> Result<(), RuntimeError> {
    match provider {
        ProviderConfig::Mock => Ok(()),
        ProviderConfig::Ollama { base_url, model } => {
            if model.trim().is_empty()
                || !(base_url.starts_with("http://localhost:")
                    || base_url.starts_with("http://127.0.0.1:"))
            {
                Err(RuntimeError::Invalid(
                    "unsafe Ollama endpoint or empty model",
                ))
            } else {
                Ok(())
            }
        }
        ProviderConfig::Openai { model, api_key_env } => {
            if model.trim().is_empty()
                || api_key_env.is_empty()
                || !api_key_env
                    .chars()
                    .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
            {
                Err(RuntimeError::Invalid(
                    "invalid OpenAI model or credential reference",
                ))
            } else {
                Ok(())
            }
        }
        ProviderConfig::CodexCli { model } | ProviderConfig::ClaudeCli { model } => {
            if model.trim().is_empty() {
                Err(RuntimeError::Invalid("model is empty"))
            } else {
                Ok(())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_unbounded_tokens_and_unsafe_endpoint() {
        let mut team = RuntimeTeam::default();
        team.agent.max_output_tokens = 0;
        assert!(RuntimeTeam::validate(team.clone()).is_err());
        team.agent.max_output_tokens = 128;
        team.provider = ProviderConfig::Ollama {
            base_url: "https://example.com".into(),
            model: "local".into(),
        };
        assert!(RuntimeTeam::validate(team).is_err());
    }
}
