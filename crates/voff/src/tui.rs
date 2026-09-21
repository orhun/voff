//! Responsive Ratatui shell and pure action reducer.
mod team_flow;

use crate::{model_catalog::ModelCatalog, onboarding::DeveloperRole};
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
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Clear, List, ListItem, Paragraph, Wrap},
};
use ratatui_textarea::TextArea;
use std::{
    collections::{HashMap, HashSet},
    io, panic,
    path::PathBuf,
    sync::Arc,
    time::Duration,
};
use thiserror::Error;
use tokio::sync::mpsc;
use tui_popup::{KnownSizeWrapper, Popup};
use voff_core::{
    AgentId, AgentProfile, Approval, ApprovalDecision, Artifact, CanonicalEvent,
    CanonicalEventKind, EventId, SessionId, Task, TaskStatus,
};
use voff_engine::{DelegationProposal, Engine, EngineEvent};
use voff_provider::ProviderHealth;
use voff_storage::{Repository, SqliteRepository};

use self::team_flow::{TeamFlow, handoff_summary};

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
    FitTeam,
    ZoomTeamIn,
    ZoomTeamOut,
    ResetTeamZoom,
    AdjustTeam,
    ShowTeam,
    ShowTasks,
    ShowDetails,
    ShowEvents,
    Escape,
    Quit,
    Mouse(MouseEvent),
    None,
}

#[derive(Debug, Eq, PartialEq)]
enum ComposerEvent {
    Ignored,
    Consumed,
    Submit(String),
}

#[derive(Debug)]
enum TeamAdjustmentEvent {
    Ignored,
    Consumed,
    Save(Box<AgentProfile>),
    Add(Box<AgentProfile>),
}

enum SavedTeamAdjustment {
    Availability(AgentProfile, CanonicalEvent),
    Added(AgentProfile, AgentProfile),
}

const ADDABLE_ROLES: [DeveloperRole; 6] = [
    DeveloperRole::Backend,
    DeveloperRole::Frontend,
    DeveloperRole::FullStack,
    DeveloperRole::Reviewer,
    DeveloperRole::Tester,
    DeveloperRole::DevOps,
];

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
    approval_selected: usize,
    team_lead: Option<AgentId>,
    team_adjustment_open: bool,
    team_adjustment_selected: usize,
    team_role_picker: bool,
    team_role_selected: usize,
    team_adjustment_saving: bool,
    team_adjustment_notice: Option<String>,
    team_flow: TeamFlow,
    streaming_text: bool,
}
impl AppModel {
    #[must_use]
    pub fn new(session: SessionId) -> Self {
        let mut composer = TextArea::default();
        composer.set_cursor_line_style(Style::default());
        composer.set_placeholder_text("Message Team Lead · /task assign · /tasks list");
        Self {
            session,
            tasks: vec![],
            events: vec![],
            selected: 0,
            panel: Panel::Agents,
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
            approval_selected: 0,
            team_lead: None,
            team_adjustment_open: false,
            team_adjustment_selected: 0,
            team_role_picker: false,
            team_role_selected: 0,
            team_adjustment_saving: false,
            team_adjustment_notice: None,
            team_flow: TeamFlow::default(),
            streaming_text: false,
        }
    }
    pub fn reduce(&mut self, action: Action) {
        if self.navigate_approvals(action) {
            return;
        }
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
            Action::FitTeam if self.panel == Panel::Agents => self.team_flow.fit_view(),
            Action::ZoomTeamIn if self.panel == Panel::Agents => self.team_flow.zoom_in(),
            Action::ZoomTeamOut if self.panel == Panel::Agents => self.team_flow.zoom_out(),
            Action::ResetTeamZoom if self.panel == Panel::Agents => self.team_flow.reset_zoom(),
            Action::AdjustTeam => {
                self.team_adjustment_open = true;
                self.team_adjustment_selected =
                    self.selected_agent.min(self.agents.len().saturating_sub(1));
                self.team_role_picker = false;
                self.team_adjustment_notice = None;
            }
            Action::ShowTeam => self.panel = Panel::Agents,
            Action::ShowTasks => self.panel = Panel::Tasks,
            Action::ShowDetails => self.panel = Panel::Details,
            Action::ShowEvents => {
                self.panel = Panel::Events;
                self.follow_newest_event();
            }
            Action::NextPanel => {
                self.panel = match self.panel {
                    Panel::Agents => Panel::Tasks,
                    Panel::Tasks => Panel::Details,
                    Panel::Details => Panel::Events,
                    Panel::Events => Panel::Agents,
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
            Action::StartMock
            | Action::Cancel
            | Action::Approve
            | Action::Deny
            | Action::FitTeam
            | Action::ZoomTeamIn
            | Action::ZoomTeamOut
            | Action::ResetTeamZoom
            | Action::None => {}
        }
    }

    fn handle_mouse(&mut self, mouse: MouseEvent) {
        if matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left))
            && self
                .composer_hitbox
                .contains((mouse.column, mouse.row).into())
        {
            self.composer_active = true;
            self.composer_notice = None;
            return;
        }
        let interaction = self.team_flow.handle_mouse(mouse);
        if interaction.handled {
            self.panel = Panel::Agents;
            if let Some(index) = interaction.selected {
                self.selected_agent = index;
            }
        }
    }

    fn handle_team_adjustment_event(&mut self, event: &Event) -> TeamAdjustmentEvent {
        if !self.team_adjustment_open {
            return TeamAdjustmentEvent::Ignored;
        }
        if self.team_adjustment_saving {
            return TeamAdjustmentEvent::Consumed;
        }
        let Event::Key(key) = event else {
            return TeamAdjustmentEvent::Consumed;
        };
        if key.kind != KeyEventKind::Press {
            return TeamAdjustmentEvent::Consumed;
        }
        if self.team_role_picker {
            match key.code {
                KeyCode::Esc => {
                    self.team_role_picker = false;
                    self.team_adjustment_notice = None;
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    self.team_role_selected = self.team_role_selected.saturating_sub(1);
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    self.team_role_selected =
                        (self.team_role_selected + 1).min(ADDABLE_ROLES.len() - 1);
                }
                KeyCode::Enter | KeyCode::Char(' ') => {
                    let Some(member) = self.new_team_member(ADDABLE_ROLES[self.team_role_selected])
                    else {
                        self.team_adjustment_notice =
                            Some("The required Team Lead profile is unavailable.".into());
                        return TeamAdjustmentEvent::Consumed;
                    };
                    self.team_adjustment_saving = true;
                    self.team_adjustment_notice = Some("Adding specialist…".into());
                    return TeamAdjustmentEvent::Add(Box::new(member));
                }
                _ => {}
            }
            return TeamAdjustmentEvent::Consumed;
        }
        match key.code {
            KeyCode::Esc | KeyCode::Char('S') => {
                self.team_adjustment_open = false;
                self.team_adjustment_notice = None;
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.team_adjustment_selected = self.team_adjustment_selected.saturating_sub(1);
                self.team_adjustment_notice = None;
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if self.team_adjustment_selected + 1 < self.agents.len() {
                    self.team_adjustment_selected += 1;
                }
                self.team_adjustment_notice = None;
            }
            KeyCode::Char('n') => {
                self.team_role_picker = true;
                self.team_role_selected = 0;
                self.team_adjustment_notice = None;
            }
            KeyCode::Enter | KeyCode::Char(' ') => {
                let Some(agent) = self.agents.get(self.team_adjustment_selected) else {
                    self.team_adjustment_notice = Some("No specialist is selected.".into());
                    return TeamAdjustmentEvent::Consumed;
                };
                if Some(agent.id) == self.team_lead {
                    self.team_adjustment_notice =
                        Some("The Team Lead is required and cannot be disabled.".into());
                    return TeamAdjustmentEvent::Consumed;
                }
                if self.agent_has_committed_work(agent) {
                    self.team_adjustment_notice = Some(
                        "Finish, cancel, or decide this specialist's active work before changing availability."
                            .into(),
                    );
                    return TeamAdjustmentEvent::Consumed;
                }
                let mut updated = agent.clone();
                updated.enabled = !updated.enabled;
                self.team_adjustment_saving = true;
                self.team_adjustment_notice = Some("Saving team adjustment…".into());
                return TeamAdjustmentEvent::Save(Box::new(updated));
            }
            _ => {}
        }
        TeamAdjustmentEvent::Consumed
    }

    fn new_team_member(&self, role: DeveloperRole) -> Option<AgentProfile> {
        let lead = self
            .team_lead
            .and_then(|id| self.agents.iter().find(|agent| agent.id == id))?;
        let base_name = match role {
            DeveloperRole::Backend => "Backend Developer",
            DeveloperRole::Frontend => "Frontend Developer",
            DeveloperRole::FullStack => "Full-stack Developer",
            DeveloperRole::Reviewer => "Reviewer",
            DeveloperRole::Tester => "Test Engineer",
            DeveloperRole::DevOps => "DevOps Engineer",
            DeveloperRole::Lead => return None,
        };
        let name = (1..)
            .map(|index| {
                if index == 1 {
                    base_name.to_owned()
                } else {
                    format!("{base_name} {index}")
                }
            })
            .find(|candidate| self.agents.iter().all(|agent| agent.name != *candidate))?;
        let mut tools = role.tools();
        tools.retain(|tool| !matches!(tool.as_str(), "task.delegate" | "review.request"));
        let delegation_limits =
            lead.delegation_limits
                .as_ref()
                .map(|limits| voff_core::TaskLimits {
                    max_delegation_depth: 0,
                    max_children: 0,
                    max_duration_seconds: limits.max_duration_seconds,
                });
        Some(AgentProfile {
            id: AgentId::new(),
            name,
            role: role.config_role().into(),
            provider: lead.provider.clone(),
            model: lead.model.clone(),
            system_prompt: role.prompt().into(),
            tools,
            max_output_tokens: lead.max_output_tokens,
            enabled: true,
            allowed_targets: vec![],
            delegation_limits,
        })
    }

    fn agent_has_committed_work(&self, agent: &AgentProfile) -> bool {
        self.tasks.iter().any(|task| {
            task.owner == agent.id
                && matches!(
                    task.status,
                    TaskStatus::Queued
                        | TaskStatus::Ready
                        | TaskStatus::Running
                        | TaskStatus::WaitingApproval
                )
        }) || self.approvals.iter().any(|approval| {
            approval.decision == ApprovalDecision::Pending
                && matches!(approval.action.as_str(), "task.delegate" | "review.request")
                && serde_json::from_str::<DelegationProposal>(&approval.reason)
                    .is_ok_and(|proposal| proposal.target_agent == agent.name)
        })
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
        let pending_count = self.pending_approval_count();
        let selected = self.approval_selected.min(pending_count.saturating_sub(1));
        self.approvals
            .iter()
            .filter(|approval| approval.decision == ApprovalDecision::Pending)
            .nth(selected)
    }
    fn pending_approval_count(&self) -> usize {
        self.approvals
            .iter()
            .filter(|approval| approval.decision == ApprovalDecision::Pending)
            .count()
    }
    fn navigate_approvals(&mut self, action: Action) -> bool {
        let count = self.pending_approval_count();
        if count == 0 {
            self.approval_selected = 0;
            return false;
        }
        self.approval_selected = self.approval_selected.min(count - 1);
        match action {
            Action::Up => {
                self.approval_selected = self.approval_selected.saturating_sub(1);
                true
            }
            Action::Down => {
                self.approval_selected = (self.approval_selected + 1).min(count - 1);
                true
            }
            _ => false,
        }
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
        KeyCode::Char('f') => Action::FitTeam,
        KeyCode::Char('+') | KeyCode::Char('=') => Action::ZoomTeamIn,
        KeyCode::Char('-') => Action::ZoomTeamOut,
        KeyCode::Char('0') => Action::ResetTeamZoom,
        KeyCode::Char('S') => Action::AdjustTeam,
        KeyCode::Char('1') => Action::ShowTeam,
        KeyCode::Char('2') => Action::ShowTasks,
        KeyCode::Char('3') => Action::ShowDetails,
        KeyCode::Char('4') => Action::ShowEvents,
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
        CanonicalEventKind::AgentAvailabilityChanged { name, enabled, .. } => format!(
            "team: {} {}",
            sanitize(name),
            if *enabled { "enabled" } else { "disabled" }
        ),
        CanonicalEventKind::AgentAdded { name, role, .. } => {
            format!("team: {} added as {}", sanitize(name), sanitize(role))
        }
    }
}

fn delegation_plan_for_available_agents(
    agents: &[AgentProfile],
    team_lead: Option<AgentId>,
    configured: &[DelegationProposal],
) -> Vec<DelegationProposal> {
    let Some(lead) = team_lead.and_then(|id| agents.iter().find(|agent| agent.id == id)) else {
        return configured.to_vec();
    };
    let eligible = agents
        .iter()
        .filter(|agent| agent.enabled && agent.id != lead.id)
        .filter(|agent| lead.allowed_targets.contains(&agent.name))
        .collect::<Vec<_>>();
    let eligible_names = eligible
        .iter()
        .map(|agent| agent.name.as_str())
        .collect::<HashSet<_>>();
    let mut plan = Vec::new();
    for proposal in configured {
        if eligible_names.contains(proposal.target_agent.as_str()) {
            plan.push(proposal.clone());
            continue;
        }
        let replacement = agents
            .iter()
            .find(|agent| agent.name == proposal.target_agent)
            .and_then(|original| {
                eligible.iter().find(|candidate| {
                    candidate.role == original.role
                        && plan.iter().all(|planned: &DelegationProposal| {
                            planned.target_agent != candidate.name
                        })
                })
            });
        if let Some(replacement) = replacement {
            let mut reassigned = proposal.clone();
            reassigned.target_agent = replacement.name.clone();
            plan.push(reassigned);
        }
    }
    for (index, agent) in eligible.into_iter().enumerate() {
        if plan
            .iter()
            .any(|proposal| proposal.target_agent == agent.name)
        {
            continue;
        }
        plan.push(supporting_delegation(agent, index + 1));
    }
    plan
}

fn supporting_delegation(agent: &AgentProfile, ordinal: usize) -> DelegationProposal {
    let stem = agent
        .name
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>()
        .trim_matches('-')
        .to_owned();
    let stem = if stem.is_empty() {
        format!("specialist-{ordinal}")
    } else {
        stem
    };
    let (objective, acceptance_criteria) = match agent.role.as_str() {
        "backend_developer" => {
            let path = format!("{stem}-backend-assessment.md");
            (
                format!(
                    "Independently assess the parent objective's backend contract and write only {path}. Do not modify another specialist-owned file. Record edge cases, interface risks, and concrete verification advice for the Team Lead."
                ),
                vec![
                    format!("{path} contains a concrete backend assessment"),
                    "No product implementation file is modified".into(),
                ],
            )
        }
        "frontend_developer" => {
            let path = format!("{stem}-frontend-assessment.md");
            (
                format!(
                    "Independently assess the parent objective's accessibility and client/interface behavior and write only {path}. Do not modify another specialist-owned file. Return actionable findings to the Team Lead."
                ),
                vec![
                    format!("{path} covers accessibility, loading, and error states"),
                    "No product implementation file is modified".into(),
                ],
            )
        }
        "code_reviewer" => (
            "Review the current workspace and parent acceptance criteria without editing files. Return prioritized correctness, security, and integration findings to the Team Lead.".into(),
            vec![
                "Findings are concrete and prioritized".into(),
                "The review identifies any blocking acceptance gap".into(),
            ],
        ),
        "test_engineer" => (
            "Design a reproducible validation matrix for the parent objective without editing specialist-owned files. Cover primary behavior, malformed input, accessibility, failure handling, and integration; return the evidence plan to the Team Lead.".into(),
            vec![
                "Validation covers success and failure paths".into(),
                "Every check has reproducible steps and expected results".into(),
            ],
        ),
        "devops_engineer" => {
            let path = format!("{stem}-runbook.md");
            (
                format!(
                    "Write only {path} with a dependency-minimal local runbook for starting, checking, and stopping the parent deliverable. Do not modify implementation files."
                ),
                vec![
                    format!("{path} documents startup, health checking, and shutdown"),
                    "The runbook states every runtime dependency explicitly".into(),
                ],
            )
        }
        _ => {
            let path = format!("{stem}-integration-notes.md");
            (
                format!(
                    "Write only {path} with an independent integration analysis for the parent objective. Do not modify another specialist's artifact."
                ),
                vec![
                    format!("{path} identifies cross-layer risks and verification steps"),
                    "No specialist-owned implementation file is modified".into(),
                ],
            )
        }
    };
    DelegationProposal {
        target_agent: agent.name.clone(),
        objective,
        acceptance_criteria,
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
    pub team_lead: AgentId,
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
            .with_team_lead(config.team_lead)
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
    app.team_lead = Some(config.team_lead);
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
    let (team_adjustment_tx, mut team_adjustment_rx) =
        mpsc::channel::<Result<SavedTeamAdjustment, String>>(1);
    let (lead_submission_tx, mut lead_submission_rx) =
        mpsc::channel::<Result<&'static str, String>>(8);
    let mut tick = tokio::time::interval(Duration::from_millis(100));
    while !model.quit {
        tokio::select! {
            Some(event)=input_rx.recv()=>{
                let team_adjustment = model.handle_team_adjustment_event(&event);
                if let TeamAdjustmentEvent::Save(profile) = team_adjustment {
                    let profile = *profile;
                    let team_adjustment_tx = team_adjustment_tx.clone();
                    let repository = Arc::clone(&credential_repository);
                    let session_id = model.session;
                    tokio::spawn(async move {
                        let audit = CanonicalEvent {
                            id: EventId::new(),
                            session_id,
                            task_id: None,
                            created_at: chrono::Utc::now(),
                            kind: CanonicalEventKind::AgentAvailabilityChanged {
                                agent_id: profile.id,
                                name: profile.name.clone(),
                                enabled: profile.enabled,
                            },
                        };
                        let result = async {
                            repository
                                .save_agent(profile.clone())
                                .await
                                .map_err(|error| error.to_string())?;
                            repository
                                .append_event(audit.clone())
                                .await
                                .map_err(|error| error.to_string())?;
                            Ok(SavedTeamAdjustment::Availability(profile, audit))
                        }
                        .await;
                        let _ = team_adjustment_tx.send(result).await;
                    });
                } else if let TeamAdjustmentEvent::Add(profile) = team_adjustment {
                    let profile = *profile;
                    let engine = Arc::clone(&engine);
                    let team_adjustment_tx = team_adjustment_tx.clone();
                    tokio::spawn(async move {
                        let result = engine
                            .add_team_member(profile.clone())
                            .await
                            .map(|lead| SavedTeamAdjustment::Added(profile, lead))
                            .map_err(|error| error.to_string());
                        let _ = team_adjustment_tx.send(result).await;
                    });
                } else if matches!(team_adjustment, TeamAdjustmentEvent::Ignored) {
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
                        ComposerEvent::Submit(message) if message == "/task" => {
                            model.composer_notice =
                                Some("Use /task <objective> to assign work to the Team Lead.".into());
                        }
                        ComposerEvent::Submit(message) if message.starts_with("/task ") => {
                            let objective = message[6..].trim().to_owned();
                            if objective.is_empty() {
                                model.composer_notice = Some(
                                    "Use /task <objective> to assign work to the Team Lead.".into(),
                                );
                            } else {
                                model.composer_notice = Some("Task sent to Team Lead.".into());
                                model.push_ui_event(format!(
                                    "you → Team Lead task: {}",
                                    sanitize(&objective)
                                ));
                                let engine = Arc::clone(&engine);
                                let lead_submission_tx = lead_submission_tx.clone();
                                tokio::spawn(async move {
                                    let result = async {
                                        let task = engine
                                            .create_task(
                                                owner,
                                                objective,
                                                vec![
                                                    "Team Lead completes or delegates the requested work"
                                                        .into(),
                                                ],
                                            )
                                            .await?;
                                        engine.mark_ready(task.id).await?;
                                        engine.run_task(task.id).await
                                    }
                                    .await
                                    .map(|()| "Task accepted by Team Lead.")
                                    .map_err(|error| error.to_string());
                                    let _ = lead_submission_tx.send(result).await;
                                });
                            }
                        }
                        ComposerEvent::Submit(message) if message.starts_with('/') => {
                            model.composer_notice = Some(format!(
                                "Unknown command: {message}. Try /task <objective> or /tasks."
                            ));
                        }
                        ComposerEvent::Submit(message) => {
                            model.composer_notice = Some("Sent to Team Lead.".into());
                            model.push_ui_event(format!("you → Team Lead: {}", sanitize(&message)));
                            let engine = Arc::clone(&engine);
                            let lead_submission_tx = lead_submission_tx.clone();
                            tokio::spawn(async move {
                                let result = async {
                                    let task = engine
                                        .create_task(
                                            owner,
                                            message,
                                            vec!["Team Lead responds with a useful result".into()],
                                        )
                                        .await?;
                                    engine.mark_ready(task.id).await?;
                                    engine.run_conversation(task.id).await
                                }
                                .await
                                .map(|()| "Team Lead replied.")
                                .map_err(|error| error.to_string());
                                let _ = lead_submission_tx.send(result).await;
                            });
                        }
                        ComposerEvent::Consumed => {}
                        ComposerEvent::Ignored => {
                            let action = action_for_event(event);
                            match action {
                                Action::StartMock => {
                                    let engine = Arc::clone(&engine);
                                    let objective = objective.clone();
                                    let plan = delegation_plan_for_available_agents(
                                        &model.agents,
                                        model.team_lead,
                                        &delegation_plan,
                                    );
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
            Some(result)=team_adjustment_rx.recv()=>{
                model.team_adjustment_saving = false;
                match result {
                    Ok(SavedTeamAdjustment::Availability(profile, audit)) => {
                        if let Some(agent) = model.agents.iter_mut().find(|agent| agent.id == profile.id) {
                            *agent = profile.clone();
                        }
                        model.team_adjustment_notice = Some(format!(
                            "{} is now {} for future Team Lead assignments.",
                            profile.name,
                            if profile.enabled { "enabled" } else { "disabled" }
                        ));
                        model.push_event(&audit.kind);
                    }
                    Ok(SavedTeamAdjustment::Added(profile, lead)) => {
                        let provider_kind = model
                            .team_lead
                            .and_then(|id| model.agent_provider_kinds.get(&id))
                            .cloned();
                        if let Some(configured_lead) = model
                            .agents
                            .iter_mut()
                            .find(|agent| agent.id == lead.id)
                        {
                            *configured_lead = lead;
                        }
                        if let Some(provider_kind) = provider_kind {
                            model.agent_provider_kinds.insert(profile.id, provider_kind);
                        }
                        model.agents.push(profile.clone());
                        model.team_adjustment_selected = model.agents.len() - 1;
                        model.selected_agent = model.team_adjustment_selected;
                        model.team_role_picker = false;
                        model.team_adjustment_notice = Some(format!(
                            "{} added and ready for Team Lead assignments.",
                            profile.name
                        ));
                    }
                    Err(error) => model.team_adjustment_notice = Some(format!(
                        "Could not save the team adjustment: {}",
                        sanitize(&error)
                    )),
                }
            }
            Some(result)=lead_submission_rx.recv()=>{
                match result {
                    Ok(notice) => model.composer_notice = Some(notice.into()),
                    Err(error) => {
                        let error = sanitize(&error);
                        model.composer_notice = Some(format!(
                            "Team Lead request failed: {error}"
                        ));
                        model.push_ui_event(format!("Team Lead request failed: {error}"));
                    }
                }
            }
            Some(message)=event_rx.recv()=>model.apply_event(message),
            _=tick.tick()=>{
                model.team_flow.tick(Duration::from_millis(100));
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
    let view = match app.panel {
        Panel::Agents => "Team",
        Panel::Tasks => "Tasks",
        Panel::Details => "Details",
        Panel::Events => "Events",
    };
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
                    "workspace · {}/{} developers enabled · {} view · session {}",
                    app.agents.iter().filter(|agent| agent.enabled).count(),
                    app.agents.len(),
                    view,
                    &session[..session.len().min(8)]
                )),
            ]),
            Line::from(Span::styled(
                format!(
                    " [1 Team] [2 Tasks] [3 Details] [4 Events] · {} · {} · circuit {:?} · {} retries · {cost}{credential}",
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

    if app.panel == Panel::Agents {
        render_team(frame, app, rows[1]);
    } else {
        render_activity(frame, app, rows[1]);
    }

    let workspace = sanitize(&app.workspace_root.display().to_string());
    let composer_title = if app.composer_active {
        format!(" Team Lead · cwd {workspace} · Enter send · Shift+Enter newline · Esc keep draft ")
    } else if let Some(notice) = &app.composer_notice {
        format!(" {notice} · cwd {workspace} · /task assign · /tasks list ")
    } else {
        format!(" Team Lead · cwd {workspace} · Enter message · /task assign · /tasks list ")
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
    match app.panel {
        Panel::Tasks => {
            let tasks = app
                .tasks
                .iter()
                .enumerate()
                .map(|(i, task)| {
                    ListItem::new(format!(
                        "{} {}{} [{:?}]",
                        if i == app.selected { "▶" } else { " " },
                        "  ".repeat(usize::from(task.depth)),
                        task.objective,
                        task.status
                    ))
                })
                .collect::<Vec<_>>();
            frame.render_widget(List::new(tasks).block(panel("2 · Tasks", true)), area);
        }
        Panel::Details => {
            let details = app.selected_task().map_or_else(
                || "No task selected. Press 2 to choose a task or r to start work.".into(),
                |task| {
                    let artifact_text = app
                        .artifacts
                        .iter()
                        .filter(|artifact| artifact.producer == task.id)
                        .map(|artifact| {
                            format!(
                                "\n\nArtifact: {} ({})\n{}",
                                artifact.location,
                                artifact.kind,
                                sanitize(&artifact.preview)
                            )
                        })
                        .collect::<String>();
                    format!(
                        "Owner: {}\nStatus: {:?}\nObjective: {}\nCriteria:\n{}{}",
                        task.owner,
                        task.status,
                        task.objective,
                        task.acceptance_criteria.join("\n"),
                        artifact_text
                    )
                },
            );
            frame.render_widget(
                Paragraph::new(details)
                    .wrap(Wrap { trim: true })
                    .block(panel("3 · Task Details", true)),
                area,
            );
        }
        Panel::Events => {
            let logs = app
                .events
                .iter()
                .enumerate()
                .rev()
                .map(|(index, event)| format!("[{}] {event}", index + 1))
                .collect::<Vec<_>>()
                .join("\n\n");
            let event_title = if app.event_follow {
                "4 · Events · live"
            } else {
                "4 · Events · paused (↑ to newest)"
            };
            frame.render_widget(
                Paragraph::new(logs)
                    .wrap(Wrap { trim: false })
                    .scroll((app.event_scroll, 0))
                    .block(panel(event_title, true)),
                area,
            );
        }
        Panel::Agents => {}
    }
}

fn render_team(frame: &mut ratatui::Frame<'_>, app: &mut AppModel, area: Rect) {
    let handoff = handoff_summary(&app.tasks, &app.approvals);
    let title = if app.panel == Panel::Agents {
        format!(
            " specialist → Lead → specialist · {handoff} · · available / ▶ live · S adjust · +/- zoom · f fit "
        )
    } else {
        format!(" specialist → Lead → specialist · {handoff} · · available / ▶ live · S adjust ")
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
    app.team_flow.sync(
        &app.agents,
        &app.tasks,
        &app.approvals,
        app.team_lead,
        app.selected_agent,
        inner,
    );
    app.team_flow.render(frame, inner);
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
        frame.render_widget(Paragraph::new("voff Help\n\nEnter/t  Message the Team Lead directly\n/task <objective>  Give executable work to the Team Lead\n/tasks  Focus the current task list\nclick composer  Focus the Team Lead input\nShift+Enter  New line while composing\nS  Add or adjust specialists\nmouse drag  Move a card / pan empty graph space\nclick card  Select a developer\n+/-/0/f  Zoom in / zoom out / reset / fit team\nr  Start configured objective\na  Add a missing provider token locally\nc  Cancel selected active task\ne  Open full-screen events\n↑/↓, PgUp/PgDn  Navigate or scroll\nTab  Change panel\nq  Safe exit\nEsc  Close / keep draft\n?  Close help").wrap(Wrap { trim: false }).block(Block::default().title("Help").borders(Borders::ALL)),popup);
    }
    if let Some(approval) = app.pending_approval() {
        render_approval_popup(frame, app, approval);
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
    if app.team_adjustment_open {
        render_team_adjustment_popup(frame, app);
    }
}

fn render_team_adjustment_popup(frame: &mut ratatui::Frame<'_>, app: &AppModel) {
    let mut lines = vec![
        Line::from(Span::styled(
            "Every assignment and delegation decision routes through the Team Lead.",
            Style::default().fg(Color::LightMagenta),
        )),
        Line::from("Specialists never dispatch work to one another."),
        Line::from(""),
    ];
    if app.team_role_picker {
        lines.push(Line::from(Span::styled(
            "Choose a specialist role. Provider, model, and token limit inherit from the Team Lead.",
            Style::default().fg(Color::Gray),
        )));
        lines.push(Line::from(""));
        for (index, role) in ADDABLE_ROLES.iter().enumerate() {
            let selected = index == app.team_role_selected;
            lines.push(Line::from(Span::styled(
                format!(
                    "{} {} {}",
                    if selected { "›" } else { " " },
                    role.icon(),
                    role.label()
                ),
                if selected {
                    Style::default()
                        .fg(Color::LightCyan)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default()
                },
            )));
        }
        lines.push(Line::from(""));
        lines.push(Line::from(
            "↑/↓ choose · Enter/Space add · Esc return to team",
        ));
    } else {
        for (index, agent) in app.agents.iter().enumerate() {
            let selected = index == app.team_adjustment_selected;
            let is_lead = Some(agent.id) == app.team_lead;
            let marker = if selected { "›" } else { " " };
            let state = if is_lead {
                "required"
            } else if agent.enabled {
                "enabled"
            } else {
                "disabled"
            };
            let style = if selected {
                Style::default()
                    .fg(Color::LightCyan)
                    .add_modifier(Modifier::BOLD)
            } else if agent.enabled {
                Style::default()
            } else {
                Style::default().fg(Color::DarkGray)
            };
            lines.push(Line::from(Span::styled(
                format!(
                    "{marker} [{}] {} · {}",
                    if agent.enabled { "on " } else { "off" },
                    sanitize(&agent.name),
                    state
                ),
                style,
            )));
        }
        lines.push(Line::from(""));
        if let Some(notice) = &app.team_adjustment_notice {
            lines.push(Line::from(Span::styled(
                sanitize(notice),
                Style::default().fg(if app.team_adjustment_saving {
                    Color::Yellow
                } else {
                    Color::LightCyan
                }),
            )));
            lines.push(Line::from(""));
        }
        lines.push(Line::from(
            "n add specialist · ↑/↓ select · Enter/Space toggle · S/Esc close",
        ));
    }
    let area = frame.area();
    let item_count = if app.team_role_picker {
        ADDABLE_ROLES.len()
    } else {
        app.agents.len()
    };
    let popup = Popup::new(KnownSizeWrapper {
        inner: Paragraph::new(lines).wrap(Wrap { trim: true }),
        width: usize::from(area.width.saturating_sub(6).clamp(1, 76)),
        height: usize::from(
            area.height
                .saturating_sub(6)
                .min(u16::try_from(item_count.saturating_add(11)).unwrap_or(u16::MAX))
                .max(1),
        ),
    })
    .title(if app.team_role_picker {
        " Add specialist · Team Lead remains coordinator "
    } else {
        " Adjust team · Team Lead remains required "
    })
    .style(Style::default().bg(Color::Black))
    .border_set(ratatui::symbols::border::ROUNDED)
    .border_style(Style::default().fg(Color::LightMagenta));
    frame.render_widget(popup, area);
}

fn render_approval_popup(frame: &mut ratatui::Frame<'_>, app: &AppModel, approval: &Approval) {
    let preview = app
        .artifacts
        .iter()
        .find(|artifact| artifact.producer == approval.task_id)
        .map(|artifact| format!("\n\nDiff preview:\n{}", sanitize(&artifact.preview)))
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
    let pending_count = app.pending_approval_count();
    let selected = app.approval_selected.min(pending_count.saturating_sub(1));
    let navigation = if pending_count > 1 {
        "[↑/↓] Select request    "
    } else {
        ""
    };
    let lead_name = app
        .team_lead
        .and_then(|id| app.agents.iter().find(|agent| agent.id == id))
        .map_or("Team Lead", |agent| agent.name.as_str());
    let task_owner = app
        .tasks
        .iter()
        .find(|task| task.id == approval.task_id)
        .and_then(|task| app.agents.iter().find(|agent| agent.id == task.owner))
        .map_or(lead_name, |agent| agent.name.as_str());
    let route = if task_owner == lead_name {
        sanitize(lead_name)
    } else {
        format!(
            "{} → {} → {}",
            sanitize(lead_name),
            sanitize(task_owner),
            sanitize(lead_name)
        )
    };
    let body = Paragraph::new(format!(
        "User decision required · Team route: {route}\nAction: {}\nTask: {}\nWorking directory: {}\n{}{}\n\n{navigation}[y] Approve once    [n] Deny",
        sanitize(&approval.action),
        approval.task_id,
        sanitize(&approval.working_directory),
        details,
        preview
    ))
    .wrap(Wrap { trim: true });
    let area = frame.area();
    let body_width = usize::from(area.width.saturating_sub(6).clamp(1, 76));
    let body_height = usize::from(area.height.saturating_sub(6).clamp(1, 22));
    let popup = Popup::new(KnownSizeWrapper {
        inner: body,
        width: body_width,
        height: body_height,
    })
    .title(format!(
        " Approval {} of {pending_count} ",
        selected.saturating_add(1)
    ))
    .style(Style::default().bg(Color::Black))
    .border_set(ratatui::symbols::border::ROUNDED)
    .border_style(Style::default().fg(Color::Yellow));
    frame.render_widget(popup, area);
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
        assert_eq!(a.panel, Panel::Tasks);
        a.reduce(Action::NextPanel);
        assert_eq!(a.panel, Panel::Details);
        a.reduce(Action::NextPanel);
        assert_eq!(a.panel, Panel::Events);
        a.reduce(Action::NextPanel);
        assert_eq!(a.panel, Panel::Agents);
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
        assert_eq!(action_for_key(KeyCode::Char('n')), Action::Deny);
        assert_eq!(action_for_key(KeyCode::Char('f')), Action::FitTeam);
        assert_eq!(action_for_key(KeyCode::Char('+')), Action::ZoomTeamIn);
        assert_eq!(action_for_key(KeyCode::Char('-')), Action::ZoomTeamOut);
        assert_eq!(action_for_key(KeyCode::Char('0')), Action::ResetTeamZoom);
        assert_eq!(action_for_key(KeyCode::Char('S')), Action::AdjustTeam);
        assert_eq!(action_for_key(KeyCode::Char('1')), Action::ShowTeam);
        assert_eq!(action_for_key(KeyCode::Char('2')), Action::ShowTasks);
        assert_eq!(action_for_key(KeyCode::Char('3')), Action::ShowDetails);
        assert_eq!(action_for_key(KeyCode::Char('4')), Action::ShowEvents);
    }

    #[test]
    fn team_adjustment_keeps_the_lead_and_toggles_an_idle_specialist() {
        let mut app = AppModel::new(SessionId::new());
        let lead = agent("Team Lead", "technical_lead", vec!["Backend".into()]);
        let specialist = agent("Backend", "backend_developer", vec![]);
        app.team_lead = Some(lead.id);
        app.agents = vec![lead, specialist.clone()];
        app.reduce(Action::AdjustTeam);

        let enter = Event::Key(crossterm::event::KeyEvent::new(
            KeyCode::Enter,
            KeyModifiers::NONE,
        ));
        assert!(matches!(
            app.handle_team_adjustment_event(&enter),
            TeamAdjustmentEvent::Consumed
        ));
        assert!(
            app.team_adjustment_notice
                .as_deref()
                .is_some_and(|notice| notice.contains("required"))
        );

        let down = Event::Key(crossterm::event::KeyEvent::new(
            KeyCode::Down,
            KeyModifiers::NONE,
        ));
        app.handle_team_adjustment_event(&down);
        let TeamAdjustmentEvent::Save(updated) = app.handle_team_adjustment_event(&enter) else {
            panic!("idle specialist should produce a persisted adjustment");
        };
        assert_eq!(updated.id, specialist.id);
        assert!(!updated.enabled);
        assert!(app.team_adjustment_saving);
    }

    #[test]
    fn team_adjustment_adds_a_unique_non_delegating_backend_draft() {
        let mut app = AppModel::new(SessionId::new());
        let lead = agent(
            "Team Lead",
            "technical_lead",
            vec!["Backend Developer".into()],
        );
        app.team_lead = Some(lead.id);
        app.agents = vec![
            lead.clone(),
            agent("Backend Developer", "backend_developer", vec![]),
        ];
        app.reduce(Action::AdjustTeam);

        let add = Event::Key(crossterm::event::KeyEvent::new(
            KeyCode::Char('n'),
            KeyModifiers::NONE,
        ));
        assert!(matches!(
            app.handle_team_adjustment_event(&add),
            TeamAdjustmentEvent::Consumed
        ));
        assert!(app.team_role_picker);
        let enter = Event::Key(crossterm::event::KeyEvent::new(
            KeyCode::Enter,
            KeyModifiers::NONE,
        ));
        let TeamAdjustmentEvent::Add(member) = app.handle_team_adjustment_event(&enter) else {
            panic!("the selected backend role should create a specialist draft");
        };

        assert_eq!(member.name, "Backend Developer 2");
        assert_eq!(member.role, "backend_developer");
        assert_eq!(member.provider, lead.provider);
        assert_eq!(member.model, lead.model);
        assert!(member.allowed_targets.is_empty());
        assert!(!member.tools.contains(&"task.delegate".into()));
        assert!(!member.tools.contains(&"review.request".into()));
    }

    #[test]
    fn configured_run_assigns_every_enabled_allowed_specialist_once() {
        let lead = agent(
            "Team Lead",
            "technical_lead",
            vec!["Backend".into(), "Reviewer".into(), "Disabled".into()],
        );
        let backend = agent("Backend", "backend_developer", vec![]);
        let reviewer = agent("Reviewer", "code_reviewer", vec![]);
        let mut disabled = agent("Disabled", "test_engineer", vec![]);
        disabled.enabled = false;
        let configured = vec![DelegationProposal {
            target_agent: backend.name.clone(),
            objective: "Own server.py".into(),
            acceptance_criteria: vec!["API complete".into()],
        }];

        let plan = delegation_plan_for_available_agents(
            &[lead.clone(), backend, reviewer, disabled],
            Some(lead.id),
            &configured,
        );

        assert_eq!(plan.len(), 2);
        assert_eq!(
            plan.iter()
                .map(|proposal| proposal.target_agent.as_str())
                .collect::<HashSet<_>>(),
            HashSet::from(["Backend", "Reviewer"])
        );
        assert_eq!(plan[0].objective, "Own server.py");
    }

    #[test]
    fn generated_support_artifacts_do_not_require_missing_directories() {
        let specialist = agent("Full-stack Developer 6", "full_stack_developer", vec![]);

        let proposal = supporting_delegation(&specialist, 1);

        assert!(
            proposal
                .objective
                .contains("full-stack-developer-6-integration-notes.md")
        );
        assert!(!proposal.objective.contains("docs/"));
    }

    #[test]
    fn team_adjustment_refuses_to_disable_a_specialist_with_active_work() {
        let mut app = AppModel::new(SessionId::new());
        let lead = agent("Team Lead", "technical_lead", vec!["Backend".into()]);
        let specialist = agent("Backend", "backend_developer", vec![]);
        app.team_lead = Some(lead.id);
        app.agents = vec![lead, specialist.clone()];
        let mut task = Task::root(
            specialist.id,
            "bounded backend work",
            vec![],
            voff_core::TaskLimits::default(),
        );
        task.transition(TaskStatus::Ready).unwrap();
        app.tasks.push(task);
        app.reduce(Action::AdjustTeam);
        app.team_adjustment_selected = 1;

        let enter = Event::Key(crossterm::event::KeyEvent::new(
            KeyCode::Enter,
            KeyModifiers::NONE,
        ));
        assert!(matches!(
            app.handle_team_adjustment_event(&enter),
            TeamAdjustmentEvent::Consumed
        ));
        assert!(
            app.team_adjustment_notice
                .as_deref()
                .is_some_and(|notice| notice.contains("active work"))
        );
        assert!(!app.team_adjustment_saving);
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
        let lead = agent("Ada", "technical_lead", vec!["Linus".into()]);
        app.team_lead = Some(lead.id);
        app.agents = vec![lead, agent("Linus", "backend_developer", vec![])];
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
        assert_eq!(app.team_flow.edges().len(), 1);
        assert!(
            app.team_flow
                .edges()
                .iter()
                .all(|edge| edge.label.is_none())
        );
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
        let backend_id = backend_agent.id;
        let frontend_id = frontend_agent.id;
        app.team_lead = Some(lead.id);
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
        assert!(app.agents.iter().any(|agent| agent.name == "Frontend"));
        assert!(rendered.contains("Approval 1 of 2"), "{rendered}");
        assert_eq!(app.team_flow.edges().len(), 2);
        let lead_id = app.team_lead.unwrap().to_string();
        let backend_id = backend_id.to_string();
        let frontend_id = frontend_id.to_string();
        for specialist_id in [backend_id.as_str(), frontend_id.as_str()] {
            assert!(
                app.team_flow.edges().iter().any(|edge| {
                    edge.source == lead_id && edge.target.as_str() == specialist_id
                })
            );
        }
        assert!(app.team_flow.edges().iter().all(|edge| {
            !((edge.source == backend_id && edge.target == frontend_id)
                || (edge.source == frontend_id && edge.target == backend_id))
        }));
        assert!(
            app.team_flow
                .edges()
                .iter()
                .all(|edge| edge.label.is_none())
        );
    }

    #[test]
    fn approval_popup_navigates_one_pending_request_at_a_time() {
        let mut app = AppModel::new(SessionId::new());
        let owner = AgentId::new();
        let task = Task::root(
            owner,
            "Coordinate implementation",
            vec!["Work is delegated".into()],
            voff_core::TaskLimits::default(),
        );
        let proposals = [
            DelegationProposal {
                target_agent: "Backend".into(),
                objective: "Create calculator API".into(),
                acceptance_criteria: vec!["API responds".into()],
            },
            DelegationProposal {
                target_agent: "Frontend".into(),
                objective: "Create calculator interface".into(),
                acceptance_criteria: vec!["UI calls API".into()],
            },
        ];
        app.approvals = proposals
            .iter()
            .map(|proposal| Approval {
                id: voff_core::ApprovalId::new(),
                task_id: task.id,
                tool_call_id: voff_core::ToolCallId::new(),
                action: "task.delegate".into(),
                working_directory: ".".into(),
                reason: serde_json::to_string(proposal).unwrap(),
                decision: ApprovalDecision::Pending,
                created_at: chrono::Utc::now(),
            })
            .collect();

        let backend = TestBackend::new(100, 30);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| render_overlays(frame, &app)).unwrap();
        let first = terminal.backend().buffer();
        let first = (0..first.area.height)
            .flat_map(|y| (0..first.area.width).map(move |x| first[(x, y)].symbol()))
            .collect::<String>();
        assert!(first.contains("Approval 1 of 2"), "{first}");
        assert!(first.contains("Target: Backend"), "{first}");
        assert!(!first.contains("Target: Frontend"), "{first}");

        app.reduce(Action::Down);
        terminal.draw(|frame| render_overlays(frame, &app)).unwrap();
        let second = terminal.backend().buffer();
        let second = (0..second.area.height)
            .flat_map(|y| (0..second.area.width).map(move |x| second[(x, y)].symbol()))
            .collect::<String>();
        assert!(second.contains("Approval 2 of 2"), "{second}");
        assert!(second.contains("Target: Frontend"), "{second}");
        assert!(!second.contains("Target: Backend"), "{second}");
        assert_eq!(app.selected, 0);
    }

    #[test]
    fn live_delegation_animates_the_edge_and_spins_on_working_roles() {
        let mut app = AppModel::new(SessionId::new());
        let lead = agent("Ada", "technical_lead", vec!["Linus".into()]);
        let implementer = agent("Linus", "backend_developer", vec![]);
        app.team_lead = Some(lead.id);
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
        app.team_flow.tick(Duration::from_millis(300));
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
        assert!(
            ["◒", "◐", "◓", "◑"]
                .iter()
                .any(|frame| rendered.contains(*frame))
        );
        assert_eq!(
            app.team_flow
                .edges()
                .iter()
                .filter(|edge| edge.animated)
                .count(),
            1
        );
        assert!(
            app.team_flow
                .edges()
                .iter()
                .all(|edge| edge.label.is_none())
        );
    }

    #[test]
    fn completed_handoff_stays_visible_in_a_narrow_terminal() {
        let mut app = AppModel::new(SessionId::new());
        let lead = agent("Ada", "technical_lead", vec!["Linus".into()]);
        let implementer = agent("Linus", "backend_developer", vec![]);
        app.team_lead = Some(lead.id);
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
        assert!(rendered.contains("⬢ Linus"), "{rendered}");
        assert!(
            app.team_flow
                .edges()
                .iter()
                .all(|edge| edge.label.is_none())
        );
        assert!(rendered.matches("complete").count() >= 3);
    }

    #[test]
    fn mouse_drag_moves_and_selects_a_developer_card() {
        let mut app = AppModel::new(SessionId::new());
        app.agents = vec![agent("Ada", "technical_lead", vec![])];
        let backend = TestBackend::new(120, 36);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| render(frame, &mut app)).unwrap();
        let before = app.team_flow.node_position(0).unwrap();
        let (left, top, right, bottom) = app.team_flow.node_terminal_rect(0).unwrap();
        let x = u16::try_from((left + right) / 2).unwrap();
        let y = u16::try_from((top + bottom) / 2).unwrap();

        app.reduce(Action::Mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            x,
            y,
        )));
        app.reduce(Action::Mouse(mouse(
            MouseEventKind::Drag(MouseButton::Left),
            x + 5,
            y + 3,
        )));
        app.reduce(Action::Mouse(mouse(
            MouseEventKind::Up(MouseButton::Left),
            x + 5,
            y + 3,
        )));

        assert_eq!(app.panel, Panel::Agents);
        assert_eq!(app.selected_agent, 0);
        let after = app.team_flow.node_position(0).unwrap();
        assert!(after.x > before.x);
        assert!(after.y > before.y);
    }

    #[test]
    fn mouse_selecting_a_role_highlights_its_lead_connections_in_yellow() {
        let mut app = AppModel::new(SessionId::new());
        let lead = agent("Ada", "technical_lead", vec!["Linus".into()]);
        let specialist = agent("Linus", "backend_developer", vec![]);
        let specialist_id = specialist.id;
        app.team_lead = Some(lead.id);
        app.agents = vec![lead, specialist];
        let backend = TestBackend::new(120, 36);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| render(frame, &mut app)).unwrap();
        let (left, top, right, bottom) = app.team_flow.node_terminal_rect(1).unwrap();
        let x = u16::try_from((left + right) / 2).unwrap();
        let y = u16::try_from((top + bottom) / 2).unwrap();

        app.reduce(Action::Mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            x,
            y,
        )));
        app.reduce(Action::Mouse(mouse(
            MouseEventKind::Up(MouseButton::Left),
            x,
            y,
        )));
        terminal.draw(|frame| render(frame, &mut app)).unwrap();

        assert_eq!(app.team_flow.mouse_highlight(), Some(specialist_id));
        let buffer = terminal.backend().buffer();
        let yellow_route_cells = (0..buffer.area.height)
            .flat_map(|row| (0..buffer.area.width).map(move |column| &buffer[(column, row)]))
            .filter(|cell| {
                cell.fg == Color::Yellow && matches!(cell.symbol(), "·" | "◆" | "▲" | "△" | "●")
            })
            .count();
        assert!(yellow_route_cells > 0);
    }
}
