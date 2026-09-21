mod config;
mod credential;
mod model_catalog;
mod onboarding;
mod tui;

use crate::config::{
    AgentConfig, Config, DelegationConfig, FormationAgentConfig, FormationConfig,
    NamedProviderConfig, ProviderConfig,
};
use crate::model_catalog::ModelCatalog;
use crate::onboarding::{DeveloperRole, ProviderChoice, SetupDraft};
use anyhow::{Context, Result};
use clap::{Parser, Subcommand, ValueEnum};
use serde::Serialize;
use std::{
    collections::{HashMap, HashSet},
    fs::OpenOptions,
    io::{BufRead, BufWriter, IsTerminal, Write},
    path::{Path, PathBuf},
    sync::Arc,
};
use voff_core::{
    AgentId, AgentProfile, CanonicalEvent, CanonicalEventKind, EventId, FormationId, Session,
    SessionId, TaskStatus,
};
use voff_engine::DelegationProposal;
use voff_provider::{MockProvider, MockStep, OllamaProvider, OpenAiProvider};
use voff_provider::{ModelProvider, ResiliencePolicy, ResilientProvider};
use voff_storage::{Repository, SqliteRepository};
use voff_tools::ScopedWorkspace;

const DEFAULT_OBJECTIVE: &str = "Build a full-stack calculator through the Team Lead. Give every enabled specialist one explicit, non-overlapping delegated task. Backend Developer owns server.py: use only the Python standard library, serve the frontend, and implement POST /api/calculate with add, subtract, multiply, divide, numeric-input validation, JSON responses, and division-by-zero errors. Frontend Developer owns index.html: build an accessible calculator that calls /api/calculate and clearly displays loading, results, validation errors, and server errors. Other enabled specialists contribute role-appropriate review, validation, integration, or operational evidence without overlapping those files. Every delegated task must complete; the app must run with python3 server.py and require no external dependencies.";

fn calculator_delegation_plan(objective: &str, agents: &[AgentProfile]) -> Vec<DelegationProposal> {
    if objective != DEFAULT_OBJECTIVE {
        return Vec::new();
    }
    let Some(backend) = agents
        .iter()
        .find(|agent| agent.role == "backend_developer")
    else {
        return Vec::new();
    };
    let Some(frontend) = agents
        .iter()
        .find(|agent| agent.role == "frontend_developer")
    else {
        return Vec::new();
    };
    vec![
        DelegationProposal {
            target_agent: backend.name.clone(),
            objective: "Create server.py for the full-stack calculator. Use only the Python standard library, serve index.html, and implement POST /api/calculate for add, subtract, multiply, and divide with numeric-input validation, JSON responses, and division-by-zero errors. Own only server.py.".into(),
            acceptance_criteria: vec![
                "server.py is complete and runnable with python3 server.py".into(),
                "POST /api/calculate returns JSON results for all four operations".into(),
                "Invalid numbers, operations, JSON, and division by zero return clear errors".into(),
            ],
        },
        DelegationProposal {
            target_agent: frontend.name.clone(),
            objective: "Create index.html for the full-stack calculator. Build an accessible browser interface that calls POST /api/calculate and clearly displays loading, results, client validation, and server errors. Own only index.html and do not implement arithmetic locally.".into(),
            acceptance_criteria: vec![
                "index.html provides inputs and controls for all four operations".into(),
                "The interface calls /api/calculate and renders its JSON response".into(),
                "Keyboard use, labels, loading state, and error feedback are present".into(),
            ],
        },
    ]
}

#[derive(Parser)]
#[command(
    name = "voff",
    version,
    about = "Provider-neutral terminal AI workspace"
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}
#[derive(Subcommand)]
enum Command {
    /// Create a safe starter configuration interactively.
    Init {
        /// Starter topology: one agent or a lead/implementer/reviewer team.
        #[arg(long, value_enum)]
        template: Option<InitTemplate>,
        #[arg(long, value_enum)]
        provider: Option<InitProvider>,
        #[arg(long)]
        model: Option<String>,
        /// Maximum output tokens generated per provider response.
        #[arg(long)]
        max_output_tokens: Option<u64>,
        #[arg(long, default_value = ".voff/config.toml")]
        output: PathBuf,
    },
    Run {
        #[arg(long, default_value = ".voff/config.toml")]
        config: PathBuf,
        #[arg(
            long,
            default_value = DEFAULT_OBJECTIVE
        )]
        objective: String,
        /// Resume the newest stored session instead of creating a clean session.
        #[arg(long)]
        resume: bool,
        /// Resume a specific stored session (mutually exclusive with --resume).
        #[arg(long, conflicts_with = "resume")]
        session: Option<SessionId>,
    },
    Doctor {
        #[arg(long, default_value = ".voff/config.toml")]
        config: PathBuf,
    },
    /// Export the newest session as a redacted diagnostic JSON file.
    Export {
        #[arg(long, default_value = ".voff/config.toml")]
        config: PathBuf,
        #[arg(long, default_value = ".voff/diagnostic.json")]
        output: PathBuf,
    },
    /// List stored sessions for a workspace configuration.
    Sessions {
        #[arg(long, default_value = ".voff/config.toml")]
        config: PathBuf,
    },
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum InitProvider {
    Ollama,
    Openai,
}
#[derive(Clone, Copy, Debug, ValueEnum)]
enum InitTemplate {
    Solo,
    Team,
}
/// Load only the invocation directory's `.env`, preserving process variables.
/// Must be called once, at synchronous executable startup before threads start.
fn load_dotenv() -> Result<()> {
    match dotenvy::from_path(".env") {
        Ok(()) => Ok(()),
        Err(dotenvy::Error::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        // Parser errors contain source text, so never attach the original error.
        Err(_) => anyhow::bail!(
            "could not load .env in the current directory; check file access and syntax"
        ),
    }
}

fn main() -> Result<()> {
    load_dotenv()?;
    tokio::runtime::Runtime::new()?.block_on(application())
}

async fn application() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_target(false)
        .init();
    let cli = Cli::parse();
    let first_launch = cli.command.is_none();
    match cli.command.unwrap_or(Command::Run {
        config: ".voff/config.toml".into(),
        objective: DEFAULT_OBJECTIVE.into(),
        resume: false,
        session: None,
    }) {
        Command::Init {
            template,
            provider,
            model,
            max_output_tokens,
            output,
        } => init(template, provider, model, max_output_tokens, &output),
        Command::Run {
            config,
            objective,
            resume,
            session,
        } => {
            if first_launch && !config.exists() {
                if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
                    anyhow::bail!(
                        "first launch needs an interactive terminal; use 'voff init --template solo --provider ollama --model <MODEL>' for non-interactive setup"
                    );
                }
                let state_path = Config::default().state_path();
                let credential_lookup = async {
                    if !state_path.exists() {
                        return false;
                    }
                    let Ok(repository) = SqliteRepository::open(&state_path).await else {
                        return false;
                    };
                    credential::resolve("OPENAI_API_KEY", &repository)
                        .await
                        .is_ok()
                };
                let (catalog, credential_available) =
                    tokio::join!(setup_model_catalog(), credential_lookup);
                let Some(draft) = onboarding::run(catalog, credential_available)? else {
                    return Ok(());
                };
                let generated = config_from_setup(&draft)?;
                if let Some(token) = draft.openai_token {
                    let repository = SqliteRepository::open(&generated.state_path()).await?;
                    credential::store("OPENAI_API_KEY", &token, &repository).await?;
                }
                write_config_new(&generated, &config)?;
            }
            run(&config, objective, resume, session).await
        }
        Command::Doctor { config } => doctor(&config).await,
        Command::Export { config, output } => export(&config, &output).await,
        Command::Sessions { config } => list_sessions(&config).await,
    }
}

async fn setup_model_catalog() -> ModelCatalog {
    let mut catalog = ModelCatalog::bundled().unwrap_or_default();
    if let Ok(provider) = OllamaProvider::new("http://127.0.0.1:11434")
        && let Ok(Ok(models)) =
            tokio::time::timeout(std::time::Duration::from_millis(750), provider.models()).await
    {
        catalog.set_ollama_models(models);
    }
    catalog
}

fn init(
    template: Option<InitTemplate>,
    provider: Option<InitProvider>,
    model: Option<String>,
    max_output_tokens: Option<u64>,
    output: &Path,
) -> Result<()> {
    let interactive = template.is_none() || provider.is_none() || model.is_none();
    let template = match template {
        Some(template) => template,
        None => match prompt("Template [solo/team]", "solo")?.as_str() {
            "solo" => InitTemplate::Solo,
            "team" => InitTemplate::Team,
            value => anyhow::bail!("unsupported template '{value}'; choose solo or team"),
        },
    };
    let provider = match provider {
        Some(provider) => provider,
        None => match prompt("Provider [ollama/openai]", "ollama")?.as_str() {
            "ollama" => InitProvider::Ollama,
            "openai" => InitProvider::Openai,
            value => anyhow::bail!("unsupported provider: {value}"),
        },
    };
    let default_model = match provider {
        InitProvider::Ollama => "qwen3:4b",
        InitProvider::Openai => "gpt-5-mini",
    };
    let model = match model {
        Some(model) => model,
        None => prompt("Model", default_model)?,
    };
    let max_output_tokens = match max_output_tokens {
        Some(value) => value,
        None if interactive => prompt("Maximum output tokens", "4096")?
            .parse()
            .context("maximum output tokens must be a whole number")?,
        None => 4096,
    };
    let mut config = Config {
        provider: match provider {
            InitProvider::Ollama => ProviderConfig::Ollama {
                base_url: "http://127.0.0.1:11434".into(),
                model,
            },
            InitProvider::Openai => ProviderConfig::Openai {
                model,
                api_key_env: "OPENAI_API_KEY".into(),
            },
        },
        ..Config::default()
    };
    config.agent.max_output_tokens = max_output_tokens;
    if matches!(template, InitTemplate::Team) {
        config.formation = Some(FormationConfig {
            name: "Lead, Implementer and Reviewer".into(),
            entry_agent: "lead".into(),
            max_parallel_tasks: 2,
        });
        config.agents = team_template();
        for agent in &mut config.agents {
            agent.max_output_tokens = max_output_tokens;
        }
    }
    let config = Config::validate(config).context("generated configuration is invalid")?;
    write_config_new(&config, output)?;
    println!("[ok] configuration written to {}", output.display());
    match provider {
        InitProvider::Ollama => {
            println!("[next] ensure Ollama is running, then run: voff doctor")
        }
        InitProvider::Openai => {
            println!("[next] set OPENAI_API_KEY in your environment, then run: voff doctor")
        }
    }
    Ok(())
}

fn write_config_new(config: &Config, output: &Path) -> Result<()> {
    let encoded = toml::to_string_pretty(config)?;
    if let Some(parent) = output.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(output)
        .with_context(|| format!("refusing to overwrite configuration: {}", output.display()))?;
    file.write_all(encoded.as_bytes())?;
    Ok(())
}

fn config_from_setup(setup: &SetupDraft) -> Result<Config> {
    let first = setup
        .developers
        .first()
        .context("the team must contain at least one developer")?;
    anyhow::ensure!(
        first.role == DeveloperRole::Lead,
        "guided setup requires the first developer to be the Team Lead"
    );
    anyhow::ensure!(
        setup
            .developers
            .iter()
            .skip(1)
            .all(|developer| developer.role != DeveloperRole::Lead),
        "guided setup permits exactly one Team Lead"
    );
    let has_specialists = setup.developers.len() > 1;
    let first_tools = tools_for(first.role, true, has_specialists);
    let mut config = Config {
        workspace: config::WorkspaceConfig {
            root: setup.working_directory.clone(),
            ..Config::default().workspace
        },
        provider: provider_from_choice(first.provider, &first.model),
        agent: AgentConfig {
            name: first.name.clone(),
            role: first.role.config_role().into(),
            system_prompt: if has_specialists {
                first.role.prompt().into()
            } else {
                "Act as the required Team Lead and sole developer. Make explicit decisions, implement scoped changes directly, and return verified evidence."
                    .into()
            },
            tools: first_tools,
            max_output_tokens: first.max_output_tokens,
        },
        ..Config::default()
    };
    if setup.developers.len() == 1 {
        return Config::validate(config).context("generated configuration is invalid");
    }

    let ids = (1..=setup.developers.len())
        .map(|index| format!("developer-{index}"))
        .collect::<Vec<_>>();
    let mut seen_providers = HashSet::new();
    config.providers = setup
        .developers
        .iter()
        .filter_map(|developer| {
            let id = developer.provider.id().to_owned();
            seen_providers
                .insert(id.clone())
                .then(|| NamedProviderConfig {
                    id,
                    config: provider_from_choice(developer.provider, &developer.model),
                })
        })
        .collect();
    config.agents = setup
        .developers
        .iter()
        .enumerate()
        .map(|(index, developer)| FormationAgentConfig {
            id: ids[index].clone(),
            name: developer.name.clone(),
            role: developer.role.config_role().into(),
            system_prompt: developer.role.prompt().into(),
            tools: tools_for(developer.role, index == 0, has_specialists),
            provider: Some(developer.provider.id().into()),
            model: Some(developer.model.clone()),
            max_output_tokens: developer.max_output_tokens,
            delegation: if index == 0 {
                DelegationConfig {
                    allowed_targets: ids.iter().skip(1).cloned().collect(),
                    max_depth: 2,
                    max_children: u16::try_from(setup.developers.len().saturating_sub(1))
                        .unwrap_or(u16::MAX),
                }
            } else {
                DelegationConfig::default()
            },
        })
        .collect();
    config.formation = Some(FormationConfig {
        name: format!("{}-developer custom team", setup.developers.len()),
        entry_agent: ids[0].clone(),
        max_parallel_tasks: setup.developers.len().min(4),
    });
    Config::validate(config).context("generated configuration is invalid")
}

fn provider_from_choice(provider: ProviderChoice, model: &str) -> ProviderConfig {
    match provider {
        ProviderChoice::Ollama => ProviderConfig::Ollama {
            base_url: "http://127.0.0.1:11434".into(),
            model: model.into(),
        },
        ProviderChoice::OpenAi => ProviderConfig::Openai {
            model: model.into(),
            api_key_env: "OPENAI_API_KEY".into(),
        },
    }
}

fn tools_for(role: DeveloperRole, team_lead: bool, has_specialists: bool) -> Vec<String> {
    let mut tools = role.tools();
    if team_lead && has_specialists {
        for tool in ["task.delegate", "review.request"] {
            if !tools.iter().any(|configured| configured == tool) {
                tools.push(tool.into());
            }
        }
    } else {
        tools.retain(|tool| !matches!(tool.as_str(), "task.delegate" | "review.request"));
    }
    if team_lead && !has_specialists {
        for tool in ["file.patch", "process.test"] {
            if !tools.iter().any(|configured| configured == tool) {
                tools.push(tool.into());
            }
        }
    }
    tools
}

fn team_template() -> Vec<FormationAgentConfig> {
    vec![
        FormationAgentConfig {
            id: "lead".into(),
            name: "Team Lead".into(),
            role: "technical_lead".into(),
            system_prompt: "Plan bounded work, delegate to approved targets, and integrate only verified results.".into(),
            tools: vec!["file.read".into(), "search.text".into(), "task.delegate".into(), "review.request".into()],
            provider: None,
            model: None,
            max_output_tokens: 4096,
            delegation: DelegationConfig { allowed_targets: vec!["implementer".into(), "reviewer".into()], max_depth: 2, max_children: 5 },
        },
        FormationAgentConfig {
            id: "implementer".into(),
            name: "Implementer".into(),
            role: "software_developer".into(),
            system_prompt: "Make minimal scoped changes and return verifiable test evidence.".into(),
            tools: vec!["file.read".into(), "search.text".into(), "file.patch".into(), "process.test".into()],
            provider: None,
            model: None,
            max_output_tokens: 4096,
            delegation: DelegationConfig::default(),
        },
        FormationAgentConfig {
            id: "reviewer".into(),
            name: "Reviewer".into(),
            role: "code_reviewer".into(),
            system_prompt: "Review immutable artifacts against acceptance criteria and report blocking findings.".into(),
            tools: vec!["file.read".into(), "search.text".into(), "process.test".into()],
            provider: None,
            model: None,
            max_output_tokens: 4096,
            delegation: DelegationConfig::default(),
        },
    ]
}

fn prompt(label: &str, default: &str) -> Result<String> {
    print!("{label} [{default}]: ");
    std::io::stdout().flush()?;
    let mut value = String::new();
    std::io::stdin().lock().read_line(&mut value)?;
    let value = value.trim();
    Ok(if value.is_empty() {
        default.to_owned()
    } else {
        value.to_owned()
    })
}
fn load(path: &Path) -> Result<Config> {
    if path.exists() {
        Config::load(path).with_context(|| format!("configuration failed: {}", path.display()))
    } else {
        Config::validate(Config::default()).context("built-in configuration failed")
    }
}
async fn run(
    path: &Path,
    objective: String,
    resume: bool,
    selected_session: Option<SessionId>,
) -> Result<()> {
    let config = load(path)?;
    let default_provider = default_provider_id(&config);
    let repo = Arc::new(SqliteRepository::open(&config.state_path()).await?);
    let sessions = repo.sessions().await?;
    let session = if let Some(id) = selected_session {
        sessions
            .into_iter()
            .find(|session| session.id == id)
            .with_context(|| {
                format!(
                    "session {id} was not found; run 'voff sessions' to list available sessions"
                )
            })?
    } else if resume && !sessions.is_empty() {
        sessions
            .into_iter()
            .max_by_key(|session| session.created_at)
            .expect("non-empty checked")
    } else {
        let value = Session {
            id: SessionId::new(),
            formation_id: FormationId::new(),
            workspace: config.workspace.root.display().to_string(),
            created_at: chrono::Utc::now(),
        };
        repo.save_session(value.clone()).await?;
        value
    };
    let mut agent_profiles = Vec::new();
    let mut agent_provider_kinds = HashMap::new();
    let owner = if let Some(formation) = &config.formation {
        let mut entry = None;
        let names = config
            .agents
            .iter()
            .map(|agent| (agent.id.as_str(), agent.name.as_str()))
            .collect::<HashMap<_, _>>();
        for configured in &config.agents {
            let provider_id = configured
                .provider
                .clone()
                .unwrap_or_else(|| default_provider.clone());
            let selected_provider = provider_config(&config, &provider_id)
                .with_context(|| format!("provider route {provider_id} is missing"))?;
            let profile = AgentProfile {
                id: AgentId::new(),
                name: configured.name.clone(),
                role: configured.role.clone(),
                provider: provider_id,
                model: configured
                    .model
                    .clone()
                    .unwrap_or_else(|| model_name(selected_provider)),
                system_prompt: configured.system_prompt.clone(),
                tools: configured.tools.clone(),
                max_output_tokens: configured.max_output_tokens,
                enabled: true,
                allowed_targets: configured
                    .delegation
                    .allowed_targets
                    .iter()
                    .filter_map(|id| names.get(id.as_str()).map(|name| (*name).to_owned()))
                    .collect(),
                delegation_limits: Some(voff_core::TaskLimits {
                    max_delegation_depth: configured.delegation.max_depth,
                    max_children: configured.delegation.max_children,
                    max_duration_seconds: config.runtime.default_timeout_seconds,
                }),
            };
            if configured.id == formation.entry_agent {
                entry = Some(profile.id);
            }
            agent_provider_kinds.insert(profile.id, provider_name(selected_provider).into());
            repo.save_agent(profile.clone()).await?;
            agent_profiles.push(profile);
        }
        entry.context("validated formation entry agent missing")?
    } else {
        let profile = AgentProfile {
            id: AgentId::new(),
            name: config.agent.name.clone(),
            role: config.agent.role.clone(),
            provider: default_provider.clone(),
            model: model_name(&config.provider),
            system_prompt: config.agent.system_prompt.clone(),
            tools: config.agent.tools.clone(),
            max_output_tokens: config.agent.max_output_tokens,
            enabled: true,
            allowed_targets: vec![],
            delegation_limits: None,
        };
        let id = profile.id;
        agent_provider_kinds.insert(profile.id, provider_name(&config.provider).into());
        repo.save_agent(profile.clone()).await?;
        agent_profiles.push(profile);
        id
    };
    agent_profiles.sort_by_key(|agent| agent.id != owner);
    recover_interrupted(repo.as_ref(), session.id).await?;
    let events = repo.events(session.id).await?;
    let task_ids = events
        .iter()
        .filter_map(|event| event.task_id)
        .collect::<HashSet<_>>();
    let tasks = repo
        .tasks()
        .await?
        .into_iter()
        .filter(|task| task_ids.contains(&task.id))
        .collect::<Vec<_>>();
    let approvals = repo.approvals(session.id).await?;
    let artifacts = repo
        .artifacts()
        .await?
        .into_iter()
        .filter(|artifact| task_ids.contains(&artifact.producer))
        .collect();
    let credential_slots = credential_slots(&config, repo.as_ref()).await;
    let credential_target = provider_routes(&config)
        .into_iter()
        .find_map(|(_, configured)| {
            if let ProviderConfig::Openai { api_key_env, .. } = configured {
                credential_slots
                    .get(api_key_env)
                    .cloned()
                    .map(|slot| (api_key_env.clone(), slot))
            } else {
                None
            }
        });
    let providers = provider_registry(&config, &credential_slots)?;
    let provider = Arc::clone(
        providers
            .get(&default_provider)
            .context("default provider route is missing")?,
    );
    let agent_name = agent_profiles
        .iter()
        .find(|a| a.id == owner)
        .map(|a| a.name.clone())
        .unwrap_or_else(|| config.agent.name.clone());
    let owner_model = agent_profiles
        .iter()
        .find(|a| a.id == owner)
        .map(|a| a.model.clone())
        .unwrap_or_else(|| model_name(&config.provider));
    let workspace_root = std::fs::canonicalize(&config.workspace.root).with_context(|| {
        format!(
            "working directory is unavailable: {}",
            config.workspace.root.display()
        )
    })?;
    let workspace = ScopedWorkspace::new(&workspace_root, 256 * 1024)?;
    let workspace_context = workspace.context_bundle(200, 64 * 1024)?;
    let delegation_plan = calculator_delegation_plan(&objective, &agent_profiles);
    tui::run_configured(tui::ConfiguredRun {
        repo,
        provider,
        providers,
        default_provider: default_provider.clone(),
        session: session.id,
        owner,
        model_name: owner_model,
        system_prompt: config.agent.system_prompt,
        provider_label: if config.providers.is_empty() {
            provider_name(&config.provider).into()
        } else {
            format!(
                "multi ({})",
                config
                    .providers
                    .iter()
                    .map(|p| p.id.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        },
        agent_name,
        agent_profiles,
        objective,
        delegation_plan,
        initial_tasks: tasks,
        initial_events: events,
        initial_approvals: approvals,
        initial_artifacts: artifacts,
        capacity: config.runtime.event_channel_capacity,
        workspace_context,
        workspace_root,
        max_parallel_tasks: config
            .formation
            .as_ref()
            .map_or(1, |f| f.max_parallel_tasks),
        input_cost_per_million: config.cost.input_per_million,
        output_cost_per_million: config.cost.output_per_million,
        cost_currency: config.cost.currency,
        model_catalog: ModelCatalog::bundled().unwrap_or_default(),
        agent_provider_kinds,
        credential_target,
        team_lead: owner,
    })
    .await?;
    Ok(())
}

async fn list_sessions(path: &Path) -> Result<()> {
    let config = load(path)?;
    let repository = SqliteRepository::open(&config.state_path()).await?;
    let mut sessions = repository.sessions().await?;
    sessions.sort_by_key(|session| std::cmp::Reverse(session.created_at));
    if sessions.is_empty() {
        println!("No sessions found. Start one with 'voff run'.");
        return Ok(());
    }
    println!("SESSION\tCREATED\tWORKSPACE");
    for session in sessions {
        println!(
            "{}\t{}\t{}",
            session.id,
            session.created_at.to_rfc3339(),
            session.workspace
        );
    }
    println!("\nResume one with: voff run --session <SESSION>");
    Ok(())
}
async fn recover_interrupted<R: Repository>(repo: &R, session: SessionId) -> Result<()> {
    let task_ids = session_task_ids(repo, session).await?;
    for task in repo
        .tasks()
        .await?
        .into_iter()
        .filter(|task| task_ids.contains(&task.id) && task.status == TaskStatus::Running)
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
async fn session_task_ids<R: Repository>(
    repo: &R,
    session: SessionId,
) -> Result<HashSet<voff_core::TaskId>> {
    Ok(repo
        .events(session)
        .await?
        .into_iter()
        .filter_map(|event| event.task_id)
        .collect())
}
async fn doctor(path: &Path) -> Result<()> {
    let config = load(path)?;
    println!("voff doctor");
    println!("[ok] config_version {}", config.config_version);
    println!(
        "[ok] provider route(s): {}",
        provider_routes(&config)
            .into_iter()
            .map(|(id, value)| format!("{id}={}", provider_name(value)))
            .collect::<Vec<_>>()
            .join(", ")
    );
    let repo = SqliteRepository::open(&config.state_path()).await?;
    println!(
        "[ok] SQLite state {} ({} session(s))",
        config.state_path().display(),
        repo.sessions().await?.len()
    );
    println!("[ok] scoped tools: {}", config.agent.tools.join(", "));
    println!("[ok] shell disabled; patch writes require digest check and explicit approval");
    let slots = credential_slots(&config, &repo).await;
    for (id, configured) in provider_routes(&config) {
        match provider(configured, &slots) {
            Ok(selected) => {
                let capabilities = selected.descriptor().capabilities;
                println!(
                    "[ok] {id} capabilities: streaming={}, tools={}, structured_output={}, cancellation={}",
                    capabilities.streaming,
                    capabilities.tools,
                    capabilities.structured_output,
                    capabilities.cancellation
                );
                if !matches!(configured, ProviderConfig::Mock) {
                    match selected.models().await {
                        Ok(models) => println!("[ok] {id} reachable ({} model(s))", models.len()),
                        Err(error) => println!("[action required] {id}: {error}"),
                    }
                }
            }
            Err(error) => println!("[action required] {id}: {error}"),
        }
    }
    match (
        config.cost.input_per_million,
        config.cost.output_per_million,
    ) {
        (Some(input), Some(output)) => println!(
            "[ok] estimated cost rates: {input}/{output} {} per 1M input/output tokens",
            config.cost.currency
        ),
        _ => println!(
            "[ok] catalog cost estimates enabled where provider/model pricing is available"
        ),
    }
    Ok(())
}

#[derive(Serialize)]
struct DiagnosticBundle {
    format_version: u16,
    session: Session,
    agents: Vec<AgentProfile>,
    tasks: Vec<voff_core::Task>,
    events: Vec<CanonicalEvent>,
    approvals: Vec<voff_core::Approval>,
    artifacts: Vec<voff_core::Artifact>,
    dependencies: Vec<voff_core::TaskDependency>,
}

async fn export(path: &Path, output: &Path) -> Result<()> {
    let config = load(path)?;
    let repo = SqliteRepository::open(&config.state_path()).await?;
    let session = repo
        .sessions()
        .await?
        .into_iter()
        .max_by_key(|session| session.created_at)
        .context("no stored session is available to export")?;
    let events = repo.events(session.id).await?;
    let session_id = session.id;
    let task_ids = events
        .iter()
        .filter_map(|event| event.task_id)
        .collect::<HashSet<_>>();
    let tasks = repo
        .tasks()
        .await?
        .into_iter()
        .filter(|task| task_ids.contains(&task.id))
        .collect::<Vec<_>>();
    let owner_ids = tasks.iter().map(|task| task.owner).collect::<HashSet<_>>();
    let bundle = DiagnosticBundle {
        format_version: 1,
        session,
        agents: repo
            .agents()
            .await?
            .into_iter()
            .filter(|agent| owner_ids.contains(&agent.id))
            .collect(),
        tasks,
        events,
        approvals: repo.approvals(session_id).await?,
        artifacts: repo
            .artifacts()
            .await?
            .into_iter()
            .filter(|artifact| task_ids.contains(&artifact.producer))
            .collect(),
        dependencies: repo
            .dependencies()
            .await?
            .into_iter()
            .filter(|dependency| {
                task_ids.contains(&dependency.task_id) && task_ids.contains(&dependency.depends_on)
            })
            .collect(),
    };
    let mut value = serde_json::to_value(bundle)?;
    redact_json(&mut value, &configured_secrets(&config, &repo).await);
    if let Some(parent) = output.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(output)
        .with_context(|| {
            format!(
                "refusing to overwrite diagnostic output: {}",
                output.display()
            )
        })?;
    serde_json::to_writer_pretty(BufWriter::new(file), &value)?;
    println!("[ok] redacted diagnostic written to {}", output.display());
    Ok(())
}

async fn configured_secrets(config: &Config, repository: &SqliteRepository) -> Vec<String> {
    let mut secrets = Vec::new();
    for (_, provider) in provider_routes(config) {
        if let ProviderConfig::Openai { api_key_env, .. } = provider
            && let Ok(secret) = credential::resolve(api_key_env, repository).await
            && !secret.is_empty()
        {
            secrets.push(secret);
        }
    }
    secrets
}

fn redact_json(value: &mut serde_json::Value, secrets: &[String]) {
    match value {
        serde_json::Value::String(text) => *text = redact_text(text, secrets),
        serde_json::Value::Array(values) => {
            for value in values {
                redact_json(value, secrets);
            }
        }
        serde_json::Value::Object(values) => {
            for value in values.values_mut() {
                redact_json(value, secrets);
            }
        }
        _ => {}
    }
}

fn redact_text(value: &str, secrets: &[String]) -> String {
    let mut redacted = secrets.iter().fold(value.to_owned(), |text, secret| {
        text.replace(secret, "[REDACTED]")
    });
    redacted = redacted
        .split_whitespace()
        .map(|token| {
            let upper = token.to_ascii_uppercase();
            if token.starts_with("sk-")
                || upper.contains("API_KEY=")
                || upper.contains("TOKEN=")
                || upper.contains("PASSWORD=")
                || upper.starts_with("BEARER")
            {
                "[REDACTED]"
            } else {
                token
            }
        })
        .collect::<Vec<_>>()
        .join(" ");
    redacted
}

fn provider_routes(config: &Config) -> Vec<(&str, &ProviderConfig)> {
    if config.providers.is_empty() {
        vec![(provider_name(&config.provider), &config.provider)]
    } else {
        config
            .providers
            .iter()
            .map(|provider| (provider.id.as_str(), &provider.config))
            .collect()
    }
}

fn default_provider_id(config: &Config) -> String {
    config
        .providers
        .first()
        .map_or_else(|| provider_name(&config.provider).into(), |p| p.id.clone())
}

fn provider_config<'a>(config: &'a Config, id: &str) -> Option<&'a ProviderConfig> {
    provider_routes(config)
        .into_iter()
        .find_map(|(candidate, provider)| (candidate == id).then_some(provider))
}

async fn credential_slots(
    config: &Config,
    repository: &SqliteRepository,
) -> HashMap<String, credential::CredentialSlot> {
    let mut slots = HashMap::new();
    for (_, configured) in provider_routes(config) {
        if let ProviderConfig::Openai { api_key_env, .. } = configured
            && !slots.contains_key(api_key_env)
        {
            let secret = credential::resolve(api_key_env, repository).await.ok();
            slots.insert(api_key_env.clone(), credential::CredentialSlot::new(secret));
        }
    }
    slots
}

fn provider_registry(
    config: &Config,
    credential_slots: &HashMap<String, credential::CredentialSlot>,
) -> Result<HashMap<String, Arc<dyn ModelProvider>>> {
    provider_routes(config)
        .into_iter()
        .map(|(id, configured)| Ok((id.to_owned(), provider(configured, credential_slots)?)))
        .collect()
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
fn provider(
    value: &ProviderConfig,
    credential_slots: &HashMap<String, credential::CredentialSlot>,
) -> Result<Arc<dyn ModelProvider>> {
    let inner: Arc<dyn ModelProvider> = match value {
        ProviderConfig::Mock => Arc::new(MockProvider::new(vec![
            MockStep::Event(voff_provider::ProviderEvent::TextDelta(
                "configured mock response".into(),
            )),
            MockStep::Event(voff_provider::ProviderEvent::Usage(voff_provider::Usage {
                input_tokens: 4,
                output_tokens: 3,
            })),
            MockStep::Event(voff_provider::ProviderEvent::Completed),
        ])),
        ProviderConfig::Ollama { base_url, .. } => Arc::new(OllamaProvider::new(base_url.clone())?),
        ProviderConfig::Openai { api_key_env, .. } => {
            let slot = credential_slots
                .get(api_key_env)
                .cloned()
                .unwrap_or_default();
            Arc::new(OpenAiProvider::new_deferred(
                "https://api.openai.com",
                Arc::new(move || slot.get()),
            )?)
        }
    };
    Ok(Arc::new(ResilientProvider::new(
        inner,
        ResiliencePolicy::default(),
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    use voff_core::{Task, TaskLimits};
    use voff_storage::InMemoryRepository;

    #[test]
    fn run_resume_is_explicit() {
        let cli = Cli::try_parse_from(["voff", "run", "--resume"]).unwrap();
        assert!(matches!(
            cli.command,
            Some(Command::Run {
                resume: true,
                session: None,
                ..
            })
        ));
        let cli = Cli::try_parse_from(["voff", "run"]).unwrap();
        assert!(matches!(
            cli.command,
            Some(Command::Run { resume: false, .. })
        ));
    }

    #[test]
    fn default_calculator_requires_all_enabled_specialist_handoffs() {
        let cli = Cli::try_parse_from(["voff", "run"]).unwrap();
        let Some(Command::Run { objective, .. }) = cli.command else {
            panic!("run command expected");
        };

        assert_eq!(objective, DEFAULT_OBJECTIVE);
        assert!(objective.contains("every enabled specialist"));
        assert!(objective.contains("Backend Developer owns server.py"));
        assert!(objective.contains("Frontend Developer owns index.html"));
        assert!(objective.contains("Every delegated task must complete"));
        let profile = |name: &str, role: &str| AgentProfile {
            id: AgentId::new(),
            name: name.into(),
            role: role.into(),
            provider: "mock".into(),
            model: "mock-v1".into(),
            system_prompt: String::new(),
            tools: vec![],
            max_output_tokens: 4096,
            enabled: true,
            allowed_targets: vec![],
            delegation_limits: None,
        };
        let plan = calculator_delegation_plan(
            &objective,
            &[
                profile("API Specialist", "backend_developer"),
                profile("UI Specialist", "frontend_developer"),
            ],
        );
        assert_eq!(plan.len(), 2);
        assert_eq!(plan[0].target_agent, "API Specialist");
        assert_eq!(plan[1].target_agent, "UI Specialist");
    }

    #[tokio::test]
    async fn openai_credential_slot_loads_from_local_sqlite() {
        let repository = SqliteRepository::open_in_memory().await.unwrap();
        repository
            .save_credential("VOFF_TEST_LOCAL_OPENAI", "sk-test-local")
            .await
            .unwrap();
        let config = Config {
            provider: ProviderConfig::Openai {
                model: "gpt-5-mini".into(),
                api_key_env: "VOFF_TEST_LOCAL_OPENAI".into(),
            },
            ..Config::default()
        };

        let slots = credential_slots(&config, &repository).await;

        assert_eq!(
            slots
                .get("VOFF_TEST_LOCAL_OPENAI")
                .and_then(credential::CredentialSlot::get)
                .as_deref(),
            Some("sk-test-local")
        );
    }

    #[tokio::test]
    async fn recovery_only_fails_tasks_from_selected_session() {
        let repo = InMemoryRepository::default();
        let selected = SessionId::new();
        let other = SessionId::new();
        let mut selected_task =
            Task::root(AgentId::new(), "selected", vec![], TaskLimits::default());
        selected_task.transition(TaskStatus::Ready).unwrap();
        selected_task.transition(TaskStatus::Running).unwrap();
        let mut other_task = Task::root(AgentId::new(), "other", vec![], TaskLimits::default());
        other_task.transition(TaskStatus::Ready).unwrap();
        other_task.transition(TaskStatus::Running).unwrap();
        repo.save_task(selected_task.clone()).await.unwrap();
        repo.save_task(other_task.clone()).await.unwrap();
        for (session, task) in [(selected, selected_task.id), (other, other_task.id)] {
            repo.append_event(CanonicalEvent {
                id: EventId::new(),
                session_id: session,
                task_id: Some(task),
                created_at: chrono::Utc::now(),
                kind: CanonicalEventKind::ProviderStarted,
            })
            .await
            .unwrap();
        }

        recover_interrupted(&repo, selected).await.unwrap();

        assert_eq!(
            repo.task(selected_task.id).await.unwrap().unwrap().status,
            TaskStatus::Failed
        );
        assert_eq!(
            repo.task(other_task.id).await.unwrap().unwrap().status,
            TaskStatus::Running
        );
    }

    #[test]
    fn diagnostic_redaction_removes_exact_and_common_secret_forms() {
        let secret = "sk-real-secret-value".to_owned();
        let mut value = serde_json::json!({
            "event": format!("OPENAI_API_KEY={secret}"),
            "nested": ["TOKEN=abc", "safe text"]
        });
        redact_json(&mut value, std::slice::from_ref(&secret));
        let encoded = serde_json::to_string(&value).unwrap();
        assert!(!encoded.contains(&secret));
        assert!(!encoded.contains("TOKEN=abc"));
        assert!(encoded.contains("safe text"));
    }

    #[test]
    fn init_writes_valid_secret_free_config_and_refuses_overwrite() {
        let directory = tempfile::tempdir().unwrap();
        let output = directory.path().join("config.toml");
        init(
            Some(InitTemplate::Solo),
            Some(InitProvider::Openai),
            Some("gpt-test".into()),
            Some(8192),
            &output,
        )
        .unwrap();
        let config = Config::load(&output).unwrap();
        assert!(matches!(
            config.provider,
            ProviderConfig::Openai {
                ref model,
                ref api_key_env
            } if model == "gpt-test" && api_key_env == "OPENAI_API_KEY"
        ));
        assert_eq!(config.agent.max_output_tokens, 8192);
        let contents = std::fs::read_to_string(&output).unwrap();
        assert!(!contents.contains("sk-"));
        assert!(
            init(
                Some(InitTemplate::Solo),
                Some(InitProvider::Ollama),
                Some("qwen3:4b".into()),
                None,
                &output
            )
            .is_err()
        );
    }

    #[test]
    fn team_init_writes_a_valid_three_agent_formation() {
        let directory = tempfile::tempdir().unwrap();
        let output = directory.path().join("team.toml");
        init(
            Some(InitTemplate::Team),
            Some(InitProvider::Ollama),
            Some("qwen3:4b".into()),
            Some(16_384),
            &output,
        )
        .unwrap();
        let config = Config::load(&output).unwrap();
        assert_eq!(config.agents.len(), 3);
        assert!(
            config
                .agents
                .iter()
                .all(|agent| agent.max_output_tokens == 16_384)
        );
        assert_eq!(config.formation.unwrap().entry_agent, "lead");
    }

    #[test]
    fn guided_setup_preserves_each_developers_provider_model_and_role() {
        let setup = SetupDraft {
            developers: vec![
                onboarding::DeveloperDraft::new(
                    "Ada",
                    DeveloperRole::Lead,
                    ProviderChoice::OpenAi,
                    "gpt-lead",
                    16_384,
                ),
                onboarding::DeveloperDraft::new(
                    "Linus",
                    DeveloperRole::Backend,
                    ProviderChoice::Ollama,
                    "qwen-backend",
                    8192,
                ),
                onboarding::DeveloperDraft::new(
                    "Grace",
                    DeveloperRole::Reviewer,
                    ProviderChoice::OpenAi,
                    "gpt-review",
                    4096,
                ),
            ],
            working_directory: "/tmp/voff-guided-team".into(),
            openai_token: Some("sk-must-never-be-serialized".into()),
        };

        let config = config_from_setup(&setup).unwrap();

        assert_eq!(config.providers.len(), 2);
        assert_eq!(config.agents.len(), 3);
        assert_eq!(config.agents[0].model.as_deref(), Some("gpt-lead"));
        assert_eq!(config.agents[1].model.as_deref(), Some("qwen-backend"));
        assert_eq!(config.agents[2].model.as_deref(), Some("gpt-review"));
        assert_eq!(config.agents[0].max_output_tokens, 16_384);
        assert_eq!(config.agents[1].max_output_tokens, 8192);
        assert_eq!(config.agents[1].role, "backend_developer");
        assert_eq!(
            config.workspace.root,
            PathBuf::from("/tmp/voff-guided-team")
        );
        assert_eq!(
            config.agents[0].delegation.allowed_targets,
            vec!["developer-2", "developer-3"]
        );
        assert!(config.agents[0].tools.contains(&"task.delegate".into()));
        assert!(
            !toml::to_string(&config)
                .unwrap()
                .contains("sk-must-never-be-serialized")
        );
    }

    #[test]
    fn guided_solo_setup_uses_the_same_safe_configuration_path() {
        let setup = SetupDraft {
            developers: vec![onboarding::DeveloperDraft::new(
                "Solo",
                DeveloperRole::Lead,
                ProviderChoice::Ollama,
                "local-model",
                2048,
            )],
            working_directory: ".".into(),
            openai_token: None,
        };

        let config = config_from_setup(&setup).unwrap();

        assert!(config.formation.is_none());
        assert!(config.agents.is_empty());
        assert_eq!(config.agent.name, "Solo");
        assert_eq!(config.agent.max_output_tokens, 2048);
        assert!(config.agent.tools.contains(&"file.patch".into()));
        assert!(config.agent.tools.contains(&"process.test".into()));
        assert!(!config.agent.tools.contains(&"task.delegate".into()));
        assert!(matches!(
            config.provider,
            ProviderConfig::Ollama { ref model, .. } if model == "local-model"
        ));
    }

    #[test]
    fn guided_setup_rejects_missing_or_duplicate_team_leads() {
        let specialist_only = SetupDraft {
            developers: vec![onboarding::DeveloperDraft::new(
                "Backend",
                DeveloperRole::Backend,
                ProviderChoice::Ollama,
                "local-model",
                2048,
            )],
            working_directory: ".".into(),
            openai_token: None,
        };
        assert!(config_from_setup(&specialist_only).is_err());

        let duplicate = SetupDraft {
            developers: vec![
                onboarding::DeveloperDraft::new(
                    "Lead One",
                    DeveloperRole::Lead,
                    ProviderChoice::Ollama,
                    "local-model",
                    2048,
                ),
                onboarding::DeveloperDraft::new(
                    "Lead Two",
                    DeveloperRole::Lead,
                    ProviderChoice::Ollama,
                    "local-model",
                    2048,
                ),
            ],
            working_directory: ".".into(),
            openai_token: None,
        };
        assert!(config_from_setup(&duplicate).is_err());
    }

    #[test]
    fn session_selection_parses_and_conflicts_with_latest_resume() {
        let id = SessionId::new();
        let cli = Cli::try_parse_from(["voff", "run", "--session", &id.to_string()]).unwrap();
        assert!(matches!(
            cli.command,
            Some(Command::Run { session: Some(value), .. }) if value == id
        ));
        assert!(
            Cli::try_parse_from(["voff", "run", "--resume", "--session", &id.to_string()]).is_err()
        );
    }

    #[test]
    fn redaction_fuzz_corpus_never_leaks_configured_secret() {
        let secret = "sk-fuzz-secret";
        let mut state = 0xa341_316c_u32;
        for length in 0..1024 {
            let mut value = String::with_capacity(length + secret.len());
            for _ in 0..length {
                state ^= state << 13;
                state ^= state >> 17;
                state ^= state << 5;
                value.push(char::from_u32(32 + state % 95).unwrap());
            }
            let midpoint = value.len() / 2;
            value.insert_str(midpoint, secret);
            let redacted = redact_text(&value, &[secret.into()]);
            assert!(!redacted.contains(secret));
        }
    }
}
