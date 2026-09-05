use agentmux_config::{Config, ProviderConfig};
use agentmux_core::{
    AgentId, AgentProfile, CanonicalEvent, CanonicalEventKind, EventId, FormationId, Session,
    SessionId, TaskStatus,
};
use agentmux_provider::{ModelProvider, ResiliencePolicy, ResilientProvider};
use agentmux_providers::{MockProvider, MockStep, OllamaProvider, OpenAiProvider};
use agentmux_storage::{Repository, SqliteRepository};
use agentmux_tools::ScopedWorkspace;
use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Arc,
};
#[derive(Parser)]
#[command(
    name = "agentmux",
    version,
    about = "Provider-neutral terminal AI workspace"
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}
#[derive(Subcommand)]
enum Command {
    Run {
        #[arg(long, default_value = ".agentmux/config.toml")]
        config: PathBuf,
        #[arg(
            long,
            default_value = "Inspect the workspace and report the most important next action."
        )]
        objective: String,
    },
    Doctor {
        #[arg(long, default_value = ".agentmux/config.toml")]
        config: PathBuf,
    },
}
#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_target(false)
        .init();
    match Cli::parse().command.unwrap_or(Command::Run {
        config: ".agentmux/config.toml".into(),
        objective: "Inspect the workspace and report the most important next action.".into(),
    }) {
        Command::Run { config, objective } => run(&config, objective).await,
        Command::Doctor { config } => doctor(&config).await,
    }
}
fn load(path: &Path) -> Result<Config> {
    if path.exists() {
        Config::load(path).with_context(|| format!("configuration failed: {}", path.display()))
    } else {
        Config::validate(Config::default()).context("built-in configuration failed")
    }
}
async fn run(path: &Path, objective: String) -> Result<()> {
    let config = load(path)?;
    let repo = Arc::new(SqliteRepository::open(&config.state_path())?);
    let sessions = repo.sessions().await?;
    let session = if sessions.is_empty() {
        let value = Session {
            id: SessionId::new(),
            formation_id: FormationId::new(),
            workspace: config.workspace.root.display().to_string(),
            created_at: chrono::Utc::now(),
        };
        repo.save_session(value.clone()).await?;
        value
    } else {
        sessions
            .into_iter()
            .max_by_key(|s| s.created_at)
            .expect("non-empty checked")
    };
    let mut agent_profiles = Vec::new();
    let owner = if let Some(formation) = &config.formation {
        let mut entry = None;
        let names = config
            .agents
            .iter()
            .map(|agent| (agent.id.as_str(), agent.name.as_str()))
            .collect::<HashMap<_, _>>();
        for configured in &config.agents {
            let profile = AgentProfile {
                id: AgentId::new(),
                name: configured.name.clone(),
                role: configured.role.clone(),
                provider: provider_name(&config.provider).into(),
                model: model_name(&config.provider),
                system_prompt: configured.system_prompt.clone(),
                tools: configured.tools.clone(),
                enabled: true,
                allowed_targets: configured
                    .delegation
                    .allowed_targets
                    .iter()
                    .filter_map(|id| names.get(id.as_str()).map(|name| (*name).to_owned()))
                    .collect(),
                delegation_limits: Some(agentmux_core::TaskLimits {
                    max_delegation_depth: configured.delegation.max_depth,
                    max_children: configured.delegation.max_children,
                    max_duration_seconds: config.runtime.default_timeout_seconds,
                }),
            };
            if configured.id == formation.entry_agent {
                entry = Some(profile.id);
            }
            repo.save_agent(profile.clone()).await?;
            agent_profiles.push(profile);
        }
        entry.context("validated formation entry agent missing")?
    } else {
        let profile = AgentProfile {
            id: AgentId::new(),
            name: config.agent.name.clone(),
            role: config.agent.role.clone(),
            provider: provider_name(&config.provider).into(),
            model: model_name(&config.provider),
            system_prompt: config.agent.system_prompt.clone(),
            tools: config.agent.tools.clone(),
            enabled: true,
            allowed_targets: vec![],
            delegation_limits: None,
        };
        let id = profile.id;
        repo.save_agent(profile.clone()).await?;
        agent_profiles.push(profile);
        id
    };
    recover_interrupted(repo.as_ref(), session.id).await?;
    let tasks = repo.tasks().await?;
    let events = repo.events(session.id).await?;
    let approvals = repo.approvals(session.id).await?;
    let artifacts = repo.artifacts().await?;
    let provider = provider(&config.provider)?;
    let agent_name = agent_profiles
        .iter()
        .find(|a| a.id == owner)
        .map(|a| a.name.clone())
        .unwrap_or_else(|| config.agent.name.clone());
    let workspace = ScopedWorkspace::new(&config.workspace.root, 256 * 1024)?;
    let workspace_context = workspace.context_bundle(200, 64 * 1024)?;
    agentmux_tui::run_configured(agentmux_tui::ConfiguredRun {
        repo,
        provider,
        session: session.id,
        owner,
        model_name: model_name(&config.provider),
        system_prompt: config.agent.system_prompt,
        provider_label: provider_name(&config.provider).into(),
        agent_name,
        agent_profiles,
        objective,
        initial_tasks: tasks,
        initial_events: events,
        initial_approvals: approvals,
        initial_artifacts: artifacts,
        capacity: config.runtime.event_channel_capacity,
        workspace_context,
        workspace_root: config.workspace.root,
        max_parallel_tasks: config
            .formation
            .as_ref()
            .map_or(1, |f| f.max_parallel_tasks),
        input_cost_per_million: config.cost.input_per_million,
        output_cost_per_million: config.cost.output_per_million,
        cost_currency: config.cost.currency,
    })
    .await?;
    Ok(())
}
async fn recover_interrupted<R: Repository>(repo: &R, session: SessionId) -> Result<()> {
    for task in repo
        .tasks()
        .await?
        .into_iter()
        .filter(|t| t.status == TaskStatus::Running)
    {
        let event = CanonicalEvent {
            id: EventId::new(),
            session_id: session,
            task_id: Some(task.id),
            created_at: chrono::Utc::now(),
            kind: CanonicalEventKind::ProviderFailed(
                "interrupted by previous process termination; retry or cancel explicitly".into(),
            ),
        };
        repo.transition_task_with_event(task.id, TaskStatus::Failed, event)
            .await?;
    }
    Ok(())
}
async fn doctor(path: &Path) -> Result<()> {
    let config = load(path)?;
    println!("AgentMux doctor");
    println!("[ok] config_version {}", config.config_version);
    println!("[ok] provider {}", provider_name(&config.provider));
    let repo = SqliteRepository::open(&config.state_path())?;
    println!(
        "[ok] SQLite state {} ({} session(s))",
        config.state_path().display(),
        repo.sessions().await?.len()
    );
    println!("[ok] scoped tools: {}", config.agent.tools.join(", "));
    println!("[ok] shell disabled; patch writes require digest check and explicit approval");
    match provider(&config.provider) {
        Ok(selected) => {
            let capabilities = selected.descriptor().capabilities;
            println!(
                "[ok] capabilities: streaming={}, tools={}, structured_output={}, cancellation={}",
                capabilities.streaming,
                capabilities.tools,
                capabilities.structured_output,
                capabilities.cancellation
            );
            if !matches!(&config.provider, ProviderConfig::Mock) {
                match selected.models().await {
                    Ok(models) => println!("[ok] provider reachable ({} model(s))", models.len()),
                    Err(error) => println!("[action required] {error}"),
                }
            }
        }
        Err(error) => println!("[action required] {error}"),
    }
    match (
        config.cost.input_per_million,
        config.cost.output_per_million,
    ) {
        (Some(input), Some(output)) => println!(
            "[ok] estimated cost rates: {input}/{output} {} per 1M input/output tokens",
            config.cost.currency
        ),
        _ => println!("[info] cost estimate disabled; configure both cost rates to enable"),
    }
    Ok(())
}
fn provider_name(value: &ProviderConfig) -> &'static str {
    match value {
        ProviderConfig::Mock => "mock",
        ProviderConfig::Ollama { .. } => "ollama",
        ProviderConfig::Openai { .. } => "openai",
    }
}
fn model_name(value: &ProviderConfig) -> String {
    match value {
        ProviderConfig::Mock => "mock-v1".into(),
        ProviderConfig::Ollama { model, .. } => model.clone(),
        ProviderConfig::Openai { model, .. } => model.clone(),
    }
}
fn provider(value: &ProviderConfig) -> Result<Arc<dyn ModelProvider>> {
    let inner: Arc<dyn ModelProvider> = match value {
        ProviderConfig::Mock => Arc::new(MockProvider::new(vec![
            MockStep::Event(agentmux_provider::ProviderEvent::TextDelta(
                "configured mock response".into(),
            )),
            MockStep::Event(agentmux_provider::ProviderEvent::Usage(
                agentmux_provider::Usage {
                    input_tokens: 4,
                    output_tokens: 3,
                },
            )),
            MockStep::Event(agentmux_provider::ProviderEvent::Completed),
        ])),
        ProviderConfig::Ollama { base_url, .. } => Arc::new(OllamaProvider::new(base_url.clone())?),
        ProviderConfig::Openai { api_key_env, .. } => {
            let api_key = std::env::var(api_key_env).with_context(|| {
                format!("OpenAI credential environment variable {api_key_env} is missing")
            })?;
            Arc::new(OpenAiProvider::new("https://api.openai.com", api_key)?)
        }
    };
    Ok(Arc::new(ResilientProvider::new(
        inner,
        ResiliencePolicy::default(),
    )))
}
