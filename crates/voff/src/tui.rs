//! Responsive Ratatui shell and pure action reducer.
mod team_flow;

use crate::{model_catalog::ModelCatalog, onboarding::DeveloperRole};
use crossterm::{
    event::{
        self, DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
        Event, KeyCode, KeyEventKind, KeyModifiers, KeyboardEnhancementFlags, MouseButton,
        MouseEvent, MouseEventKind, PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
    },
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{
    Terminal,
    backend::CrosstermBackend,
    layout::{Alignment, Constraint, Direction, Layout, Margin, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Clear, List, ListItem, Paragraph, Wrap},
};
use ratatui_textarea::{DataCursor, TextArea, WrapMode};
use std::{
    collections::{HashMap, HashSet},
    io, panic,
    path::PathBuf,
    sync::Arc,
    time::Duration,
};
use thiserror::Error;
use tokio::sync::mpsc;
use voff_core::{
    AgentId, AgentProfile, Approval, ApprovalDecision, Artifact, CanonicalEvent,
    CanonicalEventKind, EventId, SessionId, Task, TaskStatus,
};
use voff_engine::{DelegationProposal, Engine, EngineEvent};
use voff_provider::ProviderHealth;
use voff_storage::{Repository, SqliteRepository};

use self::team_flow::{TeamFlow, TeamSnapshot, handoff_summary, role_color, role_icon};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Panel {
    Tasks,
    Agents,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TaskInspector {
    Details,
    RoleActivity,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Action {
    Up,
    Down,
    NextPanel,
    ToggleHelp,
    FocusComposer,
    QuickRun,
    RetryTask,
    AddCredential,
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
    ConfigureModel,
    ShowTeam,
    ShowTasks,
    ShowTaskDetails,
    ShowRoleActivity,
    Escape,
    Quit,
    Mouse(MouseEvent),
    None,
}

#[derive(Clone, Debug)]
struct ScopedEvent {
    task_id: Option<voff_core::TaskId>,
    text: String,
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

#[derive(Debug)]
enum ModelConfigEvent {
    Ignored,
    Consumed,
    Save(Box<AgentProfile>),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RoleConfigField {
    Provider,
    Model,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ProviderRouteOption {
    pub route: String,
    pub kind: String,
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
    event_task_ids: Vec<Option<voff_core::TaskId>>,
    pub selected: usize,
    pub panel: Panel,
    pub help: bool,
    pub quit: bool,
    pub provider_label: String,
    pub agent_name: String,
    pub model_label: String,
    pub agents: Vec<AgentProfile>,
    known_agents: HashMap<AgentId, AgentProfile>,
    pub approvals: Vec<Approval>,
    pub artifacts: Vec<Artifact>,
    pub provider_health: ProviderHealth,
    pub input_cost_per_million: Option<f64>,
    pub output_cost_per_million: Option<f64>,
    pub cost_currency: String,
    pub estimated_cost: Option<f64>,
    pub model_catalog: ModelCatalog,
    pub agent_provider_kinds: HashMap<AgentId, String>,
    pub provider_routes: Vec<ProviderRouteOption>,
    pub cost_partial: bool,
    pub workspace_root: PathBuf,
    pub event_viewer: bool,
    pub event_scroll: u16,
    event_follow: bool,
    scoped_events: Vec<ScopedEvent>,
    task_inspector: TaskInspector,
    task_inspector_scroll: u16,
    task_list_hitbox: Rect,
    task_window_start: usize,
    credential_target: Option<(String, crate::credential::CredentialSlot)>,
    credential_input: Option<String>,
    credential_saving: bool,
    credential_error: Option<String>,
    composer: TextArea<'static>,
    composer_active: bool,
    composer_notice: Option<String>,
    composer_hitbox: Rect,
    quick_run_starting: bool,
    retry_starting: bool,
    selected_agent: usize,
    approval_selected: usize,
    team_lead: Option<AgentId>,
    team_adjustment_open: bool,
    team_adjustment_selected: usize,
    team_role_picker: bool,
    team_role_selected: usize,
    team_adjustment_saving: bool,
    team_adjustment_notice: Option<String>,
    model_config_open: bool,
    model_config_field: RoleConfigField,
    model_config_provider: usize,
    model_config_selected: Option<usize>,
    model_config_input: String,
    model_config_typing: bool,
    model_config_saving: bool,
    model_config_notice: Option<String>,
    team_flow: TeamFlow,
    streaming_text: bool,
    agent_responses: HashMap<AgentId, String>,
}
impl AppModel {
    #[must_use]
    pub fn new(session: SessionId) -> Self {
        let mut composer = TextArea::default();
        composer.set_cursor_line_style(Style::default());
        composer.set_cursor_style(
            Style::default()
                .fg(Color::Black)
                .bg(Color::LightCyan)
                .add_modifier(Modifier::BOLD),
        );
        composer.set_selection_style(Style::default().fg(Color::Black).bg(Color::LightBlue));
        composer.set_wrap_mode(WrapMode::WordOrGlyph);
        composer.set_tab_length(2);
        composer.set_max_histories(200);
        composer.set_placeholder_text("Press Enter to assign a task");
        Self {
            session,
            tasks: vec![],
            events: vec![],
            event_task_ids: vec![],
            selected: 0,
            panel: Panel::Agents,
            help: false,
            quit: false,
            provider_label: "mock".into(),
            agent_name: "Mock Agent".into(),
            model_label: "mock-v1".into(),
            agents: vec![],
            known_agents: HashMap::new(),
            approvals: vec![],
            artifacts: vec![],
            provider_health: ProviderHealth::default(),
            input_cost_per_million: None,
            output_cost_per_million: None,
            cost_currency: "USD".into(),
            estimated_cost: None,
            model_catalog: ModelCatalog::default(),
            agent_provider_kinds: HashMap::new(),
            provider_routes: vec![ProviderRouteOption {
                route: "mock".into(),
                kind: "mock".into(),
            }],
            cost_partial: false,
            workspace_root: ".".into(),
            event_viewer: false,
            event_scroll: 0,
            event_follow: true,
            scoped_events: vec![],
            task_inspector: TaskInspector::Details,
            task_inspector_scroll: 0,
            task_list_hitbox: Rect::default(),
            task_window_start: 0,
            credential_target: None,
            credential_input: None,
            credential_saving: false,
            credential_error: None,
            composer,
            composer_active: false,
            composer_notice: None,
            composer_hitbox: Rect::default(),
            quick_run_starting: false,
            retry_starting: false,
            selected_agent: 0,
            approval_selected: 0,
            team_lead: None,
            team_adjustment_open: false,
            team_adjustment_selected: 0,
            team_role_picker: false,
            team_role_selected: 0,
            team_adjustment_saving: false,
            team_adjustment_notice: None,
            model_config_open: false,
            model_config_field: RoleConfigField::Provider,
            model_config_provider: 0,
            model_config_selected: None,
            model_config_input: String::new(),
            model_config_typing: false,
            model_config_saving: false,
            model_config_notice: None,
            team_flow: TeamFlow::default(),
            streaming_text: false,
            agent_responses: HashMap::new(),
        }
    }
    pub fn reduce(&mut self, action: Action) {
        if self.navigate_approvals(action) {
            return;
        }
        match action {
            Action::Up => {
                if self.event_viewer {
                    self.event_scroll = self.event_scroll.saturating_sub(1);
                    self.event_follow = self.event_scroll == 0;
                } else if self.panel == Panel::Agents {
                    self.selected_agent = self.selected_agent.saturating_sub(1);
                } else {
                    self.selected = self.selected.saturating_sub(1);
                    self.task_inspector_scroll = 0;
                }
            }
            Action::Down => {
                if self.event_viewer {
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
                    self.selected += 1;
                    self.task_inspector_scroll = 0;
                }
            }
            Action::PageUp => {
                if self.event_viewer {
                    self.event_scroll = self.event_scroll.saturating_sub(10);
                    self.event_follow = self.event_scroll == 0;
                } else if self.panel == Panel::Tasks {
                    self.task_inspector_scroll = self.task_inspector_scroll.saturating_sub(10);
                }
            }
            Action::PageDown => {
                if self.event_viewer {
                    self.event_scroll = self
                        .event_scroll
                        .saturating_add(10)
                        .min(self.event_scroll_limit());
                    self.event_follow = self.event_scroll == 0;
                } else if self.panel == Panel::Tasks {
                    self.task_inspector_scroll = self.task_inspector_scroll.saturating_add(10);
                }
            }
            Action::FitTeam if self.panel == Panel::Agents => self.team_flow.fit_view(),
            Action::ZoomTeamIn if self.panel == Panel::Agents => self.team_flow.zoom_in(),
            Action::ZoomTeamOut if self.panel == Panel::Agents => self.team_flow.zoom_out(),
            Action::ResetTeamZoom if self.panel == Panel::Agents => self.team_flow.reset_zoom(),
            Action::AdjustTeam => {
                self.team_adjustment_open = true;
                self.model_config_open = false;
                self.team_adjustment_selected =
                    self.selected_agent.min(self.agents.len().saturating_sub(1));
                self.team_role_picker = false;
                self.team_adjustment_notice = None;
            }
            Action::ConfigureModel if self.panel == Panel::Agents => {
                self.open_model_config();
            }
            Action::ShowTeam => self.panel = Panel::Agents,
            Action::ShowTasks => self.panel = Panel::Tasks,
            Action::ShowTaskDetails => {
                self.panel = Panel::Tasks;
                self.task_inspector = TaskInspector::Details;
                self.task_inspector_scroll = 0;
            }
            Action::ShowRoleActivity => {
                self.panel = Panel::Tasks;
                self.task_inspector = TaskInspector::RoleActivity;
                self.task_inspector_scroll = 0;
            }
            Action::NextPanel => {
                self.panel = match self.panel {
                    Panel::Agents => Panel::Tasks,
                    Panel::Tasks => Panel::Agents,
                };
            }
            Action::ToggleHelp => self.help = !self.help,
            Action::FocusComposer => {
                self.composer_active = true;
                self.composer_notice = None;
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
                if self.event_viewer {
                    self.follow_newest_event();
                }
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
            Action::QuickRun
            | Action::RetryTask
            | Action::Cancel
            | Action::Approve
            | Action::Deny
            | Action::FitTeam
            | Action::ZoomTeamIn
            | Action::ZoomTeamOut
            | Action::ResetTeamZoom
            | Action::ConfigureModel
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
        if self.panel == Panel::Tasks {
            match mouse.kind {
                MouseEventKind::Down(MouseButton::Left)
                    if self
                        .task_list_hitbox
                        .contains((mouse.column, mouse.row).into()) =>
                {
                    let row = usize::from(
                        mouse
                            .row
                            .saturating_sub(self.task_list_hitbox.y.saturating_add(1)),
                    );
                    let index = self.task_window_start.saturating_add(row / 2);
                    if index < self.tasks.len() {
                        self.selected = index;
                        self.task_inspector_scroll = 0;
                    }
                    return;
                }
                MouseEventKind::ScrollUp => {
                    self.selected = self.selected.saturating_sub(1);
                    self.task_inspector_scroll = 0;
                    return;
                }
                MouseEventKind::ScrollDown => {
                    if self.selected + 1 < self.tasks.len() {
                        self.selected += 1;
                    }
                    self.task_inspector_scroll = 0;
                    return;
                }
                _ => return,
            }
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
            KeyCode::Esc | KeyCode::Char('t') => {
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

    fn open_model_config(&mut self) {
        let Some(agent) = self.agents.get(self.selected_agent) else {
            self.composer_notice = Some("Select a role to configure.".into());
            return;
        };
        let route = agent.provider.clone();
        let kind = self
            .agent_provider_kinds
            .get(&agent.id)
            .cloned()
            .unwrap_or_else(|| route.clone());
        let model = agent.model.clone();
        if !self
            .provider_routes
            .iter()
            .any(|provider| provider.route == route)
        {
            self.provider_routes.push(ProviderRouteOption {
                route: route.clone(),
                kind: kind.clone(),
            });
        }
        self.model_config_provider = self
            .provider_routes
            .iter()
            .position(|provider| provider.route == route)
            .unwrap_or(0);
        self.model_config_field = RoleConfigField::Provider;
        self.model_config_selected = self
            .model_catalog
            .models(&kind)
            .iter()
            .position(|option| option.id == model);
        self.model_config_input = model;
        self.model_config_typing = false;
        self.model_config_saving = false;
        self.model_config_notice = None;
        self.team_adjustment_open = false;
        self.model_config_open = true;
    }

    fn handle_model_config_event(&mut self, event: &Event) -> ModelConfigEvent {
        if !self.model_config_open {
            return ModelConfigEvent::Ignored;
        }
        if self.model_config_saving {
            return ModelConfigEvent::Consumed;
        }
        let Event::Key(key) = event else {
            if self.model_config_field == RoleConfigField::Model
                && let Event::Paste(value) = event
            {
                if !self.model_config_typing {
                    self.model_config_input.clear();
                    self.model_config_typing = true;
                }
                self.model_config_input
                    .extend(value.chars().filter(|value| !value.is_whitespace()));
                self.model_config_selected = None;
                self.model_config_notice = None;
            }
            return ModelConfigEvent::Consumed;
        };
        if key.kind != KeyEventKind::Press {
            return ModelConfigEvent::Consumed;
        }
        match key.code {
            KeyCode::Esc => {
                self.model_config_open = false;
                self.model_config_notice = None;
            }
            KeyCode::Up | KeyCode::BackTab => {
                self.model_config_field = RoleConfigField::Provider;
                self.model_config_notice = None;
            }
            KeyCode::Down | KeyCode::Tab => {
                self.model_config_field = RoleConfigField::Model;
                self.model_config_notice = None;
            }
            KeyCode::Left => self.cycle_role_config_choice(-1),
            KeyCode::Right => self.cycle_role_config_choice(1),
            KeyCode::Backspace if self.model_config_field == RoleConfigField::Model => {
                self.model_config_typing = true;
                self.model_config_selected = None;
                self.model_config_input.pop();
                self.model_config_notice = None;
            }
            KeyCode::Char('u')
                if self.model_config_field == RoleConfigField::Model
                    && key.modifiers.contains(KeyModifiers::CONTROL) =>
            {
                self.model_config_input.clear();
                self.model_config_typing = true;
                self.model_config_selected = None;
                self.model_config_notice = None;
            }
            KeyCode::Enter => {
                let model = self.model_config_input.trim();
                if model.is_empty() {
                    self.model_config_notice = Some("Model name cannot be empty.".into());
                } else if model.len() > 128 {
                    self.model_config_notice = Some("Keep model names under 128 bytes.".into());
                } else if model.chars().any(char::is_whitespace) {
                    self.model_config_notice =
                        Some("Model names cannot contain whitespace.".into());
                } else if let (Some(agent), Some(provider)) = (
                    self.agents.get(self.selected_agent),
                    self.provider_routes.get(self.model_config_provider),
                ) {
                    let mut updated = agent.clone();
                    updated.provider = provider.route.clone();
                    updated.model = model.to_owned();
                    self.model_config_saving = true;
                    self.model_config_notice = Some("Saving role configuration…".into());
                    return ModelConfigEvent::Save(Box::new(updated));
                } else {
                    self.model_config_notice = Some("The selected role is unavailable.".into());
                }
            }
            KeyCode::Char(value)
                if self.model_config_field == RoleConfigField::Model
                    && !key.modifiers.contains(KeyModifiers::CONTROL)
                    && !value.is_whitespace() =>
            {
                if !self.model_config_typing {
                    self.model_config_input.clear();
                    self.model_config_typing = true;
                }
                self.model_config_input.push(value);
                self.model_config_selected = None;
                self.model_config_notice = None;
            }
            _ => {}
        }
        ModelConfigEvent::Consumed
    }

    fn cycle_role_config_choice(&mut self, delta: isize) {
        if self.provider_routes.is_empty() {
            self.model_config_notice = Some("No configured provider routes are available.".into());
            return;
        }
        if self.model_config_field == RoleConfigField::Provider {
            self.model_config_provider = (self.model_config_provider as isize + delta)
                .rem_euclid(self.provider_routes.len() as isize)
                as usize;
            let kind = &self.provider_routes[self.model_config_provider].kind;
            self.model_config_input = self.model_catalog.first_model(kind);
            self.model_config_selected = self
                .model_catalog
                .models(kind)
                .iter()
                .position(|model| model.id == self.model_config_input);
            self.model_config_typing = false;
            self.model_config_notice = None;
            return;
        }
        let kind = &self.provider_routes[self.model_config_provider].kind;
        let models = self.model_catalog.models(kind);
        if models.is_empty() {
            self.model_config_notice = Some("No discovered models; type a model name.".into());
            return;
        }
        let next = match self.model_config_selected {
            Some(current) => (current as isize + delta).rem_euclid(models.len() as isize) as usize,
            None if delta.is_negative() => models.len() - 1,
            None => 0,
        };
        self.model_config_selected = Some(next);
        self.model_config_input.clone_from(&models[next].id);
        self.model_config_typing = false;
        self.model_config_notice = None;
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
                    self.quit = true;
                    ComposerEvent::Consumed
                }
                KeyCode::Enter
                    if key
                        .modifiers
                        .intersects(KeyModifiers::SHIFT | KeyModifiers::ALT) =>
                {
                    self.composer.insert_newline();
                    self.composer_notice = None;
                    ComposerEvent::Consumed
                }
                KeyCode::Char('j') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    self.composer.insert_newline();
                    self.composer_notice = None;
                    ComposerEvent::Consumed
                }
                KeyCode::Enter => {
                    let message = self.composer.lines().join("\n").trim().to_owned();
                    if message.is_empty() {
                        self.composer_notice = Some("Describe a task first.".into());
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
                KeyCode::Char('z') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    self.composer.undo();
                    ComposerEvent::Consumed
                }
                KeyCode::Char('y') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    self.composer.redo();
                    ComposerEvent::Consumed
                }
                _ => {
                    self.composer.input(*key);
                    self.composer_notice = None;
                    ComposerEvent::Consumed
                }
            },
            Event::Mouse(mouse)
                if self
                    .composer_hitbox
                    .contains((mouse.column, mouse.row).into()) =>
            {
                self.composer.input(*mouse);
                ComposerEvent::Consumed
            }
            _ => ComposerEvent::Consumed,
        }
    }

    fn quick_run_active(&self) -> bool {
        self.quick_run_starting
            || self.tasks.iter().any(|task| {
                task.objective == QUICK_RUN_OBJECTIVE
                    && matches!(
                        task.status,
                        TaskStatus::Queued
                            | TaskStatus::Ready
                            | TaskStatus::Running
                            | TaskStatus::WaitingApproval
                    )
            })
    }
    pub fn apply_event(&mut self, event: EngineEvent) {
        match event {
            EngineEvent::TaskUpdated(task) => {
                let is_new = !self.tasks.iter().any(|existing| existing.id == task.id);
                if task.objective == QUICK_RUN_OBJECTIVE
                    && matches!(
                        task.status,
                        TaskStatus::Blocked
                            | TaskStatus::Completed
                            | TaskStatus::Failed
                            | TaskStatus::Cancelled
                    )
                {
                    self.quick_run_starting = false;
                }
                if let Some(old) = self.tasks.iter_mut().find(|x| x.id == task.id) {
                    *old = task
                } else {
                    self.tasks.push(task);
                }
                self.sort_tasks_for_browser();
                if is_new {
                    self.selected = 0;
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
                self.push_scoped_event(e.task_id, &e.kind)
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
            let drain = self.events.len() - 500;
            self.events.drain(..drain);
            self.event_task_ids
                .drain(..drain.min(self.event_task_ids.len()));
        }
        if self.scoped_events.len() > 500 {
            self.scoped_events.drain(..self.scoped_events.len() - 500);
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
        self.push_scoped_event(None, kind);
    }

    fn push_scoped_event(&mut self, task_id: Option<voff_core::TaskId>, kind: &CanonicalEventKind) {
        if let Some(owner) = task_id.and_then(|id| {
            self.tasks
                .iter()
                .find(|task| task.id == id)
                .map(|task| task.owner)
        }) {
            match kind {
                CanonicalEventKind::ProviderStarted => {
                    self.agent_responses.insert(owner, String::new());
                }
                CanonicalEventKind::TextDelta(delta) => {
                    let response = self.agent_responses.entry(owner).or_default();
                    response.push_str(&sanitize(delta));
                    trim_response_buffer(response);
                }
                _ => {}
            }
        }
        self.event_task_ids.resize(self.events.len(), None);
        let was_streaming = self.streaming_text;
        if let CanonicalEventKind::TextDelta(delta) = kind {
            let clean = sanitize(delta);
            if was_streaming {
                if let Some(last) = self.events.last_mut() {
                    last.push_str(&clean);
                }
            } else {
                self.events.push(clean);
                self.event_task_ids.push(task_id);
                self.streaming_text = true;
            }
        } else {
            self.streaming_text = false;
            self.events.push(format_event(kind));
            self.event_task_ids.push(task_id);
        }
        if let CanonicalEventKind::TextDelta(delta) = kind {
            let clean = sanitize(delta);
            if was_streaming
                && self
                    .scoped_events
                    .last()
                    .is_some_and(|event| event.task_id == task_id)
            {
                if let Some(last) = self.scoped_events.last_mut() {
                    last.text.push_str(&clean);
                }
            } else {
                self.scoped_events.push(ScopedEvent {
                    task_id,
                    text: clean,
                });
            }
        } else {
            self.scoped_events.push(ScopedEvent {
                task_id,
                text: format_event(kind),
            });
        }
        self.on_events_changed();
    }

    fn push_ui_event(&mut self, message: String) {
        self.event_task_ids.resize(self.events.len(), None);
        self.streaming_text = false;
        self.events.push(message);
        self.event_task_ids.push(None);
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
    fn sort_tasks_for_browser(&mut self) {
        let selected_id = self.selected_task().map(|task| task.id);
        let order = task_browser_order(&self.tasks, self.team_lead);
        let ranks = order
            .into_iter()
            .enumerate()
            .map(|(rank, id)| (id, rank))
            .collect::<HashMap<_, _>>();
        self.tasks
            .sort_by_key(|task| ranks.get(&task.id).copied().unwrap_or(usize::MAX));
        self.selected = selected_id
            .and_then(|id| self.tasks.iter().position(|task| task.id == id))
            .unwrap_or(0);
    }
    fn agent_for_owner(&self, owner: AgentId) -> Option<&AgentProfile> {
        self.agents
            .iter()
            .find(|agent| agent.id == owner)
            .or_else(|| self.known_agents.get(&owner))
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
        KeyCode::Enter => Action::FocusComposer,
        KeyCode::Char('R') => Action::QuickRun,
        KeyCode::Char('r') => Action::RetryTask,
        KeyCode::Char('A') => Action::AddCredential,
        KeyCode::Char('c') => Action::ConfigureModel,
        KeyCode::Char('x') => Action::Cancel,
        KeyCode::Char('y') => Action::Approve,
        KeyCode::Char('n') => Action::Deny,
        KeyCode::Char('e') => Action::ToggleEvents,
        KeyCode::PageUp => Action::PageUp,
        KeyCode::PageDown => Action::PageDown,
        KeyCode::Char('f') => Action::FitTeam,
        KeyCode::Char('+') | KeyCode::Char('=') => Action::ZoomTeamIn,
        KeyCode::Char('-') => Action::ZoomTeamOut,
        KeyCode::Char('0') => Action::ResetTeamZoom,
        KeyCode::Char('t') => Action::AdjustTeam,
        KeyCode::Char('1') => Action::ShowTeam,
        KeyCode::Char('2') => Action::ShowTasks,
        KeyCode::Char('s') => Action::ShowTaskDetails,
        KeyCode::Char('a') => Action::ShowRoleActivity,
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
        CanonicalEventKind::TaskRetried {
            previous,
            retry,
            attempt,
        } => format!(
            "re-run attempt {attempt}: {} -> {}",
            short_task_id(*previous),
            short_task_id(*retry)
        ),
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
            format!(
                "delegated {} from {}",
                short_task_id(*child),
                short_task_id(*parent)
            )
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

fn task_event_prefix(task_id: voff_core::TaskId) -> String {
    format!("[{}] ", short_task_id(task_id))
}

fn task_browser_order(tasks: &[Task], team_lead: Option<AgentId>) -> Vec<voff_core::TaskId> {
    let by_id = tasks
        .iter()
        .map(|task| (task.id, task))
        .collect::<HashMap<_, _>>();
    let mut roots = Vec::new();
    let mut children = HashMap::<voff_core::TaskId, Vec<voff_core::TaskId>>::new();
    for task in tasks {
        if let Some(parent) = task.parent.filter(|parent| by_id.contains_key(parent)) {
            children.entry(parent).or_default().push(task.id);
        } else {
            roots.push(task.id);
        }
    }

    let compare = |a: &voff_core::TaskId, b: &voff_core::TaskId| {
        let a = by_id[a];
        let b = by_id[b];
        let a_is_lead = Some(a.owner) == team_lead;
        let b_is_lead = Some(b.owner) == team_lead;
        b_is_lead
            .cmp(&a_is_lead)
            .then_with(|| b.created_at.cmp(&a.created_at))
            .then_with(|| a.id.to_string().cmp(&b.id.to_string()))
    };
    roots.sort_by(compare);
    for siblings in children.values_mut() {
        siblings.sort_by(compare);
    }

    let mut ordered = Vec::with_capacity(tasks.len());
    let mut visited = HashSet::with_capacity(tasks.len());
    let mut stack = roots.iter().rev().copied().collect::<Vec<_>>();
    while let Some(id) = stack.pop() {
        if !visited.insert(id) {
            continue;
        }
        ordered.push(id);
        if let Some(child_ids) = children.get(&id) {
            stack.extend(child_ids.iter().rev().copied());
        }
    }

    let mut remaining = tasks
        .iter()
        .map(|task| task.id)
        .filter(|id| !visited.contains(id))
        .collect::<Vec<_>>();
    remaining.sort_by(compare);
    for id in remaining {
        if visited.contains(&id) {
            continue;
        }
        stack.push(id);
        while let Some(nested_id) = stack.pop() {
            if !visited.insert(nested_id) {
                continue;
            }
            ordered.push(nested_id);
            if let Some(child_ids) = children.get(&nested_id) {
                stack.extend(child_ids.iter().rev().copied());
            }
        }
    }
    ordered
}

fn event_text_color(text: &str) -> Color {
    let text = text.to_ascii_lowercase();
    if text.starts_with("tool proposed:")
        || text.starts_with("approval requested:")
        || text.starts_with("approval:")
    {
        Color::Yellow
    } else if text.contains("failed")
        || text.contains("conflict")
        || text.contains("cancelled")
        || text.contains("denied")
    {
        Color::LightRed
    } else if text.starts_with("provider completed")
        || text.starts_with("result:")
        || text.starts_with("artifact published:")
    {
        Color::Green
    } else if text.starts_with("task ")
        || text.starts_with("task:")
        || text.starts_with("re-run attempt ")
        || text.starts_with("delegated ")
        || text.starts_with("dependency:")
    {
        Color::LightCyan
    } else if text.starts_with("usage:") {
        Color::LightMagenta
    } else {
        Color::White
    }
}

fn event_viewer_lines(app: &AppModel) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    for (index, event) in app.events.iter().enumerate().rev() {
        let task_id = app.event_task_ids.get(index).copied().flatten();
        let color = event_text_color(event);
        let mut first = true;
        for event_line in event.split('\n') {
            let mut spans = Vec::new();
            if first {
                spans.push(Span::styled(
                    format!("#{} · ", index + 1),
                    Style::default().fg(Color::DarkGray),
                ));
                if let Some(task_id) = task_id {
                    spans.push(Span::styled(
                        task_event_prefix(task_id),
                        Style::default().fg(Color::LightBlue),
                    ));
                }
                first = false;
            }
            spans.push(Span::styled(
                event_line.to_owned(),
                Style::default().fg(color),
            ));
            lines.push(Line::from(spans));
        }
        lines.push(Line::from(""));
    }
    lines
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
    pub known_agents: HashMap<AgentId, AgentProfile>,
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
    pub provider_routes: Vec<ProviderRouteOption>,
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
    app.known_agents = config.known_agents;
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
    app.provider_routes = config.provider_routes;
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
    restore_initial_history(
        &mut app,
        config.initial_tasks,
        config.initial_events,
        config.initial_approvals,
        config.initial_artifacts,
    );
    terminal_loop(
        &mut app,
        engine,
        LoopConfig {
            owner: config.owner,
            max_parallel_tasks: config.max_parallel_tasks,
            credential_repository,
        },
        input_tx,
        input_rx,
        event_rx,
    )
    .await
}

fn restore_initial_history(
    app: &mut AppModel,
    tasks: Vec<Task>,
    events: Vec<CanonicalEvent>,
    approvals: Vec<Approval>,
    artifacts: Vec<Artifact>,
) {
    app.tasks = tasks;
    app.sort_tasks_for_browser();
    app.approvals = approvals;
    app.artifacts = artifacts;
    for event in events {
        app.push_scoped_event(event.task_id, &event.kind);
    }
    if !app.tasks.is_empty() {
        app.selected = 0;
    }
}
struct LoopConfig {
    owner: AgentId,
    max_parallel_tasks: usize,
    credential_repository: Arc<SqliteRepository>,
}

const QUICK_RUN_OBJECTIVE: &str = "Quick Run: analyze the workspace Markdown files as the project brief. Reconcile their active requirements, TODOs, acceptance criteria, and roadmap items with the current source; ignore work that is already completed, superseded, generated, or out of scope. Build a prioritized implementation plan, then create bounded tasks with concrete acceptance criteria. When enabled specialists are available, delegate every independent workstream to the best-fitting role with non-overlapping ownership; in a solo team, execute the work directly. Carry approved tasks through implementation and validation, request review where useful, and integrate the resulting evidence. Preserve user control: use the normal approval flow for every delegation, patch, command, or other side effect.";

const QUICK_RUN_ACCEPTANCE: [&str; 5] = [
    "Relevant Markdown requirements are reconciled with the current workspace state",
    "Actionable work is prioritized into bounded tasks with concrete acceptance criteria",
    "Independent workstreams are delegated to appropriate enabled specialists when available",
    "Approved work is implemented and validated with inspectable evidence",
    "Completed, superseded, generated, and out-of-scope items are not reimplemented",
];

const INPUT_POLL_INTERVAL: Duration = Duration::from_millis(100);

fn forward_terminal_events<P, R>(input_tx: mpsc::Sender<Event>, mut poll: P, mut read: R)
where
    P: FnMut(Duration) -> io::Result<bool>,
    R: FnMut() -> io::Result<Event>,
{
    while !input_tx.is_closed() {
        match poll(INPUT_POLL_INTERVAL) {
            Ok(true) => {
                let Ok(event) = read() else {
                    break;
                };
                if input_tx.blocking_send(event).is_err() {
                    break;
                }
            }
            Ok(false) => {}
            Err(_) => break,
        }
    }
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
        max_parallel_tasks,
        credential_repository,
    } = config;
    let _guard = TerminalGuard::enter()?;
    let mut terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;
    let input = tokio::task::spawn_blocking(move || {
        forward_terminal_events(input_tx, event::poll, event::read);
    });
    let (credential_tx, mut credential_rx) = mpsc::channel(1);
    let (team_adjustment_tx, mut team_adjustment_rx) =
        mpsc::channel::<Result<SavedTeamAdjustment, String>>(1);
    let (model_config_tx, mut model_config_rx) = mpsc::channel::<Result<AgentProfile, String>>(1);
    let (lead_submission_tx, mut lead_submission_rx) =
        mpsc::channel::<Result<&'static str, String>>(8);
    let (quick_run_tx, mut quick_run_rx) = mpsc::channel::<Result<(), String>>(1);
    let (retry_tx, mut retry_rx) = mpsc::channel::<Result<(voff_core::TaskId, u16), String>>(1);
    let mut tick = tokio::time::interval(Duration::from_millis(100));
    let mut loop_result = Ok(());
    'terminal: while !model.quit {
        tokio::select! {
            Some(event)=input_rx.recv()=>{
                let model_config = model.handle_model_config_event(&event);
                if let ModelConfigEvent::Save(profile) = model_config {
                    let profile = *profile;
                    let model_config_tx = model_config_tx.clone();
                    let repository = Arc::clone(&credential_repository);
                    tokio::spawn(async move {
                        let result = repository
                            .save_agent(profile.clone())
                            .await
                            .map(|()| profile)
                            .map_err(|error| error.to_string());
                        let _ = model_config_tx.send(result).await;
                    });
                } else if matches!(model_config, ModelConfigEvent::Ignored) {
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
                        ComposerEvent::Submit(objective) => {
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
                        ComposerEvent::Consumed => {}
                        ComposerEvent::Ignored => {
                            let action = action_for_event(event);
                            match action {
                                Action::QuickRun => {
                                    if model.quick_run_active() {
                                        model.composer_notice =
                                            Some("Quick Run is already active in Tasks.".into());
                                        model.panel = Panel::Tasks;
                                    } else {
                                        model.quick_run_starting = true;
                                        model.panel = Panel::Tasks;
                                        model.event_viewer = false;
                                        model.help = false;
                                        model.selected = 0;
                                        model.composer_notice = Some(
                                            "Quick Run started: Team Lead is analysing Markdown."
                                                .into(),
                                        );
                                        model.push_ui_event(
                                            "Quick Run → Team Lead: analyze Markdown, plan, delegate, execute"
                                                .into(),
                                        );
                                        let engine = Arc::clone(&engine);
                                        let quick_run_tx = quick_run_tx.clone();
                                        tokio::spawn(async move {
                                            let result = async {
                                                let task = engine
                                                    .create_task(
                                                        owner,
                                                        QUICK_RUN_OBJECTIVE,
                                                        QUICK_RUN_ACCEPTANCE
                                                            .iter()
                                                            .map(|criterion| (*criterion).into())
                                                            .collect(),
                                                    )
                                                    .await?;
                                                engine.mark_ready(task.id).await?;
                                                engine.run_task(task.id).await
                                            }
                                            .await
                                            .map_err(|error| error.to_string());
                                            let _ = quick_run_tx.send(result).await;
                                        });
                                    }
                                }
                                Action::RetryTask if model.panel == Panel::Tasks => {
                                    if model.retry_starting {
                                        model.composer_notice =
                                            Some("A task re-run is already being prepared.".into());
                                    } else if let Some(task) = model.selected_task().cloned() {
                                        if task.status.can_retry() {
                                            model.retry_starting = true;
                                            model.composer_notice = Some(format!(
                                                "Preparing attempt {} for the selected task.",
                                                task.attempt.saturating_add(1)
                                            ));
                                            let engine = Arc::clone(&engine);
                                            let retry_tx = retry_tx.clone();
                                            tokio::spawn(async move {
                                                let prepared = async {
                                                    let retry = engine.retry_task(task.id).await?;
                                                    engine.mark_ready(retry.id).await?;
                                                    Ok::<_, voff_engine::EngineError>((
                                                        retry.id,
                                                        retry.attempt,
                                                    ))
                                                }
                                                .await
                                                .map_err(|error| error.to_string());
                                                match prepared {
                                                    Ok((retry_id, attempt)) => {
                                                        let _ = retry_tx
                                                            .send(Ok((retry_id, attempt)))
                                                            .await;
                                                        let _ = engine.run_task(retry_id).await;
                                                    }
                                                    Err(error) => {
                                                        let _ = retry_tx.send(Err(error)).await;
                                                    }
                                                }
                                            });
                                        } else {
                                            model.composer_notice = Some(format!(
                                                "A {:?} task cannot be re-run; cancel it first if it is active.",
                                                task.status
                                            ));
                                        }
                                    } else {
                                        model.composer_notice =
                                            Some("Select a task to re-run.".into());
                                    }
                                }
                                Action::Cancel => {
                                    if let Some(id) = model
                                        .selected_task()
                                        .filter(|task| {
                                            matches!(
                                                task.status,
                                                TaskStatus::Queued
                                                    | TaskStatus::Ready
                                                    | TaskStatus::Running
                                            )
                                        })
                                        .map(|task| task.id)
                                    {
                                        let engine = Arc::clone(&engine);
                                        tokio::spawn(async move {
                                            let _ = engine.cancel_task(id).await;
                                        });
                                    }
                                }
                                Action::Approve | Action::Deny => {
                                    if let Some(id) = model.pending_approval().map(|value| value.id) {
                                        let engine = Arc::clone(&engine);
                                        let decision = if action == Action::Approve {
                                            ApprovalDecision::ApprovedOnce
                                        } else {
                                            ApprovalDecision::Denied
                                        };
                                        tokio::spawn(async move {
                                            if engine
                                                .decide_approval(id, decision)
                                                .await
                                                .is_ok()
                                                && decision == ApprovalDecision::ApprovedOnce
                                            {
                                                let _ = engine
                                                    .run_until_idle_bounded(max_parallel_tasks)
                                                    .await;
                                            }
                                        });
                                    }
                                }
                                other => model.reduce(other),
                            }
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
                        model.push_ui_event(
                            "Provider credential stored locally; the team is ready to run.".into(),
                        );
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
            Some(result)=model_config_rx.recv()=>{
                model.model_config_saving = false;
                match result {
                    Ok(profile) => {
                        let name = profile.name.clone();
                        let selected_provider = profile.provider.clone();
                        let selected_model = profile.model.clone();
                        let provider = model
                            .provider_routes
                            .iter()
                            .find(|provider| provider.route == selected_provider)
                            .cloned()
                            .unwrap_or(ProviderRouteOption {
                                route: selected_provider.clone(),
                                kind: selected_provider.clone(),
                            });
                        let provider_label = provider_option_label(&provider);
                        if let Some(agent) = model.agents.iter_mut().find(|agent| agent.id == profile.id) {
                            *agent = profile.clone();
                        }
                        model.known_agents.insert(profile.id, profile.clone());
                        model.agent_provider_kinds.insert(profile.id, provider.kind);
                        if Some(profile.id) == model.team_lead {
                            model.provider_label.clone_from(&provider_label);
                            model.model_label.clone_from(&selected_model);
                        }
                        model.model_config_open = false;
                        model.model_config_notice = None;
                        model.composer_notice = Some(format!(
                            "{name} will use {provider_label} · {selected_model} for the next task."
                        ));
                        model.push_ui_event(format!(
                            "team: {name} provider/model changed to {selected_provider}/{selected_model}"
                        ));
                    }
                    Err(error) => model.model_config_notice = Some(format!(
                        "Could not save the role configuration: {}",
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
            Some(result)=quick_run_rx.recv()=>{
                match result {
                    Ok(()) => {
                        model.composer_notice = Some(
                            "Quick Run planned. Review and approve its visible actions.".into()
                        );
                    }
                    Err(error) => {
                        model.quick_run_starting = false;
                        let error = sanitize(&error);
                        model.composer_notice = Some(format!(
                            "Quick Run failed: {error}"
                        ));
                        model.push_ui_event(format!("Quick Run failed: {error}"));
                    }
                }
            }
            Some(result)=retry_rx.recv()=>{
                model.retry_starting = false;
                match result {
                    Ok((task_id, attempt)) => {
                        model.composer_notice = Some(format!(
                            "Re-running attempt {attempt} as {}.",
                            short_task_id(task_id)
                        ));
                    }
                    Err(error) => {
                        let error = sanitize(&error);
                        model.composer_notice = Some(format!(
                            "Could not re-run task: {error}"
                        ));
                        model.push_ui_event(format!("Task re-run failed: {error}"));
                    }
                }
            }
            Some(message)=event_rx.recv()=>model.apply_event(message),
            _=tick.tick()=>{
                model.team_flow.tick(Duration::from_millis(100));
                if let Err(error) = terminal.draw(|frame|render(frame,model)) {
                    loop_result = Err(TuiError::Io(error));
                    break 'terminal;
                }
            }
            else=>{model.quit=true}
        }
    }
    drop(input_rx);
    let input_result = input.await;
    match (loop_result, input_result) {
        (Err(error), _) => Err(error),
        (Ok(()), Ok(())) => Ok(()),
        (Ok(()), Err(error)) => Err(TuiError::Io(io::Error::other(format!(
            "terminal input worker failed: {error}"
        )))),
    }
}
fn composer_height(app: &AppModel, area: Rect) -> u16 {
    const MIN_HEIGHT: u16 = 3;
    const MAX_HEIGHT: u16 = 12;

    if !app.composer_active || area.height <= MIN_HEIGHT {
        return MIN_HEIGHT.min(area.height);
    }

    let wrap_width = usize::from(area.width.saturating_sub(2).max(1));
    let visual_lines = app
        .composer
        .lines()
        .iter()
        .map(|line| line.chars().count().max(1).div_ceil(wrap_width))
        .sum::<usize>();
    let desired = u16::try_from(visual_lines.saturating_add(2)).unwrap_or(u16::MAX);
    let available = area.height.saturating_sub(12).max(MIN_HEIGHT);
    desired.clamp(MIN_HEIGHT, MAX_HEIGHT.min(available))
}

fn render(frame: &mut ratatui::Frame<'_>, app: &mut AppModel) {
    let composer_height = composer_height(app, frame.area());
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(4),
            Constraint::Min(8),
            Constraint::Length(composer_height),
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
                " · no token [A]".to_owned()
            }
        })
        .unwrap_or_default();
    let view = match app.panel {
        Panel::Agents => "Team",
        Panel::Tasks => "Tasks",
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
                    "workspace · {}/{} developers enabled · {} · session {}",
                    app.agents.iter().filter(|agent| agent.enabled).count(),
                    app.agents.len(),
                    view,
                    &session[..session.len().min(8)]
                )),
            ]),
            Line::from(Span::styled(
                format!(
                    " [1 Team] [2 Tasks] · {} · {} · circuit {:?} · {} retries · {cost}{credential}",
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

    match app.panel {
        Panel::Agents => render_team(frame, app, rows[1]),
        Panel::Tasks => render_tasks_workspace(frame, app, rows[1]),
    }

    let workspace = sanitize(&app.workspace_root.display().to_string());
    app.composer.set_placeholder_text(if app.composer_active {
        "Describe the task for the Team Lead"
    } else {
        "Press Enter to assign a task"
    });
    let composer_status = if app.composer_active {
        let DataCursor(line, column) = app.composer.cursor();
        Some((
            Line::from(format!(
                " Enter send · Shift+Enter/Ctrl+J newline · Ctrl+Z/Y undo/redo · {}:{} ",
                line + 1,
                column + 1
            ))
            .style(Style::default().fg(Color::LightCyan)),
            Alignment::Left,
        ))
    } else {
        Some(app.composer_notice.as_deref().map_or_else(
            || {
                (
                    Line::from(" [R] Quick Run ").style(Style::default().fg(Color::White)),
                    Alignment::Center,
                )
            },
            |notice| {
                (
                    Line::from(format!(" {} ", sanitize(notice)))
                        .style(Style::default().fg(Color::DarkGray)),
                    Alignment::Left,
                )
            },
        ))
    };
    let composer_color = if app.composer_active {
        Color::LightCyan
    } else {
        Color::DarkGray
    };
    let mut composer_block = Block::default()
        .title(" Team Lead ")
        .title_bottom(
            Line::from(format!(" ? help · cwd {workspace} "))
                .style(Style::default().fg(Color::DarkGray))
                .alignment(ratatui::layout::Alignment::Right),
        )
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(composer_color));
    if let Some((status, status_alignment)) = composer_status {
        composer_block = composer_block.title_bottom(status.alignment(status_alignment));
    }
    app.composer.set_block(composer_block);
    app.composer.set_cursor_style(if app.composer_active {
        Style::default()
            .fg(Color::Black)
            .bg(Color::LightCyan)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default()
    });
    app.composer_hitbox = rows[2];
    frame.render_widget(&app.composer, rows[2]);
    render_overlays(frame, app);
}

fn render_tasks_workspace(frame: &mut ratatui::Frame<'_>, app: &mut AppModel, area: Rect) {
    let panes = if area.width >= 88 {
        Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(38), Constraint::Percentage(62)])
            .split(area)
    } else {
        Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Percentage(42), Constraint::Percentage(58)])
            .split(area)
    };
    let browser = panes[0];
    let inspector = panes[1];
    app.task_list_hitbox = browser;

    let active = app
        .tasks
        .iter()
        .filter(|task| matches!(task.status, TaskStatus::Ready | TaskStatus::Running))
        .count();
    let attention = app
        .tasks
        .iter()
        .filter(|task| {
            matches!(
                task.status,
                TaskStatus::WaitingApproval | TaskStatus::Blocked | TaskStatus::Failed
            )
        })
        .count();
    let completed = app
        .tasks
        .iter()
        .filter(|task| task.status == TaskStatus::Completed)
        .count();
    let quick_run = if app.quick_run_active() {
        " · R QUICK RUN"
    } else {
        ""
    };
    let browser_title = format!(
        " Tasks · lead first · newest peers · [r] re-run{quick_run} · {} total · {active} active · {attention} attention · {completed} done ",
        app.tasks.len()
    );

    if app.tasks.is_empty() {
        app.task_window_start = 0;
        frame.render_widget(
            Paragraph::new(vec![
                Line::from(Span::styled(
                    "No tasks in this session",
                    Style::default().add_modifier(Modifier::BOLD),
                )),
                Line::from(""),
                Line::from(Span::styled(
                    "Press Enter and describe work for the Team Lead.",
                    Style::default().fg(Color::DarkGray),
                )),
                Line::from(Span::styled(
                    "Or press R for Quick Run: analyze Markdown, plan, delegate, execute.",
                    Style::default().fg(Color::Yellow),
                )),
            ])
            .wrap(Wrap { trim: true })
            .block(panel(&browser_title, true).border_type(BorderType::Rounded)),
            browser,
        );
    } else {
        app.selected = app.selected.min(app.tasks.len() - 1);
        let visible = usize::from(browser.height.saturating_sub(2) / 2).max(1);
        let start = app
            .selected
            .saturating_add(1)
            .saturating_sub(visible)
            .min(app.tasks.len().saturating_sub(visible));
        app.task_window_start = start;
        let objective_width = usize::from(browser.width.saturating_sub(9)).max(8);
        let items = app
            .tasks
            .iter()
            .enumerate()
            .skip(start)
            .take(visible)
            .map(|(index, task)| {
                let selected = index == app.selected;
                let (icon, status, color) = task_status_visual(task.status);
                let (owner, owner_icon, owner_color) = app.agent_for_owner(task.owner).map_or_else(
                    || (short_agent_id(task.owner), "●", Color::Cyan),
                    |agent| {
                        (
                            sanitize(&agent.name),
                            role_icon(&agent.role),
                            role_color(&agent.role),
                        )
                    },
                );
                let max_visible_depth = usize::from(browser.width.saturating_sub(24) / 2);
                let visible_depth = usize::from(task.depth).min(max_visible_depth);
                let indent = "  ".repeat(visible_depth);
                let objective = truncate_text(
                    &one_line(&sanitize(&task.objective)),
                    objective_width.saturating_sub(indent.len()).max(8),
                );
                let heading = vec![
                    Span::raw(indent.clone()),
                    Span::styled(
                        format!("{owner_icon} "),
                        Style::default()
                            .fg(owner_color)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(owner, Style::default().add_modifier(Modifier::BOLD)),
                    Span::styled(format!("  {icon} {status}"), Style::default().fg(color)),
                    Span::styled(
                        if task.attempt > 1 {
                            format!("  ↻ attempt {}", task.attempt)
                        } else {
                            String::new()
                        },
                        Style::default().fg(Color::LightCyan),
                    ),
                ];
                let lines = vec![
                    Line::from(heading),
                    Line::from(Span::styled(
                        format!("{indent}{objective}"),
                        Style::default().fg(if selected { Color::White } else { Color::Gray }),
                    )),
                ];
                ListItem::new(lines).style(if selected {
                    Style::default()
                        .bg(Color::DarkGray)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default()
                })
            })
            .collect::<Vec<_>>();
        frame.render_widget(
            List::new(items).block(panel(&browser_title, true).border_type(BorderType::Rounded)),
            browser,
        );
    }

    let Some(task) = app.selected_task().cloned() else {
        frame.render_widget(
            Paragraph::new("Select or create a task to inspect its work.")
                .style(Style::default().fg(Color::DarkGray))
                .block(panel(" Inspector ", true).border_type(BorderType::Rounded)),
            inspector,
        );
        return;
    };
    let (title, lines) = match app.task_inspector {
        TaskInspector::Details => (
            " Summary · [r] re-run · [s] summary · [a] activity · PgUp/PgDn scroll ",
            task_detail_lines(app, &task),
        ),
        TaskInspector::RoleActivity => (
            " Activity · [r] re-run · [s] summary · [a] activity · PgUp/PgDn scroll ",
            role_activity_lines(app, &task),
        ),
    };
    frame.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .scroll((app.task_inspector_scroll, 0))
            .block(panel(title, true).border_type(BorderType::Rounded)),
        inspector,
    );
}

fn task_detail_lines(app: &AppModel, task: &Task) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    let (icon, status, color) = task_status_visual(task.status);
    let agent = app.agent_for_owner(task.owner);
    let owner = agent.map_or_else(|| short_agent_id(task.owner), |agent| sanitize(&agent.name));
    let role = agent.map_or_else(|| "stored role".into(), |agent| human_role(&agent.role));
    lines.push(Line::from(vec![
        Span::styled(
            format!(" {icon} {status} "),
            Style::default().fg(color).add_modifier(Modifier::BOLD),
        ),
        Span::styled(owner, Style::default().add_modifier(Modifier::BOLD)),
        Span::styled(format!("  ·  {role}"), Style::default().fg(Color::Gray)),
    ]));
    lines.push(Line::from(Span::styled(
        format!(" {}  ·  attempt {}", task.id, task.attempt),
        Style::default().fg(Color::DarkGray),
    )));
    if let Some(previous) = task.retry_of {
        lines.push(Line::from(Span::styled(
            format!(" re-run of {previous}"),
            Style::default().fg(Color::LightCyan),
        )));
    }
    lines.push(Line::from(""));
    push_section(&mut lines, "OBJECTIVE", Color::LightCyan);
    push_text_lines(&mut lines, &sanitize(&task.objective), "  ");

    lines.push(Line::from(""));
    push_section(&mut lines, "ACCEPTANCE", Color::LightCyan);
    if task.acceptance_criteria.is_empty() {
        lines.push(Line::from(Span::styled(
            "  No explicit criteria",
            Style::default().fg(Color::DarkGray),
        )));
    } else {
        for criterion in &task.acceptance_criteria {
            lines.push(Line::from(format!("  • {}", sanitize(criterion))));
        }
    }

    if let Some(result) = &task.result {
        lines.push(Line::from(""));
        push_section(&mut lines, "RESULT", Color::Green);
        push_text_lines(&mut lines, &sanitize(&result.summary), "  ");
        if !result.evidence.is_empty() {
            lines.push(Line::from(Span::styled(
                format!(
                    "  Evidence  {}",
                    result
                        .evidence
                        .iter()
                        .map(ToString::to_string)
                        .collect::<Vec<_>>()
                        .join(" · ")
                ),
                Style::default().fg(Color::Gray),
            )));
        }
    }

    let artifacts = app
        .artifacts
        .iter()
        .filter(|artifact| artifact.producer == task.id)
        .collect::<Vec<_>>();
    if !artifacts.is_empty() {
        lines.push(Line::from(""));
        push_section(&mut lines, "ARTIFACTS", Color::LightMagenta);
        for artifact in artifacts {
            lines.push(Line::from(vec![
                Span::styled("  ◆ ", Style::default().fg(Color::LightMagenta)),
                Span::styled(
                    sanitize(&artifact.location),
                    Style::default().add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    format!("  {}", sanitize(&artifact.kind)),
                    Style::default().fg(Color::Gray),
                ),
            ]));
            if !artifact.preview.trim().is_empty() {
                push_text_lines(&mut lines, &sanitize(&artifact.preview), "    ");
            }
        }
    }

    lines.push(Line::from(""));
    let parent = task
        .parent
        .map_or_else(|| "root".into(), |parent| parent.to_string());
    lines.push(Line::from(Span::styled(
        format!(
            "  Parent {parent}  ·  depth {}  ·  updated {}",
            task.depth,
            task.updated_at.format("%Y-%m-%d %H:%M:%S UTC")
        ),
        Style::default().fg(Color::DarkGray),
    )));
    lines
}

fn role_activity_lines(app: &AppModel, selected: &Task) -> Vec<Line<'static>> {
    let agent = app.agent_for_owner(selected.owner);
    let owner = agent.map_or_else(
        || short_agent_id(selected.owner),
        |agent| sanitize(&agent.name),
    );
    let role = agent.map_or_else(|| "stored role".into(), |agent| human_role(&agent.role));
    let mut lines = vec![
        Line::from(vec![
            Span::styled(" ◉ ", Style::default().fg(Color::LightMagenta)),
            Span::styled(owner, Style::default().add_modifier(Modifier::BOLD)),
            Span::styled(format!("  ·  {role}"), Style::default().fg(Color::Gray)),
        ]),
        Line::from(Span::styled(
            " Newest first · all events from this role's tasks in the current session",
            Style::default().fg(Color::DarkGray),
        )),
        Line::from(""),
    ];
    let mut count = 0usize;
    for event in app.scoped_events.iter().rev() {
        let Some(task_id) = event.task_id else {
            continue;
        };
        let Some(task) = app.tasks.iter().find(|task| task.id == task_id) else {
            continue;
        };
        if task.owner != selected.owner {
            continue;
        }
        count += 1;
        let selected_marker = if task.id == selected.id { "●" } else { "○" };
        let objective = truncate_text(&one_line(&sanitize(&task.objective)), 52);
        lines.push(Line::from(vec![
            Span::styled(
                format!("{selected_marker} "),
                Style::default().fg(if task.id == selected.id {
                    Color::LightCyan
                } else {
                    Color::DarkGray
                }),
            ),
            Span::styled(
                task_event_prefix(task.id),
                Style::default().fg(Color::LightBlue),
            ),
            Span::styled(objective, Style::default().add_modifier(Modifier::BOLD)),
        ]));
        let event_style = Style::default().fg(event_text_color(&event.text));
        for event_line in event.text.lines() {
            lines.push(Line::from(Span::styled(event_line.to_owned(), event_style)));
        }
        lines.push(Line::from(""));
    }
    if count == 0 {
        lines.push(Line::from(Span::styled(
            "  No task-scoped events recorded for this role yet.",
            Style::default().fg(Color::DarkGray),
        )));
    }
    lines
}

fn push_section(lines: &mut Vec<Line<'static>>, title: &str, color: Color) {
    lines.push(Line::from(Span::styled(
        format!(" {title}"),
        Style::default().fg(color).add_modifier(Modifier::BOLD),
    )));
}

fn push_text_lines(lines: &mut Vec<Line<'static>>, text: &str, prefix: &str) {
    let mut any = false;
    for line in text.lines() {
        any = true;
        lines.push(Line::from(format!("{prefix}{line}")));
    }
    if !any {
        lines.push(Line::from(prefix.to_owned()));
    }
}

fn popup_section(title: &str) -> Line<'static> {
    Line::from(Span::styled(
        format!(" {title} "),
        Style::default()
            .fg(Color::LightMagenta)
            .add_modifier(Modifier::BOLD),
    ))
}

fn popup_section_into(lines: &mut Vec<Line<'static>>, title: &str) {
    lines.push(popup_section(title));
}

fn centered_popup(area: Rect, requested_width: u16, requested_height: u16) -> Rect {
    let width = requested_width.min(area.width);
    let height = requested_height.min(area.height);
    Rect {
        x: area.x + area.width.saturating_sub(width) / 2,
        y: area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    }
}

fn task_status_visual(status: TaskStatus) -> (&'static str, &'static str, Color) {
    match status {
        TaskStatus::Queued => ("○", "queued", Color::Gray),
        TaskStatus::Ready => ("◇", "ready", Color::LightBlue),
        TaskStatus::Running => ("▶", "running", Color::LightCyan),
        TaskStatus::WaitingApproval => ("◆", "approval", Color::Yellow),
        TaskStatus::Blocked => ("!", "blocked", Color::LightRed),
        TaskStatus::Completed => ("✓", "completed", Color::Green),
        TaskStatus::Failed => ("×", "failed", Color::Red),
        TaskStatus::Cancelled => ("–", "cancelled", Color::DarkGray),
    }
}

fn human_role(role: &str) -> String {
    role.replace('_', " ")
}

fn short_task_id(id: voff_core::TaskId) -> String {
    short_id(&id.to_string())
}

fn short_agent_id(id: AgentId) -> String {
    short_id(&id.to_string())
}

fn short_id(id: &str) -> String {
    let suffix = id.chars().rev().take(6).collect::<String>();
    format!("…{}", suffix.chars().rev().collect::<String>())
}

fn one_line(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn truncate_text(value: &str, width: usize) -> String {
    if value.chars().count() <= width {
        return value.to_owned();
    }
    let mut truncated = value
        .chars()
        .take(width.saturating_sub(1))
        .collect::<String>();
    truncated.push('…');
    truncated
}

fn trim_response_buffer(response: &mut String) {
    const MAX_RESPONSE_CHARS: usize = 1_024;
    let count = response.chars().count();
    if count <= MAX_RESPONSE_CHARS {
        return;
    }
    let keep_from = response
        .char_indices()
        .nth(count - MAX_RESPONSE_CHARS)
        .map_or(0, |(index, _)| index);
    response.drain(..keep_from);
}

fn render_team(frame: &mut ratatui::Frame<'_>, app: &mut AppModel, area: Rect) {
    let handoff = handoff_summary(&app.tasks, &app.approvals);
    let title = if app.panel == Panel::Agents {
        format!(
            " specialist → Lead → specialist · {handoff} · · available / ▶ live · [t] team · [c] config · +/- zoom · f fit "
        )
    } else {
        format!(
            " specialist → Lead → specialist · {handoff} · · available / ▶ live · [t] team · [c] config "
        )
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
        TeamSnapshot {
            agents: &app.agents,
            tasks: &app.tasks,
            approvals: &app.approvals,
            responses: &app.agent_responses,
            provider_kinds: &app.agent_provider_kinds,
            team_lead: app.team_lead,
        },
        app.selected_agent,
        inner,
    );
    app.team_flow.render(frame, inner);
}
fn render_overlays(frame: &mut ratatui::Frame<'_>, app: &AppModel) {
    if app.event_viewer {
        let area = frame.area();
        let popup = centered_popup(
            area,
            area.width.saturating_sub(4).min(100),
            area.height.saturating_sub(4).min(32),
        );
        let text = event_viewer_lines(app);
        frame.render_widget(Clear, popup);
        let title = if app.event_follow {
            " Events · live · newest first "
        } else {
            " Events · paused "
        };
        let footer = if app.event_follow {
            " ↓/PgDn older · e/Esc close "
        } else {
            " ↑/PgUp newer · e/Esc close "
        };
        frame.render_widget(
            Paragraph::new(text)
                .wrap(Wrap { trim: false })
                .scroll((app.event_scroll, 0))
                .block(
                    Block::default()
                        .title(title)
                        .title_bottom(
                            Line::from(footer)
                                .style(Style::default().fg(Color::LightCyan))
                                .alignment(ratatui::layout::Alignment::Center),
                        )
                        .borders(Borders::ALL)
                        .border_type(BorderType::Rounded)
                        .border_style(Style::default().fg(Color::Cyan)),
                ),
            popup,
        );
    }
    if app.help {
        let area = frame.area();
        let popup = centered_popup(
            area,
            area.width.saturating_sub(4).min(68),
            area.height.saturating_sub(4).min(24),
        );
        frame.render_widget(Clear, popup);
        let help = vec![
            popup_section("WORKSPACE"),
            Line::from("  1 / 2         Team / Tasks"),
            Line::from("  Tab           Toggle Team / Tasks"),
            Line::from("  ↑/↓ or j/k    Browse tasks and developers"),
            Line::from(""),
            popup_section("TASK DEBUGGING"),
            Line::from("  s             Selected task summary"),
            Line::from("  a             Owning role's activity"),
            Line::from("  PgUp/PgDn     Scroll details or events"),
            Line::from("  x             Cancel selected active task"),
            Line::from("  r             Re-run selected terminal/blocked task"),
            Line::from(""),
            popup_section("TEAM & INPUT"),
            Line::from("  R             Quick Run Markdown → tasks → execution"),
            Line::from("  Enter         Create a Team Lead task"),
            Line::from("  Shift+Enter   Insert newline (composer grows)"),
            Line::from("  Ctrl+J/Alt+↵  Newline fallback"),
            Line::from("  ←/→ · Home/End · Ctrl+←/→   Navigate text"),
            Line::from("  Ctrl+W/K      Delete word / to line end"),
            Line::from("  Ctrl+Z/Y      Undo / redo"),
            Line::from("  Shift+arrows  Select · PgUp/PgDn scroll"),
            Line::from("  t             Add or adjust the team"),
            Line::from("  c             Configure role provider/model"),
            Line::from("  +/-/0/f       Zoom / reset / fit team"),
            Line::from("  e             Session event viewer"),
        ];
        frame.render_widget(
            Paragraph::new(help).wrap(Wrap { trim: false }).block(
                Block::default()
                    .title(" Help ")
                    .title_bottom(
                        Line::from(" ? / Esc close · q safe exit ")
                            .style(Style::default().fg(Color::LightCyan))
                            .alignment(ratatui::layout::Alignment::Center),
                    )
                    .borders(Borders::ALL)
                    .border_type(BorderType::Rounded)
                    .border_style(Style::default().fg(Color::Cyan)),
            ),
            popup,
        );
    }
    if let Some(approval) = app.pending_approval() {
        render_approval_popup(frame, app, approval);
    }
    if let Some(input) = &app.credential_input {
        let area = frame.area();
        let popup = centered_popup(
            area,
            area.width.saturating_sub(4).min(68),
            area.height.saturating_sub(2).min(11),
        );
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
                "Paste the configured provider API credential\n\n  {status}  \n\nStored in the local .voff SQLite database, never in project config or logs.{error}"
            ))
            .alignment(ratatui::layout::Alignment::Center)
            .wrap(Wrap { trim: true })
            .block(
                Block::default()
                    .title(" Add token (optional) ")
                    .title_bottom(
                        Line::from(" Enter save · Esc cancel ")
                            .style(Style::default().fg(Color::LightCyan))
                            .alignment(ratatui::layout::Alignment::Center),
                    )
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
    if app.model_config_open {
        render_model_config_popup(frame, app);
    }
}

fn render_model_config_popup(frame: &mut ratatui::Frame<'_>, app: &AppModel) {
    let Some(agent) = app.agents.get(app.selected_agent) else {
        return;
    };
    let Some(provider) = app.provider_routes.get(app.model_config_provider) else {
        return;
    };
    let option = app
        .model_catalog
        .option(&provider.kind, &app.model_config_input);
    let pricing = option.map_or_else(
        || "price unavailable".into(),
        crate::model_catalog::ModelOption::price_label,
    );
    let context = option.and_then(|model| model.context_limit).map_or_else(
        || "context unknown".into(),
        |limit| format!("{limit} context"),
    );
    let model_label = if app.model_catalog.models(&provider.kind).is_empty() {
        app.model_config_input.clone()
    } else {
        format!(
            "{}  ({})",
            app.model_config_input,
            app.model_catalog
                .selection_label(&provider.kind, &app.model_config_input)
        )
    };
    let provider_label = provider_option_label(provider);
    let mut lines = vec![
        Line::from(vec![
            Span::styled("Role  ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                sanitize(&agent.name),
                Style::default().add_modifier(Modifier::BOLD),
            ),
        ]),
        Line::from(""),
        role_config_line(
            "Provider",
            &provider_label,
            app.model_config_field == RoleConfigField::Provider,
            true,
        ),
        Line::from(""),
        role_config_line(
            "Model",
            &model_label,
            app.model_config_field == RoleConfigField::Model,
            !app.model_catalog.model_is_editable(&provider.kind),
        ),
        Line::from(Span::styled(
            format!("  {pricing} · {context}"),
            Style::default().fg(Color::DarkGray),
        )),
        Line::from(Span::styled(
            format!(
                "  {} configured provider route{}",
                app.provider_routes.len(),
                if app.provider_routes.len() == 1 {
                    ""
                } else {
                    "s"
                }
            ),
            Style::default().fg(Color::DarkGray),
        )),
    ];
    if let Some(notice) = &app.model_config_notice {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            sanitize(notice),
            Style::default().fg(if app.model_config_saving {
                Color::Yellow
            } else {
                Color::LightRed
            }),
        )));
    }
    let area = frame.area();
    let popup = centered_popup(
        area,
        area.width.saturating_sub(6).clamp(1, 86),
        area.height.saturating_sub(4).clamp(1, 14),
    );
    frame.render_widget(Clear, popup);
    let block = Block::default()
        .title(" Configure role ")
        .title_bottom(
            Line::from(" ↑/↓ or Tab field · ←/→ choice · type model · Enter save · Esc cancel ")
                .style(Style::default().fg(Color::LightCyan))
                .alignment(ratatui::layout::Alignment::Center),
        )
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(Color::LightCyan));
    let inner = block.inner(popup);
    frame.render_widget(block, popup);
    let columns = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(62), Constraint::Percentage(38)])
        .split(inner);
    frame.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: true })
            .style(Style::default().bg(Color::Black)),
        columns[0],
    );
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(Span::styled(
                format!("{} {}", role_icon(&agent.role), sanitize(&agent.name)),
                Style::default()
                    .fg(role_color(&agent.role))
                    .add_modifier(Modifier::BOLD),
            )),
            Line::from(""),
            Line::from(Span::styled(
                provider_option_label(provider),
                Style::default().fg(Color::LightMagenta),
            )),
            Line::from(Span::styled(
                sanitize(&app.model_config_input),
                Style::default().fg(Color::LightCyan),
            )),
            Line::from(""),
            Line::from(Span::styled(
                "Applies to the next task",
                Style::default().fg(Color::Gray),
            )),
        ])
        .wrap(Wrap { trim: true })
        .block(
            Block::default()
                .title(" Preview ")
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(role_color(&agent.role))),
        ),
        columns[1],
    );
}

fn role_config_line<'a>(label: &'a str, value: &'a str, selected: bool, choice: bool) -> Line<'a> {
    let value = if choice {
        format!("‹ {value} ›")
    } else {
        format!("{value}{}", if selected { "▏" } else { "" })
    };
    Line::from(vec![
        Span::styled(
            format!("  {label:<13}"),
            if selected {
                Style::default()
                    .fg(Color::LightCyan)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::DarkGray)
            },
        ),
        Span::styled(
            value,
            if selected {
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::Gray)
            },
        ),
    ])
}

fn provider_option_label(provider: &ProviderRouteOption) -> String {
    let base = match provider.kind.as_str() {
        "ollama" => "Ollama · local",
        "openai" => "OpenAI · cloud",
        "codex_cli" => "Codex · CLI",
        "claude_cli" => "Claude · CLI",
        "mock" => "Mock · local",
        value => value,
    };
    if provider.route == provider.kind {
        base.into()
    } else {
        format!("{base} · {}", provider.route)
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
                format!("{} {}", role.icon(), role.label()),
                if selected {
                    Style::default()
                        .fg(Color::LightCyan)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default()
                },
            )));
        }
    } else {
        for (index, agent) in app.agents.iter().enumerate() {
            let selected = index == app.team_adjustment_selected;
            let is_lead = Some(agent.id) == app.team_lead;
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
                    "[{}] {} · {}",
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
    }
    let area = frame.area();
    let item_count = if app.team_role_picker {
        ADDABLE_ROLES.len()
    } else {
        app.agents.len()
    };
    let popup = centered_popup(
        area,
        area.width.saturating_sub(6).clamp(1, 76),
        area.height
            .saturating_sub(6)
            .min(u16::try_from(item_count.saturating_add(9)).unwrap_or(u16::MAX))
            .max(1),
    );
    let (title, footer) = if app.team_role_picker {
        (
            " Add specialist · Team Lead remains coordinator ",
            " ↑/↓ choose · Enter/Space add · Esc back ",
        )
    } else {
        (
            " Adjust team · Team Lead remains required ",
            " n add · ↑/↓ select · Enter/Space toggle · t/Esc close ",
        )
    };
    frame.render_widget(Clear, popup);
    frame.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: true })
            .style(Style::default().bg(Color::Black))
            .block(
                Block::default()
                    .title(title)
                    .title_bottom(
                        Line::from(footer)
                            .style(Style::default().fg(Color::LightCyan))
                            .alignment(ratatui::layout::Alignment::Center),
                    )
                    .borders(Borders::ALL)
                    .border_type(BorderType::Rounded)
                    .border_style(Style::default().fg(Color::LightMagenta)),
            ),
        popup,
    );
}

fn render_approval_popup(frame: &mut ratatui::Frame<'_>, app: &AppModel, approval: &Approval) {
    let delegation = serde_json::from_str::<DelegationProposal>(&approval.reason)
        .ok()
        .filter(|_| approval.action == "task.delegate");
    let pending_count = app.pending_approval_count();
    let selected = app.approval_selected.min(pending_count.saturating_sub(1));
    let lead_name = app
        .team_lead
        .and_then(|id| app.agents.iter().find(|agent| agent.id == id))
        .map_or("Team Lead", |agent| agent.name.as_str());
    let task_owner = app
        .tasks
        .iter()
        .find(|task| task.id == approval.task_id)
        .and_then(|task| app.agent_for_owner(task.owner))
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
    let mut lines = vec![
        popup_section("ROUTE"),
        Line::from(format!("  {route}")),
        Line::from(""),
        popup_section("ACTION"),
        Line::from(Span::styled(
            format!("  {}", sanitize(&approval.action)),
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(Span::styled(
            format!("  Task  {}", approval.task_id),
            Style::default().fg(Color::Gray),
        )),
        Line::from(Span::styled(
            format!(
                "  Working directory  {}",
                sanitize(&approval.working_directory)
            ),
            Style::default().fg(Color::Gray),
        )),
        Line::from(""),
        popup_section("REQUEST"),
    ];
    if let Some(proposal) = delegation {
        lines.push(Line::from(vec![
            Span::styled("  Target: ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                sanitize(&proposal.target_agent),
                Style::default().add_modifier(Modifier::BOLD),
            ),
        ]));
        lines.push(Line::from("  Objective"));
        push_text_lines(&mut lines, &sanitize(&proposal.objective), "    ");
        if !proposal.acceptance_criteria.is_empty() {
            lines.push(Line::from("  Acceptance"));
            for criterion in proposal.acceptance_criteria {
                lines.push(Line::from(format!("    • {}", sanitize(&criterion))));
            }
        }
    } else {
        push_text_lines(&mut lines, &sanitize(&approval.reason), "  ");
    }
    if let Some(artifact) = app
        .artifacts
        .iter()
        .find(|artifact| artifact.producer == approval.task_id)
    {
        lines.push(Line::from(""));
        popup_section_into(&mut lines, "PREVIEW");
        lines.push(Line::from(Span::styled(
            format!(
                "  {} · {}",
                sanitize(&artifact.location),
                sanitize(&artifact.kind)
            ),
            Style::default().fg(Color::Gray),
        )));
        push_text_lines(&mut lines, &sanitize(&artifact.preview), "  ");
    }
    let area = frame.area();
    let popup = centered_popup(
        area,
        area.width.saturating_sub(4).clamp(1, 82),
        area.height.saturating_sub(4).clamp(1, 24),
    );
    let mut footer = vec![
        Span::styled(
            " [y] Approve",
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" · "),
        Span::styled(
            "[n] Deny ",
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        ),
    ];
    if pending_count > 1 {
        footer.push(Span::styled(
            "· ↑/↓ request ",
            Style::default().fg(Color::LightCyan),
        ));
    }
    frame.render_widget(Clear, popup);
    frame.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .style(Style::default().bg(Color::Black))
            .block(
                Block::default()
                    .title(format!(
                        " Approval {} of {pending_count} · User decision ",
                        selected.saturating_add(1)
                    ))
                    .title_bottom(Line::from(footer).alignment(ratatui::layout::Alignment::Center))
                    .borders(Borders::ALL)
                    .border_type(BorderType::Rounded)
                    .border_style(Style::default().fg(Color::Yellow)),
            ),
        popup,
    );
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
            EnableBracketedPaste,
            PushKeyboardEnhancementFlags(
                KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES
                    | KeyboardEnhancementFlags::REPORT_ALL_KEYS_AS_ESCAPE_CODES
            )
        )?;
        let old = panic::take_hook();
        panic::set_hook(Box::new(move |info| {
            let _ = disable_raw_mode();
            let _ = execute!(
                io::stdout(),
                PopKeyboardEnhancementFlags,
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
            PopKeyboardEnhancementFlags,
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
        assert_eq!(a.panel, Panel::Agents);
        a.reduce(Action::ToggleHelp);
        assert!(a.help)
    }
    #[test]
    fn keys_map_to_actions() {
        assert_eq!(action_for_key(KeyCode::Char('r')), Action::RetryTask);
        assert_eq!(action_for_key(KeyCode::Char('R')), Action::QuickRun);
        assert_eq!(action_for_key(KeyCode::Char('A')), Action::AddCredential);
        assert_eq!(action_for_key(KeyCode::Enter), Action::FocusComposer);
        assert_eq!(action_for_key(KeyCode::Char('t')), Action::AdjustTeam);
        assert_eq!(action_for_key(KeyCode::Char('/')), Action::None);
        assert_eq!(action_for_key(KeyCode::Char('c')), Action::ConfigureModel);
        assert_eq!(action_for_key(KeyCode::Char('x')), Action::Cancel);
        assert_eq!(action_for_key(KeyCode::Char('q')), Action::Quit);
        assert_eq!(action_for_key(KeyCode::Char('y')), Action::Approve);
        assert_eq!(action_for_key(KeyCode::Char('n')), Action::Deny);
        assert_eq!(action_for_key(KeyCode::Char('f')), Action::FitTeam);
        assert_eq!(action_for_key(KeyCode::Char('+')), Action::ZoomTeamIn);
        assert_eq!(action_for_key(KeyCode::Char('-')), Action::ZoomTeamOut);
        assert_eq!(action_for_key(KeyCode::Char('0')), Action::ResetTeamZoom);
        assert_eq!(action_for_key(KeyCode::Char('S')), Action::None);
        assert_eq!(action_for_key(KeyCode::Char('1')), Action::ShowTeam);
        assert_eq!(action_for_key(KeyCode::Char('2')), Action::ShowTasks);
        assert_eq!(action_for_key(KeyCode::Char('3')), Action::None);
        assert_eq!(action_for_key(KeyCode::Char('4')), Action::None);
        assert_eq!(action_for_key(KeyCode::Char('s')), Action::ShowTaskDetails);
        assert_eq!(action_for_key(KeyCode::Char('a')), Action::ShowRoleActivity);
        assert_eq!(action_for_key(KeyCode::Char('d')), Action::None);
        assert_eq!(action_for_key(KeyCode::Char('l')), Action::None);
    }

    #[test]
    fn quick_run_is_an_explicit_team_lead_workflow() {
        assert!(QUICK_RUN_OBJECTIVE.contains("Markdown"));
        assert!(QUICK_RUN_OBJECTIVE.contains("delegate"));
        assert!(QUICK_RUN_OBJECTIVE.contains("normal approval flow"));
        assert_eq!(QUICK_RUN_ACCEPTANCE.len(), 5);

        let mut app = AppModel::new(SessionId::new());
        assert!(!app.quick_run_active());
        app.quick_run_starting = true;
        assert!(app.quick_run_active());
        app.quick_run_starting = false;
        app.tasks.push(Task::root(
            AgentId::new(),
            QUICK_RUN_OBJECTIVE,
            QUICK_RUN_ACCEPTANCE
                .iter()
                .map(|criterion| (*criterion).into())
                .collect(),
            voff_core::TaskLimits::default(),
        ));
        assert!(app.quick_run_active());
        app.tasks[0].status = TaskStatus::Blocked;
        assert!(!app.quick_run_active());
    }

    #[test]
    fn terminal_input_forwarder_stops_after_its_receiver_closes() {
        let (input_tx, input_rx) = mpsc::channel(1);
        let mut input_rx = Some(input_rx);
        let mut polls = 0;

        forward_terminal_events(
            input_tx,
            |_| {
                polls += 1;
                drop(input_rx.take());
                Ok(false)
            },
            || panic!("closed input must not be read"),
        );

        assert_eq!(polls, 1);
    }

    #[test]
    fn restored_history_keeps_team_open_and_selects_the_newest_task() {
        let mut app = AppModel::new(SessionId::new());
        let owner = AgentId::new();
        let mut historical_owner = agent("Historical Reviewer", "code_reviewer", vec![]);
        historical_owner.id = owner;
        app.known_agents.insert(owner, historical_owner);
        let older = Task::root(
            owner,
            "older task",
            vec![],
            voff_core::TaskLimits::default(),
        );
        let mut newest = Task::root(
            owner,
            "verify restored details",
            vec!["saved result is visible".into()],
            voff_core::TaskLimits::default(),
        );
        newest.transition(TaskStatus::Ready).unwrap();
        newest.transition(TaskStatus::Running).unwrap();
        let evidence = voff_core::ArtifactId::new();
        newest
            .complete(voff_core::TaskResult {
                summary: "restored summary".into(),
                evidence: vec![evidence],
            })
            .unwrap();

        restore_initial_history(
            &mut app,
            vec![older, newest.clone()],
            vec![],
            vec![],
            vec![Artifact {
                id: evidence,
                producer: newest.id,
                kind: "test report".into(),
                location: "reports/restored.txt".into(),
                digest: "sha256:test".into(),
                preview: "all checks passed".into(),
            }],
        );

        assert_eq!(app.panel, Panel::Agents);
        assert_eq!(app.selected_task().map(|task| task.id), Some(newest.id));
        app.reduce(Action::ShowTasks);
        let backend = TestBackend::new(120, 36);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| render(frame, &mut app)).unwrap();
        let buffer = terminal.backend().buffer();
        let rendered = (0..buffer.area.height)
            .flat_map(|y| (0..buffer.area.width).map(move |x| buffer[(x, y)].symbol()))
            .collect::<String>();
        assert!(rendered.contains("restored summary"), "{rendered}");
        assert!(rendered.contains("Historical Reviewer"), "{rendered}");
        assert!(rendered.contains("code reviewer"), "{rendered}");
        assert!(rendered.contains("reports/restored.txt"), "{rendered}");
        assert!(rendered.contains("test report"), "{rendered}");
    }

    #[test]
    fn task_browser_keeps_the_latest_created_task_at_the_top() {
        let mut app = AppModel::new(SessionId::new());
        let owner = AgentId::new();
        let mut newest = Task::root(
            owner,
            "latest task marker",
            vec![],
            voff_core::TaskLimits::default(),
        );
        let mut older = Task::root(
            owner,
            "older task marker",
            vec![],
            voff_core::TaskLimits::default(),
        );
        older.created_at = newest.created_at - chrono::Duration::minutes(1);
        newest.updated_at = newest.created_at;
        newest.attempt = 2;
        newest.retry_of = Some(voff_core::TaskId::new());

        app.apply_event(EngineEvent::TaskUpdated(newest.clone()));
        app.apply_event(EngineEvent::TaskUpdated(older.clone()));

        assert_eq!(app.tasks[0].id, newest.id);
        assert_eq!(app.tasks[1].id, older.id);
        assert_eq!(app.selected_task().map(|task| task.id), Some(newest.id));
        app.panel = Panel::Tasks;
        app.reduce(Action::Down);
        assert_eq!(app.selected_task().map(|task| task.id), Some(older.id));
        app.reduce(Action::Up);
        assert_eq!(app.selected_task().map(|task| task.id), Some(newest.id));
        let backend = TestBackend::new(100, 28);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| render(frame, &mut app)).unwrap();
        let buffer = terminal.backend().buffer();
        let rendered = (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        let latest = rendered.find("latest task marker").unwrap();
        let older = rendered.find("older task marker").unwrap();
        assert!(latest < older, "{rendered}");
        assert!(
            rendered.contains("Tasks · lead first · newest peers"),
            "{rendered}"
        );
        assert!(rendered.contains("[r] re-run"), "{rendered}");
        assert!(rendered.contains("↻ attempt 2"), "{rendered}");
    }

    #[test]
    fn task_browser_indents_the_entire_card_by_delegation_depth() {
        let mut app = AppModel::new(SessionId::new());
        let lead = agent("Lead Coordinator", "technical_lead", vec![]);
        let frontend = agent("Frontend Developer", "frontend_developer", vec![]);
        let backend = agent("Backend Developer", "backend_developer", vec![]);
        let parent = Task::root(
            lead.id,
            "root objective",
            vec![],
            voff_core::TaskLimits::default(),
        );
        let mut child = Task::root(
            frontend.id,
            "child objective",
            vec![],
            voff_core::TaskLimits::default(),
        );
        child.parent = Some(parent.id);
        child.depth = 1;
        let mut grandchild = Task::root(
            backend.id,
            "grandchild objective",
            vec![],
            voff_core::TaskLimits::default(),
        );
        grandchild.parent = Some(child.id);
        grandchild.depth = 2;
        let parent_id = parent.id;
        let child_id = child.id;
        let grandchild_id = grandchild.id;
        app.team_lead = Some(lead.id);
        app.agents = vec![lead, frontend, backend];
        app.tasks = vec![grandchild, child, parent];
        app.sort_tasks_for_browser();
        assert_eq!(
            app.tasks.iter().map(|task| task.id).collect::<Vec<_>>(),
            vec![parent_id, child_id, grandchild_id]
        );
        app.selected = 0;
        app.panel = Panel::Tasks;

        let backend = TestBackend::new(120, 36);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| render(frame, &mut app)).unwrap();

        let buffer = terminal.backend().buffer();
        let rows = (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>();
        let position = |needle: &str| {
            rows.iter()
                .enumerate()
                .find_map(|(y, row)| {
                    row.find(needle)
                        .map(|byte_index| (row[..byte_index].chars().count(), y))
                })
                .unwrap_or_else(|| panic!("{needle} should be rendered"))
        };
        let lead_position = position("Lead Coordinator");
        let frontend_position = position("Frontend Developer");
        let backend_position = position("Backend Developer");
        assert_eq!(frontend_position.0, lead_position.0 + 2);
        assert_eq!(backend_position.0, lead_position.0 + 4);
        let icon_cell = |(x, y): (usize, usize)| {
            let cell = &buffer[((x - 2) as u16, y as u16)];
            (cell.symbol().to_owned(), cell.fg)
        };
        assert_eq!(icon_cell(lead_position), ("◆".into(), Color::Magenta));
        assert_eq!(icon_cell(frontend_position), ("▣".into(), Color::LightCyan));
        assert_eq!(icon_cell(backend_position), ("⬢".into(), Color::Blue));

        let root_objective = position("root objective");
        let child_objective = position("child objective");
        let grandchild_objective = position("grandchild objective");
        assert_eq!(child_objective.0, root_objective.0 + 2);
        assert_eq!(grandchild_objective.0, root_objective.0 + 4);
    }

    #[test]
    fn task_workspace_stacks_browser_and_inspector_on_narrow_terminals() {
        let mut app = AppModel::new(SessionId::new());
        let owner = agent("API Engineer", "backend_developer", vec![]);
        app.tasks.push(Task::root(
            owner.id,
            "Narrow terminal task",
            vec!["Readable at small widths".into()],
            voff_core::TaskLimits::default(),
        ));
        app.agents.push(owner);
        app.reduce(Action::ShowTasks);
        let backend = TestBackend::new(68, 28);
        let mut terminal = Terminal::new(backend).unwrap();

        terminal.draw(|frame| render(frame, &mut app)).unwrap();

        let buffer = terminal.backend().buffer();
        let rendered = (0..buffer.area.height)
            .flat_map(|y| (0..buffer.area.width).map(move |x| buffer[(x, y)].symbol()))
            .collect::<String>();
        assert!(rendered.contains("Narrow terminal task"), "{rendered}");
        assert!(rendered.contains("OBJECTIVE"), "{rendered}");
        assert!(rendered.contains("API Engineer"), "{rendered}");
        assert!(rendered.contains("· Tasks · session"), "{rendered}");
        assert!(!rendered.contains("Tasks view"), "{rendered}");
        assert!(rendered.contains("[s] summary"), "{rendered}");
        assert!(rendered.contains("[a] activity"), "{rendered}");
    }

    #[test]
    fn role_activity_key_filters_events_to_the_selected_tasks_owner() {
        let mut app = AppModel::new(SessionId::new());
        let backend = agent("Backend Developer", "backend_developer", vec![]);
        let frontend = agent("Frontend Developer", "frontend_developer", vec![]);
        let backend_task = Task::root(
            backend.id,
            "Implement API",
            vec![],
            voff_core::TaskLimits::default(),
        );
        let frontend_task = Task::root(
            frontend.id,
            "Implement UI",
            vec![],
            voff_core::TaskLimits::default(),
        );
        app.agents = vec![backend, frontend];
        app.tasks = vec![backend_task.clone(), frontend_task.clone()];
        for (task_id, message) in [
            (backend_task.id, "backend-event-visible"),
            (frontend_task.id, "frontend-event-hidden"),
        ] {
            app.apply_event(EngineEvent::Canonical(CanonicalEvent {
                id: EventId::new(),
                session_id: app.session,
                task_id: Some(task_id),
                created_at: chrono::Utc::now(),
                kind: CanonicalEventKind::ProviderFailed(message.into()),
            }));
        }
        app.reduce(action_for_key(KeyCode::Char('a')));

        let backend = TestBackend::new(120, 36);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| render(frame, &mut app)).unwrap();
        let buffer = terminal.backend().buffer();
        let rendered = (0..buffer.area.height)
            .flat_map(|y| (0..buffer.area.width).map(move |x| buffer[(x, y)].symbol()))
            .collect::<String>();
        assert!(rendered.contains("Backend Developer"), "{rendered}");
        assert!(rendered.contains("backend-event-visible"), "{rendered}");
        assert!(!rendered.contains("frontend-event-hidden"), "{rendered}");
    }

    #[test]
    fn task_events_are_flat_and_semantically_colored() {
        let mut app = AppModel::new(SessionId::new());
        let owner = AgentId::new();
        let parent = Task::root(
            owner,
            "Coordinate work",
            vec![],
            voff_core::TaskLimits::default(),
        );
        let mut child = Task::root(
            owner,
            "Delegated work",
            vec![],
            voff_core::TaskLimits::default(),
        );
        child.parent = Some(parent.id);
        child.depth = 1;
        app.tasks = vec![child.clone(), parent.clone()];

        let root_prefix = task_event_prefix(parent.id);
        let child_prefix = task_event_prefix(child.id);
        assert!(root_prefix.starts_with('['), "{root_prefix}");
        assert!(child_prefix.starts_with('['), "{child_prefix}");
        assert!(!root_prefix.starts_with(' '), "{root_prefix}");
        assert!(!child_prefix.starts_with(' '), "{child_prefix}");
        assert_eq!(event_text_color("tool proposed: file.patch"), Color::Yellow);
        assert_eq!(
            event_text_color("provider failed: timeout"),
            Color::LightRed
        );
        assert_eq!(event_text_color("provider completed"), Color::Green);
        assert_eq!(event_text_color("task created"), Color::LightCyan);
        assert_eq!(
            event_text_color("re-run attempt 2: abc -> def"),
            Color::LightCyan
        );
        assert_eq!(
            event_text_color("usage: 10 in / 2 out"),
            Color::LightMagenta
        );

        app.push_scoped_event(
            Some(child.id),
            &CanonicalEventKind::TaskDelegated {
                parent: parent.id,
                child: child.id,
                owner,
            },
        );
        app.event_viewer = true;
        let backend = TestBackend::new(100, 28);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| render_overlays(frame, &app)).unwrap();
        let buffer = terminal.backend().buffer();
        let rendered = (0..buffer.area.height)
            .flat_map(|y| (0..buffer.area.width).map(move |x| buffer[(x, y)].symbol()))
            .collect::<String>();
        assert!(
            rendered.contains("Events · live · newest first"),
            "{rendered}"
        );
        assert!(!rendered.contains("└─"), "{rendered}");
        assert!(rendered.contains("delegated"), "{rendered}");
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
    fn team_lists_highlight_text_without_selector_glyphs() {
        let mut app = AppModel::new(SessionId::new());
        let lead = agent("Team Lead", "technical_lead", vec!["Backend".into()]);
        app.team_lead = Some(lead.id);
        app.agents = vec![lead, agent("Backend", "backend_developer", vec![])];
        app.reduce(Action::AdjustTeam);
        let backend = TestBackend::new(90, 24);
        let mut terminal = Terminal::new(backend).unwrap();

        terminal.draw(|frame| render_overlays(frame, &app)).unwrap();

        let buffer = terminal.backend().buffer();
        let rendered = (0..buffer.area.height)
            .flat_map(|y| (0..buffer.area.width).map(move |x| buffer[(x, y)].symbol()))
            .collect::<String>();
        assert!(!rendered.contains('›'), "{rendered}");
        assert!(
            rendered.contains("[on ] Team Lead · required"),
            "{rendered}"
        );
    }

    #[test]
    fn selected_roles_can_change_configured_provider_and_model() {
        let mut app = AppModel::new(SessionId::new());
        let mut lead = agent("Team Lead", "technical_lead", vec![]);
        lead.provider = "local-codex".into();
        lead.model = "default".into();
        app.agent_provider_kinds.insert(lead.id, "codex_cli".into());
        app.provider_routes = vec![
            ProviderRouteOption {
                route: "local-codex".into(),
                kind: "codex_cli".into(),
            },
            ProviderRouteOption {
                route: "cloud".into(),
                kind: "openai".into(),
            },
        ];
        app.agents.push(lead.clone());

        app.reduce(Action::ConfigureModel);
        assert!(app.model_config_open);
        assert_eq!(app.model_config_field, RoleConfigField::Provider);
        assert_eq!(app.model_config_input, "default");
        assert_eq!(app.model_config_selected, Some(0));

        let backend = TestBackend::new(100, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| render_overlays(frame, &app)).unwrap();
        let buffer = terminal.backend().buffer();
        let rendered = (0..buffer.area.height)
            .flat_map(|y| (0..buffer.area.width).map(move |x| buffer[(x, y)].symbol()))
            .collect::<String>();
        assert!(rendered.contains("Configure role"), "{rendered}");
        assert!(rendered.contains("Provider"), "{rendered}");
        assert!(rendered.contains("Codex · CLI"), "{rendered}");
        assert!(rendered.contains("Model"), "{rendered}");
        assert!(rendered.contains("Preview"), "{rendered}");

        let right = Event::Key(crossterm::event::KeyEvent::new(
            KeyCode::Right,
            KeyModifiers::NONE,
        ));
        assert!(matches!(
            app.handle_model_config_event(&right),
            ModelConfigEvent::Consumed
        ));
        assert_eq!(app.model_config_provider, 1);
        assert_eq!(app.model_config_input, "gpt-5-mini");
        let down = Event::Key(crossterm::event::KeyEvent::new(
            KeyCode::Down,
            KeyModifiers::NONE,
        ));
        app.handle_model_config_event(&down);
        assert_eq!(app.model_config_field, RoleConfigField::Model);
        assert!(matches!(
            app.handle_model_config_event(&Event::Paste("gpt-5.4".into())),
            ModelConfigEvent::Consumed
        ));
        let enter = Event::Key(crossterm::event::KeyEvent::new(
            KeyCode::Enter,
            KeyModifiers::NONE,
        ));
        let ModelConfigEvent::Save(updated) = app.handle_model_config_event(&enter) else {
            panic!("a valid custom model should be persisted");
        };
        assert_eq!(updated.id, lead.id);
        assert_eq!(updated.provider, "cloud");
        assert_eq!(updated.model, "gpt-5.4");
        assert!(app.model_config_saving);
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
    fn textarea_submits_a_task_to_the_team_lead_flow() {
        let mut app = AppModel::new(SessionId::new());
        app.composer_active = true;
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
    fn multiline_composer_grows_and_supports_modern_undo_redo() {
        let mut app = AppModel::new(SessionId::new());
        app.composer_active = true;
        let area = Rect::new(0, 0, 100, 24);
        assert_eq!(composer_height(&app, area), 3);

        app.handle_composer_event(&Event::Paste("first line".into()));
        let newline = Event::Key(crossterm::event::KeyEvent::new(
            KeyCode::Enter,
            KeyModifiers::SHIFT,
        ));
        assert_eq!(app.handle_composer_event(&newline), ComposerEvent::Consumed);
        assert_eq!(app.composer.lines(), ["first line", ""]);
        assert_eq!(composer_height(&app, area), 4);

        let fallback_newline = Event::Key(crossterm::event::KeyEvent::new(
            KeyCode::Char('j'),
            KeyModifiers::CONTROL,
        ));
        assert_eq!(
            app.handle_composer_event(&fallback_newline),
            ComposerEvent::Consumed
        );
        assert_eq!(app.composer.lines(), ["first line", "", ""]);
        assert_eq!(composer_height(&app, area), 5);

        let typed = Event::Key(crossterm::event::KeyEvent::new(
            KeyCode::Char('x'),
            KeyModifiers::NONE,
        ));
        app.handle_composer_event(&typed);
        let undo = Event::Key(crossterm::event::KeyEvent::new(
            KeyCode::Char('z'),
            KeyModifiers::CONTROL,
        ));
        app.handle_composer_event(&undo);
        assert_eq!(app.composer.lines(), ["first line", "", ""]);
        let redo = Event::Key(crossterm::event::KeyEvent::new(
            KeyCode::Char('y'),
            KeyModifiers::CONTROL,
        ));
        app.handle_composer_event(&redo);
        assert_eq!(app.composer.lines(), ["first line", "", "x"]);
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
    fn slash_prefixed_text_is_submitted_verbatim_as_a_task() {
        let mut app = AppModel::new(SessionId::new());
        app.panel = Panel::Agents;
        app.tasks.push(Task::root(
            AgentId::new(),
            "current work",
            vec![],
            voff_core::TaskLimits::default(),
        ));
        app.reduce(Action::FocusComposer);
        assert_eq!(app.composer.lines(), [""]);
        app.handle_composer_event(&Event::Paste("/tasks".into()));
        let enter = Event::Key(crossterm::event::KeyEvent::new(
            KeyCode::Enter,
            KeyModifiers::NONE,
        ));
        assert_eq!(
            app.handle_composer_event(&enter),
            ComposerEvent::Submit("/tasks".into())
        );

        assert_eq!(app.panel, Panel::Agents);
        assert!(app.composer_notice.is_none());
    }

    #[test]
    fn enter_focuses_an_empty_composer_and_escape_quits() {
        let mut app = AppModel::new(SessionId::new());

        app.reduce(Action::FocusComposer);

        assert!(app.composer_active);
        assert_eq!(app.composer.lines(), [""]);
        let escape = Event::Key(crossterm::event::KeyEvent::new(
            KeyCode::Esc,
            KeyModifiers::NONE,
        ));
        assert_eq!(app.handle_composer_event(&escape), ComposerEvent::Consumed);
        assert!(app.quit);
        assert!(!app.composer_active);
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
        app.panel = Panel::Tasks;
        app.events = vec!["a sufficiently long event message for scrolling".into(); 20];
        app.reduce(Action::ToggleEvents);
        assert!(app.event_viewer);
        assert_eq!(app.panel, Panel::Tasks);
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
        app.reduce(Action::ToggleEvents);
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
        assert!(rendered.contains("Model · qwen-test"));
        assert!(rendered.contains("4096 token limit"));
        assert!(!rendered.contains("no delegation"));
        assert!(rendered.contains("[1 Team] [2 Tasks]"), "{rendered}");
        assert!(!rendered.contains("[R Quick Run]"), "{rendered}");
        assert!(!rendered.contains("[3 Events]"), "{rendered}");
        assert!(rendered.contains("· Team · session"), "{rendered}");
        assert!(!rendered.contains("Team view"), "{rendered}");
        assert_eq!(app.team_flow.edges().len(), 1);
        assert!(
            app.team_flow
                .edges()
                .iter()
                .all(|edge| edge.label.is_none())
        );
        let composer = (buffer.area.height.saturating_sub(3)..buffer.area.height)
            .flat_map(|y| (0..buffer.area.width).map(move |x| buffer[(x, y)].symbol()))
            .collect::<String>();
        assert!(composer.contains("Team Lead"), "{composer}");
        assert!(
            composer.contains("Press Enter to assign a task"),
            "{composer}"
        );
        assert!(!composer.contains("/task assign"), "{composer}");
        assert!(!composer.contains("/tasks browse"), "{composer}");
        assert!(composer.contains("[R] Quick Run"), "{composer}");
        assert!(!composer.contains("1/2/3 views"), "{composer}");
        let bottom_border = (0..buffer.area.width)
            .map(|x| buffer[(x, buffer.area.height - 1)].symbol())
            .collect::<String>();
        assert!(bottom_border.contains("? help · cwd"), "{bottom_border}");
        let hint_x = (0..buffer.area.width.saturating_sub(2))
            .find(|x| {
                buffer[(*x, buffer.area.height - 1)].symbol() == "["
                    && buffer[(x.saturating_add(1), buffer.area.height - 1)].symbol() == "R"
            })
            .expect("Quick Run should be visible on the bottom border");
        assert!(
            hint_x.abs_diff(buffer.area.width / 2) <= 10,
            "{bottom_border}"
        );
        assert_eq!(buffer[(hint_x, buffer.area.height - 1)].fg, Color::White);
        assert_eq!(
            buffer[(hint_x + 1, buffer.area.height - 1)].fg,
            Color::White
        );
        assert_eq!(
            buffer[(hint_x + 2, buffer.area.height - 1)].fg,
            Color::White
        );
        assert_eq!(
            buffer[(hint_x + 4, buffer.area.height - 1)].fg,
            Color::White
        );
        let cwd = bottom_border
            .find("cwd /workspaces/voff-demo")
            .expect("cwd should be visible on the bottom border");
        assert!(cwd > usize::from(buffer.area.width / 2), "{bottom_border}");
    }

    #[test]
    fn role_cards_render_one_icon_labeled_identity_inside_the_border() {
        let mut app = AppModel::new(SessionId::new());
        app.agents = vec![agent("Backend Developer", "backend_developer", vec![])];
        let backend = TestBackend::new(80, 20);
        let mut terminal = Terminal::new(backend).unwrap();

        terminal.draw(|frame| render(frame, &mut app)).unwrap();

        let buffer = terminal.backend().buffer();
        let rendered = (0..buffer.area.height)
            .flat_map(|y| (0..buffer.area.width).map(move |x| buffer[(x, y)].symbol()))
            .collect::<String>();
        assert!(rendered.contains("⬢ Backend Developer"), "{rendered}");
        assert_eq!(
            rendered.matches("Backend Developer").count(),
            1,
            "{rendered}"
        );
    }

    #[test]
    fn role_cards_explain_cli_automatic_and_show_latest_provider_text() {
        let mut app = AppModel::new(SessionId::new());
        let mut lead = agent("Team Lead", "technical_lead", vec![]);
        lead.provider = "local-codex".into();
        lead.model = "default".into();
        app.team_lead = Some(lead.id);
        app.agent_provider_kinds.insert(lead.id, "codex_cli".into());
        let task = Task::root(
            lead.id,
            "Inspect the workspace",
            vec![],
            voff_core::TaskLimits::default(),
        );
        app.agents.push(lead);
        app.tasks.push(task.clone());
        app.push_scoped_event(Some(task.id), &CanonicalEventKind::ProviderStarted);
        app.push_scoped_event(
            Some(task.id),
            &CanonicalEventKind::TextDelta("Reviewing the current Rust modules".into()),
        );
        let backend = TestBackend::new(100, 24);
        let mut terminal = Terminal::new(backend).unwrap();

        terminal.draw(|frame| render(frame, &mut app)).unwrap();

        let buffer = terminal.backend().buffer();
        let rendered = (0..buffer.area.height)
            .flat_map(|y| (0..buffer.area.width).map(move |x| buffer[(x, y)].symbol()))
            .collect::<String>();
        assert!(rendered.contains("Model · CLI automatic"), "{rendered}");
        assert!(
            rendered.contains("Reviewing the current Rust"),
            "{rendered}"
        );
        assert!(!rendered.contains("Model · default"), "{rendered}");
    }

    #[test]
    fn composer_hint_stays_task_focused_after_enter() {
        let mut app = AppModel::new(SessionId::new());
        let backend = TestBackend::new(100, 24);
        let mut terminal = Terminal::new(backend).unwrap();

        app.reduce(Action::FocusComposer);
        terminal.draw(|frame| render(frame, &mut app)).unwrap();

        let buffer = terminal.backend().buffer();
        let composer = (buffer.area.height.saturating_sub(3)..buffer.area.height)
            .flat_map(|y| (0..buffer.area.width).map(move |x| buffer[(x, y)].symbol()))
            .collect::<String>();
        assert!(
            !composer.contains("Press Enter to assign a task"),
            "{composer}"
        );
        assert!(
            composer.contains("Describe the task for the Team Lead"),
            "{composer}"
        );
        assert!(!composer.contains("/task"), "{composer}");
        assert!(!composer.contains("/tasks"), "{composer}");
        let cursor = (
            app.composer_hitbox.x.saturating_add(1),
            app.composer_hitbox.y.saturating_add(1),
        );
        assert_eq!(buffer[cursor].bg, Color::LightCyan);
        assert_eq!(buffer[cursor].fg, Color::Black);
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
    fn approval_border_keeps_decisions_visible_and_highlights_the_action() {
        let mut app = AppModel::new(SessionId::new());
        let owner = AgentId::new();
        let task = Task::root(
            owner,
            "Apply a reviewed change",
            vec!["Patch is inspectable".into()],
            voff_core::TaskLimits::default(),
        );
        app.approvals.push(Approval {
            id: voff_core::ApprovalId::new(),
            task_id: task.id,
            tool_call_id: voff_core::ToolCallId::new(),
            action: "file.patch".into(),
            working_directory: "/workspaces/voff".into(),
            reason: "Apply the proposed file changes after reviewing the preview. ".repeat(8),
            decision: ApprovalDecision::Pending,
            created_at: chrono::Utc::now(),
        });
        app.artifacts.push(Artifact {
            id: voff_core::ArtifactId::new(),
            producer: task.id,
            kind: "patch".into(),
            location: "changes.diff".into(),
            digest: "sha256:test".into(),
            preview: "+ a deliberately long preview line\n".repeat(30),
        });
        app.tasks.push(task);

        let backend = TestBackend::new(44, 16);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| render_overlays(frame, &app)).unwrap();

        let buffer = terminal.backend().buffer();
        let mut rendered = String::new();
        let mut action_position = None;
        for y in 0..buffer.area.height {
            let row = (0..buffer.area.width)
                .map(|x| buffer[(x, y)].symbol())
                .collect::<String>();
            if let Some(x) = row.find("file.patch") {
                action_position = Some((u16::try_from(x).unwrap(), y));
            }
            rendered.push_str(&row);
        }
        assert!(rendered.contains("[y] Approve"), "{rendered}");
        assert!(rendered.contains("[n] Deny"), "{rendered}");
        let action_position = action_position.expect("action should be visible");
        assert_eq!(buffer[action_position].fg, Color::Yellow);
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
        assert!(rendered.contains("⬢ Backend Developer"), "{rendered}");
        assert!(rendered.contains("Linus"), "{rendered}");
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
