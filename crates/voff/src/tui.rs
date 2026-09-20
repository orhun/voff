//! Responsive Ratatui shell and pure action reducer.
use crate::model_catalog::ModelCatalog;
use crossterm::{
    event::{
        self, DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
        Event, KeyCode, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
    },
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{
    Terminal,
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Margin, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, BorderType, Borders, Clear, List, ListItem, Paragraph, Wrap},
};
use ratatui_spinner::{FluxFrames, FluxSpinner};
use ratatui_textarea::TextArea;
use std::{collections::HashMap, io, panic, path::PathBuf, sync::Arc, time::Duration};
use thiserror::Error;
use tokio::sync::mpsc;
use voff_core::{
    AgentId, AgentProfile, Approval, ApprovalDecision, Artifact, CanonicalEventKind, SessionId,
    Task, TaskStatus,
};
use voff_engine::{DelegationProposal, Engine, EngineEvent};
use voff_provider::ProviderHealth;
use voff_storage::{Repository, SqliteRepository};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Panel {
    Tasks,
    Details,
    Agents,
    Events,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Action {
    Up,
    Down,
    NextPanel,
    ToggleHelp,
    Compose,
    ComposeCommand,
    AddCredential,
    StartMock,
    Cancel,
    Approve,
    Deny,
    ToggleEvents,
    PageUp,
    PageDown,
    Escape,
    Quit,
    Mouse(MouseEvent),
    None,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct AgentPosition {
    x: i16,
    y: i16,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct DragState {
    index: usize,
    column: u16,
    row: u16,
}

#[derive(Debug, Eq, PartialEq)]
enum ComposerEvent {
    Ignored,
    Consumed,
    Submit(String),
}

pub struct AppModel {
    pub session: SessionId,
    pub tasks: Vec<Task>,
    pub events: Vec<String>,
    pub selected: usize,
    pub panel: Panel,
    pub help: bool,
    pub quit: bool,
    pub provider_label: String,
    pub agent_name: String,
    pub model_label: String,
    pub agents: Vec<AgentProfile>,
    pub approvals: Vec<Approval>,
    pub artifacts: Vec<Artifact>,
    pub provider_health: ProviderHealth,
    pub input_cost_per_million: Option<f64>,
    pub output_cost_per_million: Option<f64>,
    pub cost_currency: String,
    pub estimated_cost: Option<f64>,
    pub model_catalog: ModelCatalog,
    pub agent_provider_kinds: HashMap<AgentId, String>,
    pub cost_partial: bool,
    pub workspace_root: PathBuf,
    pub event_viewer: bool,
    pub event_scroll: u16,
    event_follow: bool,
    credential_target: Option<(String, crate::credential::CredentialSlot)>,
    credential_input: Option<String>,
    credential_saving: bool,
    credential_error: Option<String>,
    composer: TextArea<'static>,
    composer_active: bool,
    composer_notice: Option<String>,
    composer_hitbox: Rect,
    selected_agent: usize,
    agent_positions: Vec<AgentPosition>,
    agent_hitboxes: Vec<Rect>,
    dragging_agent: Option<DragState>,
    streaming_text: bool,
    animation_tick: u64,
}
impl AppModel {
    #[must_use]
    pub fn new(session: SessionId) -> Self {
        let mut composer = TextArea::default();
        composer.set_cursor_line_style(Style::default());
        composer.set_placeholder_text("Message the Team Lead, or type /tasks");
        Self {
            session,
            tasks: vec![],
            events: vec![],
            selected: 0,
            panel: Panel::Tasks,
            help: false,
            quit: false,
            provider_label: "mock".into(),
            agent_name: "Mock Agent".into(),
            model_label: "mock-v1".into(),
            agents: vec![],
            approvals: vec![],
            artifacts: vec![],
            provider_health: ProviderHealth::default(),
            input_cost_per_million: None,
            output_cost_per_million: None,
            cost_currency: "USD".into(),
            estimated_cost: None,
            model_catalog: ModelCatalog::default(),
            agent_provider_kinds: HashMap::new(),
            cost_partial: false,
            workspace_root: ".".into(),
            event_viewer: false,
            event_scroll: 0,
            event_follow: true,
            credential_target: None,
            credential_input: None,
            credential_saving: false,
            credential_error: None,
            composer,
            composer_active: false,
            composer_notice: None,
            composer_hitbox: Rect::default(),
            selected_agent: 0,
            agent_positions: vec![],
            agent_hitboxes: vec![],
            dragging_agent: None,
            streaming_text: false,
            animation_tick: 0,
        }
    }
    pub fn reduce(&mut self, action: Action) {
        match action {
            Action::Up => {
                if self.event_viewer || self.panel == Panel::Events {
                    self.event_scroll = self.event_scroll.saturating_sub(1);
                    self.event_follow = self.event_scroll == 0;
                } else if self.panel == Panel::Agents {
                    self.selected_agent = self.selected_agent.saturating_sub(1);
                } else {
                    self.selected = self.selected.saturating_sub(1);
                }
            }
            Action::Down => {
                if self.event_viewer || self.panel == Panel::Events {
                    self.event_scroll = self
                        .event_scroll
                        .saturating_add(1)
                        .min(self.event_scroll_limit());
                    self.event_follow = self.event_scroll == 0;
                } else if self.panel == Panel::Agents {
                    if self.selected_agent + 1 < self.agents.len() {
                        self.selected_agent += 1;
                    }
                } else if self.selected + 1 < self.tasks.len() {
                    self.selected += 1
                }
            }
            Action::PageUp => {
                self.event_scroll = self.event_scroll.saturating_sub(10);
                self.event_follow = self.event_scroll == 0;
            }
            Action::PageDown => {
                self.event_scroll = self
                    .event_scroll
                    .saturating_add(10)
                    .min(self.event_scroll_limit());
                self.event_follow = self.event_scroll == 0;
            }
            Action::NextPanel => {
                self.panel = match self.panel {
                    Panel::Tasks => Panel::Details,
                    Panel::Details => Panel::Agents,
                    Panel::Agents => Panel::Events,
                    Panel::Events => Panel::Tasks,
                };
                if self.panel == Panel::Events {
                    self.follow_newest_event();
                }
            }
            Action::ToggleHelp => self.help = !self.help,
            Action::Compose => {
                self.composer_active = true;
                self.composer_notice = None;
            }
            Action::ComposeCommand => {
                self.composer_active = true;
                self.composer_notice = None;
                if self.composer.lines() == [""] {
                    self.composer.insert_str("/");
                }
            }
            Action::AddCredential => {
                if self
                    .credential_target
                    .as_ref()
                    .is_some_and(|(_, slot)| !slot.is_available())
                {
                    self.credential_input = Some(String::new());
                    self.credential_error = None;
                }
            }
            Action::ToggleEvents => {
                self.event_viewer = !self.event_viewer;
                self.panel = Panel::Events;
                self.follow_newest_event();
            }
            Action::Escape => {
                if self.event_viewer {
                    self.event_viewer = false;
                } else if self.help {
                    self.help = false;
                } else {
                    self.quit = true;
                }
            }
            Action::Quit => self.quit = true,
            Action::Mouse(mouse) => self.handle_mouse(mouse),
            Action::StartMock | Action::Cancel | Action::Approve | Action::Deny | Action::None => {}
        }
    }

    fn handle_mouse(&mut self, mouse: MouseEvent) {
        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                if self
                    .composer_hitbox
                    .contains((mouse.column, mouse.row).into())
                {
                    self.composer_active = true;
                    self.composer_notice = None;
                    self.dragging_agent = None;
                    return;
                }
                if let Some((index, _)) = self
                    .agent_hitboxes
                    .iter()
                    .enumerate()
                    .find(|(_, area)| area.contains((mouse.column, mouse.row).into()))
                {
                    self.selected_agent = index;
                    self.panel = Panel::Agents;
                    self.dragging_agent = Some(DragState {
                        index,
                        column: mouse.column,
                        row: mouse.row,
                    });
                }
            }
            MouseEventKind::Drag(MouseButton::Left) => {
                if let Some(drag) = &mut self.dragging_agent
                    && let Some(position) = self.agent_positions.get_mut(drag.index)
                {
                    let dx = i32::from(mouse.column) - i32::from(drag.column);
                    let dy = i32::from(mouse.row) - i32::from(drag.row);
                    position.x = (i32::from(position.x) + dx)
                        .clamp(i32::from(i16::MIN), i32::from(i16::MAX))
                        as i16;
                    position.y = (i32::from(position.y) + dy)
                        .clamp(i32::from(i16::MIN), i32::from(i16::MAX))
                        as i16;
                    drag.column = mouse.column;
                    drag.row = mouse.row;
                }
            }
            MouseEventKind::Up(MouseButton::Left) => self.dragging_agent = None,
            _ => {}
        }
    }

    fn handle_credential_event(
        &mut self,
        event: &Event,
    ) -> (
        bool,
        Option<(String, crate::credential::CredentialSlot, String)>,
    ) {
        let Some(input) = self.credential_input.as_mut() else {
            return (false, None);
        };
        if self.credential_saving {
            return (true, None);
        }
        match event {
            Event::Paste(value) => {
                input.push_str(value.trim());
                self.credential_error = None;
                (true, None)
            }
            Event::Key(key) if key.kind == KeyEventKind::Press => match key.code {
                KeyCode::Esc => {
                    input.clear();
                    self.credential_input = None;
                    self.credential_error = None;
                    (true, None)
                }
                KeyCode::Backspace => {
                    input.pop();
                    self.credential_error = None;
                    (true, None)
                }
                KeyCode::Enter if input.trim().is_empty() => {
                    self.credential_error =
                        Some("Paste a token, or press Esc to keep using voff without one.".into());
                    (true, None)
                }
                KeyCode::Enter => {
                    let token = std::mem::take(input);
                    let submission = self
                        .credential_target
                        .as_ref()
                        .map(|(reference, slot)| (reference.clone(), slot.clone(), token));
                    (true, submission)
                }
                KeyCode::Char(value) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                    input.push(value);
                    self.credential_error = None;
                    (true, None)
                }
                _ => (true, None),
            },
            _ => (true, None),
        }
    }

    fn handle_composer_event(&mut self, event: &Event) -> ComposerEvent {
        if !self.composer_active {
            return ComposerEvent::Ignored;
        }
        match event {
            Event::Paste(value) => {
                self.composer.insert_str(value);
                ComposerEvent::Consumed
            }
            Event::Key(key) if key.kind == KeyEventKind::Press => match key.code {
                KeyCode::Esc => {
                    self.composer_active = false;
                    self.composer_notice = Some("Draft kept. Press Enter to continue.".into());
                    ComposerEvent::Consumed
                }
                KeyCode::Enter if !key.modifiers.contains(KeyModifiers::SHIFT) => {
                    let message = self.composer.lines().join("\n").trim().to_owned();
                    if message.is_empty() {
                        self.composer_notice = Some("Type a message or /tasks first.".into());
                        ComposerEvent::Consumed
                    } else if message.len() > 8_000 {
                        self.composer_notice = Some("Keep messages under 8,000 bytes.".into());
                        ComposerEvent::Consumed
                    } else {
                        self.composer.clear();
                        self.composer_active = false;
                        ComposerEvent::Submit(message)
                    }
                }
                _ => {
                    self.composer.input(*key);
                    ComposerEvent::Consumed
                }
            },
            _ => ComposerEvent::Consumed,
        }
    }

    fn list_current_tasks(&mut self) {
        self.panel = Panel::Tasks;
        self.event_viewer = false;
        self.selected = self.selected.min(self.tasks.len().saturating_sub(1));
        self.composer_notice = Some(format!(
            "Showing {} current task{}.",
            self.tasks.len(),
            if self.tasks.len() == 1 { "" } else { "s" }
        ));
    }
    pub fn apply_event(&mut self, event: EngineEvent) {
        match event {
            EngineEvent::TaskUpdated(task) => {
                if let Some(old) = self.tasks.iter_mut().find(|x| x.id == task.id) {
                    *old = task
                } else {
                    self.tasks.push(task)
                }
            }
            EngineEvent::Canonical(e) => {
                if let CanonicalEventKind::Usage {
                    input_tokens,
                    output_tokens,
                } = &e.kind
                {
                    if let Some(amount) = self.usage_cost(e.task_id, *input_tokens, *output_tokens)
                    {
                        *self.estimated_cost.get_or_insert(0.0) += amount;
                    } else {
                        self.cost_partial = true;
                    }
                }
                self.push_event(&e.kind)
            }
            EngineEvent::ApprovalUpdated(approval) => {
                if let Some(old) = self.approvals.iter_mut().find(|v| v.id == approval.id) {
                    *old = approval
                } else {
                    self.approvals.push(approval)
                }
            }
            EngineEvent::ArtifactUpdated(artifact) => {
                if let Some(old) = self.artifacts.iter_mut().find(|v| v.id == artifact.id) {
                    *old = artifact
                } else {
                    self.artifacts.push(artifact)
                }
            }
            EngineEvent::ProviderHealth(health) => self.provider_health = health,
        }
        if self.events.len() > 500 {
            self.events.drain(..self.events.len() - 500);
        }
    }
    fn usage_cost(
        &self,
        task_id: Option<voff_core::TaskId>,
        input_tokens: u64,
        output_tokens: u64,
    ) -> Option<f64> {
        match (self.input_cost_per_million, self.output_cost_per_million) {
            (Some(input_rate), Some(output_rate)) => Some(
                input_tokens as f64 * input_rate / 1_000_000.0
                    + output_tokens as f64 * output_rate / 1_000_000.0,
            ),
            (Some(_), None) | (None, Some(_)) => None,
            (None, None) => {
                let owner = self
                    .tasks
                    .iter()
                    .find(|task| Some(task.id) == task_id)?
                    .owner;
                let agent = self.agents.iter().find(|agent| agent.id == owner)?;
                let provider = self.agent_provider_kinds.get(&owner)?;
                self.model_catalog
                    .estimate_usd(provider, &agent.model, input_tokens, output_tokens)
            }
        }
    }
    fn pending_approval(&self) -> Option<&Approval> {
        self.approvals
            .iter()
            .find(|v| v.decision == ApprovalDecision::Pending)
    }
    fn push_event(&mut self, kind: &CanonicalEventKind) {
        if let CanonicalEventKind::TextDelta(delta) = kind {
            let clean = sanitize(delta);
            if self.streaming_text {
                if let Some(last) = self.events.last_mut() {
                    last.push_str(&clean);
                }
            } else {
                self.events.push(clean);
                self.streaming_text = true;
            }
        } else {
            self.streaming_text = false;
            self.events.push(format_event(kind));
        }
        self.on_events_changed();
    }

    fn push_ui_event(&mut self, message: String) {
        self.streaming_text = false;
        self.events.push(message);
        self.on_events_changed();
    }

    fn on_events_changed(&mut self) {
        if self.event_follow {
            self.event_scroll = 0;
        } else {
            self.event_scroll = self.event_scroll.min(self.event_scroll_limit());
        }
    }

    fn follow_newest_event(&mut self) {
        self.event_follow = true;
        self.event_scroll = 0;
    }
    #[must_use]
    pub fn selected_task(&self) -> Option<&Task> {
        self.tasks.get(self.selected)
    }
    fn event_scroll_limit(&self) -> u16 {
        self.events
            .iter()
            .map(|event| event.chars().count().div_ceil(40).saturating_add(2))
            .sum::<usize>()
            .min(usize::from(u16::MAX)) as u16
    }
}
#[must_use]
pub const fn action_for_key(code: KeyCode) -> Action {
    match code {
        KeyCode::Up | KeyCode::Char('k') => Action::Up,
        KeyCode::Down | KeyCode::Char('j') => Action::Down,
        KeyCode::Tab => Action::NextPanel,
        KeyCode::Char('?') => Action::ToggleHelp,
        KeyCode::Enter | KeyCode::Char('t') => Action::Compose,
        KeyCode::Char('/') => Action::ComposeCommand,
        KeyCode::Char('a') => Action::AddCredential,
        KeyCode::Char('r') => Action::StartMock,
        KeyCode::Char('c') => Action::Cancel,
        KeyCode::Char('y') => Action::Approve,
        KeyCode::Char('n') => Action::Deny,
        KeyCode::Char('e') => Action::ToggleEvents,
        KeyCode::PageUp => Action::PageUp,
        KeyCode::PageDown => Action::PageDown,
        KeyCode::Esc => Action::Escape,
        KeyCode::Char('q') => Action::Quit,
        _ => Action::None,
    }
}

fn action_for_event(value: Event) -> Action {
    match value {
        Event::Key(key) => action_for_key(key.code),
        Event::Mouse(mouse) => Action::Mouse(mouse),
        Event::FocusGained | Event::FocusLost | Event::Paste(_) | Event::Resize(_, _) => {
            Action::None
        }
    }
}

fn format_event(kind: &CanonicalEventKind) -> String {
    match kind {
        CanonicalEventKind::TaskCreated => "task created".into(),
        CanonicalEventKind::TaskTransition { from, to } => format!("task: {from:?} -> {to:?}"),
        CanonicalEventKind::ProviderStarted => "provider started".into(),
        CanonicalEventKind::TextDelta(s) => sanitize(s),
        CanonicalEventKind::Usage {
            input_tokens,
            output_tokens,
        } => format!("usage: {input_tokens} in / {output_tokens} out"),
        CanonicalEventKind::ProviderCompleted => "provider completed".into(),
        CanonicalEventKind::ProviderFailed(e) => format!("provider failed: {}", sanitize(e)),
        CanonicalEventKind::TaskCancelled => "task cancelled".into(),
        CanonicalEventKind::ToolProposed { name, .. } => format!("tool proposed: {name}"),
        CanonicalEventKind::ApprovalRequested { summary, .. } => {
            format!("approval requested: {}", sanitize(summary))
        }
        CanonicalEventKind::ApprovalDecided { decision, .. } => format!("approval: {decision:?}"),
        CanonicalEventKind::ArtifactPublished { kind, .. } => format!("artifact published: {kind}"),
        CanonicalEventKind::PatchConflict { path, .. } => {
            format!("patch conflict: {}", sanitize(path))
        }
        CanonicalEventKind::ProcessFinished {
            program,
            exit_code,
            truncated,
        } => format!(
            "process {program}: exit {exit_code:?}{}",
            if *truncated { " (truncated)" } else { "" }
        ),
        CanonicalEventKind::TaskDelegated { parent, child, .. } => {
            format!("delegated: {parent} -> {child}")
        }
        CanonicalEventKind::DependencyAdded { task, depends_on } => {
            format!("dependency: {task} waits for {depends_on}")
        }
        CanonicalEventKind::TaskResultPublished { summary, .. } => {
            format!("result: {}", sanitize(summary))
        }
    }
}
fn sanitize(value: &str) -> String {
    let clean: String = value
        .chars()
        .filter(|c| !c.is_control() || *c == '\n' || *c == '\t')
        .collect();
    let mut output = String::with_capacity(clean.len());
    let mut token = String::new();
    for character in clean.chars() {
        if character.is_whitespace() {
            redact_token(&mut output, &token);
            token.clear();
            output.push(character);
        } else {
            token.push(character);
        }
    }
    redact_token(&mut output, &token);
    output
}

fn redact_token(output: &mut String, token: &str) {
    let upper = token.to_ascii_uppercase();
    if upper.contains("API_KEY=")
        || upper.contains("TOKEN=")
        || upper.contains("PASSWORD=")
        || token.starts_with("sk-")
    {
        output.push_str("[REDACTED]");
    } else {
        output.push_str(token);
    }
}

#[derive(Debug, Error)]
pub enum TuiError {
    #[error("terminal error: {0}")]
    Io(#[from] io::Error),
}
pub(crate) struct ConfiguredRun {
    pub repo: Arc<SqliteRepository>,
    pub provider: Arc<dyn voff_provider::ModelProvider>,
    pub providers: std::collections::HashMap<String, Arc<dyn voff_provider::ModelProvider>>,
    pub default_provider: String,
    pub session: SessionId,
    pub owner: AgentId,
    pub model_name: String,
    pub system_prompt: String,
    pub provider_label: String,
    pub agent_name: String,
    pub agent_profiles: Vec<voff_core::AgentProfile>,
    pub objective: String,
    pub delegation_plan: Vec<DelegationProposal>,
    pub initial_tasks: Vec<Task>,
    pub initial_events: Vec<voff_core::CanonicalEvent>,
    pub initial_approvals: Vec<Approval>,
    pub initial_artifacts: Vec<Artifact>,
    pub capacity: usize,
    pub workspace_context: String,
    pub workspace_root: std::path::PathBuf,
    pub max_parallel_tasks: usize,
    pub input_cost_per_million: Option<f64>,
    pub output_cost_per_million: Option<f64>,
    pub cost_currency: String,
    pub model_catalog: ModelCatalog,
    pub agent_provider_kinds: HashMap<AgentId, String>,
    pub credential_target: Option<(String, crate::credential::CredentialSlot)>,
}
pub(crate) async fn run_configured(config: ConfiguredRun) -> Result<(), TuiError> {
    let (event_tx, event_rx) = mpsc::channel(config.capacity);
    let model_label = config.model_name.clone();
    let credential_repository = Arc::clone(&config.repo);
    let agent_profiles = config.agent_profiles.clone();
    let engine = Arc::new(
        Engine::new(config.repo, config.provider, config.session, event_tx)
            .with_provider_registry(config.providers, config.default_provider)
            .with_formation_agents(&agent_profiles)
            .with_runtime(config.model_name, config.system_prompt)
            .with_workspace_context(config.workspace_context)
            .with_safe_workflow(&config.workspace_root)
            .map_err(|e| TuiError::Io(io::Error::other(e)))?,
    );
    let (input_tx, input_rx) = mpsc::channel(32);
    let mut app = AppModel::new(config.session);
    app.workspace_root = config.workspace_root.clone();
    app.provider_label = config.provider_label;
    app.agent_name = config.agent_name;
    app.agent_positions = vec![AgentPosition::default(); agent_profiles.len()];
    app.agents = agent_profiles;
    app.model_label = model_label;
    app.input_cost_per_million = config.input_cost_per_million;
    app.output_cost_per_million = config.output_cost_per_million;
    app.cost_currency =
        if app.input_cost_per_million.is_some() || app.output_cost_per_million.is_some() {
            config.cost_currency
        } else {
            "USD".into()
        };
    app.model_catalog = config.model_catalog;
    app.agent_provider_kinds = config.agent_provider_kinds;
    app.credential_target = config.credential_target;
    if app.input_cost_per_million.is_some() && app.output_cost_per_million.is_some()
        || app.agents.iter().any(|agent| {
            app.agent_provider_kinds
                .get(&agent.id)
                .and_then(|provider| app.model_catalog.option(provider, &agent.model))
                .is_some_and(|model| model.input_price.is_some() && model.output_price.is_some())
        })
    {
        app.estimated_cost = Some(0.0);
    }
    app.tasks = config.initial_tasks;
    app.approvals = config.initial_approvals;
    app.artifacts = config.initial_artifacts;
    for event in config.initial_events {
        app.push_event(&event.kind);
    }
    terminal_loop(
        &mut app,
        engine,
        LoopConfig {
            owner: config.owner,
            objective: config.objective,
            delegation_plan: config.delegation_plan,
            max_parallel_tasks: config.max_parallel_tasks,
            credential_repository,
        },
        input_tx,
        input_rx,
        event_rx,
    )
    .await
}
struct LoopConfig {
    owner: AgentId,
    objective: String,
    delegation_plan: Vec<DelegationProposal>,
    max_parallel_tasks: usize,
    credential_repository: Arc<SqliteRepository>,
}
async fn terminal_loop<R: Repository + 'static>(
    model: &mut AppModel,
    engine: Arc<Engine<R>>,
    config: LoopConfig,
    input_tx: mpsc::Sender<Event>,
    mut input_rx: mpsc::Receiver<Event>,
    mut event_rx: mpsc::Receiver<EngineEvent>,
) -> Result<(), TuiError> {
    let LoopConfig {
        owner,
        objective,
        delegation_plan,
        max_parallel_tasks,
        credential_repository,
    } = config;
    let _guard = TerminalGuard::enter()?;
    let mut terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;
    let input = tokio::task::spawn_blocking(move || {
        while let Ok(event) = event::read() {
            if input_tx.blocking_send(event).is_err() {
                break;
            }
        }
    });
    let (credential_tx, mut credential_rx) = mpsc::channel(1);
    let mut tick = tokio::time::interval(Duration::from_millis(100));
    while !model.quit {
        tokio::select! {
            Some(event)=input_rx.recv()=>{
                let (handled, submission) = model.handle_credential_event(&event);
                if let Some((reference, slot, token)) = submission {
                    model.credential_saving = true;
                    let credential_tx = credential_tx.clone();
                    let repository = Arc::clone(&credential_repository);
                    tokio::spawn(async move {
                        let result = crate::credential::store(&reference, &token, &repository)
                            .await
                            .map(|()| slot.set(token))
                            .map_err(|error| error.to_string());
                        let _ = credential_tx.send(result).await;
                    });
                } else if !handled {
                    match model.handle_composer_event(&event) {
                        ComposerEvent::Submit(message) if message == "/tasks" => {
                            model.list_current_tasks();
                        }
                        ComposerEvent::Submit(message) if message.starts_with('/') => {
                            model.composer_notice = Some(format!(
                                "Unknown command: {message}. Try /tasks."
                            ));
                        }
                        ComposerEvent::Submit(message) => {
                            model.composer_notice = Some("Sent to Team Lead.".into());
                            model.push_ui_event(format!("you → Team Lead: {}", sanitize(&message)));
                            let engine=Arc::clone(&engine);
                            tokio::spawn(async move{
                                if let Ok(task)=engine.create_task(owner,message,vec!["Team Lead responds with a useful result".into()]).await{
                                    let _=engine.mark_ready(task.id).await;
                                    let _=engine.run_task(task.id).await;
                                }
                            });
                        }
                        ComposerEvent::Consumed => {}
                        ComposerEvent::Ignored => {
                            let action = action_for_event(event);
                            match action {
                                Action::StartMock => {
                                    let engine = Arc::clone(&engine);
                                    let objective = objective.clone();
                                    let plan = delegation_plan.clone();
                                    tokio::spawn(async move {
                                        let criteria = if plan.is_empty() {
                                            vec!["Provider run completes".into()]
                                        } else {
                                            vec!["Every planned delegated workstream completes".into()]
                                        };
                                        if let Ok(task) =
                                            engine.create_task(owner, objective, criteria).await
                                            && engine.mark_ready(task.id).await.is_ok()
                                        {
                                            if plan.is_empty() {
                                                let _ = engine.run_task(task.id).await;
                                            } else {
                                                let _ = engine
                                                    .propose_delegation_plan(task.id, &plan)
                                                    .await;
                                            }
                                        }
                                    });
                                }
                                Action::Cancel=>{if let Some(id)=model.selected_task().filter(|t|matches!(t.status,TaskStatus::Queued|TaskStatus::Ready|TaskStatus::Running)).map(|t|t.id){let engine=Arc::clone(&engine);tokio::spawn(async move{let _=engine.cancel_task(id).await;});}}
                                Action::Approve|Action::Deny=>{if let Some(id)=model.pending_approval().map(|v|v.id){let engine=Arc::clone(&engine);let decision=if action==Action::Approve{ApprovalDecision::ApprovedOnce}else{ApprovalDecision::Denied};tokio::spawn(async move{if engine.decide_approval(id,decision).await.is_ok() && decision==ApprovalDecision::ApprovedOnce { let _=engine.run_until_idle_bounded(max_parallel_tasks).await; }});}}
                                other=>model.reduce(other)
                            }
                        }
                    }
                }
            }
            Some(result)=credential_rx.recv()=>{
                model.credential_saving = false;
                match result {
                    Ok(()) => {
                        model.credential_input = None;
                        model.credential_error = None;
                        model.push_ui_event("OpenAI credential stored securely; the team is ready to run.".into());
                    }
                    Err(error) => model.credential_error = Some(error),
                }
            }
            Some(message)=event_rx.recv()=>model.apply_event(message),
            _=tick.tick()=>{
                model.animation_tick = model.animation_tick.wrapping_add(1);
                terminal.draw(|frame|render(frame,model))?;
            }
            else=>{model.quit=true}
        }
    }
    input.abort();
    Ok(())
}
fn render(frame: &mut ratatui::Frame<'_>, app: &mut AppModel) {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(4),
            Constraint::Min(8),
            Constraint::Length(3),
        ])
        .split(frame.area());
    let session = app.session.to_string();
    let cost_source =
        if app.input_cost_per_million.is_some() || app.output_cost_per_million.is_some() {
            "manual rates"
        } else {
            "catalog"
        };
    let cost = app
        .estimated_cost
        .map(|value| {
            format!(
                "{value:.4} {} est. · {cost_source}{}",
                app.cost_currency,
                if app.cost_partial { " · partial" } else { "" }
            )
        })
        .unwrap_or_else(|| "cost unconfigured".into());
    let credential = app
        .credential_target
        .as_ref()
        .map(|(_, slot)| {
            if slot.is_available() {
                " · token ready".to_owned()
            } else {
                " · no token [a]".to_owned()
            }
        })
        .unwrap_or_default();
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(vec![
                Span::styled(
                    " ◆ voff ",
                    Style::default()
                        .fg(Color::Magenta)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw(format!(
                    "workspace · {} developers · session {}",
                    app.agents.len(),
                    &session[..session.len().min(8)]
                )),
            ]),
            Line::from(Span::styled(
                format!(
                    " {} · {} · circuit {:?} · {} retries · {cost}{credential}",
                    app.provider_label,
                    app.model_label,
                    app.provider_health.circuit,
                    app.provider_health.retries
                ),
                Style::default().fg(Color::DarkGray),
            )),
        ])
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded),
        ),
        rows[0],
    );

    let horizontal = frame.area().width >= 100;
    let body = Layout::default()
        .direction(if horizontal {
            Direction::Horizontal
        } else {
            Direction::Vertical
        })
        .constraints([Constraint::Percentage(58), Constraint::Percentage(42)])
        .split(rows[1]);
    render_team(frame, app, body[0]);
    render_activity(frame, app, body[1]);

    let workspace = sanitize(&app.workspace_root.display().to_string());
    let composer_title = if app.composer_active {
        format!(" Team Lead · cwd {workspace} · Enter send · Shift+Enter newline · Esc keep draft ")
    } else if let Some(notice) = &app.composer_notice {
        format!(" {notice} · cwd {workspace} · Enter message · /tasks list ")
    } else {
        format!(" Team Lead · cwd {workspace} · Enter message · /tasks list · t also focuses ")
    };
    let composer_color = if app.composer_active {
        Color::LightCyan
    } else {
        Color::DarkGray
    };
    app.composer.set_block(
        Block::default()
            .title(composer_title)
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(composer_color)),
    );
    app.composer
        .set_cursor_style(Style::default().fg(composer_color));
    app.composer_hitbox = rows[2];
    frame.render_widget(&app.composer, rows[2]);
    render_overlays(frame, app);
}

fn render_activity(frame: &mut ratatui::Frame<'_>, app: &AppModel, area: Rect) {
    let sections = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage(32),
            Constraint::Percentage(34),
            Constraint::Percentage(34),
        ])
        .split(area);
    let tasks = app
        .tasks
        .iter()
        .enumerate()
        .map(|(i, t)| {
            ListItem::new(format!(
                "{} {}{} [{:?}]",
                if i == app.selected { "▶" } else { " " },
                "  ".repeat(usize::from(t.depth)),
                t.objective,
                t.status
            ))
        })
        .collect::<Vec<_>>();
    frame.render_widget(
        List::new(tasks).block(panel("Tasks", app.panel == Panel::Tasks)),
        sections[0],
    );
    let details = app.selected_task().map_or_else(
        || "Your team is ready. Press r to start the configured objective.".into(),
        |t| {
            let artifact_text = app
                .artifacts
                .iter()
                .filter(|a| a.producer == t.id)
                .map(|a| {
                    format!(
                        "\n\nArtifact: {} ({})\n{}",
                        a.location,
                        a.kind,
                        sanitize(&a.preview)
                    )
                })
                .collect::<String>();
            format!(
                "Owner: {}\nStatus: {:?}\nObjective: {}\nCriteria:\n{}{}",
                t.owner,
                t.status,
                t.objective,
                t.acceptance_criteria.join("\n"),
                artifact_text
            )
        },
    );
    frame.render_widget(
        Paragraph::new(details)
            .wrap(Wrap { trim: true })
            .block(panel("Task Details", app.panel == Panel::Details)),
        sections[1],
    );
    let logs = app
        .events
        .iter()
        .enumerate()
        .rev()
        .map(|(index, event)| format!("[{}] {event}", index + 1))
        .collect::<Vec<_>>()
        .join("\n\n");
    let event_title = if app.event_follow {
        "Events · live (e: expand)"
    } else {
        "Events · paused (↑ to newest)"
    };
    frame.render_widget(
        Paragraph::new(logs)
            .wrap(Wrap { trim: false })
            .scroll((app.event_scroll, 0))
            .block(panel(event_title, app.panel == Panel::Events)),
        sections[2],
    );
}

fn render_team(frame: &mut ratatui::Frame<'_>, app: &mut AppModel, area: Rect) {
    let handoff = handoff_summary(&app.tasks, &app.approvals);
    let title = if app.panel == Panel::Agents {
        format!(" Team · {handoff} · selected · click & drag cards ")
    } else {
        format!(" Team · {handoff} · Tab select · drag cards ")
    };
    frame.render_widget(
        panel(&title, app.panel == Panel::Agents).border_type(BorderType::Rounded),
        area,
    );
    let inner = area.inner(Margin {
        horizontal: 2,
        vertical: 1,
    });
    if app.agents.is_empty() || inner.width < 8 || inner.height < 4 {
        frame.render_widget(
            Paragraph::new("No configured developers.").style(Style::default().fg(Color::DarkGray)),
            inner,
        );
        return;
    }
    if app.agent_positions.len() != app.agents.len() {
        app.agent_positions = vec![AgentPosition::default(); app.agents.len()];
    }
    let cards = agent_layout(inner, app.agents.len(), &app.agent_positions);
    draw_threads(
        frame,
        &app.agents,
        &app.tasks,
        &app.approvals,
        &cards,
        app.animation_tick,
    );
    app.agent_hitboxes = cards.clone();
    for (index, (agent, card)) in app.agents.iter().zip(cards.iter().copied()).enumerate() {
        if card.width < 4 || card.height < 3 {
            continue;
        }
        let selected = index == app.selected_agent;
        let pending = pending_delegation_for_target(&app.approvals, &agent.name);
        let (status, status_color) = agent_status(&app.tasks, agent.id, pending.is_some());
        let display_task = agent_display_task(&app.tasks, agent.id);
        let color = role_color(&agent.role);
        let title = format!(" {} {} ", role_icon(&agent.role), sanitize(&agent.name));
        let status_line = || {
            Line::from(vec![
                Span::styled(
                    format!("{} {status}", status_symbol(status)),
                    Style::default().fg(status_color),
                ),
                Span::styled(
                    format!("  {}", agent.provider),
                    Style::default().fg(Color::DarkGray),
                ),
            ])
        };
        let mut lines = if card.height <= 3 {
            vec![status_line()]
        } else {
            vec![
                Line::from(Span::styled(
                    human_role(&agent.role),
                    Style::default().fg(Color::Gray),
                )),
                status_line(),
            ]
        };
        if card.height >= 6 {
            lines.push(Line::from(Span::styled(
                agent.model.as_str(),
                Style::default().fg(Color::LightCyan),
            )));
        }
        if card.height >= 7 {
            lines.push(Line::from(Span::styled(
                if let Some(task) = display_task {
                    format!(
                        "{} {}",
                        task_status_marker(task.status),
                        truncate_label(&sanitize(&task.objective), card.width.saturating_sub(4))
                    )
                } else if let Some(proposal) = &pending {
                    format!(
                        "? {}",
                        truncate_label(
                            &sanitize(&proposal.objective),
                            card.width.saturating_sub(4)
                        )
                    )
                } else if agent.allowed_targets.is_empty() {
                    format!("{} tokens · no delegation", agent.max_output_tokens)
                } else {
                    format!("{} tokens · delegation", agent.max_output_tokens)
                },
                Style::default().fg(if display_task.is_some() || pending.is_some() {
                    status_color
                } else {
                    Color::DarkGray
                }),
            )));
        }
        frame.render_widget(
            Paragraph::new(Text::from(lines))
                .wrap(Wrap { trim: true })
                .block(
                    Block::default()
                        .title(title)
                        .borders(Borders::ALL)
                        .border_type(BorderType::Rounded)
                        .border_style(if selected {
                            Style::default().fg(color).add_modifier(Modifier::BOLD)
                        } else {
                            Style::default().fg(Color::DarkGray)
                        }),
                ),
            card,
        );
        if status == "working" {
            frame.render_widget(
                FluxSpinner::new(app.animation_tick)
                    .frames(FluxFrames::ORBIT)
                    .color(status_color),
                Rect::new(card.x.saturating_add(1), card.y.saturating_add(2), 1, 1),
            );
        }
    }
    draw_connection_ports(frame, &app.agents, &app.tasks, &app.approvals, &cards);
}

fn handoff_summary(tasks: &[Task], approvals: &[Approval]) -> String {
    let delegated = tasks
        .iter()
        .filter(|task| task.parent.is_some())
        .collect::<Vec<_>>();
    let pending = approvals
        .iter()
        .filter(|approval| {
            approval.action == "task.delegate"
                && approval.decision == ApprovalDecision::Pending
                && serde_json::from_str::<DelegationProposal>(&approval.reason).is_ok()
        })
        .count();
    if delegated.is_empty() && pending == 0 {
        return "no handoffs yet".into();
    }
    let active = delegated
        .iter()
        .filter(|task| {
            matches!(
                task.status,
                TaskStatus::Queued
                    | TaskStatus::Ready
                    | TaskStatus::Running
                    | TaskStatus::WaitingApproval
            )
        })
        .count();
    let completed = delegated
        .iter()
        .filter(|task| task.status == TaskStatus::Completed)
        .count();
    let failed = delegated
        .iter()
        .filter(|task| {
            matches!(
                task.status,
                TaskStatus::Blocked | TaskStatus::Failed | TaskStatus::Cancelled
            )
        })
        .count();
    if active > 0 && pending > 0 {
        format!("{active} active · {pending} awaiting approval")
    } else if pending > 0 {
        format!(
            "{pending} handoff{} awaiting approval",
            plural_suffix(pending)
        )
    } else if active > 0 {
        format!("{active} handoff active")
    } else if failed > 0 {
        format!("{failed} handoff failed")
    } else {
        format!("{completed} handoff complete")
    }
}

const fn plural_suffix(count: usize) -> &'static str {
    if count == 1 { "" } else { "s" }
}

fn agent_layout(area: Rect, count: usize, positions: &[AgentPosition]) -> Vec<Rect> {
    if count == 0 {
        return vec![];
    }
    let columns = usize::from((area.width / 24).max(1)).min(count.max(1));
    let worker_rows = count.saturating_sub(1).div_ceil(columns);
    let total_rows = 1 + worker_rows;
    let row_gaps = total_rows.saturating_sub(1);
    let row_gap = if row_gaps > 0
        && usize::from(area.height) >= total_rows.saturating_mul(4) + row_gaps.saturating_mul(3)
    {
        3
    } else if row_gaps > 0 {
        1
    } else {
        0
    };
    let used_by_gaps = row_gaps.saturating_mul(row_gap);
    let card_height = (area
        .height
        .saturating_sub(u16::try_from(used_by_gaps).unwrap_or(u16::MAX))
        / u16::try_from(total_rows).unwrap_or(u16::MAX))
    .clamp(3, 7)
    .min(area.height);
    let column_width = area.width / u16::try_from(columns).unwrap_or(1).max(1);
    let card_width = column_width.saturating_sub(2).clamp(8, 28).min(area.width);
    (0..count)
        .map(|index| {
            let (base_x, base_y) = if index == 0 {
                (area.x + area.width.saturating_sub(card_width) / 2, area.y)
            } else {
                let worker = index - 1;
                let column = worker % columns;
                let row = worker / columns + 1;
                (
                    area.x
                        + u16::try_from(column).unwrap_or(0) * column_width
                        + column_width.saturating_sub(card_width) / 2,
                    area.y
                        + u16::try_from(row).unwrap_or(u16::MAX).saturating_mul(
                            card_height.saturating_add(u16::try_from(row_gap).unwrap_or(u16::MAX)),
                        ),
                )
            };
            let position = positions.get(index).copied().unwrap_or_default();
            let max_x = area.right().saturating_sub(card_width);
            let max_y = area.bottom().saturating_sub(card_height);
            Rect::new(
                offset(base_x, position.x).clamp(area.x, max_x),
                offset(base_y, position.y).clamp(area.y, max_y),
                card_width,
                card_height,
            )
        })
        .collect()
}

fn offset(value: u16, delta: i16) -> u16 {
    (i32::from(value) + i32::from(delta)).clamp(0, i32::from(u16::MAX)) as u16
}

fn draw_threads(
    frame: &mut ratatui::Frame<'_>,
    agents: &[AgentProfile],
    tasks: &[Task],
    approvals: &[Approval],
    cards: &[Rect],
    tick: u64,
) {
    for (source_index, source) in agents.iter().enumerate() {
        for (target_index, target) in agents.iter().enumerate() {
            if source_index == target_index {
                continue;
            }
            let delegated_task = delegated_task(tasks, source.id, target.id);
            let pending = pending_delegation(approvals, tasks, source.id, &target.name);
            if !source.allowed_targets.contains(&target.name)
                && delegated_task.is_none()
                && pending.is_none()
            {
                continue;
            }
            let Some((source_card, target_card)) =
                cards.get(source_index).zip(cards.get(target_index))
            else {
                continue;
            };
            draw_thread(
                frame,
                source_card,
                target_card,
                delegated_task,
                pending.as_ref(),
                tick,
            );
        }
    }
}

fn draw_thread(
    frame: &mut ratatui::Frame<'_>,
    source: &Rect,
    target: &Rect,
    task: Option<&Task>,
    pending: Option<&DelegationProposal>,
    tick: u64,
) {
    let (from, to, downward) = thread_endpoints(source, target);
    let bend = from.1.min(to.1) + from.1.abs_diff(to.1) / 2;
    draw_line(frame, from, (from.0, bend), Color::DarkGray, "┊", "┈");
    draw_line(
        frame,
        (from.0, bend),
        (to.0, bend),
        Color::DarkGray,
        "┊",
        "┈",
    );
    draw_line(frame, (to.0, bend), to, Color::DarkGray, "┊", "┈");
    let arrow_y = if downward {
        to.1.saturating_sub(1)
    } else {
        to.1.saturating_add(1)
    };
    set_thread_cell(
        frame,
        to.0,
        arrow_y,
        if downward { "▼" } else { "▲" },
        Color::DarkGray,
    );

    let (color, marker, objective, running) = if let Some(task) = task {
        (
            task_status_color(task.status),
            match task.status {
                TaskStatus::Running => "▶ working",
                TaskStatus::WaitingApproval => "? approval",
                TaskStatus::Completed => "✓ done",
                TaskStatus::Failed | TaskStatus::Blocked => "! failed",
                TaskStatus::Cancelled => "× cancelled",
                TaskStatus::Queued | TaskStatus::Ready => "◇ queued",
            },
            task.objective.as_str(),
            task.status == TaskStatus::Running,
        )
    } else if let Some(proposal) = pending {
        (
            Color::Yellow,
            "? approval",
            proposal.objective.as_str(),
            false,
        )
    } else {
        return;
    };
    draw_line(frame, from, (from.0, bend), color, "┃", "━");
    draw_line(frame, (from.0, bend), (to.0, bend), color, "┃", "━");
    draw_line(frame, (to.0, bend), to, color, "┃", "━");
    set_thread_cell(
        frame,
        to.0,
        arrow_y,
        if downward { "▼" } else { "▲" },
        color,
    );
    if running {
        let route = orthogonal_route(from, (from.0, bend), (to.0, bend), to);
        let route_len = u64::try_from(route.len()).unwrap_or(u64::MAX).max(1);
        let pulse_index = usize::try_from((tick / 2) % route_len).unwrap_or(0);
        if let Some(&(x, y)) = route.get(pulse_index) {
            set_thread_cell(frame, x, y, "◆", Color::White);
        }
    }
    draw_delegation_label(frame, from, to, bend, marker, objective, color);
}

fn thread_endpoints(source: &Rect, target: &Rect) -> ((u16, u16), (u16, u16), bool) {
    let downward = source.y + source.height / 2 <= target.y + target.height / 2;
    if downward {
        (
            (
                source.x + source.width / 2,
                source.bottom().saturating_sub(1),
            ),
            (target.x.saturating_add(1), target.y),
            true,
        )
    } else {
        (
            (source.x.saturating_add(1), source.y),
            (
                target.x + target.width / 2,
                target.bottom().saturating_sub(1),
            ),
            false,
        )
    }
}

fn draw_connection_ports(
    frame: &mut ratatui::Frame<'_>,
    agents: &[AgentProfile],
    tasks: &[Task],
    approvals: &[Approval],
    cards: &[Rect],
) {
    for (source_index, source) in agents.iter().enumerate() {
        for (target_index, target) in agents.iter().enumerate() {
            if source_index == target_index {
                continue;
            }
            let task = delegated_task(tasks, source.id, target.id);
            let pending = pending_delegation(approvals, tasks, source.id, &target.name);
            if !source.allowed_targets.contains(&target.name) && task.is_none() && pending.is_none()
            {
                continue;
            }
            let Some((source_card, target_card)) =
                cards.get(source_index).zip(cards.get(target_index))
            else {
                continue;
            };
            let (source_port, target_port, _) = thread_endpoints(source_card, target_card);
            let (symbol, color) = task.map_or_else(
                || {
                    if pending.is_some() {
                        ("●", Color::Yellow)
                    } else {
                        ("○", Color::DarkGray)
                    }
                },
                |task| ("●", task_status_color(task.status)),
            );
            set_thread_cell(frame, source_port.0, source_port.1, symbol, color);
            set_thread_cell(frame, target_port.0, target_port.1, symbol, color);
        }
    }
}

fn draw_line(
    frame: &mut ratatui::Frame<'_>,
    from: (u16, u16),
    to: (u16, u16),
    color: Color,
    vertical: &'static str,
    horizontal: &'static str,
) {
    if from.0 == to.0 {
        for y in from.1.min(to.1)..=from.1.max(to.1) {
            set_thread_cell(frame, from.0, y, vertical, color);
        }
    } else if from.1 == to.1 {
        for x in from.0.min(to.0)..=from.0.max(to.0) {
            set_thread_cell(frame, x, from.1, horizontal, color);
        }
    }
}

fn set_thread_cell(
    frame: &mut ratatui::Frame<'_>,
    x: u16,
    y: u16,
    symbol: &'static str,
    color: Color,
) {
    if let Some(cell) = frame.buffer_mut().cell_mut((x, y)) {
        cell.set_symbol(symbol).set_fg(color);
    }
}

fn delegated_task(tasks: &[Task], source: AgentId, target: AgentId) -> Option<&Task> {
    tasks
        .iter()
        .filter(|task| task.owner == target)
        .filter(|task| {
            task.parent.is_some_and(|parent_id| {
                tasks
                    .iter()
                    .any(|parent| parent.id == parent_id && parent.owner == source)
            })
        })
        .max_by(|left, right| {
            task_priority(left.status)
                .cmp(&task_priority(right.status))
                .then_with(|| left.updated_at.cmp(&right.updated_at))
        })
}

fn pending_delegation(
    approvals: &[Approval],
    tasks: &[Task],
    source: AgentId,
    target_name: &str,
) -> Option<DelegationProposal> {
    approvals.iter().rev().find_map(|approval| {
        if approval.action != "task.delegate" || approval.decision != ApprovalDecision::Pending {
            return None;
        }
        let parent_matches = tasks
            .iter()
            .any(|task| task.id == approval.task_id && task.owner == source);
        if !parent_matches {
            return None;
        }
        let proposal = serde_json::from_str::<DelegationProposal>(&approval.reason).ok()?;
        (proposal.target_agent == target_name).then_some(proposal)
    })
}

fn pending_delegation_for_target(
    approvals: &[Approval],
    target_name: &str,
) -> Option<DelegationProposal> {
    approvals.iter().rev().find_map(|approval| {
        if approval.action != "task.delegate" || approval.decision != ApprovalDecision::Pending {
            return None;
        }
        let proposal = serde_json::from_str::<DelegationProposal>(&approval.reason).ok()?;
        (proposal.target_agent == target_name).then_some(proposal)
    })
}

fn agent_display_task(tasks: &[Task], owner: AgentId) -> Option<&Task> {
    tasks
        .iter()
        .filter(|task| task.owner == owner)
        .max_by(|left, right| {
            task_priority(left.status)
                .cmp(&task_priority(right.status))
                .then_with(|| left.updated_at.cmp(&right.updated_at))
        })
}

const fn task_priority(status: TaskStatus) -> u8 {
    match status {
        TaskStatus::Running => 7,
        TaskStatus::WaitingApproval => 6,
        TaskStatus::Ready => 5,
        TaskStatus::Queued => 4,
        TaskStatus::Blocked => 3,
        TaskStatus::Failed => 2,
        TaskStatus::Completed => 1,
        TaskStatus::Cancelled => 0,
    }
}

const fn task_status_color(status: TaskStatus) -> Color {
    match status {
        TaskStatus::Running => Color::LightGreen,
        TaskStatus::WaitingApproval => Color::Yellow,
        TaskStatus::Ready => Color::LightCyan,
        TaskStatus::Queued => Color::Gray,
        TaskStatus::Blocked => Color::LightRed,
        TaskStatus::Failed => Color::Red,
        TaskStatus::Completed => Color::Green,
        TaskStatus::Cancelled => Color::DarkGray,
    }
}

fn orthogonal_route(
    from: (u16, u16),
    first_bend: (u16, u16),
    second_bend: (u16, u16),
    to: (u16, u16),
) -> Vec<(u16, u16)> {
    let mut route = Vec::new();
    append_segment(&mut route, from, first_bend);
    append_segment(&mut route, first_bend, second_bend);
    append_segment(&mut route, second_bend, to);
    route.dedup();
    route
}

fn append_segment(route: &mut Vec<(u16, u16)>, from: (u16, u16), to: (u16, u16)) {
    if from.0 == to.0 {
        if from.1 <= to.1 {
            route.extend((from.1..=to.1).map(|y| (from.0, y)));
        } else {
            route.extend((to.1..=from.1).rev().map(|y| (from.0, y)));
        }
    } else if from.1 == to.1 {
        if from.0 <= to.0 {
            route.extend((from.0..=to.0).map(|x| (x, from.1)));
        } else {
            route.extend((to.0..=from.0).rev().map(|x| (x, from.1)));
        }
    }
}

fn draw_delegation_label(
    frame: &mut ratatui::Frame<'_>,
    from: (u16, u16),
    to: (u16, u16),
    bend: u16,
    marker: &str,
    objective: &str,
    color: Color,
) {
    let frame_area = frame.area();
    let horizontal_room = from.0.abs_diff(to.0).saturating_sub(1);
    let (x, width) = if horizontal_room >= 6 {
        (from.0.min(to.0).saturating_add(1), horizontal_room)
    } else {
        let x = from.0.min(to.0).saturating_add(1);
        (x, frame_area.right().saturating_sub(x).saturating_sub(1))
    };
    let width = width.min(28);
    if width < 3 || bend >= frame_area.bottom() {
        return;
    }
    let label = truncate_label(&format!("{marker} · {}", sanitize(objective)), width);
    frame.render_widget(
        Paragraph::new(label).style(Style::default().fg(color).add_modifier(Modifier::BOLD)),
        Rect::new(x, bend, width, 1),
    );
}

const fn task_status_marker(status: TaskStatus) -> &'static str {
    match status {
        TaskStatus::Running => "▶",
        TaskStatus::WaitingApproval => "?",
        TaskStatus::Completed => "✓",
        TaskStatus::Failed | TaskStatus::Blocked => "!",
        TaskStatus::Cancelled => "×",
        TaskStatus::Queued | TaskStatus::Ready => "◇",
    }
}

fn truncate_label(value: &str, width: u16) -> String {
    let width = usize::from(width);
    if value.chars().count() <= width {
        return value.to_owned();
    }
    if width <= 1 {
        return "…".chars().take(width).collect();
    }
    let mut shortened = value.chars().take(width - 1).collect::<String>();
    shortened.push('…');
    shortened
}

fn agent_status(tasks: &[Task], owner: AgentId, pending_delegation: bool) -> (&'static str, Color) {
    let statuses = tasks
        .iter()
        .filter(|task| task.owner == owner)
        .map(|task| task.status)
        .collect::<Vec<_>>();
    for (status, label, color) in [
        (TaskStatus::WaitingApproval, "approval", Color::Yellow),
        (TaskStatus::Running, "working", Color::LightGreen),
        (TaskStatus::Ready, "ready", Color::LightCyan),
        (TaskStatus::Queued, "queued", Color::Gray),
        (TaskStatus::Blocked, "blocked", Color::LightRed),
        (TaskStatus::Failed, "failed", Color::Red),
        (TaskStatus::Completed, "complete", Color::Green),
        (TaskStatus::Cancelled, "cancelled", Color::DarkGray),
    ] {
        if statuses.contains(&status) {
            return (label, color);
        }
    }
    if pending_delegation {
        return ("approval", Color::Yellow);
    }
    ("idle", Color::DarkGray)
}

fn status_symbol(status: &str) -> &'static str {
    match status {
        "working" => "●",
        "complete" => "✓",
        "failed" | "blocked" => "!",
        "approval" => "?",
        _ => "○",
    }
}

fn role_color(role: &str) -> Color {
    match role {
        "technical_lead" => Color::Magenta,
        "frontend_developer" => Color::LightCyan,
        "backend_developer" => Color::Blue,
        "code_reviewer" => Color::Yellow,
        "test_engineer" => Color::Green,
        "devops_engineer" => Color::LightRed,
        _ => Color::Cyan,
    }
}

fn role_icon(role: &str) -> &'static str {
    match role {
        "technical_lead" => "◆",
        "frontend_developer" => "▣",
        "backend_developer" => "⬢",
        "code_reviewer" => "◇",
        "test_engineer" => "✓",
        "devops_engineer" => "▲",
        _ => "●",
    }
}

fn human_role(role: &str) -> String {
    role.split('_')
        .map(|word| {
            let mut chars = word.chars();
            chars.next().map_or_else(String::new, |first| {
                first.to_uppercase().collect::<String>() + chars.as_str()
            })
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn render_overlays(frame: &mut ratatui::Frame<'_>, app: &AppModel) {
    if app.event_viewer {
        let area = frame.area();
        let popup = ratatui::layout::Rect {
            x: area.width / 12,
            y: area.height / 12,
            width: area.width * 5 / 6,
            height: area.height * 5 / 6,
        };
        let text = app
            .events
            .iter()
            .enumerate()
            .rev()
            .map(|(index, event)| format!("[{}] {}", index + 1, event))
            .collect::<Vec<_>>()
            .join("\n\n");
        frame.render_widget(Clear, popup);
        let title = if app.event_follow {
            "Events — live, newest first | ↓/PgDn older | e/Esc close"
        } else {
            "Events — paused | ↑/PgUp newest | e/Esc close"
        };
        frame.render_widget(
            Paragraph::new(text)
                .wrap(Wrap { trim: false })
                .scroll((app.event_scroll, 0))
                .block(
                    Block::default()
                        .title(title)
                        .borders(Borders::ALL)
                        .border_style(Style::default().fg(Color::Cyan)),
                ),
            popup,
        );
    }
    if app.help {
        let area = frame.area();
        let popup = ratatui::layout::Rect {
            x: area.width / 4,
            y: area.height / 4,
            width: area.width / 2,
            height: area.height / 2,
        };
        frame.render_widget(Clear, popup);
        frame.render_widget(Paragraph::new("voff Help\n\nEnter/t  Message the Team Lead\nclick composer  Focus the Team Lead input\n/tasks  Focus the current task list\nShift+Enter  New line while composing\nmouse drag  Move a developer card\nclick card  Select a developer\nr  Start configured objective\na  Add a missing provider token locally\nc  Cancel selected active task\ne  Open full-screen events\n↑/↓, PgUp/PgDn  Navigate or scroll\nTab  Change panel\nq  Safe exit\nEsc  Close / keep draft\n?  Close help").wrap(Wrap { trim: false }).block(Block::default().title("Help").borders(Borders::ALL)),popup);
    }
    if let Some(approval) = app.pending_approval() {
        let area = frame.area();
        let popup = if area.width >= 100 {
            Rect {
                x: area.x + area.width * 58 / 100,
                y: area.y.saturating_add(4),
                width: area.width * 42 / 100,
                height: area.height.saturating_sub(7),
            }
        } else {
            Rect {
                x: area.x + area.width / 10,
                y: area.y + area.height / 4,
                width: area.width * 4 / 5,
                height: area.height / 2,
            }
        };
        frame.render_widget(Clear, popup);
        let preview = app
            .artifacts
            .iter()
            .find(|a| a.producer == approval.task_id)
            .map(|a| format!("\n\nDiff preview:\n{}", sanitize(&a.preview)))
            .unwrap_or_default();
        let details = serde_json::from_str::<DelegationProposal>(&approval.reason)
            .ok()
            .filter(|_| approval.action == "task.delegate")
            .map_or_else(
                || format!("Reason: {}", sanitize(&approval.reason)),
                |proposal| {
                    let criteria = proposal
                        .acceptance_criteria
                        .iter()
                        .map(|criterion| format!("- {}", sanitize(criterion)))
                        .collect::<Vec<_>>()
                        .join("\n");
                    format!(
                        "Target: {}\nObjective: {}\nAcceptance:\n{}",
                        sanitize(&proposal.target_agent),
                        sanitize(&proposal.objective),
                        criteria
                    )
                },
            );
        let pending_count = app
            .approvals
            .iter()
            .filter(|candidate| candidate.decision == ApprovalDecision::Pending)
            .count();
        let title = format!("Approval required · {pending_count} pending");
        frame.render_widget(
            Paragraph::new(format!(
                "Action: {}\nTask: {}\nWorking directory: {}\n{}{}\n\n[y] Approve once    [n] Deny",
                sanitize(&approval.action),
                approval.task_id,
                sanitize(&approval.working_directory),
                details,
                preview
            ))
            .wrap(Wrap { trim: true })
            .block(
                Block::default()
                    .title(title)
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(Color::Yellow)),
            ),
            popup,
        );
    }
    if let Some(input) = &app.credential_input {
        let area = frame.area();
        let popup = Rect {
            x: area.width / 5,
            y: area.height / 3,
            width: area.width * 3 / 5,
            height: 11.min(area.height),
        };
        let bullets = "•".repeat(input.chars().count().min(42));
        let status = if app.credential_saving {
            "Saving securely…"
        } else if bullets.is_empty() {
            "token goes here"
        } else {
            &bullets
        };
        let error = app
            .credential_error
            .as_deref()
            .map_or_else(String::new, |error| format!("\n{error}"));
        frame.render_widget(Clear, popup);
        frame.render_widget(
            Paragraph::new(format!(
                "Paste your OpenAI API token\n\n  {status}  \n\nStored in the local .voff SQLite database, never in project config or logs.{error}\n\nEnter save · Esc keep using voff without a token"
            ))
            .alignment(ratatui::layout::Alignment::Center)
            .wrap(Wrap { trim: true })
            .block(
                Block::default()
                    .title(" Add token (optional) ")
                    .borders(Borders::ALL)
                    .border_type(BorderType::Rounded)
                    .border_style(Style::default().fg(Color::LightCyan)),
            ),
            popup,
        );
    }
}
fn panel(title: &str, active: bool) -> Block<'_> {
    Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_style(if active {
            Style::default().fg(Color::Cyan)
        } else {
            Style::default()
        })
}
struct TerminalGuard;
impl TerminalGuard {
    fn enter() -> io::Result<Self> {
        enable_raw_mode()?;
        execute!(
            io::stdout(),
            EnterAlternateScreen,
            EnableMouseCapture,
            EnableBracketedPaste
        )?;
        let old = panic::take_hook();
        panic::set_hook(Box::new(move |info| {
            let _ = disable_raw_mode();
            let _ = execute!(
                io::stdout(),
                DisableBracketedPaste,
                DisableMouseCapture,
                LeaveAlternateScreen
            );
            old(info);
        }));
        Ok(Self)
    }
}
impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(
            io::stdout(),
            DisableBracketedPaste,
            DisableMouseCapture,
            LeaveAlternateScreen
        );
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyModifiers;
    use ratatui::backend::TestBackend;

    fn agent(name: &str, role: &str, allowed_targets: Vec<String>) -> AgentProfile {
        AgentProfile {
            id: AgentId::new(),
            name: name.into(),
            role: role.into(),
            provider: "ollama".into(),
            model: "qwen-test".into(),
            system_prompt: "test".into(),
            tools: vec!["file.read".into()],
            max_output_tokens: 4096,
            enabled: true,
            allowed_targets,
            delegation_limits: None,
        }
    }

    fn mouse(kind: MouseEventKind, column: u16, row: u16) -> MouseEvent {
        MouseEvent {
            kind,
            column,
            row,
            modifiers: KeyModifiers::NONE,
        }
    }
    #[test]
    fn reducer_navigation_is_bounded() {
        let mut a = AppModel::new(SessionId::new());
        a.reduce(Action::Down);
        assert_eq!(a.selected, 0);
        a.reduce(Action::NextPanel);
        assert_eq!(a.panel, Panel::Details);
        a.reduce(Action::ToggleHelp);
        assert!(a.help)
    }
    #[test]
    fn keys_map_to_actions() {
        assert_eq!(action_for_key(KeyCode::Char('r')), Action::StartMock);
        assert_eq!(action_for_key(KeyCode::Char('a')), Action::AddCredential);
        assert_eq!(action_for_key(KeyCode::Enter), Action::Compose);
        assert_eq!(action_for_key(KeyCode::Char('/')), Action::ComposeCommand);
        assert_eq!(action_for_key(KeyCode::Char('c')), Action::Cancel);
        assert_eq!(action_for_key(KeyCode::Char('q')), Action::Quit);
        assert_eq!(action_for_key(KeyCode::Char('y')), Action::Approve);
        assert_eq!(action_for_key(KeyCode::Char('n')), Action::Deny)
    }

    #[test]
    fn textarea_submits_a_message_to_the_team_lead_flow() {
        let mut app = AppModel::new(SessionId::new());
        app.reduce(Action::Compose);
        assert_eq!(
            app.handle_composer_event(&Event::Paste("Please review the current plan".into())),
            ComposerEvent::Consumed
        );
        let enter = Event::Key(crossterm::event::KeyEvent::new(
            KeyCode::Enter,
            KeyModifiers::NONE,
        ));
        assert_eq!(
            app.handle_composer_event(&enter),
            ComposerEvent::Submit("Please review the current plan".into())
        );
        assert!(!app.composer_active);
        assert_eq!(app.composer.lines(), [""]);
    }

    #[test]
    fn clicking_the_textarea_keeps_focus_while_typing() {
        let mut app = AppModel::new(SessionId::new());
        app.composer_hitbox = Rect::new(0, 20, 100, 3);

        app.reduce(Action::Mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            10,
            21,
        )));
        assert!(app.composer_active);

        let typed = Event::Key(crossterm::event::KeyEvent::new(
            KeyCode::Char('a'),
            KeyModifiers::NONE,
        ));
        assert_eq!(app.handle_composer_event(&typed), ComposerEvent::Consumed);
        assert!(app.composer_active);
        assert_eq!(app.composer.lines(), ["a"]);
        assert!(app.credential_input.is_none());
    }

    #[test]
    fn tasks_command_focuses_the_current_task_list() {
        let mut app = AppModel::new(SessionId::new());
        app.panel = Panel::Events;
        app.tasks.push(Task::root(
            AgentId::new(),
            "current work",
            vec![],
            voff_core::TaskLimits::default(),
        ));
        app.reduce(Action::ComposeCommand);
        assert_eq!(app.composer.lines(), ["/"]);
        app.handle_composer_event(&Event::Paste("tasks".into()));
        let enter = Event::Key(crossterm::event::KeyEvent::new(
            KeyCode::Enter,
            KeyModifiers::NONE,
        ));
        assert_eq!(
            app.handle_composer_event(&enter),
            ComposerEvent::Submit("/tasks".into())
        );

        app.list_current_tasks();
        assert_eq!(app.panel, Panel::Tasks);
        assert_eq!(
            app.composer_notice.as_deref(),
            Some("Showing 1 current task.")
        );
    }

    #[test]
    fn post_launch_credential_entry_is_masked_and_cancellable() {
        let mut app = AppModel::new(SessionId::new());
        app.credential_target = Some((
            "OPENAI_API_KEY".into(),
            crate::credential::CredentialSlot::default(),
        ));
        app.reduce(Action::AddCredential);
        let (handled, submission) =
            app.handle_credential_event(&Event::Paste("sk-test-secret".into()));
        assert!(handled);
        assert!(submission.is_none());

        let backend = TestBackend::new(100, 34);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| render(frame, &mut app)).unwrap();
        let buffer = terminal.backend().buffer();
        let rendered = (0..buffer.area.height)
            .flat_map(|y| (0..buffer.area.width).map(move |x| buffer[(x, y)].symbol()))
            .collect::<String>();
        assert!(!rendered.contains("sk-test-secret"));
        assert!(rendered.contains('•'));

        let escape = Event::Key(crossterm::event::KeyEvent::new(
            KeyCode::Esc,
            KeyModifiers::NONE,
        ));
        let (handled, _) = app.handle_credential_event(&escape);
        assert!(handled);
        assert!(app.credential_input.is_none());
        assert!(!app.quit);
    }
    #[test]
    fn full_screen_events_open_and_scroll_without_moving_task_selection() {
        let mut app = AppModel::new(SessionId::new());
        app.events = vec!["a sufficiently long event message for scrolling".into(); 20];
        app.reduce(Action::ToggleEvents);
        assert!(app.event_viewer);
        assert_eq!(app.panel, Panel::Events);
        assert!(app.event_follow);
        app.reduce(Action::PageDown);
        assert_eq!(app.event_scroll, 10);
        assert!(!app.event_follow);
        assert_eq!(app.selected, 0);
        app.reduce(Action::Escape);
        assert!(!app.event_viewer);
        assert!(!app.quit);
    }

    #[test]
    fn events_follow_new_activity_until_the_user_scrolls_history() {
        let mut app = AppModel::new(SessionId::new());
        app.panel = Panel::Events;
        app.events = vec!["older event".into(); 20];

        app.reduce(Action::PageDown);
        assert_eq!(app.event_scroll, 10);
        assert!(!app.event_follow);
        app.push_event(&CanonicalEventKind::ProviderCompleted);
        assert_eq!(app.event_scroll, 10);

        app.reduce(Action::PageUp);
        assert_eq!(app.event_scroll, 0);
        assert!(app.event_follow);
        app.push_event(&CanonicalEventKind::ProviderCompleted);
        assert_eq!(app.event_scroll, 0);
        assert!(app.event_follow);
    }
    #[test]
    fn terminal_controls_are_sanitized() {
        assert_eq!(sanitize("ok\u{1b}[31m"), "ok[31m");
        assert_eq!(
            sanitize("API_KEY=secret TOKEN=abc sk-live"),
            "[REDACTED] [REDACTED] [REDACTED]"
        );
        assert_eq!(
            sanitize("Başlık\n\n- ilk madde\n  devam"),
            "Başlık\n\n- ilk madde\n  devam"
        );
        assert_eq!(sanitize(" birleştirilen parça"), " birleştirilen parça");
    }
    #[test]
    fn streaming_deltas_are_coalesced() {
        let mut app = AppModel::new(SessionId::new());
        app.push_event(&CanonicalEventKind::TextDelta("Mer".into()));
        app.push_event(&CanonicalEventKind::TextDelta("haba".into()));
        assert_eq!(app.events, vec!["Merhaba"]);
        app.push_event(&CanonicalEventKind::ProviderCompleted);
        assert_eq!(app.events.len(), 2);
    }
    #[test]
    fn streaming_preserves_spaces_and_markdown_lines() {
        let mut app = AppModel::new(SessionId::new());
        app.push_event(&CanonicalEventKind::TextDelta("Repo küçük,".into()));
        app.push_event(&CanonicalEventKind::TextDelta(" iyi ayrılmış.\n\n".into()));
        app.push_event(&CanonicalEventKind::TextDelta("- İlk öneri".into()));
        assert_eq!(app.events, vec!["Repo küçük, iyi ayrılmış.\n\n- İlk öneri"]);
    }

    #[test]
    fn usage_is_priced_for_the_task_owners_catalog_model() {
        let mut app = AppModel::new(SessionId::new());
        let mut openai = agent("Ada", "technical_lead", vec![]);
        openai.provider = "cloud".into();
        openai.model = "gpt-5-mini".into();
        let task = Task::root(
            openai.id,
            "price this run",
            vec!["usage recorded".into()],
            voff_core::TaskLimits::default(),
        );
        app.model_catalog = ModelCatalog::bundled().unwrap();
        app.agent_provider_kinds.insert(openai.id, "openai".into());
        app.agents.push(openai);
        app.tasks.push(task.clone());

        app.apply_event(EngineEvent::Canonical(voff_core::CanonicalEvent {
            id: voff_core::EventId::new(),
            session_id: app.session,
            task_id: Some(task.id),
            created_at: chrono::Utc::now(),
            kind: CanonicalEventKind::Usage {
                input_tokens: 1_000,
                output_tokens: 500,
            },
        }));

        assert!(app.estimated_cost.is_some_and(|cost| cost > 0.0));
        assert!(!app.cost_partial);
    }

    #[test]
    fn team_graph_renders_developer_cards_and_delegation_threads() {
        let mut app = AppModel::new(SessionId::new());
        app.workspace_root = "/workspaces/voff-demo".into();
        app.agents = vec![
            agent("Ada", "technical_lead", vec!["Linus".into()]),
            agent("Linus", "backend_developer", vec![]),
        ];
        app.agent_positions = vec![AgentPosition::default(); 2];
        let backend = TestBackend::new(120, 36);
        let mut terminal = Terminal::new(backend).unwrap();

        terminal.draw(|frame| render(frame, &mut app)).unwrap();

        let buffer = terminal.backend().buffer();
        let rendered = (0..buffer.area.height)
            .flat_map(|y| (0..buffer.area.width).map(move |x| buffer[(x, y)].symbol()))
            .collect::<String>();
        assert!(rendered.contains("Ada"));
        assert!(rendered.contains("Linus"));
        assert!(rendered.contains("qwen-test"));
        assert!(rendered.contains("4096 tokens"));
        assert!(rendered.contains('┈'));
        assert!(rendered.contains("cwd /workspaces/voff-demo"));
    }

    #[test]
    fn planned_handoffs_are_visible_before_approval() {
        let mut app = AppModel::new(SessionId::new());
        let lead = agent(
            "Ada",
            "technical_lead",
            vec!["Backend".into(), "Frontend".into()],
        );
        let backend_agent = agent("Backend", "backend_developer", vec![]);
        let frontend_agent = agent("Frontend", "frontend_developer", vec![]);
        let mut root = Task::root(
            lead.id,
            "Build full-stack calculator",
            vec!["Both workstreams complete".into()],
            voff_core::TaskLimits::default(),
        );
        root.transition(TaskStatus::Ready).unwrap();
        root.transition(TaskStatus::Running).unwrap();
        root.transition(TaskStatus::WaitingApproval).unwrap();
        let proposals = [
            DelegationProposal {
                target_agent: backend_agent.name.clone(),
                objective: "Create calculator API".into(),
                acceptance_criteria: vec!["API responds".into()],
            },
            DelegationProposal {
                target_agent: frontend_agent.name.clone(),
                objective: "Create calculator interface".into(),
                acceptance_criteria: vec!["UI calls API".into()],
            },
        ];
        app.approvals = proposals
            .iter()
            .map(|proposal| Approval {
                id: voff_core::ApprovalId::new(),
                task_id: root.id,
                tool_call_id: voff_core::ToolCallId::new(),
                action: "task.delegate".into(),
                working_directory: ".".into(),
                reason: serde_json::to_string(proposal).unwrap(),
                decision: ApprovalDecision::Pending,
                created_at: chrono::Utc::now(),
            })
            .collect();
        app.tasks = vec![root];
        app.agents = vec![lead, backend_agent, frontend_agent];
        app.agent_positions = vec![AgentPosition::default(); 3];
        let backend = TestBackend::new(120, 36);
        let mut terminal = Terminal::new(backend).unwrap();

        terminal.draw(|frame| render(frame, &mut app)).unwrap();

        let buffer = terminal.backend().buffer();
        let rendered = (0..buffer.area.height)
            .flat_map(|y| (0..buffer.area.width).map(move |x| buffer[(x, y)].symbol()))
            .collect::<String>();
        assert!(
            rendered.contains("2 handoffs awaiting approval"),
            "{rendered}"
        );
        assert!(rendered.contains("Backend"), "{rendered}");
        assert!(rendered.contains("Frontend"), "{rendered}");
        assert!(rendered.matches("approval").count() >= 3, "{rendered}");
        assert!(rendered.matches('●').count() >= 2, "{rendered}");
    }

    #[test]
    fn live_delegation_labels_the_edge_and_spins_on_working_roles() {
        let mut app = AppModel::new(SessionId::new());
        let lead = agent("Ada", "technical_lead", vec!["Linus".into()]);
        let implementer = agent("Linus", "backend_developer", vec![]);
        let mut parent = Task::root(
            lead.id,
            "Build the calculator",
            vec!["Calculator works".into()],
            voff_core::TaskLimits::default(),
        );
        parent.transition(TaskStatus::Ready).unwrap();
        parent.transition(TaskStatus::Running).unwrap();
        let parent_id = parent.id;
        let mut graph = voff_core::TaskGraph::default();
        graph.add_root(parent);
        let child_id = graph
            .add_child(
                parent_id,
                implementer.id,
                "Implement calculator logic",
                vec!["Tests pass".into()],
            )
            .unwrap();
        let mut tasks = graph.tasks();
        let child = tasks.iter_mut().find(|task| task.id == child_id).unwrap();
        child.transition(TaskStatus::Ready).unwrap();
        child.transition(TaskStatus::Running).unwrap();

        app.agents = vec![lead, implementer];
        app.tasks = tasks;
        app.animation_tick = 3;
        let backend = TestBackend::new(120, 36);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| render(frame, &mut app)).unwrap();

        let buffer = terminal.backend().buffer();
        let rendered = (0..buffer.area.height)
            .flat_map(|y| (0..buffer.area.width).map(move |x| buffer[(x, y)].symbol()))
            .collect::<String>();
        assert!(rendered.contains("Build the calculator"));
        assert!(rendered.contains("Implement calculator"));
        assert!(rendered.contains('▶'));
        assert!(rendered.contains('━'));
        assert!(
            FluxFrames::ORBIT
                .iter()
                .any(|frame| rendered.contains(*frame))
        );
    }

    #[test]
    fn completed_handoff_stays_visible_in_a_narrow_terminal() {
        let mut app = AppModel::new(SessionId::new());
        let lead = agent("Ada", "technical_lead", vec!["Linus".into()]);
        let implementer = agent("Linus", "backend_developer", vec![]);
        let mut parent = Task::root(
            lead.id,
            "Build the calculator",
            vec!["Calculator works".into()],
            voff_core::TaskLimits::default(),
        );
        parent.transition(TaskStatus::Ready).unwrap();
        parent.transition(TaskStatus::Running).unwrap();
        let parent_id = parent.id;
        let mut graph = voff_core::TaskGraph::default();
        graph.add_root(parent);
        let child_id = graph
            .add_child(
                parent_id,
                implementer.id,
                "Implement calculator UI",
                vec!["Works offline".into()],
            )
            .unwrap();
        let mut tasks = graph.tasks();
        let child = tasks.iter_mut().find(|task| task.id == child_id).unwrap();
        child.transition(TaskStatus::Ready).unwrap();
        child.transition(TaskStatus::Running).unwrap();
        child.transition(TaskStatus::Completed).unwrap();
        let parent = tasks.iter_mut().find(|task| task.id == parent_id).unwrap();
        parent.transition(TaskStatus::Completed).unwrap();

        app.agents = vec![lead, implementer];
        app.tasks = tasks;
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| render(frame, &mut app)).unwrap();

        let buffer = terminal.backend().buffer();
        let rendered = (0..buffer.area.height)
            .flat_map(|y| (0..buffer.area.width).map(move |x| buffer[(x, y)].symbol()))
            .collect::<String>();
        assert!(rendered.contains("1 handoff complete"));
        assert!(rendered.contains("✓ done"), "{rendered}");
        assert!(rendered.contains("⬢ Linus"), "{rendered}");
        assert!(rendered.contains('━'));
        assert!(rendered.contains('●'));
        assert!(rendered.matches("complete").count() >= 3);
    }

    #[test]
    fn mouse_drag_moves_and_selects_a_developer_card() {
        let mut app = AppModel::new(SessionId::new());
        app.agents = vec![agent("Ada", "technical_lead", vec![])];
        app.agent_positions = vec![AgentPosition::default()];
        app.agent_hitboxes = vec![Rect::new(10, 5, 20, 7)];

        app.reduce(Action::Mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            12,
            6,
        )));
        app.reduce(Action::Mouse(mouse(
            MouseEventKind::Drag(MouseButton::Left),
            17,
            9,
        )));
        app.reduce(Action::Mouse(mouse(
            MouseEventKind::Up(MouseButton::Left),
            17,
            9,
        )));

        assert_eq!(app.panel, Panel::Agents);
        assert_eq!(app.selected_agent, 0);
        assert_eq!(app.agent_positions[0], AgentPosition { x: 5, y: 3 });
        assert!(app.dragging_agent.is_none());
    }
}
