//! Responsive Ratatui shell and pure action reducer.
use agentmux_core::{
    AgentId, Approval, ApprovalDecision, Artifact, CanonicalEventKind, SessionId, Task, TaskStatus,
};
use agentmux_engine::{Engine, EngineEvent};
use agentmux_provider::ProviderHealth;
use agentmux_providers::{MockProvider, MockStep};
use agentmux_storage::{InMemoryRepository, Repository, SqliteRepository};
use crossterm::{
    event::{self, Event, KeyCode},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{
    Terminal,
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout},
    style::{Color, Style},
    text::Line,
    widgets::{Block, Borders, Clear, List, ListItem, Paragraph, Wrap},
};
use std::{io, panic, sync::Arc, time::Duration};
use thiserror::Error;
use tokio::sync::mpsc;

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
    StartMock,
    Cancel,
    Approve,
    Deny,
    ToggleEvents,
    PageUp,
    PageDown,
    Escape,
    Quit,
    None,
}
#[derive(Debug)]
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
    pub agent_lines: Vec<String>,
    pub approvals: Vec<Approval>,
    pub artifacts: Vec<Artifact>,
    pub provider_health: ProviderHealth,
    pub input_cost_per_million: Option<f64>,
    pub output_cost_per_million: Option<f64>,
    pub cost_currency: String,
    pub estimated_cost: Option<f64>,
    pub event_viewer: bool,
    pub event_scroll: u16,
    streaming_text: bool,
}
impl AppModel {
    #[must_use]
    pub fn new(session: SessionId) -> Self {
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
            agent_lines: vec![],
            approvals: vec![],
            artifacts: vec![],
            provider_health: ProviderHealth::default(),
            input_cost_per_million: None,
            output_cost_per_million: None,
            cost_currency: "USD".into(),
            estimated_cost: None,
            event_viewer: false,
            event_scroll: 0,
            streaming_text: false,
        }
    }
    pub fn reduce(&mut self, action: Action) {
        match action {
            Action::Up => {
                if self.event_viewer || self.panel == Panel::Events {
                    self.event_scroll = self.event_scroll.saturating_sub(1);
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
                } else if self.selected + 1 < self.tasks.len() {
                    self.selected += 1
                }
            }
            Action::PageUp => self.event_scroll = self.event_scroll.saturating_sub(10),
            Action::PageDown => {
                self.event_scroll = self
                    .event_scroll
                    .saturating_add(10)
                    .min(self.event_scroll_limit());
            }
            Action::NextPanel => {
                self.panel = match self.panel {
                    Panel::Tasks => Panel::Details,
                    Panel::Details => Panel::Agents,
                    Panel::Agents => Panel::Events,
                    Panel::Events => Panel::Tasks,
                }
            }
            Action::ToggleHelp => self.help = !self.help,
            Action::ToggleEvents => {
                self.event_viewer = !self.event_viewer;
                self.panel = Panel::Events;
                self.event_scroll = 0;
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
            Action::StartMock | Action::Cancel | Action::Approve | Action::Deny | Action::None => {}
        }
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
                    && let (Some(input_rate), Some(output_rate)) =
                        (self.input_cost_per_million, self.output_cost_per_million)
                {
                    let amount = *input_tokens as f64 * input_rate / 1_000_000.0
                        + *output_tokens as f64 * output_rate / 1_000_000.0;
                    *self.estimated_cost.get_or_insert(0.0) += amount;
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
        KeyCode::Char('r') => Action::StartMock,
        KeyCode::Char('c') => Action::Cancel,
        KeyCode::Char('y') => Action::Approve,
        KeyCode::Char('n') => Action::Deny,
        KeyCode::Char('e') | KeyCode::Enter => Action::ToggleEvents,
        KeyCode::PageUp => Action::PageUp,
        KeyCode::PageDown => Action::PageDown,
        KeyCode::Esc => Action::Escape,
        KeyCode::Char('q') => Action::Quit,
        _ => Action::None,
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
pub async fn run() -> Result<(), TuiError> {
    let session = SessionId::new();
    let repo = Arc::new(InMemoryRepository::default());
    let provider = Arc::new(MockProvider::new(vec![
        MockStep::Event(agentmux_provider::ProviderEvent::TextDelta(
            "mock analysis".into(),
        )),
        MockStep::Delay(Duration::from_millis(400)),
        MockStep::Event(agentmux_provider::ProviderEvent::Completed),
    ]));
    let (event_tx, event_rx) = mpsc::channel(128);
    let engine = Arc::new(Engine::new(repo, provider, session, event_tx));
    let (action_tx, action_rx) = mpsc::channel(32);
    let mut model = AppModel::new(session);
    terminal_loop(
        &mut model,
        engine,
        LoopConfig {
            owner: AgentId::new(),
            objective: "Run deterministic mock task".into(),
            max_parallel_tasks: 1,
        },
        action_tx,
        action_rx,
        event_rx,
    )
    .await
}
pub struct ConfiguredRun {
    pub repo: Arc<SqliteRepository>,
    pub provider: Arc<dyn agentmux_provider::ModelProvider>,
    pub session: SessionId,
    pub owner: AgentId,
    pub model_name: String,
    pub system_prompt: String,
    pub provider_label: String,
    pub agent_name: String,
    pub agent_profiles: Vec<agentmux_core::AgentProfile>,
    pub objective: String,
    pub initial_tasks: Vec<Task>,
    pub initial_events: Vec<agentmux_core::CanonicalEvent>,
    pub initial_approvals: Vec<Approval>,
    pub initial_artifacts: Vec<Artifact>,
    pub capacity: usize,
    pub workspace_context: String,
    pub workspace_root: std::path::PathBuf,
    pub max_parallel_tasks: usize,
    pub input_cost_per_million: Option<f64>,
    pub output_cost_per_million: Option<f64>,
    pub cost_currency: String,
}
pub async fn run_configured(config: ConfiguredRun) -> Result<(), TuiError> {
    let (event_tx, event_rx) = mpsc::channel(config.capacity);
    let model_label = config.model_name.clone();
    let engine = Arc::new(
        Engine::new(config.repo, config.provider, config.session, event_tx)
            .with_runtime(config.model_name, config.system_prompt)
            .with_workspace_context(config.workspace_context)
            .with_safe_workflow(&config.workspace_root)
            .map_err(|e| TuiError::Io(io::Error::other(e)))?,
    );
    let (action_tx, action_rx) = mpsc::channel(32);
    let mut app = AppModel::new(config.session);
    app.provider_label = config.provider_label;
    app.agent_name = config.agent_name;
    app.agent_lines = config
        .agent_profiles
        .into_iter()
        .map(|a| format!("{} — {}", a.name, a.role))
        .collect();
    app.model_label = model_label;
    app.input_cost_per_million = config.input_cost_per_million;
    app.output_cost_per_million = config.output_cost_per_million;
    app.cost_currency = config.cost_currency;
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
            max_parallel_tasks: config.max_parallel_tasks,
        },
        action_tx,
        action_rx,
        event_rx,
    )
    .await
}
struct LoopConfig {
    owner: AgentId,
    objective: String,
    max_parallel_tasks: usize,
}
async fn terminal_loop<R: Repository + 'static>(
    model: &mut AppModel,
    engine: Arc<Engine<R>>,
    config: LoopConfig,
    action_tx: mpsc::Sender<Action>,
    mut action_rx: mpsc::Receiver<Action>,
    mut event_rx: mpsc::Receiver<EngineEvent>,
) -> Result<(), TuiError> {
    let LoopConfig {
        owner,
        objective,
        max_parallel_tasks,
    } = config;
    let _guard = TerminalGuard::enter()?;
    let mut terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;
    let input = tokio::task::spawn_blocking(move || {
        loop {
            match event::read() {
                Ok(Event::Key(k)) => {
                    let action = action_for_key(k.code);
                    if action_tx.blocking_send(action).is_err() || action == Action::Quit {
                        break;
                    }
                }
                Ok(_) => {}
                Err(_) => break,
            }
        }
    });
    let mut tick = tokio::time::interval(Duration::from_millis(100));
    while !model.quit {
        tokio::select! {Some(action)=action_rx.recv()=>{match action{Action::StartMock=>{let engine=Arc::clone(&engine);let objective=objective.clone();tokio::spawn(async move{if let Ok(task)=engine.create_task(owner,objective,vec!["Provider run completes".into()]).await{let _=engine.mark_ready(task.id).await;let _=engine.run_task(task.id).await;}});}Action::Cancel=>{if let Some(id)=model.selected_task().filter(|t|matches!(t.status,TaskStatus::Queued|TaskStatus::Ready|TaskStatus::Running)).map(|t|t.id){let engine=Arc::clone(&engine);tokio::spawn(async move{let _=engine.cancel_task(id).await;});}}Action::Approve|Action::Deny=>{if let Some(id)=model.pending_approval().map(|v|v.id){let engine=Arc::clone(&engine);let decision=if action==Action::Approve{ApprovalDecision::ApprovedOnce}else{ApprovalDecision::Denied};tokio::spawn(async move{if engine.decide_approval(id,decision).await.is_ok() && decision==ApprovalDecision::ApprovedOnce { let _=engine.run_until_idle_bounded(max_parallel_tasks).await; }});}}other=>model.reduce(other)}}Some(message)=event_rx.recv()=>model.apply_event(message),_=tick.tick()=>{terminal.draw(|frame|render(frame,model))?;}else=>{model.quit=true}}
    }
    input.abort();
    Ok(())
}
fn render(frame: &mut ratatui::Frame<'_>, app: &AppModel) {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(8),
            Constraint::Length(2),
        ])
        .split(frame.area());
    frame.render_widget(
        Paragraph::new(format!(
            "AgentMux | session {} | provider {}",
            app.session, app.provider_label
        ))
        .block(Block::default().borders(Borders::ALL)),
        rows[0],
    );
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage(30),
            Constraint::Percentage(45),
            Constraint::Percentage(25),
        ])
        .split(rows[1]);
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
        cols[0],
    );
    let details = app.selected_task().map_or_else(
        || "No task selected. Press r to start the configured task.".into(),
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
        cols[1],
    );
    let right = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Percentage(35), Constraint::Percentage(65)])
        .split(cols[2]);
    frame.render_widget(
        Paragraph::new(format!(
            "{}\nmodel: {}\nprovider: {:?}\nretries: {} | failures: {}\ncost: {}{}",
            if app.agent_lines.is_empty() {
                app.agent_name.clone()
            } else {
                app.agent_lines.join("\n")
            },
            app.model_label,
            app.provider_health.circuit,
            app.provider_health.retries,
            app.provider_health.consecutive_failures,
            app.estimated_cost
                .map(|cost| format!("{cost:.6} {} (estimated)", app.cost_currency))
                .unwrap_or_else(|| "not configured".into()),
            app.provider_health
                .last_error
                .as_ref()
                .map(|error| format!("\nlast error: {}", sanitize(error)))
                .unwrap_or_default()
        ))
        .block(panel("Agents", app.panel == Panel::Agents)),
        right[0],
    );
    let logs = app
        .events
        .iter()
        .rev()
        .take(20)
        .rev()
        .map(|x| Line::from(x.as_str()))
        .collect::<Vec<_>>();
    frame.render_widget(
        Paragraph::new(logs)
            .wrap(Wrap { trim: false })
            .block(panel("Events (e: expand)", app.panel == Panel::Events)),
        right[1],
    );
    frame.render_widget(
        Paragraph::new(
            "↑/↓ or j/k navigate | Tab panel | e events | ? help | r run | c cancel | q quit",
        ),
        rows[2],
    );
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
        frame.render_widget(
            Paragraph::new(text)
                .wrap(Wrap { trim: false })
                .scroll((app.event_scroll, 0))
                .block(
                    Block::default()
                        .title("Events — newest first | ↑/↓, PgUp/PgDn scroll | e/Esc close")
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
        frame.render_widget(Paragraph::new("AgentMux Help\n\nr  Start configured task\nc  Cancel selected active task\ne/Enter  Open full-screen events\n↑/↓, PgUp/PgDn  Scroll events\nTab  Change panel\nq  Safe exit\nEsc  Close popup / exit\n?  Close help").wrap(Wrap { trim: false }).block(Block::default().title("Help").borders(Borders::ALL)),popup);
    }
    if let Some(approval) = app.pending_approval() {
        let area = frame.area();
        let popup = ratatui::layout::Rect {
            x: area.width / 5,
            y: area.height / 4,
            width: area.width * 3 / 5,
            height: area.height / 2,
        };
        frame.render_widget(Clear, popup);
        let preview = app
            .artifacts
            .iter()
            .find(|a| a.producer == approval.task_id)
            .map(|a| format!("\n\nDiff preview:\n{}", sanitize(&a.preview)))
            .unwrap_or_default();
        frame.render_widget(Paragraph::new(format!("Action: {}\nTask: {}\nWorking directory: {}\nReason: {}{}\n\n[y] Approve once    [n] Deny", sanitize(&approval.action), approval.task_id, sanitize(&approval.working_directory), sanitize(&approval.reason), preview)).wrap(Wrap { trim: true }).block(Block::default().title("Approval required").borders(Borders::ALL).border_style(Style::default().fg(Color::Yellow))), popup);
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
        execute!(io::stdout(), EnterAlternateScreen)?;
        let old = panic::take_hook();
        panic::set_hook(Box::new(move |info| {
            let _ = disable_raw_mode();
            let _ = execute!(io::stdout(), LeaveAlternateScreen);
            old(info);
        }));
        Ok(Self)
    }
}
impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen);
    }
}
#[cfg(test)]
mod tests {
    use super::*;
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
        assert_eq!(action_for_key(KeyCode::Char('c')), Action::Cancel);
        assert_eq!(action_for_key(KeyCode::Char('q')), Action::Quit);
        assert_eq!(action_for_key(KeyCode::Char('y')), Action::Approve);
        assert_eq!(action_for_key(KeyCode::Char('n')), Action::Deny)
    }
    #[test]
    fn full_screen_events_open_and_scroll_without_moving_task_selection() {
        let mut app = AppModel::new(SessionId::new());
        app.events = vec!["a sufficiently long event message for scrolling".into(); 20];
        app.reduce(Action::ToggleEvents);
        assert!(app.event_viewer);
        assert_eq!(app.panel, Panel::Events);
        app.reduce(Action::PageDown);
        assert_eq!(app.event_scroll, 10);
        assert_eq!(app.selected, 0);
        app.reduce(Action::Escape);
        assert!(!app.event_viewer);
        assert!(!app.quit);
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
}
