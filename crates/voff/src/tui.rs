//! Responsive Ratatui shell and pure action reducer.
mod task_layout;
mod team_flow;

use crate::model_catalog::ModelCatalog;
use crate::theme;
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
    text::{Line, Span, Text},
    widgets::{
        Block, BorderType, Borders, Clear, List, ListItem, Paragraph, Scrollbar,
        ScrollbarOrientation, ScrollbarState, Wrap,
    },
};
use ratatui_notifications::{
    Anchor, Animation, AutoDismiss, Level, Notification, Notifications, SizeConstraint,
};
use ratatui_textarea::{DataCursor, TextArea, WrapMode};
use std::{
    collections::{HashMap, HashSet, VecDeque},
    io, panic,
    path::PathBuf,
    sync::Arc,
    time::Duration,
};
use thiserror::Error;
use tokio::sync::mpsc;
use unicode_width::UnicodeWidthStr;
use voff_core::{
    AgentId, AgentProfile, Approval, ApprovalDecision, Artifact, CanonicalEvent,
    CanonicalEventKind, SessionId, Task, TaskStatus,
};
use voff_engine::{
    DelegationProposal, Engine, EngineEvent, OutputContinuation, is_conversational_request,
};
use voff_prompts::workflow::routed_agent_objective;
use voff_storage::{PROMPT_HISTORY_LIMIT, Repository, SqliteRepository};

use self::team_flow::{
    TeamFlow, TeamSnapshot, TokenUsage, compact_count, estimate_cost, format_usd, handoff_summary,
    role_color, role_icon,
};
use tui_piechart::{PieChart, PieSlice};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Panel {
    Tasks,
    Agents,
}
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
enum TabId {
    Team,
    Tasks,
    Role(AgentId),
}

#[derive(Clone, Copy, Debug)]
struct TabHitbox {
    id: TabId,
    area: Rect,
    close: Option<Rect>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Action {
    Up,
    Down,
    NextTab,
    PreviousTab,
    ToggleTaskFocus,
    ToggleTaskPane,
    ToggleHelp,
    ToggleUsage,
    FocusComposer,
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
    MoveSelectedAgent(i8, i8),
    ResizeTasks(i8),
    ResetTaskLayout,
    AdjustTeam,
    ChangeWorkspace,
    AddAgent,
    OpenSelectedAgentTab,
    DeleteAgent,
    ShowTeam,
    ShowTasks,
    CloseTab,
    ToggleExpanded,
    ToggleApprovalDetails,
    Escape,
    Quit,
    Mouse(MouseEvent),
    None,
}

#[derive(Clone, Debug)]
struct ScopedEvent {
    task_id: Option<voff_core::TaskId>,
    text: String,
    is_output: bool,
    headline: Option<&'static str>,
    related_task: Option<voff_core::TaskId>,
    source_task: Option<voff_core::TaskId>,
    accent: Color,
    approval_id: Option<voff_core::ApprovalId>,
    tool_call_id: Option<voff_core::ToolCallId>,
    review: Option<ReviewContext>,
    conversational: bool,
    speaker: Option<AgentId>,
    details_only: bool,
}

#[derive(Clone, Debug)]
struct ReviewContext {
    reviewer: AgentId,
    decision: Option<(bool, String)>,
}

#[derive(Debug, Eq, PartialEq)]
enum ComposerEvent {
    Ignored,
    Consumed,
    Submit(String),
}

pub struct AppModel {
    notifications: Notifications,
    transient_notification_ids: VecDeque<u64>,
    automatic_review: bool,
    lead_reviews: HashMap<voff_core::TaskId, String>,
    application_notice: Option<String>,
    pub session: SessionId,
    pub tasks: Vec<Task>,
    pub events: Vec<String>,
    event_task_ids: Vec<Option<voff_core::TaskId>>,
    pub selected: usize,
    pub panel: Panel,
    pub help: bool,
    usage_viewer: bool,
    usage_scroll: u16,
    pub quit: bool,
    pub agents: Vec<AgentProfile>,
    known_agents: HashMap<AgentId, AgentProfile>,
    pub approvals: Vec<Approval>,
    pub artifacts: Vec<Artifact>,
    pub model_catalog: ModelCatalog,
    pub agent_provider_kinds: HashMap<AgentId, String>,
    agent_runtime_models: HashMap<AgentId, String>,
    agent_estimated_costs: HashMap<AgentId, Option<f64>>,
    pub workspace_root: PathBuf,
    pub event_viewer: bool,
    pub event_scroll: u16,
    event_follow: bool,
    scoped_events: Vec<ScopedEvent>,
    task_inspector_scroll: u16,
    expanded_output: bool,
    task_list_hitbox: Rect,
    task_layout: task_layout::TaskLayout,
    task_inspector_hitbox: Rect,
    task_pane_hitbox: Rect,
    task_pane_visible: bool,
    task_pane_focused: bool,
    task_pane_visible_before_tasks_tab: bool,
    task_pane_focused_before_tasks_tab: bool,
    team_tab_hitbox: Rect,
    tasks_tab_hitbox: Rect,
    tab_hitboxes: Vec<TabHitbox>,
    add_role_tab_hitbox: Rect,
    role_tabs: Vec<AgentId>,
    active_role_tab: Option<AgentId>,
    role_click_candidate: Option<(usize, u16, u16)>,
    last_role_click: Option<(AgentId, std::time::Instant)>,
    role_scroll: u16,
    role_requests: Vec<(AgentId, String)>,
    team_edit_hitbox: Rect,
    team_add_hitbox: Rect,
    task_expand_hitbox: Rect,
    task_retry_hitbox: Rect,
    help_hitbox: Rect,
    workspace_hitbox: Rect,
    task_window_start: usize,
    credential_target: Option<(String, crate::credential::CredentialSlot)>,
    credential_input: Option<String>,
    credential_saving: bool,
    credential_error: Option<String>,
    composer: TextArea<'static>,
    composer_drafts: HashMap<TabId, TextArea<'static>>,
    composer_history: Vec<String>,
    composer_history_index: Option<usize>,
    composer_history_draft: Option<String>,
    composer_active: bool,
    composer_hitbox: Rect,
    retry_starting: bool,
    pending_submissions: usize,
    selected_agent: usize,
    approval_selected: usize,
    approval_details: bool,
    approval_scroll: u16,
    team_lead: Option<AgentId>,
    team_setup: Option<crate::onboarding::SetupDraft>,
    pending_delete: Option<AgentId>,
    setup_editor: Option<crate::onboarding::Wizard>,
    setup_result: Option<crate::onboarding::SetupDraft>,
    setup_notice: Option<String>,
    startup_scan_pending: bool,
    startup_scan_summary: bool,
    startup_setup_prompt: String,
    startup_popup_size: Option<(u16, u16)>,
    startup_scan_result: Option<Result<(crate::onboarding::SetupDraft, String), String>>,
    startup_scan_frame: usize,
    startup_prompt_scroll: u16,
    startup_scan_error: Option<String>,
    workspace_editor: Option<crate::onboarding::Wizard>,
    workspace_result: Option<PathBuf>,
    team_flow: TeamFlow,
    streaming_text: bool,
    agent_responses: HashMap<AgentId, String>,
    agent_usage: HashMap<AgentId, TokenUsage>,
}
impl AppModel {
    #[must_use]
    pub fn new(session: SessionId) -> Self {
        let composer = new_composer();
        Self {
            notifications: Notifications::new(),
            transient_notification_ids: VecDeque::new(),
            session,
            tasks: vec![],
            events: vec![],
            event_task_ids: vec![],
            selected: 0,
            panel: Panel::Agents,
            help: false,
            usage_viewer: false,
            usage_scroll: 0,
            quit: false,
            agents: vec![],
            known_agents: HashMap::new(),
            approvals: vec![],
            automatic_review: false,
            lead_reviews: HashMap::new(),
            application_notice: None,
            artifacts: vec![],
            model_catalog: ModelCatalog::default(),
            agent_provider_kinds: HashMap::new(),
            agent_runtime_models: HashMap::new(),
            agent_estimated_costs: HashMap::new(),
            workspace_root: ".".into(),
            event_viewer: false,
            event_scroll: 0,
            event_follow: true,
            scoped_events: vec![],
            task_inspector_scroll: 0,
            expanded_output: false,
            task_list_hitbox: Rect::default(),
            task_layout: task_layout::TaskLayout::default(),
            task_inspector_hitbox: Rect::default(),
            task_pane_hitbox: Rect::default(),
            task_pane_visible: true,
            task_pane_focused: false,
            task_pane_visible_before_tasks_tab: true,
            task_pane_focused_before_tasks_tab: false,
            team_tab_hitbox: Rect::default(),
            tasks_tab_hitbox: Rect::default(),
            tab_hitboxes: Vec::new(),
            add_role_tab_hitbox: Rect::default(),
            role_tabs: Vec::new(),
            active_role_tab: None,
            role_click_candidate: None,
            last_role_click: None,
            role_scroll: 0,
            role_requests: Vec::new(),
            team_edit_hitbox: Rect::default(),
            team_add_hitbox: Rect::default(),
            task_expand_hitbox: Rect::default(),
            task_retry_hitbox: Rect::default(),
            help_hitbox: Rect::default(),
            workspace_hitbox: Rect::default(),
            task_window_start: 0,
            credential_target: None,
            credential_input: None,
            credential_saving: false,
            credential_error: None,
            composer,
            composer_drafts: HashMap::new(),
            composer_history: Vec::new(),
            composer_history_index: None,
            composer_history_draft: None,
            composer_active: false,
            composer_hitbox: Rect::default(),
            retry_starting: false,
            pending_submissions: 0,
            selected_agent: 0,
            approval_selected: 0,
            approval_details: false,
            approval_scroll: 0,
            team_lead: None,
            team_setup: None,
            pending_delete: None,
            setup_editor: None,
            setup_result: None,
            setup_notice: None,
            startup_scan_pending: false,
            startup_scan_summary: false,
            startup_setup_prompt: String::new(),
            startup_popup_size: None,
            startup_scan_result: None,
            startup_scan_frame: 0,
            startup_prompt_scroll: 0,
            startup_scan_error: None,
            workspace_editor: None,
            workspace_result: None,
            team_flow: TeamFlow::default(),
            streaming_text: false,
            agent_responses: HashMap::new(),
            agent_usage: HashMap::new(),
        }
    }

    fn active_tab(&self) -> TabId {
        self.active_role_tab.map_or_else(
            || match self.panel {
                Panel::Agents => TabId::Team,
                Panel::Tasks => TabId::Tasks,
            },
            TabId::Role,
        )
    }

    fn select_tab(&mut self, tab: TabId) {
        if matches!(tab, TabId::Role(id) if !self.role_tabs.contains(&id)) {
            return;
        }
        let current = self.active_tab();
        if tab == TabId::Tasks {
            if current != TabId::Tasks {
                self.task_pane_visible_before_tasks_tab = self.task_pane_visible;
                self.task_pane_focused_before_tasks_tab = self.task_pane_focused;
            }
            self.task_pane_visible = false;
        } else if current == TabId::Tasks {
            self.task_pane_visible = self.task_pane_visible_before_tasks_tab;
            self.task_pane_focused = self.task_pane_focused_before_tasks_tab;
        }
        if current != tab {
            self.reset_composer_history_navigation();
            self.task_layout.cancel_drag();
            self.task_inspector_scroll = 0;
            self.notifications.clear();
            self.transient_notification_ids.clear();
            let next = self
                .composer_drafts
                .remove(&tab)
                .unwrap_or_else(new_composer);
            let previous = std::mem::replace(&mut self.composer, next);
            self.composer_drafts.insert(current, previous);
        }
        self.composer_active = false;
        if current != TabId::Tasks || tab == TabId::Tasks {
            self.task_pane_focused = false;
        }
        match tab {
            TabId::Team => {
                self.panel = Panel::Agents;
                self.active_role_tab = None;
            }
            TabId::Tasks => {
                self.panel = Panel::Tasks;
                self.active_role_tab = None;
            }
            TabId::Role(id) => {
                self.active_role_tab = Some(id);
                self.role_scroll = 0;
            }
        }
    }

    fn cycle_tab(&mut self, offset: isize) {
        let mut tabs = vec![TabId::Team, TabId::Tasks];
        tabs.extend(
            self.role_tabs
                .iter()
                .copied()
                .filter(|id| self.agents.iter().any(|agent| agent.id == *id))
                .map(TabId::Role),
        );
        let current = tabs
            .iter()
            .position(|tab| *tab == self.active_tab())
            .unwrap_or(0);
        let next = (current as isize + offset).rem_euclid(tabs.len() as isize) as usize;
        self.select_tab(tabs[next]);
    }

    fn open_role_tab(&mut self, id: AgentId) {
        let Some(index) = self.agents.iter().position(|agent| agent.id == id) else {
            return;
        };
        self.selected_agent = index;
        if !self.role_tabs.contains(&id) {
            self.role_tabs.push(id);
        }
        self.select_tab(TabId::Role(id));
    }

    fn close_role_tab(&mut self, id: AgentId) {
        self.role_tabs.retain(|open| *open != id);
        if self.active_role_tab == Some(id) {
            self.select_tab(TabId::Team);
        }
    }

    fn composer_target(&self) -> Option<&AgentProfile> {
        self.active_role_tab
            .and_then(|id| self.agents.iter().find(|agent| agent.id == id))
    }

    fn root_agent(&self) -> Option<&AgentProfile> {
        self.team_lead
            .and_then(|id| self.agents.iter().find(|agent| agent.id == id))
            .or_else(|| self.agents.first())
    }

    fn routed_objective(&self, message: &str) -> String {
        self.composer_target().map_or_else(
            || message.to_owned(),
            |agent| {
                if Some(agent.id) == self.team_lead {
                    message.to_owned()
                } else {
                    routed_agent_objective(&agent.name, &agent.role, message)
                }
            },
        )
    }

    pub fn reduce(&mut self, action: Action) {
        if action == Action::Escape && self.application_notice.take().is_some() {
            return;
        }
        if self.navigate_approvals(action) {
            return;
        }
        match action {
            Action::ResizeTasks(_) | Action::ResetTaskLayout => {
                if self.active_tab() == TabId::Tasks
                    && !self.task_pane_visible
                    && !self.help
                    && !self.event_viewer
                    && !self.composer_active
                    && self.pending_approval().is_none()
                {
                    if let Action::ResizeTasks(steps) = action {
                        self.task_layout.adjust(steps);
                    } else {
                        self.task_layout.reset();
                    }
                }
            }
            Action::Up => {
                if self.usage_viewer {
                    self.usage_scroll = self.usage_scroll.saturating_sub(1);
                } else if self.event_viewer {
                    self.event_scroll = self.event_scroll.saturating_sub(1);
                    self.event_follow = self.event_scroll == 0;
                } else if self.task_pane_focused {
                    self.selected = self.selected.saturating_sub(1);
                    self.task_inspector_scroll = 0;
                } else if self.active_role_tab.is_some() {
                    self.role_scroll = self.role_scroll.saturating_add(1);
                } else if self.panel == Panel::Tasks && self.task_pane_visible {
                    self.task_inspector_scroll = self.task_inspector_scroll.saturating_add(1);
                } else if self.panel == Panel::Agents {
                    self.selected_agent = self.selected_agent.saturating_sub(1);
                } else {
                    self.selected = self.selected.saturating_sub(1);
                    self.task_inspector_scroll = 0;
                }
            }
            Action::Down => {
                if self.usage_viewer {
                    self.usage_scroll = self.usage_scroll.saturating_add(1);
                } else if self.event_viewer {
                    self.event_scroll = self
                        .event_scroll
                        .saturating_add(1)
                        .min(self.event_scroll_limit());
                    self.event_follow = self.event_scroll == 0;
                } else if self.task_pane_focused {
                    if self.selected + 1 < self.tasks.len() {
                        self.selected += 1;
                    }
                    self.task_inspector_scroll = 0;
                } else if self.active_role_tab.is_some() {
                    self.role_scroll = self.role_scroll.saturating_sub(1);
                } else if self.panel == Panel::Tasks && self.task_pane_visible {
                    self.task_inspector_scroll = self.task_inspector_scroll.saturating_sub(1);
                } else if self.panel == Panel::Agents {
                    if self.selected_agent + 1 < self.agents.len() {
                        self.selected_agent += 1;
                    }
                } else if self.selected + 1 < self.tasks.len() {
                    self.selected += 1;
                    self.task_inspector_scroll = 0;
                }
            }
            Action::NextTab => self.cycle_tab(1),
            Action::PreviousTab => self.cycle_tab(-1),
            Action::PageUp => {
                if self.usage_viewer {
                    self.usage_scroll = self.usage_scroll.saturating_sub(8);
                } else if self.event_viewer {
                    self.event_scroll = self.event_scroll.saturating_sub(10);
                    self.event_follow = self.event_scroll == 0;
                } else if self.task_pane_focused {
                    self.selected = self.selected.saturating_sub(10);
                    self.task_inspector_scroll = 0;
                } else if self.active_role_tab.is_some() {
                    self.role_scroll = self.role_scroll.saturating_add(10);
                } else if self.panel == Panel::Tasks {
                    self.task_inspector_scroll = self.task_inspector_scroll.saturating_add(10);
                }
            }
            Action::PageDown => {
                if self.usage_viewer {
                    self.usage_scroll = self.usage_scroll.saturating_add(8);
                } else if self.event_viewer {
                    self.event_scroll = self
                        .event_scroll
                        .saturating_add(10)
                        .min(self.event_scroll_limit());
                    self.event_follow = self.event_scroll == 0;
                } else if self.task_pane_focused {
                    self.selected = self
                        .selected
                        .saturating_add(10)
                        .min(self.tasks.len().saturating_sub(1));
                    self.task_inspector_scroll = 0;
                } else if self.active_role_tab.is_some() {
                    self.role_scroll = self.role_scroll.saturating_sub(10);
                } else if self.panel == Panel::Tasks {
                    self.task_inspector_scroll = self.task_inspector_scroll.saturating_sub(10);
                }
            }
            Action::FitTeam if self.panel == Panel::Agents => self.team_flow.fit_view(),
            Action::ZoomTeamIn if self.panel == Panel::Agents => self.team_flow.zoom_in(),
            Action::ZoomTeamOut if self.panel == Panel::Agents => self.team_flow.zoom_out(),
            Action::ResetTeamZoom if self.panel == Panel::Agents => self.team_flow.reset_zoom(),
            Action::MoveSelectedAgent(dx, dy)
                if self.panel == Panel::Agents
                    && self.active_role_tab.is_none()
                    && !self.task_pane_focused
                    && !self.help
                    && !self.event_viewer
                    && !self.usage_viewer
                    && !self.composer_active
                    && self.pending_approval().is_none()
                    && self.credential_input.is_none()
                    && self.application_notice.is_none() =>
            {
                self.team_flow.move_node(self.selected_agent, dx, dy);
            }
            Action::AdjustTeam => {
                if let Some(id) = self.active_role_tab {
                    self.close_role_tab(id);
                } else {
                    self.open_team_setup();
                }
            }
            Action::ChangeWorkspace => self.open_workspace_picker(),
            Action::AddAgent => {
                if self.panel == Panel::Agents || self.active_role_tab.is_some() {
                    self.open_agent_setup(true);
                }
            }
            Action::OpenSelectedAgentTab => {
                if self.panel == Panel::Agents
                    && self.active_role_tab.is_none()
                    && let Some(id) = self.agents.get(self.selected_agent).map(|agent| agent.id)
                {
                    self.open_role_tab(id);
                }
            }
            Action::DeleteAgent => self.open_delete_confirmation(),
            Action::ShowTeam => self.select_tab(TabId::Team),
            Action::ShowTasks => self.select_tab(TabId::Tasks),
            Action::ToggleTaskFocus => {
                if self.panel == Panel::Agents
                    && self.active_role_tab.is_none()
                    && self.task_pane_visible
                {
                    self.task_pane_focused = !self.task_pane_focused;
                }
            }
            Action::ToggleTaskPane => {
                if self.active_role_tab.is_none() {
                    self.task_pane_visible = !self.task_pane_visible;
                    self.task_pane_focused = self.task_pane_visible;
                    if self.task_pane_visible {
                        self.task_layout.cancel_drag();
                    }
                }
            }
            Action::CloseTab => {
                if let Some(id) = self.active_role_tab {
                    self.close_role_tab(id);
                }
            }
            Action::ToggleExpanded => {
                self.expanded_output = !self.expanded_output;
                self.task_inspector_scroll = 0;
                self.role_scroll = 0;
                self.event_scroll = 0;
            }
            Action::ToggleHelp => {
                self.help = !self.help;
                if self.help {
                    self.usage_viewer = false;
                }
            }
            Action::FocusComposer => {
                self.composer_active = true;
            }
            Action::AddCredential => {
                if self.credential_target.is_some() {
                    self.credential_input = Some(String::new());
                    self.credential_error = None;
                    self.composer_active = false;
                } else {
                    self.warn("This provider has no API token to edit. Check the agent's provider settings or local CLI login.");
                }
            }
            Action::ToggleEvents => {
                self.event_viewer = !self.event_viewer;
                if self.event_viewer {
                    self.usage_viewer = false;
                    self.follow_newest_event();
                }
            }
            Action::ToggleUsage => {
                if self.usage_viewer {
                    self.usage_viewer = false;
                } else if self.pending_approval().is_none() {
                    self.usage_viewer = true;
                    self.help = false;
                    self.event_viewer = false;
                    self.usage_scroll = 0;
                }
            }
            Action::Escape => {
                if self.usage_viewer {
                    self.usage_viewer = false;
                } else if self.event_viewer {
                    self.event_viewer = false;
                } else if self.help {
                    self.help = false;
                } else {
                    self.quit = true;
                }
            }
            Action::Quit => self.quit = true,
            Action::Mouse(mouse) => self.handle_mouse(mouse),
            Action::ToggleApprovalDetails
            | Action::RetryTask
            | Action::Cancel
            | Action::Approve
            | Action::Deny
            | Action::FitTeam
            | Action::ZoomTeamIn
            | Action::ZoomTeamOut
            | Action::ResetTeamZoom
            | Action::MoveSelectedAgent(_, _)
            | Action::None => {}
        }
    }

    fn handle_mouse(&mut self, mouse: MouseEvent) {
        if self.help || self.event_viewer {
            return;
        }
        if matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left)) {
            let point = (mouse.column, mouse.row).into();
            if let Some(hitbox) = self
                .tab_hitboxes
                .iter()
                .copied()
                .find(|tab| tab.area.contains(point))
            {
                if hitbox.close.is_some_and(|close| close.contains(point)) {
                    if let TabId::Role(id) = hitbox.id {
                        self.close_role_tab(id);
                    }
                } else if hitbox.id == TabId::Team {
                    self.reduce(Action::ShowTeam);
                } else {
                    self.select_tab(hitbox.id);
                }
                return;
            }
            if self.add_role_tab_hitbox.contains(point) {
                let id = self.agents.get(self.selected_agent).map(|agent| agent.id);
                if let Some(id) = id {
                    self.open_role_tab(id);
                }
                return;
            }
        }
        if matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left))
            && self
                .composer_hitbox
                .contains((mouse.column, mouse.row).into())
        {
            self.composer_active = true;
            return;
        }
        if matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left)) {
            self.composer_active = false;
        }
        let point = (mouse.column, mouse.row).into();
        if self.task_pane_visible && self.task_pane_hitbox.contains(point) {
            match mouse.kind {
                MouseEventKind::Down(MouseButton::Left)
                    if self.task_list_hitbox.contains(point) =>
                {
                    self.task_pane_focused = true;
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
                    self.task_pane_focused = true;
                    self.selected = self.selected.saturating_sub(1);
                    self.task_inspector_scroll = 0;
                    return;
                }
                MouseEventKind::ScrollDown => {
                    self.task_pane_focused = true;
                    if self.selected + 1 < self.tasks.len() {
                        self.selected += 1;
                    }
                    self.task_inspector_scroll = 0;
                    return;
                }
                _ => {}
            }
        }
        if self.task_pane_visible && matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left)) {
            self.task_pane_focused = false;
        }
        if self.active_role_tab.is_some() {
            match mouse.kind {
                MouseEventKind::ScrollUp => self.role_scroll = self.role_scroll.saturating_add(3),
                MouseEventKind::ScrollDown => self.role_scroll = self.role_scroll.saturating_sub(3),
                _ => {}
            }
            return;
        }
        if self.panel == Panel::Tasks {
            if !self.task_pane_visible && self.task_layout.handle_mouse(mouse) {
                self.composer_active = false;
                return;
            }
            let over_inspector = self
                .task_inspector_hitbox
                .contains((mouse.column, mouse.row).into());
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
                    if over_inspector {
                        self.task_inspector_scroll = self.task_inspector_scroll.saturating_add(3);
                    } else {
                        self.selected = self.selected.saturating_sub(1);
                        self.task_inspector_scroll = 0;
                    }
                    return;
                }
                MouseEventKind::ScrollDown => {
                    if over_inspector {
                        self.task_inspector_scroll = self.task_inspector_scroll.saturating_sub(3);
                    } else if self.selected + 1 < self.tasks.len() {
                        self.selected += 1;
                        self.task_inspector_scroll = 0;
                    }
                    return;
                }
                _ => return,
            }
        }
        if matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left)) {
            self.role_click_candidate = self
                .team_flow
                .node_at(mouse.column, mouse.row)
                .map(|index| (index, mouse.column, mouse.row));
        }
        let interaction = self.team_flow.handle_mouse(mouse);
        if interaction.handled {
            self.panel = Panel::Agents;
            if let Some(index) = interaction.selected {
                self.selected_agent = index;
            }
            if matches!(mouse.kind, MouseEventKind::Drag(MouseButton::Left)) {
                self.role_click_candidate = None;
                self.last_role_click = None;
            }
            if matches!(mouse.kind, MouseEventKind::Up(MouseButton::Left))
                && let Some((index, x, y)) = self.role_click_candidate.take()
                && x == mouse.column
                && y == mouse.row
                && let Some(id) = self.agents.get(index).map(|agent| agent.id)
            {
                let now = std::time::Instant::now();
                let double_click = self.last_role_click.is_some_and(|(previous, at)| {
                    previous == id && now.duration_since(at) <= Duration::from_millis(500)
                });
                self.last_role_click = Some((id, now));
                if double_click {
                    self.last_role_click = None;
                    self.open_role_tab(id);
                }
            }
        }
    }

    fn open_team_setup(&mut self) {
        self.open_agent_setup(false);
    }

    fn team_edit_blocked(&self) -> bool {
        has_unsettled_tasks(&self.tasks)
            || self.retry_starting
            || self.pending_submissions > 0
            || self
                .approvals
                .iter()
                .any(|approval| approval.decision == ApprovalDecision::Pending)
            || !self.lead_reviews.is_empty()
    }

    fn open_delete_confirmation(&mut self) {
        if (self.panel != Panel::Agents && self.active_role_tab.is_none())
            || self.help
            || self.event_viewer
        {
            return;
        }
        if self.team_edit_blocked() {
            self.warn("Finish or cancel pending work before editing the team.");
            return;
        }
        let Some(draft) = &self.team_setup else {
            self.warn("This team cannot be edited safely.");
            return;
        };
        let index = self
            .active_role_tab
            .and_then(|id| self.agents.iter().position(|agent| agent.id == id))
            .unwrap_or(self.selected_agent);
        let Some(agent) = self.agents.get(index) else {
            return;
        };
        if Some(agent.id) == self.team_lead || index == 0 {
            self.warn("The Team Lead cannot be deleted.");
        } else if draft
            .developers
            .iter()
            .any(|developer| developer.reports_to == Some(index))
        {
            self.warn("Reassign this agent's direct reports before deleting it.");
        } else if draft.developers.get(index).is_some() {
            self.pending_delete = Some(agent.id);
            self.composer_active = false;
        }
    }

    fn handle_delete_event(&mut self, event: &Event, area: Rect) -> bool {
        let Some(id) = self.pending_delete else {
            return false;
        };
        let decision = match event {
            Event::Key(key) if key.kind == KeyEventKind::Press => match key.code {
                KeyCode::Char('y') => Some(true),
                KeyCode::Char('n') | KeyCode::Esc => Some(false),
                _ => None,
            },
            Event::Mouse(mouse) if mouse.kind == MouseEventKind::Down(MouseButton::Left) => {
                let popup = delete_confirmation_area(area);
                if popup.contains((mouse.column, mouse.row).into())
                    && mouse.row == popup.bottom().saturating_sub(1)
                {
                    Some(mouse.column < popup.x + popup.width / 2)
                } else {
                    None
                }
            }
            _ => None,
        };
        let Some(confirm) = decision else {
            return true;
        };
        self.pending_delete = None;
        if !confirm {
            return true;
        }
        let Some(index) = self.agents.iter().position(|agent| agent.id == id) else {
            return true;
        };
        let Some(mut draft) = self.team_setup.clone() else {
            return true;
        };
        if index == 0
            || Some(id) == self.team_lead
            || draft.developers.get(index).is_none()
            || draft
                .developers
                .iter()
                .any(|developer| developer.reports_to == Some(index))
            || self.team_edit_blocked()
        {
            self.warn("The team changed; deletion was cancelled.");
            return true;
        }
        let removed = draft.developers.remove(index).name;
        for developer in &mut draft.developers {
            if let Some(parent) = developer.reports_to.as_mut()
                && *parent > index
            {
                *parent -= 1;
            }
        }
        self.setup_notice = Some(format!("{removed} removed from the team"));
        self.setup_result = Some(draft);
        self.quit = true;
        true
    }

    fn open_workspace_picker(&mut self) {
        if self.team_edit_blocked() {
            self.warn("Finish or cancel pending work before changing workspaces.");
            return;
        }
        self.workspace_editor = Some(crate::onboarding::Wizard::workspace(&self.workspace_root));
    }

    fn open_agent_setup(&mut self, adding: bool) {
        if has_unsettled_tasks(&self.tasks)
            || self.retry_starting
            || self.pending_submissions > 0
            || self
                .approvals
                .iter()
                .any(|approval| approval.decision == ApprovalDecision::Pending)
            || !self.lead_reviews.is_empty()
        {
            self.warn("Finish or cancel pending work before editing the team.");
            return;
        }
        let Some(mut draft) = self.team_setup.clone() else {
            self.warn("This custom team cannot be represented safely in guided setup.");
            return;
        };
        for (developer, agent) in draft.developers.iter_mut().zip(&self.agents) {
            developer.model.clone_from(&agent.model);
            developer.max_output_tokens = agent.max_output_tokens;
        }
        draft.working_directory = self.workspace_root.clone();
        let selected = self
            .active_role_tab
            .and_then(|id| self.agents.iter().position(|agent| agent.id == id))
            .unwrap_or(self.selected_agent)
            .min(draft.developers.len().saturating_sub(1));
        self.setup_editor = Some(if adding {
            crate::onboarding::Wizard::add_agent(self.model_catalog.clone(), draft, selected)
        } else {
            crate::onboarding::Wizard::configure_agent(self.model_catalog.clone(), draft, selected)
        });
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

    fn reset_composer_history_navigation(&mut self) {
        self.composer_history_index = None;
        self.composer_history_draft = None;
    }

    fn remember_composer_prompt(&mut self, prompt: &str) {
        if self
            .composer_history
            .last()
            .is_none_or(|previous| previous != prompt)
        {
            self.composer_history.push(prompt.to_owned());
            if self.composer_history.len() > PROMPT_HISTORY_LIMIT {
                self.composer_history.remove(0);
            }
        }
        self.reset_composer_history_navigation();
    }

    fn set_composer_text(&mut self, text: &str) {
        self.composer.clear();
        self.composer.insert_str(text);
    }

    fn navigate_composer_history(&mut self, older: bool) -> bool {
        if self.composer_history.is_empty() {
            return false;
        }

        if self.composer_history_index.is_none() {
            if !older {
                return false;
            }
            let DataCursor(line, _) = self.composer.cursor();
            if line != 0 {
                return false;
            }
        }

        if older {
            let index = self.composer_history_index.map_or_else(
                || {
                    self.composer_history_draft = Some(self.composer.lines().join("\n"));
                    self.composer_history.len() - 1
                },
                |index| index.saturating_sub(1),
            );
            self.composer_history_index = Some(index);
            self.set_composer_text(&self.composer_history[index].clone());
        } else if let Some(index) = self.composer_history_index {
            if index + 1 < self.composer_history.len() {
                let next = index + 1;
                self.composer_history_index = Some(next);
                self.set_composer_text(&self.composer_history[next].clone());
            } else {
                self.composer_history_index = None;
                let draft = self.composer_history_draft.take().unwrap_or_default();
                self.set_composer_text(&draft);
            }
        }
        true
    }

    fn handle_composer_event(&mut self, event: &Event) -> ComposerEvent {
        if self.application_notice.is_some()
            && matches!(event, Event::Key(key) if matches!(key.code, KeyCode::Char('A' | 'e')))
        {
            return ComposerEvent::Ignored;
        }
        if self.application_notice.is_some()
            && matches!(event, Event::Key(key) if key.code == KeyCode::Esc)
        {
            self.application_notice = None;
            return ComposerEvent::Consumed;
        }
        if self.pending_approval().is_some() {
            return ComposerEvent::Ignored;
        }
        if !self.composer_active {
            return ComposerEvent::Ignored;
        }
        match event {
            Event::Paste(value) => {
                self.reset_composer_history_navigation();
                self.composer.insert_str(value);
                ComposerEvent::Consumed
            }
            Event::Key(key) if key.kind == KeyEventKind::Press => match key.code {
                KeyCode::Esc => {
                    self.reset_composer_history_navigation();
                    self.composer_active = false;
                    self.quit = true;
                    ComposerEvent::Consumed
                }
                KeyCode::Enter
                    if key
                        .modifiers
                        .intersects(KeyModifiers::SHIFT | KeyModifiers::ALT) =>
                {
                    self.reset_composer_history_navigation();
                    self.composer.insert_newline();
                    ComposerEvent::Consumed
                }
                KeyCode::Char('j') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    self.reset_composer_history_navigation();
                    self.composer.insert_newline();
                    ComposerEvent::Consumed
                }
                KeyCode::Enter => {
                    let message = self.composer.lines().join("\n").trim().to_owned();
                    if message.is_empty() {
                        self.warn("Describe a task first.");
                        ComposerEvent::Consumed
                    } else if message.len() > 8_000 {
                        self.warn("Keep messages under 8,000 bytes.");
                        ComposerEvent::Consumed
                    } else {
                        self.remember_composer_prompt(&message);
                        self.composer.clear();
                        self.composer_active = false;
                        ComposerEvent::Submit(message)
                    }
                }
                KeyCode::Char('z') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    self.reset_composer_history_navigation();
                    self.composer.undo();
                    ComposerEvent::Consumed
                }
                KeyCode::Char('y') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    self.reset_composer_history_navigation();
                    self.composer.redo();
                    ComposerEvent::Consumed
                }
                KeyCode::Char('w') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    ComposerEvent::Ignored
                }
                KeyCode::Up => {
                    if !self.navigate_composer_history(true) {
                        self.composer.input(*key);
                    }
                    ComposerEvent::Consumed
                }
                KeyCode::Down => {
                    if !self.navigate_composer_history(false) {
                        self.composer.input(*key);
                    }
                    ComposerEvent::Consumed
                }
                KeyCode::Tab | KeyCode::BackTab => ComposerEvent::Ignored,
                _ => {
                    self.reset_composer_history_navigation();
                    self.composer.input(*key);
                    ComposerEvent::Consumed
                }
            },
            Event::Mouse(mouse)
                if self.help_hitbox.contains((mouse.column, mouse.row).into())
                    || self
                        .workspace_hitbox
                        .contains((mouse.column, mouse.row).into()) =>
            {
                ComposerEvent::Ignored
            }
            Event::Mouse(mouse)
                if self
                    .composer_hitbox
                    .contains((mouse.column, mouse.row).into()) =>
            {
                self.reset_composer_history_navigation();
                self.composer.input(*mouse);
                ComposerEvent::Consumed
            }
            Event::Mouse(_) => ComposerEvent::Ignored,
            _ => ComposerEvent::Consumed,
        }
    }

    fn action_for_mouse(&self, mouse: MouseEvent, area: Rect) -> Action {
        if self.pending_approval().is_none() {
            if self.usage_viewer {
                let popup = usage_popup_area(area);
                return if matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left))
                    && !popup.contains((mouse.column, mouse.row).into())
                {
                    Action::ToggleUsage
                } else {
                    Action::None
                };
            }
            if self.help || self.event_viewer {
                let popup = if self.help {
                    help_popup_area(area)
                } else {
                    centered_popup(
                        area,
                        area.width.saturating_sub(4).min(100),
                        area.height.saturating_sub(4).min(32),
                    )
                };
                if matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left))
                    && (!popup.contains((mouse.column, mouse.row).into())
                        || mouse.row == popup.bottom().saturating_sub(1))
                {
                    return if self.help {
                        Action::ToggleHelp
                    } else {
                        Action::ToggleEvents
                    };
                }
                return Action::None;
            }
            if self.credential_input.is_some() || self.application_notice.is_some() {
                return Action::None;
            }
            if matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left)) {
                let point = (mouse.column, mouse.row).into();
                if self.help_hitbox.contains(point) {
                    return Action::ToggleHelp;
                }
                if self.workspace_hitbox.contains(point) {
                    return Action::ChangeWorkspace;
                }
                if self.panel == Panel::Agents && self.active_role_tab.is_none() {
                    if self.team_edit_hitbox.contains(point) {
                        return Action::AdjustTeam;
                    }
                    if self.team_add_hitbox.contains(point) {
                        return Action::AddAgent;
                    }
                } else {
                    if self.task_expand_hitbox.contains(point) {
                        return Action::ToggleExpanded;
                    }
                    if self.task_retry_hitbox.contains(point) {
                        return Action::RetryTask;
                    }
                }
            }
            return Action::Mouse(mouse);
        }
        if !matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left)) {
            return Action::None;
        }
        let popup = if self.approval_details {
            centered_popup(
                area,
                area.width.saturating_sub(2).max(1),
                area.height.saturating_sub(2).max(1),
            )
        } else {
            compact_approval_area(area)
        };
        let point = (mouse.column, mouse.row).into();
        if !popup.contains(point) {
            return Action::None;
        }
        if mouse.row != popup.bottom().saturating_sub(1) {
            return if self.approval_details {
                Action::None
            } else {
                Action::ToggleApprovalDetails
            };
        }
        let offset = mouse.column.saturating_sub(popup.x.saturating_add(1));
        let (approve_end, deny_end, detail_end) = if self.approval_details {
            (14, 27, 47)
        } else {
            (5, 9, 24)
        };
        if offset < approve_end {
            Action::Approve
        } else if offset < deny_end {
            Action::Deny
        } else if offset < detail_end {
            Action::ToggleApprovalDetails
        } else {
            Action::None
        }
    }

    pub fn apply_event(&mut self, event: EngineEvent) {
        match event {
            EngineEvent::TaskUpdated(task) => {
                let previous_status = self
                    .tasks
                    .iter()
                    .find(|existing| existing.id == task.id)
                    .map(|existing| existing.status);
                let is_new = previous_status.is_none();
                if previous_status != Some(task.status) {
                    self.notify_task_status(&task);
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
                if let Some(task_id) = e.task_id {
                    match &e.kind {
                        CanonicalEventKind::LeadReviewStarted { approval_id, .. } => {
                            self.lead_reviews.insert(
                                task_id,
                                if approval_id.is_some() {
                                    "Reviewing proposal"
                                } else {
                                    "Diagnosing failure"
                                }
                                .into(),
                            );
                        }
                        CanonicalEventKind::LeadReviewFinished {
                            lead,
                            approved,
                            reason,
                            ..
                        } => {
                            self.lead_reviews.remove(&task_id);
                            self.agent_responses.insert(
                                *lead,
                                format!(
                                    "{}: {}",
                                    if *approved { "Approved" } else { "Rejected" },
                                    sanitize(reason)
                                ),
                            );
                            let reviewer = self
                                .agents
                                .iter()
                                .find(|agent| agent.id == *lead)
                                .map_or("Reporting agent", |agent| agent.name.as_str());
                            let reviewer = truncate_text(&one_line(&sanitize(reviewer)), 24);
                            let review = format!(
                                "{reviewer} {}: {}",
                                if *approved { "approved" } else { "rejected" },
                                sanitize(reason)
                            );
                            if *approved {
                                let _ = self.notify(
                                    &format!("{reviewer} approved"),
                                    &truncate_text(&one_line(&sanitize(reason)), 100),
                                    Level::Info,
                                    theme::GREEN,
                                    false,
                                );
                            } else {
                                self.warn(&review);
                            }
                        }
                        CanonicalEventKind::RevisionRequested { .. } => {
                            let reviewer = self
                                .tasks
                                .iter()
                                .find(|task| task.id == task_id)
                                .and_then(|task| task.parent)
                                .and_then(|parent| self.tasks.iter().find(|task| task.id == parent))
                                .and_then(|parent| {
                                    self.agents.iter().find(|agent| agent.id == parent.owner)
                                })
                                .map_or("Team Lead", |agent| agent.name.as_str());
                            let reviewer = truncate_text(&one_line(&sanitize(reviewer)), 24);
                            let _ = self.notify(
                                "Revision requested",
                                &format!("{reviewer} returned feedback; agent revising"),
                                Level::Info,
                                theme::AQUA,
                                false,
                            );
                        }
                        CanonicalEventKind::RevisionStopped(reason) => {
                            self.warn(&format!("Revision stopped: {}", sanitize(reason)));
                        }
                        CanonicalEventKind::ApplicationIssue(message) => {
                            self.application_notice = Some(sanitize(message));
                        }
                        CanonicalEventKind::ProviderWarning(message) => {
                            self.warn(&format!("Provider warning: {}", sanitize(message)));
                        }
                        _ => {}
                    }
                }
                self.push_scoped_event(e.task_id, &e.kind)
            }
            EngineEvent::ApprovalUpdated(approval) => {
                if approval.decision != ApprovalDecision::Pending
                    && self.pending_approval().is_some_and(|a| a.id == approval.id)
                {
                    self.approval_details = false;
                    self.approval_scroll = 0;
                }
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
            EngineEvent::ProcessOutput { task_id, text } => {
                self.push_process_output(task_id, &text);
            }
            EngineEvent::ProviderHealth(_) => {}
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

    fn notify(
        &mut self,
        title: &str,
        message: &str,
        level: Level,
        accent: Color,
        persistent: bool,
    ) -> Option<u64> {
        let mut lines = vec![Line::from(Span::styled(
            title.to_owned(),
            Style::default().fg(accent).add_modifier(Modifier::BOLD),
        ))];
        lines.extend(message.lines().map(|line| Line::from(line.to_owned())));
        let content = Text::from(lines);
        let notification = Notification::new(content)
            .level(level)
            .anchor(Anchor::TopRight)
            .margin(3)
            .animation(Animation::Fade)
            .auto_dismiss(if persistent {
                AutoDismiss::Never
            } else {
                AutoDismiss::After(Duration::from_secs(5))
            })
            .max_size(SizeConstraint::Absolute(48), SizeConstraint::Absolute(5))
            .style(theme::surface())
            .border_style(Style::default().fg(accent))
            .title_style(
                Style::default()
                    .fg(theme::TEXT)
                    .add_modifier(Modifier::BOLD),
            )
            .build();
        let id = notification
            .ok()
            .and_then(|notification| self.notifications.add(notification).ok());
        if let Some(id) = id
            && !persistent
        {
            self.transient_notification_ids.push_back(id);
            if self.transient_notification_ids.len() > 2
                && let Some(oldest) = self.transient_notification_ids.pop_front()
            {
                self.notifications.remove(oldest);
            }
        }
        id
    }

    fn warn(&mut self, message: &str) {
        let _ = self.notify(
            "Action unavailable",
            message,
            Level::Warn,
            theme::YELLOW,
            false,
        );
    }

    fn notify_task_status(&mut self, task: &Task) {
        let (title, level, accent, action) = match task.status {
            TaskStatus::Completed => ("Task completed", Level::Info, theme::GREEN, "Review"),
            TaskStatus::Failed => ("Task failed", Level::Error, theme::RED, "Inspect"),
            TaskStatus::Blocked => ("Task blocked", Level::Warn, theme::YELLOW, "Inspect"),
            TaskStatus::Cancelled => ("Task cancelled", Level::Warn, theme::MUTED, "Inspect"),
            _ => return,
        };
        let role = self
            .agent_for_owner(task.owner)
            .map_or("Agent", |agent| agent.name.as_str());
        let role = sanitize(role).replace(['\n', '\t'], " ");
        let _ = self.notify(
            title,
            &format!("{} · {} · {action} in Tasks", role, short_task_id(task.id)),
            level,
            accent,
            false,
        );
    }
    fn pending_approval(&self) -> Option<&Approval> {
        let pending_count = self.pending_approval_count();
        let selected = self.approval_selected.min(pending_count.saturating_sub(1));
        self.approvals
            .iter()
            .filter(|approval| {
                approval.decision == ApprovalDecision::Pending
                    && (!self.automatic_review || approval.action == "provider.continue")
            })
            .nth(selected)
    }
    fn pending_approval_count(&self) -> usize {
        self.approvals
            .iter()
            .filter(|approval| {
                approval.decision == ApprovalDecision::Pending
                    && (!self.automatic_review || approval.action == "provider.continue")
            })
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
                self.approval_scroll = 0;
                true
            }
            Action::Down => {
                self.approval_selected = (self.approval_selected + 1).min(count - 1);
                self.approval_scroll = 0;
                true
            }
            Action::ToggleApprovalDetails => {
                self.approval_details = !self.approval_details;
                self.approval_scroll = 0;
                true
            }
            Action::Escape if self.approval_details => {
                self.approval_details = false;
                self.approval_scroll = 0;
                true
            }
            Action::PageUp if self.approval_details => {
                self.approval_scroll = self.approval_scroll.saturating_sub(10);
                true
            }
            Action::PageDown if self.approval_details => {
                self.approval_scroll = self.approval_scroll.saturating_add(10);
                true
            }
            _ => false,
        }
    }
    #[cfg(test)]
    fn push_event(&mut self, kind: &CanonicalEventKind) {
        self.push_scoped_event(None, kind);
    }

    fn push_process_output(&mut self, task_id: voff_core::TaskId, text: &str) {
        const MAX_LIVE_OUTPUT_BYTES: usize = 64 * 1024;
        if text.is_empty() {
            return;
        }
        let last_task_event = self
            .scoped_events
            .iter()
            .rposition(|event| event.task_id == Some(task_id));
        if let Some(index) = last_task_event.filter(|&index| {
            self.scoped_events[index].is_output
                && self.scoped_events[index].headline == Some("Test output")
        }) {
            let event = &mut self.scoped_events[index];
            event.text.push_str(text);
            trim_tail(&mut event.text, MAX_LIVE_OUTPUT_BYTES);
        } else {
            let mut output = text.to_owned();
            trim_tail(&mut output, MAX_LIVE_OUTPUT_BYTES);
            self.scoped_events.push(ScopedEvent {
                task_id: Some(task_id),
                text: output,
                is_output: true,
                headline: Some("Test output"),
                related_task: None,
                source_task: None,
                accent: theme::TEXT,
                approval_id: None,
                tool_call_id: None,
                review: None,
                conversational: true,
                speaker: None,
                details_only: false,
            });
        }
        self.on_events_changed();
    }

    fn push_scoped_event(&mut self, task_id: Option<voff_core::TaskId>, kind: &CanonicalEventKind) {
        let reviewer = task_id
            .and_then(|id| self.tasks.iter().find(|task| task.id == id))
            .and_then(|task| task.parent)
            .and_then(|parent| self.tasks.iter().find(|task| task.id == parent))
            .and_then(|parent| self.agents.iter().find(|agent| agent.id == parent.owner))
            .map(|agent| agent.name.clone());
        let description = format_event(kind);
        let description = reviewer.as_deref().map_or_else(
            || description.clone(),
            |name| description.replace("Team Lead", name),
        );
        if let Some(owner) = task_id.and_then(|id| {
            self.tasks
                .iter()
                .find(|task| task.id == id)
                .map(|task| task.owner)
        }) {
            match kind {
                CanonicalEventKind::ProviderStarted
                | CanonicalEventKind::ProviderRetrying { .. } => {
                    self.agent_responses.insert(owner, String::new());
                }
                CanonicalEventKind::TextDelta(delta)
                | CanonicalEventKind::GenerationDelta(delta) => {
                    let response = self.agent_responses.entry(owner).or_default();
                    response.push_str(&sanitize(delta));
                    trim_response_buffer(response);
                }
                CanonicalEventKind::Usage {
                    input_tokens,
                    cached_input_tokens,
                    output_tokens,
                    model,
                } => {
                    let usage = self.agent_usage.entry(owner).or_default();
                    usage.input_tokens = usage.input_tokens.saturating_add(*input_tokens);
                    usage.cached_input_tokens = usage
                        .cached_input_tokens
                        .saturating_add(*cached_input_tokens);
                    usage.output_tokens = usage.output_tokens.saturating_add(*output_tokens);
                    if let Some(model) = model.as_ref().filter(|model| !model.trim().is_empty()) {
                        self.agent_runtime_models.insert(owner, model.clone());
                    }
                    let provider = self
                        .agent_provider_kinds
                        .get(&owner)
                        .map(String::as_str)
                        .or_else(|| {
                            self.agents
                                .iter()
                                .chain(self.known_agents.values())
                                .find(|agent| agent.id == owner)
                                .map(|agent| agent.provider.as_str())
                        })
                        .unwrap_or_default();
                    let effective_model = model
                        .as_deref()
                        .filter(|model| !model.trim().is_empty())
                        .or_else(|| self.agent_runtime_models.get(&owner).map(String::as_str))
                        .or_else(|| {
                            self.agents
                                .iter()
                                .chain(self.known_agents.values())
                                .find(|agent| agent.id == owner)
                                .map(|agent| agent.model.as_str())
                        })
                        .unwrap_or_default();
                    // Re-estimate the agent's accumulated usage with its latest
                    // resolved model. Older persisted events may not have model
                    // metadata, so they can only be priced once one is known.
                    let estimate = estimate_cost(
                        provider,
                        effective_model,
                        self.agent_usage.get(&owner).copied().unwrap_or_default(),
                    );
                    self.agent_estimated_costs.insert(owner, estimate);
                }
                _ => {}
            }
        }
        self.event_task_ids.resize(self.events.len(), None);
        let was_streaming = self.streaming_text
            && self
                .event_task_ids
                .last()
                .is_some_and(|last| *last == task_id);
        if let CanonicalEventKind::TextDelta(delta) | CanonicalEventKind::GenerationDelta(delta) =
            kind
        {
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
            self.events.push(description.clone());
            self.event_task_ids.push(task_id);
        }
        if let CanonicalEventKind::TextDelta(delta) | CanonicalEventKind::GenerationDelta(delta) =
            kind
        {
            let clean = sanitize(delta);
            if was_streaming
                && self.scoped_events.last().is_some_and(|event| {
                    event.task_id == task_id
                        && event.details_only
                            == matches!(kind, CanonicalEventKind::GenerationDelta(_))
                })
            {
                if let Some(last) = self.scoped_events.last_mut() {
                    last.text.push_str(&clean);
                }
            } else {
                self.scoped_events.push(ScopedEvent {
                    task_id,
                    text: clean,
                    is_output: true,
                    headline: Some("Reply"),
                    related_task: None,
                    source_task: None,
                    accent: theme::BLUE,
                    approval_id: None,
                    tool_call_id: None,
                    review: None,
                    conversational: true,
                    speaker: None,
                    details_only: matches!(kind, CanonicalEventKind::GenerationDelta(_)),
                });
            }
        } else {
            self.scoped_events.push(ScopedEvent {
                task_id,
                text: match kind {
                    CanonicalEventKind::RevisionRequested { feedback, .. } => {
                        format!("Revise and resubmit: {}", sanitize(feedback))
                    }
                    _ => description,
                },
                is_output: false,
                details_only: matches!(
                    kind,
                    CanonicalEventKind::ToolProposed { .. }
                        | CanonicalEventKind::LeadReviewStarted { .. }
                        | CanonicalEventKind::LeadReviewFinished { approved: true, .. }
                        | CanonicalEventKind::WorkRecoveryRequested { .. }
                ),
                headline: activity_headline(kind),
                approval_id: match kind {
                    CanonicalEventKind::ApprovalRequested { approval_id, .. }
                    | CanonicalEventKind::ApprovalDecided { approval_id, .. }
                    | CanonicalEventKind::RevisionRequested { approval_id, .. } => {
                        Some(*approval_id)
                    }
                    CanonicalEventKind::LeadReviewStarted { approval_id, .. }
                    | CanonicalEventKind::LeadReviewFinished { approval_id, .. } => *approval_id,
                    _ => None,
                },
                tool_call_id: match kind {
                    CanonicalEventKind::ToolProposed { tool_call_id, .. } => Some(*tool_call_id),
                    _ => None,
                },
                review: match kind {
                    CanonicalEventKind::LeadReviewStarted { lead, .. } => Some(ReviewContext {
                        reviewer: *lead,
                        decision: None,
                    }),
                    CanonicalEventKind::LeadReviewFinished {
                        lead,
                        approved,
                        reason,
                        ..
                    } => Some(ReviewContext {
                        reviewer: *lead,
                        decision: Some((*approved, sanitize(reason))),
                    }),
                    _ => None,
                },
                conversational: matches!(
                    kind,
                    CanonicalEventKind::TaskDelegated { .. }
                        | CanonicalEventKind::ToolProposed { .. }
                        | CanonicalEventKind::LeadReviewStarted { .. }
                        | CanonicalEventKind::LeadReviewFinished { .. }
                        | CanonicalEventKind::RevisionRequested { .. }
                        | CanonicalEventKind::WorkRecoveryRequested { .. }
                        | CanonicalEventKind::RevisionStopped(_)
                        | CanonicalEventKind::ProviderFailed(_)
                        | CanonicalEventKind::TaskResultPublished { .. }
                ),
                speaker: match kind {
                    CanonicalEventKind::LeadReviewFinished { lead, .. } => Some(*lead),
                    CanonicalEventKind::RevisionRequested { .. } => task_id
                        .and_then(|id| self.tasks.iter().find(|task| task.id == id))
                        .and_then(|task| task.parent)
                        .and_then(|id| self.tasks.iter().find(|task| task.id == id))
                        .map(|task| task.owner),
                    _ => None,
                },
                related_task: match kind {
                    CanonicalEventKind::TaskDelegated { child, .. } => Some(*child),
                    CanonicalEventKind::DependencyAdded { depends_on, .. } => Some(*depends_on),
                    _ => None,
                },
                source_task: match kind {
                    CanonicalEventKind::TaskDelegated { parent, .. } => Some(*parent),
                    CanonicalEventKind::DependencyAdded { task, .. } => Some(*task),
                    _ => None,
                },
                accent: match kind {
                    CanonicalEventKind::LeadReviewFinished { approved: true, .. }
                    | CanonicalEventKind::ProcessFinished {
                        exit_code: Some(0), ..
                    }
                    | CanonicalEventKind::TaskResultPublished { .. } => theme::GREEN,
                    CanonicalEventKind::LeadReviewFinished {
                        approved: false, ..
                    }
                    | CanonicalEventKind::ProcessFinished { .. }
                    | CanonicalEventKind::ProviderFailed(_) => theme::RED,
                    CanonicalEventKind::LeadReviewStarted { .. }
                    | CanonicalEventKind::RevisionRequested { .. }
                    | CanonicalEventKind::TaskDelegated { .. } => theme::AQUA,
                    _ => event_text_color(&format_event(kind)),
                },
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
        self.role_scroll = 0;
        self.task_inspector_scroll = 0;
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
    fn agent_color(&self, agent: &AgentProfile) -> Color {
        self.agents
            .iter()
            .position(|member| member.id == agent.id)
            .map_or_else(|| role_color(&agent.role), theme::agent_color)
    }
    fn event_scroll_limit(&self) -> u16 {
        self.events
            .iter()
            .map(|event| event.chars().count().div_ceil(40).saturating_add(2))
            .sum::<usize>()
            .min(usize::from(u16::MAX)) as u16
    }
}

fn trim_tail(value: &mut String, max_bytes: usize) {
    if value.len() <= max_bytes {
        return;
    }
    let mut start = value.len() - max_bytes;
    while !value.is_char_boundary(start) {
        start += 1;
    }
    if let Some(line_end) = value[start..].find('\n') {
        start += line_end + 1;
    }
    value.drain(..start);
}

fn has_unsettled_tasks(tasks: &[Task]) -> bool {
    tasks.iter().any(|task| {
        matches!(
            task.status,
            TaskStatus::Queued
                | TaskStatus::Ready
                | TaskStatus::Running
                | TaskStatus::WaitingApproval
        )
    })
}

fn new_composer() -> TextArea<'static> {
    let mut composer = TextArea::default();
    composer.set_cursor_line_style(Style::default());
    composer.set_cursor_style(
        Style::default()
            .fg(theme::BACKGROUND)
            .bg(theme::AQUA)
            .add_modifier(Modifier::BOLD),
    );
    composer.set_selection_style(Style::default().fg(theme::BACKGROUND).bg(theme::YELLOW));
    composer.set_style(theme::canvas());
    composer.set_wrap_mode(WrapMode::WordOrGlyph);
    composer.set_tab_length(2);
    composer.set_max_histories(200);
    composer.set_placeholder_text("");
    composer
}
#[must_use]
pub const fn action_for_key(code: KeyCode) -> Action {
    match code {
        KeyCode::Up | KeyCode::Char('k') => Action::Up,
        KeyCode::Down | KeyCode::Char('j') => Action::Down,
        KeyCode::Tab => Action::NextTab,
        KeyCode::BackTab => Action::PreviousTab,
        KeyCode::Char('?') => Action::ToggleHelp,
        KeyCode::Char('u') => Action::ToggleUsage,
        KeyCode::Enter => Action::FocusComposer,
        KeyCode::Char('r') => Action::RetryTask,
        KeyCode::Char('A') => Action::AddCredential,
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
        KeyCode::Char('c') => Action::AdjustTeam,
        KeyCode::Char('g') => Action::ChangeWorkspace,
        KeyCode::Char('a') => Action::AddAgent,
        KeyCode::Char('o') => Action::OpenSelectedAgentTab,
        KeyCode::Char('1') => Action::ShowTeam,
        KeyCode::Char('2') => Action::ShowTasks,
        KeyCode::Char('i') => Action::ShowTasks,
        KeyCode::Char('t') => Action::ToggleTaskFocus,
        KeyCode::Char('h') => Action::ToggleTaskPane,
        KeyCode::Delete => Action::DeleteAgent,
        KeyCode::Char('w') => Action::ChangeWorkspace,
        KeyCode::Char('v') => Action::ToggleExpanded,
        KeyCode::Char('d') => Action::ToggleApprovalDetails,
        KeyCode::Esc => Action::Escape,
        KeyCode::Char('q') => Action::Quit,
        _ => Action::None,
    }
}

fn action_for_event(value: Event) -> Action {
    match value {
        Event::Key(key) if key.kind == KeyEventKind::Release => Action::None,
        Event::Key(key)
            if key.code == KeyCode::Char('c')
                && key.modifiers.contains(KeyModifiers::CONTROL)
                && key.kind != KeyEventKind::Release =>
        {
            Action::Quit
        }
        Event::Key(key)
            if key.modifiers.contains(KeyModifiers::CONTROL)
                && matches!(
                    key.code,
                    KeyCode::Left | KeyCode::Right | KeyCode::Up | KeyCode::Down
                )
                && key.kind != KeyEventKind::Release =>
        {
            match key.code {
                KeyCode::Left => Action::MoveSelectedAgent(-1, 0),
                KeyCode::Right => Action::MoveSelectedAgent(1, 0),
                KeyCode::Up => Action::MoveSelectedAgent(0, -1),
                KeyCode::Down => Action::MoveSelectedAgent(0, 1),
                _ => Action::None,
            }
        }
        Event::Key(key)
            if key.modifiers == KeyModifiers::ALT && key.kind != KeyEventKind::Release =>
        {
            match key.code {
                KeyCode::Up => Action::ResizeTasks(-1),
                KeyCode::Down => Action::ResizeTasks(1),
                KeyCode::Char('0') => Action::ResetTaskLayout,
                _ => Action::None,
            }
        }
        Event::Key(key)
            if key.code == KeyCode::Char('w') && key.modifiers.contains(KeyModifiers::CONTROL) =>
        {
            Action::CloseTab
        }
        Event::Key(key)
            if key.code == KeyCode::Tab && key.modifiers.contains(KeyModifiers::SHIFT) =>
        {
            Action::PreviousTab
        }
        Event::Key(key) => action_for_key(key.code),
        Event::Mouse(mouse) => Action::Mouse(mouse),
        Event::FocusGained | Event::FocusLost | Event::Paste(_) | Event::Resize(_, _) => {
            Action::None
        }
    }
}

fn activity_headline(kind: &CanonicalEventKind) -> Option<&'static str> {
    match kind {
        CanonicalEventKind::TaskDelegated { .. } => Some("↳ Delegated"),
        CanonicalEventKind::DependencyAdded { .. } => Some("Waiting for another task"),
        CanonicalEventKind::LeadReviewStarted { .. } => Some("↶ Sent for review"),
        CanonicalEventKind::LeadReviewFinished { approved: true, .. } => Some("✓ Approved"),
        CanonicalEventKind::LeadReviewFinished {
            approved: false, ..
        } => Some("↶ Changes requested"),
        CanonicalEventKind::RevisionRequested { .. } => Some("↳ Feedback returned"),
        CanonicalEventKind::TaskRetried { .. } => Some("↻ New attempt"),
        CanonicalEventKind::WorkRecoveryRequested { .. } => Some("↶ Diagnosis requested"),
        CanonicalEventKind::RevisionStopped(_) | CanonicalEventKind::ApplicationIssue(_) => {
            Some("! Needs attention")
        }
        CanonicalEventKind::ProviderFailed(_) | CanonicalEventKind::PatchConflict { .. } => {
            Some("! Problem")
        }
        CanonicalEventKind::ProviderWarning(_) | CanonicalEventKind::ProviderRetrying { .. } => {
            Some("! Provider update")
        }
        CanonicalEventKind::ToolProposed { .. } => Some("Action proposed"),
        CanonicalEventKind::ProcessFinished { .. } => Some("Check finished"),
        CanonicalEventKind::ArtifactPublished { .. } => Some("Artifact recorded"),
        CanonicalEventKind::TaskResultPublished { .. } => Some("✓ Result"),
        CanonicalEventKind::TaskTransition {
            to: TaskStatus::Running,
            ..
        } => Some("▶ Working"),
        CanonicalEventKind::TaskTransition {
            to: TaskStatus::Completed,
            ..
        } => Some("✓ Completed"),
        CanonicalEventKind::TaskTransition {
            to: TaskStatus::Blocked | TaskStatus::Failed,
            ..
        } => Some("! Stopped"),
        CanonicalEventKind::TaskCancelled => Some("× Cancelled"),
        _ => None,
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
        CanonicalEventKind::TextDelta(s) | CanonicalEventKind::GenerationDelta(s) => sanitize(s),
        CanonicalEventKind::ProviderRetrying { attempt } => {
            format!("retrying provider · attempt {attempt} · previous preview was incomplete")
        }
        CanonicalEventKind::Usage {
            input_tokens,
            output_tokens,
            ..
        } => format!("usage: {input_tokens} in / {output_tokens} out"),
        CanonicalEventKind::ProviderCompleted => "provider completed".into(),
        CanonicalEventKind::ProviderFailed(e) => format!("provider failed: {}", sanitize(e)),
        CanonicalEventKind::ProviderWarning(e) => format!("provider warning: {}", sanitize(e)),
        CanonicalEventKind::ApplicationIssue(e) => {
            format!("application needs attention: {}", sanitize(e))
        }
        CanonicalEventKind::WorkRecoveryRequested { reason, .. } => {
            format!("failure → Team Lead: {}", sanitize(reason))
        }
        CanonicalEventKind::RevisionRequested { feedback, .. } => format!(
            "Team Lead → developer: revise and resubmit · {}",
            sanitize(feedback)
        ),
        CanonicalEventKind::RevisionStopped(reason) => {
            format!("revision blocked: {}", sanitize(reason))
        }
        CanonicalEventKind::LeadReviewStarted { approval_id, .. } => format!(
            "→ Team Lead: {}",
            if approval_id.is_some() {
                "reviewing proposal"
            } else {
                "diagnosing failure"
            }
        ),
        CanonicalEventKind::LeadReviewFinished {
            approved, reason, ..
        } => format!(
            "Team Lead → {}: {}",
            if *approved { "approved" } else { "rejected" },
            sanitize(reason)
        ),
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
            verification,
            exit_code,
            truncated,
        } => format!(
            "{} {program}: exit {exit_code:?}{}",
            if *verification {
                "verification"
            } else {
                "process"
            },
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
    if text.starts_with("team lead → approved") {
        theme::GREEN
    } else if text.starts_with("team lead → rejected")
        || text.starts_with("application needs attention")
        || text.starts_with("revision blocked:")
    {
        theme::RED
    } else if text.contains("→ team lead") || text.starts_with("team lead → developer:") {
        theme::AQUA
    } else if text.starts_with("tool proposed:")
        || text.starts_with("approval requested:")
        || text.starts_with("approval:")
    {
        theme::YELLOW
    } else if text.contains("failed")
        || text.contains("conflict")
        || text.contains("cancelled")
        || text.contains("denied")
    {
        theme::RED
    } else if text.starts_with("provider completed")
        || text.starts_with("result:")
        || text.starts_with("artifact published:")
    {
        theme::GREEN
    } else if text.starts_with("task ")
        || text.starts_with("task:")
        || text.starts_with("re-run attempt ")
        || text.starts_with("delegated ")
        || text.starts_with("dependency:")
    {
        theme::AQUA
    } else if text.starts_with("usage:") {
        theme::PURPLE
    } else {
        theme::TEXT
    }
}

fn event_viewer_lines(app: &AppModel) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    for (index, event) in app.events.iter().enumerate().rev() {
        let task_id = app.event_task_ids.get(index).copied().flatten();
        let color = event_text_color(event);
        let mut first = true;
        for event_line in compact_text(event, app.expanded_output, true).split('\n') {
            let mut spans = Vec::new();
            if first {
                spans.push(Span::styled(
                    format!("#{} · ", index + 1),
                    Style::default().fg(theme::TEXT),
                ));
                if let Some(task_id) = task_id {
                    spans.push(Span::styled(
                        task_event_prefix(task_id),
                        Style::default().fg(theme::BLUE),
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

pub(crate) fn sanitize(value: &str) -> String {
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
    pub automatic_review: bool,
    pub repo: Arc<SqliteRepository>,
    pub provider: Arc<dyn voff_provider::ModelProvider>,
    pub providers: std::collections::HashMap<String, Arc<dyn voff_provider::ModelProvider>>,
    pub default_provider: String,
    pub session: SessionId,
    pub owner: AgentId,
    pub model_name: String,
    pub system_prompt: String,
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
    pub model_catalog: ModelCatalog,
    pub agent_provider_kinds: HashMap<AgentId, String>,
    pub credential_target: Option<(String, crate::credential::CredentialSlot)>,
    pub team_lead: AgentId,
    pub team_setup: Option<crate::onboarding::SetupDraft>,
    pub startup_notice: Option<String>,
    pub startup_terminal_handoff: Option<crate::onboarding::TerminalHandoff>,
    pub startup_scan_pending: bool,
    pub setup_prompts: crate::onboarding::SetupPrompts,
    pub startup_goal: Option<String>,
    pub startup_popup_size: Option<(u16, u16)>,
}
pub(crate) enum TuiExit {
    Team(crate::onboarding::SetupDraft, Option<String>),
    Workspace(PathBuf),
}

pub(crate) async fn run_configured(config: ConfiguredRun) -> Result<Option<TuiExit>, TuiError> {
    let (event_tx, event_rx) = mpsc::channel(config.capacity);
    let credential_repository = Arc::clone(&config.repo);
    let composer_history = credential_repository
        .prompt_history(&config.workspace_root)
        .await
        .map_err(|error| TuiError::Io(io::Error::other(error)))?;
    let agent_profiles = config.agent_profiles.clone();
    let startup_scan_provider = config.providers.get(&config.default_provider).cloned();
    let startup_setup_prompt = if config.startup_goal.is_some() {
        config.setup_prompts.build_from_description.clone()
    } else {
        config.setup_prompts.scan_workspace.clone()
    };
    let engine = Arc::new(
        Engine::new(config.repo, config.provider, config.session, event_tx)
            .with_provider_registry(config.providers, config.default_provider)
            .with_formation_agents(&agent_profiles)
            .with_team_lead(config.team_lead)
            .with_automatic_review(config.automatic_review)
            .with_verified_workflow()
            .with_runtime(config.model_name, config.system_prompt)
            .with_workspace_context(config.workspace_context)
            .with_safe_workflow(&config.workspace_root)
            .map_err(|e| TuiError::Io(io::Error::other(e)))?,
    );
    let (input_tx, input_rx) = mpsc::channel(32);
    let mut app = AppModel::new(config.session);
    app.automatic_review = config.automatic_review;
    app.workspace_root = config.workspace_root.clone();
    app.composer_history = composer_history;
    app.agents = agent_profiles;
    app.known_agents = config.known_agents;
    app.model_catalog = config.model_catalog;
    app.agent_provider_kinds = config.agent_provider_kinds;
    app.credential_target = config.credential_target;
    app.team_lead = Some(config.team_lead);
    app.team_setup = config.team_setup;
    app.startup_scan_pending = config.startup_scan_pending;
    app.startup_setup_prompt = startup_setup_prompt;
    app.startup_popup_size = config.startup_popup_size;
    if let Some(notice) = config.startup_notice {
        let _ = app.notify("Team updated", &notice, Level::Info, theme::GREEN, false);
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
            startup_scan_provider,
            startup_goal: config.startup_goal,
            setup_prompts: config.setup_prompts,
            startup_terminal_handoff: config.startup_terminal_handoff,
        },
        input_tx,
        input_rx,
        event_rx,
    )
    .await?;
    Ok(app
        .workspace_result
        .take()
        .map(TuiExit::Workspace)
        .or_else(|| {
            app.setup_result
                .take()
                .map(|draft| TuiExit::Team(draft, app.setup_notice.take()))
        }))
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
        if let CanonicalEventKind::ApplicationIssue(message) = &event.kind {
            app.application_notice = Some(sanitize(message));
        }
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
    startup_scan_provider: Option<Arc<dyn voff_provider::ModelProvider>>,
    startup_goal: Option<String>,
    setup_prompts: crate::onboarding::SetupPrompts,
    startup_terminal_handoff: Option<crate::onboarding::TerminalHandoff>,
}

const INPUT_POLL_INTERVAL: Duration = Duration::from_millis(100);

pub(crate) fn redraw_on_resize<B: ratatui::backend::Backend>(
    terminal: &mut Terminal<B>,
    event: &Event,
    draw: impl FnOnce(&mut ratatui::Frame<'_>),
) -> Result<bool, B::Error> {
    let Event::Resize(width, height) = *event else {
        return Ok(false);
    };
    // Minimized terminals may report an empty viewport; wait for a usable size.
    if width > 0 && height > 0 {
        terminal.resize(Rect::new(0, 0, width, height))?;
        terminal.draw(draw)?;
    }
    Ok(true)
}

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
        startup_scan_provider,
        startup_goal,
        setup_prompts,
        startup_terminal_handoff,
    } = config;
    let (_guard, startup_terminal) = TerminalGuard::enter_with_handoff(startup_terminal_handoff)?;
    let mut terminal = if let Some(terminal) = startup_terminal {
        terminal
    } else {
        Terminal::new(CrosstermBackend::new(io::stdout()))?
    };
    let input = tokio::task::spawn_blocking(move || {
        forward_terminal_events(input_tx, event::poll, event::read);
    });
    let (credential_tx, mut credential_rx) = mpsc::channel(1);
    let (lead_submission_tx, mut lead_submission_rx) =
        mpsc::channel::<Result<&'static str, String>>(8);
    let (retry_tx, mut retry_rx) = mpsc::channel::<Result<(voff_core::TaskId, u16), String>>(1);
    let (team_scan_tx, mut team_scan_rx) =
        mpsc::channel::<Result<(crate::onboarding::SetupDraft, String), String>>(1);
    if model.startup_scan_pending {
        let workspace = model.workspace_root.clone();
        let previous = model.team_setup.clone();
        tokio::spawn(async move {
            let result = match (
                startup_scan_provider,
                previous.and_then(|setup| setup.developers.first().cloned()),
            ) {
                (Some(provider), Some(lead)) => {
                    crate::onboarding::build_workspace_team(
                        workspace,
                        lead.provider,
                        lead.model,
                        lead.credential_reference,
                        startup_goal,
                        setup_prompts,
                        provider,
                    )
                    .await
                }
                (None, _) => Err("The selected provider is unavailable.".into()),
                (_, None) => Err("The setup has no selected provider.".into()),
            };
            let _ = team_scan_tx.send(result).await;
        });
    }
    if model.automatic_review && has_unsettled_tasks(&model.tasks) {
        model.pending_submissions += 1;
        let engine = Arc::clone(&engine);
        let notices = lead_submission_tx.clone();
        tokio::spawn(async move {
            let result = engine
                .run_until_idle_bounded(max_parallel_tasks)
                .await
                .map(|_| "Pending work processed.")
                .map_err(|error| error.to_string());
            let _ = notices.send(result).await;
        });
    }
    let mut tick = tokio::time::interval(Duration::from_millis(100));
    let mut loop_result = Ok(());
    'terminal: while !model.quit {
        tokio::select! {
            Some(event)=input_rx.recv()=>{
                match redraw_on_resize(&mut terminal, &event, |frame| render(frame, model)) {
                    Ok(true) => continue,
                    Ok(false) => {}
                    Err(error) => {
                        loop_result = Err(TuiError::Io(error));
                        break 'terminal;
                    }
                }
                if matches!(&event, Event::Key(key)
                    if key.kind == KeyEventKind::Press
                        && key.code == KeyCode::Char('c')
                        && key.modifiers.contains(KeyModifiers::CONTROL))
                {
                    if model.startup_scan_summary {
                        model.setup_result = None;
                        model.setup_notice = None;
                    }
                    model.quit = true;
                    continue;
                }
                if model.startup_scan_pending
                    || model.startup_scan_error.is_some()
                    || model.startup_scan_summary
                {
                    if let Event::Key(key) = &event
                        && key.kind == KeyEventKind::Press
                    {
                        match key.code {
                            KeyCode::Up | KeyCode::Char('k') => {
                                model.startup_prompt_scroll = model.startup_prompt_scroll.saturating_sub(1);
                            }
                            KeyCode::Down | KeyCode::Char('j') => {
                                model.startup_prompt_scroll = model.startup_prompt_scroll.saturating_add(1);
                            }
                            KeyCode::PageUp => {
                                model.startup_prompt_scroll = model.startup_prompt_scroll.saturating_sub(8);
                            }
                            KeyCode::PageDown => {
                                model.startup_prompt_scroll = model.startup_prompt_scroll.saturating_add(8);
                            }
                            KeyCode::Enter if model.startup_scan_summary => {
                                model.startup_scan_summary = false;
                                model.quit = true;
                            }
                            KeyCode::Esc | KeyCode::Char('q') if model.startup_scan_summary => {
                                model.setup_result = None;
                                model.setup_notice = None;
                                model.startup_scan_summary = false;
                                model.quit = true;
                            }
                            KeyCode::Enter | KeyCode::Esc | KeyCode::Char('q')
                                if !model.startup_scan_pending && !model.startup_scan_summary => {
                                model.startup_scan_error = None;
                            }
                            _ => {}
                        }
                    } else if let Event::Mouse(mouse) = &event {
                        match mouse.kind {
                            MouseEventKind::ScrollUp => {
                                model.startup_prompt_scroll = model.startup_prompt_scroll.saturating_sub(3);
                            }
                            MouseEventKind::ScrollDown => {
                                model.startup_prompt_scroll = model.startup_prompt_scroll.saturating_add(3);
                            }
                            _ => {}
                        }
                    }
                    continue;
                }
                if model.usage_viewer {
                    match &event {
                        Event::Key(key) if key.kind == KeyEventKind::Press => match key.code {
                            KeyCode::Esc | KeyCode::Char('u') => {
                                model.reduce(Action::ToggleUsage);
                            }
                            KeyCode::Char('q') => model.quit = true,
                            KeyCode::Up | KeyCode::Char('k') => model.reduce(Action::Up),
                            KeyCode::Down | KeyCode::Char('j') => model.reduce(Action::Down),
                            KeyCode::PageUp => model.reduce(Action::PageUp),
                            KeyCode::PageDown => model.reduce(Action::PageDown),
                            _ => {}
                        },
                        Event::Mouse(mouse) => {
                            let action = model.action_for_mouse(*mouse, terminal.size()?.into());
                            model.reduce(action);
                        }
                        _ => {}
                    }
                    continue;
                }
                if let Some(wizard) = model.workspace_editor.as_mut() {
                    wizard.handle_event(&event, terminal.size()?.into());
                    if wizard.cancelled() {
                        model.workspace_editor = None;
                    } else if wizard.complete() {
                        model.workspace_result = Some(wizard.selected_workspace().to_path_buf());
                        model.workspace_editor = None;
                        model.quit = true;
                    }
                    continue;
                }
                if let Some(wizard) = model.setup_editor.as_mut() {
                    wizard.handle_event(&event, terminal.size()?.into());
                    if wizard.cancelled() {
                        model.setup_editor = None;
                    } else if wizard.complete() {
                        model.setup_result = model.setup_editor.take().map(crate::onboarding::Wizard::into_draft);
                        model.quit = true;
                    }
                    continue;
                }
                if model.handle_delete_event(&event, terminal.size()?.into()) {
                    continue;
                }
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
                            if let Err(error) = credential_repository
                                .save_prompt_history(&model.workspace_root, &objective)
                                .await
                            {
                                let _ = model.notify(
                                    "Prompt history not saved",
                                    &error.to_string(),
                                    Level::Error,
                                    theme::RED,
                                    false,
                                );
                            }
                            let role_target = model
                                .composer_target()
                                .filter(|agent| Some(agent.id) != model.team_lead)
                                .map(|agent| (agent.id, agent.name.clone()));
                            let conversational = role_target.is_none()
                                && is_conversational_request(&objective);
                            let routed = model.routed_objective(&objective);
                            let acceptance_criteria = if conversational {
                                vec!["Answer the user's question from available context".into()]
                            } else {
                                vec![
                                    if role_target.is_some() {
                                        "Team Lead delegates to the requested role and integrates its response, or reports a blocker"
                                    } else {
                                        "Team Lead completes or delegates the requested work"
                                    }
                                    .into(),
                                ]
                            };
                            model.pending_submissions += 1;
                            if let Some((id, name)) = &role_target {
                                model.role_requests.push((*id, objective.clone()));
                                if model.role_requests.len() > 100 {
                                    model.role_requests.remove(0);
                                }
                                let _ = model.notify(
                                    "Task sent to Team Lead",
                                    &format!("Requested {}", truncate_text(&one_line(&sanitize(name)), 28)),
                                    Level::Info,
                                    theme::AQUA,
                                    false,
                                );
                                model.push_ui_event(format!(
                                    "you → Team Lead (requested {name}): {}",
                                    sanitize(&objective)
                                ));
                            } else {
                                let _ = model.notify(
                                    "Task sent to Team Lead",
                                    "Waiting for the task to start",
                                    Level::Info,
                                    theme::AQUA,
                                    false,
                                );
                                model.push_ui_event(format!(
                                    "you → Team Lead task: {}",
                                    sanitize(&objective)
                                ));
                            }
                            let engine = Arc::clone(&engine);
                            let lead_submission_tx = lead_submission_tx.clone();
                            tokio::spawn(async move {
                                let result = async {
                                    let task = if let Some((target_id, _)) = role_target {
                                        engine
                                            .create_task_for_agent(
                                                owner,
                                                target_id,
                                                routed,
                                                acceptance_criteria,
                                            )
                                            .await?
                                    } else {
                                        engine
                                            .create_task(
                                                owner,
                                                routed,
                                                acceptance_criteria,
                                            )
                                            .await?
                                    };
                                    engine.mark_ready(task.id).await?;
                                    engine.run_task_and_schedule(task.id, max_parallel_tasks).await
                                }
                                .await
                                .map(|()| "Task accepted by Team Lead.")
                                .map_err(|error| error.to_string());
                                let _ = lead_submission_tx.send(result).await;
                            });
                        }
                        ComposerEvent::Consumed => {}
                        ComposerEvent::Ignored => {
                            let action = match event {
                                Event::Mouse(mouse) => {
                                    model.action_for_mouse(mouse, terminal.size()?.into())
                                }
                                other => action_for_event(other),
                            };
                            match action {
                                Action::RetryTask
                                    if model.panel == Panel::Tasks || model.task_pane_focused =>
                                {
                                    if model.retry_starting {
                                        model.warn("A task re-run is already being prepared.");
                                    } else if let Some(task) = model.selected_task().cloned() {
                                        if task.status.can_retry() {
                                            model.retry_starting = true;
                                            let _ = model.notify(
                                                "Preparing retry",
                                                &format!(
                                                    "Attempt {} for the selected task",
                                                    task.attempt.saturating_add(1)
                                                ),
                                                Level::Info,
                                                theme::AQUA,
                                                false,
                                            );
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
                                                        if let Err(error) = engine.run_task_and_schedule(retry_id, max_parallel_tasks).await {
                                                            let _ = retry_tx.send(Err(error.to_string())).await;
                                                        }
                                                    }
                                                    Err(error) => {
                                                        let _ = retry_tx.send(Err(error)).await;
                                                    }
                                                }
                                            });
                                        } else {
                                            model.warn(&format!(
                                                "A {:?} task cannot be re-run; cancel it first if it is active.",
                                                task.status
                                            ));
                                        }
                                    } else {
                                        model.warn("Select a task to re-run.");
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
                                                    | TaskStatus::WaitingApproval
                                            ) || model.lead_reviews.contains_key(&task.id)
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
            Some(result)=lead_submission_rx.recv()=>{
                model.pending_submissions = model.pending_submissions.saturating_sub(1);
                match result {
                    Ok(notice) => {
                        let _ = model.notify(
                            "Task accepted",
                            notice,
                            Level::Info,
                            theme::GREEN,
                            false,
                        );
                    }
                    Err(error) => {
                        let error = sanitize(&error);
                        model.warn(&format!("Team Lead request failed: {error}"));
                        model.push_ui_event(format!("Team Lead request failed: {error}"));
                    }
                }
            }
            Some(result)=retry_rx.recv()=>{
                model.retry_starting = false;
                match result {
                    Ok((task_id, attempt)) => {
                        let _ = model.notify(
                            "Task re-running",
                            &format!("Attempt {attempt} · {}", short_task_id(task_id)),
                            Level::Info,
                            theme::AQUA,
                            false,
                        );
                    }
                    Err(error) => {
                        let error = sanitize(&error);
                        model.warn(&format!("Could not re-run task: {error}"));
                        model.push_ui_event(format!("Task re-run failed: {error}"));
                    }
                }
            }
            Some(result)=team_scan_rx.recv()=>{
                model.startup_scan_result = Some(result);
            }
            Some(message)=event_rx.recv()=>model.apply_event(message),
            _=tick.tick()=>{
                model.startup_scan_frame = model.startup_scan_frame.wrapping_add(1);
                if model.startup_scan_pending
                    && let Some(result) = model.startup_scan_result.take()
                {
                    model.startup_scan_pending = false;
                    match result {
                        Ok((draft, notice)) => {
                            model.setup_result = Some(draft);
                            model.setup_notice = Some(notice);
                            model.startup_prompt_scroll = 0;
                            model.startup_scan_summary = true;
                        }
                        Err(error) => model.startup_scan_error = Some(sanitize(&error)),
                    }
                }
                model.team_flow.tick(Duration::from_millis(100));
                model.notifications.tick(Duration::from_millis(100));
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
    if app.startup_scan_pending || app.startup_scan_error.is_some() || app.startup_scan_summary {
        let setup_prompt = sanitize(&app.startup_setup_prompt);
        let error = app.startup_scan_error.as_deref().map(sanitize);
        let summary = app.setup_notice.as_deref().map(sanitize);
        crate::onboarding::render_team_building_screen(
            frame,
            crate::onboarding::TeamBuildingView {
                popup_size: app.startup_popup_size,
                setup_prompt: &setup_prompt,
                team: app.setup_result.as_ref(),
                summary: summary.as_deref(),
                frame_index: app.startup_scan_frame,
                scroll: app.startup_prompt_scroll,
                error: error.as_deref(),
            },
        );
        return;
    }
    frame.render_widget(Block::default().style(theme::canvas()), frame.area());
    let composer_height = composer_height(app, frame.area());
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(2),
            Constraint::Min(8),
            Constraint::Length(composer_height),
        ])
        .split(frame.area());
    render_tab_bar(frame, app, rows[0]);

    app.task_inspector_hitbox = Rect::default();
    app.task_expand_hitbox = Rect::default();
    app.task_retry_hitbox = Rect::default();
    let sidebar_visible =
        app.task_pane_visible && app.active_role_tab.is_none() && rows[1].width >= 40;
    if sidebar_visible {
        let [sidebar, gutter, workspace] = task_sidebar_panes(rows[1]);
        app.task_pane_hitbox = sidebar;
        frame.render_widget(Block::default().style(theme::canvas()), gutter);
        render_task_sidebar(frame, app, sidebar);
        match app.active_role_tab {
            Some(id) => render_role_workspace(frame, app, id, workspace),
            None if app.panel == Panel::Tasks => {
                render_task_inspector(frame, app, workspace);
            }
            None => render_team(frame, app, workspace),
        }
    } else {
        app.task_pane_hitbox = Rect::default();
        if let Some(id) = app.active_role_tab {
            render_role_workspace(frame, app, id, rows[1]);
        } else {
            match app.panel {
                Panel::Agents => render_team(frame, app, rows[1]),
                Panel::Tasks => render_tasks_workspace(frame, app, rows[1]),
            }
        }
    }

    let workspace = sanitize(&app.workspace_root.display().to_string());
    let right_title = format!(" [?] help · [u] usage · [w] {workspace} ");
    app.help_hitbox = Rect::new(
        rows[2]
            .right()
            .saturating_sub(1)
            .saturating_sub(right_title.chars().count() as u16),
        rows[2].bottom().saturating_sub(1),
        8,
        1,
    );
    app.workspace_hitbox = Rect::new(
        rows[2]
            .right()
            .saturating_sub(1)
            .saturating_sub(workspace.chars().count() as u16 + 5),
        rows[2].bottom().saturating_sub(1),
        (workspace.chars().count() as u16 + 5).min(rows[2].width),
        1,
    );
    app.composer.set_placeholder_text("");
    let composer_status = if app.composer_active {
        let DataCursor(line, column) = app.composer.cursor();
        Some((
            Line::from(format!(
                " Enter send · Shift+Enter newline · ↑/↓ history · {}:{} ",
                line + 1,
                column + 1
            ))
            .style(Style::default().fg(theme::MUTED)),
            Alignment::Left,
        ))
    } else {
        None
    };
    let composer_color = if app.composer_active {
        theme::AQUA
    } else {
        theme::BORDER
    };
    let mut composer_block = Block::default()
        .title_bottom(
            Line::from(right_title)
                .style(Style::default().fg(theme::TEXT))
                .alignment(ratatui::layout::Alignment::Right),
        )
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .style(theme::canvas())
        .border_style(Style::default().fg(composer_color));
    if let Some((status, status_alignment)) = composer_status {
        composer_block = composer_block.title_bottom(status.alignment(status_alignment));
    }
    frame.render_widget(composer_block, rows[2]);
    let composer_inner = rows[2].inner(ratatui::layout::Margin::new(1, 1));
    if composer_inner.width >= 4 {
        frame.render_widget(
            Paragraph::new("🐶").style(Style::default().fg(theme::ORANGE)),
            Rect::new(composer_inner.x, composer_inner.y, 2, 1),
        );
    }
    // Leave one cell between the wide dog emoji and the composer text.
    let input_offset = 3;
    let input_area = Rect::new(
        composer_inner.x.saturating_add(input_offset),
        composer_inner.y,
        composer_inner.width.saturating_sub(input_offset),
        composer_inner.height,
    );
    app.composer
        .set_block(Block::default().style(theme::canvas()));
    app.composer.set_cursor_style(if app.composer_active {
        Style::default()
            .fg(theme::BACKGROUND)
            .bg(theme::AQUA)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default()
    });
    app.composer_hitbox = rows[2];
    frame.render_widget(&app.composer, input_area);
    if !app.composer_active && app.composer.lines().iter().all(String::is_empty) {
        let target = app.composer_target().or_else(|| app.root_agent());
        let (name, color) = target.map_or_else(
            || ("Team Lead".to_owned(), theme::AQUA),
            |agent| (sanitize(&agent.name), app.agent_color(agent)),
        );
        let placeholder = Line::from(vec![
            Span::styled("Ask ", Style::default().fg(theme::MUTED)),
            Span::styled(name, Style::default().fg(color)),
            Span::styled(" to do anything", Style::default().fg(theme::MUTED)),
        ]);
        frame.render_widget(Paragraph::new(placeholder), input_area);
    }
    if frame.area().width >= 48 && frame.area().height >= 12 {
        app.notifications.render(frame, frame.area());
    }
    if let Some(wizard) = &app.setup_editor {
        crate::onboarding::render_popup(frame, wizard);
        return;
    }
    if let Some(wizard) = &app.workspace_editor {
        crate::onboarding::render_popup(frame, wizard);
        return;
    }
    render_overlays(frame, app);
}

fn render_tab_bar(frame: &mut ratatui::Frame<'_>, app: &mut AppModel, area: Rect) {
    let session = app.session.to_string();
    let short = &session[..session.len().min(8)];
    let mut spans = if area.width < 48 {
        vec![Span::styled(
            " voff ",
            Style::default()
                .fg(theme::ORANGE)
                .add_modifier(Modifier::BOLD),
        )]
    } else {
        vec![
            Span::styled(
                " voff ",
                Style::default()
                    .fg(theme::ORANGE)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(format!("· {short}   "), Style::default().fg(theme::TEXT)),
        ]
    };
    let mut x = area
        .x
        .saturating_add(u16::try_from(Line::from(spans.clone()).width()).unwrap_or(u16::MAX));
    let right = area.right().saturating_sub(1);
    let row = area.y;
    app.tab_hitboxes.clear();
    app.add_role_tab_hitbox = Rect::default();
    app.team_tab_hitbox = Rect::default();
    app.tasks_tab_hitbox = Rect::default();
    let active = app.active_tab();
    let fixed_tabs = [
        (TabId::Team, " [1] Team ".to_owned()),
        (TabId::Tasks, " [2] Tasks ".to_owned()),
    ];
    for (id, label) in fixed_tabs {
        let tab = menu_tab(&label, active == id);
        let width = u16::try_from(Line::from(tab.content.as_ref()).width()).unwrap_or(u16::MAX);
        if x.saturating_add(width) > right {
            break;
        }
        let hitbox = Rect::new(x, row, width, 1);
        if id == TabId::Team {
            app.team_tab_hitbox = hitbox;
        } else {
            app.tasks_tab_hitbox = hitbox;
        }
        app.tab_hitboxes.push(TabHitbox {
            id,
            area: hitbox,
            close: None,
        });
        spans.push(tab);
        x = x.saturating_add(width);
        spans.push(Span::raw(" "));
        x = x.saturating_add(1);
    }
    let mut hidden = 0;
    for &id in &app.role_tabs {
        let Some(agent) = app.agents.iter().find(|agent| agent.id == id) else {
            continue;
        };
        let name = truncate_text(&sanitize(&agent.name), 18);
        let label = format!(" {name} × ");
        let width = u16::try_from(Line::from(label.as_str()).width()).unwrap_or(u16::MAX);
        if x.saturating_add(width).saturating_add(1) > right {
            hidden += 1;
            continue;
        }
        let hitbox = Rect::new(x, row, width, 1);
        let close_x = x.saturating_add(width.saturating_sub(2));
        app.tab_hitboxes.push(TabHitbox {
            id: TabId::Role(id),
            area: hitbox,
            close: Some(Rect::new(close_x, row, 1, 1)),
        });
        spans.push(Span::styled(
            label,
            if active == TabId::Role(id) {
                Style::default()
                    .fg(app.agent_color(agent))
                    .bg(theme::BORDER)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default()
                    .fg(app.agent_color(agent))
                    .bg(theme::ELEVATED)
            },
        ));
        x = x.saturating_add(width);
        spans.push(Span::raw(" "));
        x = x.saturating_add(1);
    }
    if hidden > 0 && x.saturating_add(3) < right {
        spans.push(Span::styled(
            format!("+{hidden} "),
            Style::default().fg(theme::MUTED),
        ));
        x = x.saturating_add(3);
    }
    if x.saturating_add(3) <= right {
        spans.push(Span::styled(
            " + ",
            Style::default().fg(theme::TEXT).bg(theme::ELEVATED),
        ));
        app.add_role_tab_hitbox = Rect::new(x, row, 3, 1);
    }
    frame.render_widget(
        Block::default()
            .borders(Borders::BOTTOM)
            .style(theme::surface())
            .border_style(Style::default().fg(theme::BORDER)),
        area,
    );
    frame.render_widget(
        Paragraph::new(Line::from(spans)).style(theme::surface()),
        Rect::new(area.x, row, area.width, 1),
    );
}

fn render_role_workspace(
    frame: &mut ratatui::Frame<'_>,
    app: &mut AppModel,
    id: AgentId,
    area: Rect,
) {
    let Some(agent) = app.agents.iter().find(|agent| agent.id == id) else {
        app.select_tab(TabId::Team);
        render_team(frame, app, area);
        return;
    };
    let latest = app.tasks.iter().find(|task| task.owner == id);
    let mut activity = Vec::new();
    for (_, message) in app.role_requests.iter().filter(|(target, _)| *target == id) {
        activity.push(Line::from(Span::styled(
            " You → Team Lead · requested this role",
            Style::default()
                .fg(theme::ORANGE)
                .add_modifier(Modifier::BOLD),
        )));
        for line in compact_text(&sanitize(message), false, false).lines() {
            activity.push(Line::from(format!("  {line}")).style(Style::default().fg(theme::TEXT)));
        }
        activity.push(Line::from(""));
    }
    if let Some(task) = latest {
        activity.extend(role_activity_lines(app, task, area.width.saturating_sub(2)));
    } else if activity.is_empty() {
        activity.push(Line::from("No activity for this role yet."));
    }
    render_chat(
        frame,
        activity,
        panel(
            &format!(
                " {} · Activity · ↑/↓ scroll · [c] close tab ",
                sanitize(&agent.name)
            ),
            true,
        ),
        area,
        &mut app.role_scroll,
    );
}

fn task_sidebar_panes(area: Rect) -> [Rect; 3] {
    let preferred = (area.width / 4).clamp(18, 32);
    let sidebar = preferred.min(area.width.saturating_sub(25));
    let panes = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Length(sidebar),
            Constraint::Length(1),
            Constraint::Min(0),
        ])
        .split(area);
    [panes[0], panes[1], panes[2]]
}

fn render_task_sidebar(frame: &mut ratatui::Frame<'_>, app: &mut AppModel, area: Rect) {
    app.task_list_hitbox = area;
    app.task_inspector_hitbox = Rect::default();
    app.task_expand_hitbox = Rect::default();
    app.task_retry_hitbox = Rect::default();

    let title = if area.width >= 28 {
        format!(" Tasks · {} · [i] inspect ", app.tasks.len())
    } else if area.width >= 20 {
        format!(" Tasks · {} · [i] ", app.tasks.len())
    } else {
        format!(" Tasks · {} ", app.tasks.len())
    };
    let border_color = if app.task_pane_focused {
        theme::AQUA
    } else {
        theme::BORDER
    };
    let block = panel(&title, app.task_pane_focused)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(border_color));

    if app.tasks.is_empty() {
        frame.render_widget(
            Paragraph::new(vec![
                Line::from(Span::styled(
                    "No tasks yet",
                    Style::default().add_modifier(Modifier::BOLD),
                )),
                Line::from("Enter to start work"),
            ])
            .style(Style::default().fg(theme::TEXT))
            .wrap(Wrap { trim: true })
            .block(block),
            area,
        );
        app.task_window_start = 0;
        return;
    }

    app.selected = app.selected.min(app.tasks.len().saturating_sub(1));
    let visible = usize::from(area.height.saturating_sub(2) / 2).max(1);
    let start = app
        .selected
        .saturating_add(1)
        .saturating_sub(visible)
        .min(app.tasks.len().saturating_sub(visible));
    app.task_window_start = start;
    let items = app
        .tasks
        .iter()
        .enumerate()
        .skip(start)
        .take(visible)
        .map(|(index, task)| {
            let (icon, _, color) = task_status_visual(task.status);
            let max_visible_depth = usize::from(area.width.saturating_sub(10) / 2);
            let indent = "  ".repeat(usize::from(task.depth).min(max_visible_depth));
            let (owner, owner_color) = app.agent_for_owner(task.owner).map_or_else(
                || (short_agent_id(task.owner), theme::AQUA),
                |agent| (sanitize(&agent.name), app.agent_color(agent)),
            );
            let owner_width = usize::from(area.width.saturating_sub(6))
                .saturating_sub(indent.len())
                .max(3);
            let objective_width = usize::from(area.width.saturating_sub(4))
                .saturating_sub(indent.len())
                .max(3);
            let lines = vec![
                Line::from(vec![
                    Span::raw(indent.clone()),
                    Span::styled(format!("{icon} "), Style::default().fg(color)),
                    Span::styled(
                        truncate_text(&owner, owner_width),
                        Style::default()
                            .fg(owner_color)
                            .add_modifier(Modifier::BOLD),
                    ),
                ]),
                Line::from(Span::styled(
                    format!(
                        "{indent}{}",
                        truncate_text(&one_line(&sanitize(&task.objective)), objective_width)
                    ),
                    Style::default().fg(theme::MUTED),
                )),
            ];
            ListItem::new(lines).style(if index == app.selected {
                Style::default()
                    .bg(theme::ELEVATED)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            })
        })
        .collect::<Vec<_>>();
    frame.render_widget(List::new(items).block(block), area);
    if app.tasks.len() > visible {
        let mut scrollbar = ScrollbarState::new(app.tasks.len().saturating_sub(visible) + 1)
            .position(start)
            .viewport_content_length(visible);
        frame.render_stateful_widget(
            Scrollbar::new(ScrollbarOrientation::VerticalRight)
                .begin_symbol(None)
                .end_symbol(None)
                .track_style(Style::default().fg(theme::BORDER))
                .thumb_style(Style::default().fg(theme::AQUA)),
            area.inner(Margin {
                horizontal: 0,
                vertical: 1,
            }),
            &mut scrollbar,
        );
    }
}

fn render_tasks_workspace(frame: &mut ratatui::Frame<'_>, app: &mut AppModel, area: Rect) {
    let panes = app.task_layout.panes(area);
    let browser = panes[0];
    let inspector = panes[1];
    app.task_list_hitbox = browser;
    app.task_inspector_hitbox = inspector;
    app.task_expand_hitbox = Rect::default();
    app.task_retry_hitbox = Rect::default();

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
    let browser_title = format!(
        " Tasks · {} total · {active} active · {attention} attention · {completed} done ",
        app.tasks.len()
    );
    let browser_block = panel(&browser_title, true)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(if app.task_layout.dragging() {
            theme::AQUA
        } else {
            theme::BORDER
        }));

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
                    Style::default().fg(theme::TEXT),
                )),
            ])
            .wrap(Wrap { trim: true })
            .block(browser_block),
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
                    || (short_agent_id(task.owner), "●", theme::AQUA),
                    |agent| {
                        (
                            sanitize(&agent.name),
                            role_icon(&agent.role),
                            app.agent_color(agent),
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
                    Span::styled(
                        owner,
                        Style::default()
                            .fg(owner_color)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(format!("  {icon} {status}"), Style::default().fg(color)),
                    Span::styled(
                        if task.attempt > 1 {
                            format!("  ↻ attempt {}", task.attempt)
                        } else {
                            String::new()
                        },
                        Style::default().fg(theme::AQUA),
                    ),
                ];
                let lines = vec![
                    Line::from(heading),
                    Line::from(Span::styled(
                        format!("{indent}{objective}"),
                        Style::default().fg(theme::TEXT),
                    )),
                ];
                ListItem::new(lines).style(if selected {
                    Style::default()
                        .bg(theme::ELEVATED)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default()
                })
            })
            .collect::<Vec<_>>();
        frame.render_widget(List::new(items).block(browser_block), browser);
        if app.tasks.len() > visible {
            let mut scrollbar = ScrollbarState::new(app.tasks.len().saturating_sub(visible) + 1)
                .position(start)
                .viewport_content_length(visible);
            frame.render_stateful_widget(
                Scrollbar::new(ScrollbarOrientation::VerticalRight)
                    .begin_symbol(None)
                    .end_symbol(None)
                    .track_style(Style::default().fg(theme::BORDER))
                    .thumb_style(Style::default().fg(theme::AQUA)),
                browser.inner(Margin {
                    horizontal: 0,
                    vertical: 1,
                }),
                &mut scrollbar,
            );
        }
    }

    render_task_inspector(frame, app, inspector);
}

fn render_task_inspector(frame: &mut ratatui::Frame<'_>, app: &mut AppModel, inspector: Rect) {
    app.task_inspector_hitbox = inspector;
    let Some(task) = app.selected_task().cloned() else {
        frame.render_widget(
            Paragraph::new("Select or create a task to inspect its work.")
                .style(Style::default().fg(theme::TEXT))
                .block(panel(" Inspector ", true).border_type(BorderType::Rounded)),
            inspector,
        );
        return;
    };
    let mut lines = task_detail_lines(app, &task);
    lines.push(Line::from(""));
    lines.push(conversation_heading(app, &task));
    let pair = reporting_pair(app, &task);
    let mut thread = task_thread_ids(&app.tasks, task.id);
    if pair.is_some() {
        thread.retain(|id| {
            app.tasks
                .iter()
                .any(|candidate| candidate.id == *id && candidate.owner == task.owner)
        });
    }
    lines.extend(conversation_lines(
        app,
        &thread,
        inspector.width.saturating_sub(2),
        pair,
    ));
    let controls = format!(
        " [v] {} · [r] re-run · PgUp/PgDn scroll ",
        if app.expanded_output {
            "compact"
        } else {
            "details"
        }
    );
    let title = format!(" Inspector ·{controls}");
    app.task_expand_hitbox = title_action_hitbox(inspector, &title, "[v]", 4, true);
    app.task_retry_hitbox = title_action_hitbox(inspector, &title, "[r]", 4, true);
    render_chat(
        frame,
        lines,
        panel(&title, true).border_type(BorderType::Rounded),
        inspector,
        &mut app.task_inspector_scroll,
    );
}

fn render_chat(
    frame: &mut ratatui::Frame<'_>,
    lines: Vec<Line<'static>>,
    block: Block<'_>,
    area: Rect,
    from_bottom: &mut u16,
) {
    let inner = block.inner(area);
    let paragraph = Paragraph::new(lines).wrap(Wrap { trim: false });
    let height = usize::from(inner.height);
    let max_scroll = paragraph
        .line_count(inner.width)
        .saturating_sub(height)
        .min(usize::from(u16::MAX));
    *from_bottom = (*from_bottom).min(max_scroll as u16);
    let position = max_scroll.saturating_sub(usize::from(*from_bottom));
    frame.render_widget(block, area);
    frame.render_widget(paragraph.scroll((position as u16, 0)), inner);
    if max_scroll > 0 {
        let mut state = ScrollbarState::new(max_scroll.saturating_add(1))
            .position(position)
            .viewport_content_length(height);
        frame.render_stateful_widget(
            Scrollbar::new(ScrollbarOrientation::VerticalRight)
                .begin_symbol(None)
                .end_symbol(None)
                .track_style(Style::default().fg(theme::BORDER))
                .thumb_style(Style::default().fg(theme::AQUA)),
            area.inner(Margin {
                horizontal: 0,
                vertical: 1,
            }),
            &mut state,
        );
    }
}

fn same_result_content(summary: &str, message: &str) -> bool {
    let summary = one_line(&sanitize(summary)).to_lowercase();
    let message = one_line(&sanitize(message)).to_lowercase();
    !summary.is_empty()
        && (summary == message || (summary.chars().count() >= 40 && message.contains(&summary)))
}

fn task_result_is_in_conversation(app: &AppModel, task: &Task) -> bool {
    let Some(result) = &task.result else {
        return false;
    };
    app.scoped_events.iter().any(|event| {
        event.task_id == Some(task.id)
            && event.conversational
            && !event.details_only
            && event.tool_call_id.is_none()
            && (event.is_output || event.headline == Some("✓ Result"))
            && same_result_content(
                &result.summary,
                event.text.strip_prefix("result: ").unwrap_or(&event.text),
            )
    })
}

fn task_output_is_in_conversation(event: &ScopedEvent) -> bool {
    event.conversational && !event.details_only && event.tool_call_id.is_none()
}

fn conversation_result_repeats_output(app: &AppModel, event: &ScopedEvent) -> bool {
    let result_message = event.text.strip_prefix("result: ").unwrap_or(&event.text);
    app.scoped_events.iter().any(|output| {
        output.task_id == event.task_id
            && output.is_output
            && task_output_is_in_conversation(output)
            && same_result_content(result_message, &output.text)
    })
}

fn task_detail_lines(app: &AppModel, task: &Task) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    let (icon, status, color) = task_status_visual(task.status);
    let agent = app.agent_for_owner(task.owner);
    let owner = agent.map_or_else(|| short_agent_id(task.owner), |agent| sanitize(&agent.name));
    let owner_color = agent.map_or(theme::AQUA, |agent| app.agent_color(agent));
    lines.push(Line::from(vec![
        Span::styled(
            format!(" {icon} {status} "),
            Style::default().fg(color).add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            owner,
            Style::default()
                .fg(owner_color)
                .add_modifier(Modifier::BOLD),
        ),
    ]));
    if !app.expanded_output {
        push_text_lines(
            &mut lines,
            &truncate_text(&one_line(&sanitize(&task.objective)), 180),
            " ",
        );
        if let Some(result) = &task.result {
            if !task_result_is_in_conversation(app, task) {
                push_section(&mut lines, "OUTCOME", theme::GREEN);
                push_text_lines(
                    &mut lines,
                    &compact_text(&sanitize(&result.summary), false, false),
                    " ",
                );
            }
        } else if app.lead_reviews.contains_key(&task.id) {
            lines.push(Line::from(Span::styled(
                " ↶ Awaiting reporting agent review",
                Style::default().fg(theme::AQUA),
            )));
        }
        let thread = task_thread_ids(&app.tasks, task.id);
        let mut children = app
            .tasks
            .iter()
            .filter(|child| child.id != task.id && thread.contains(&child.id))
            .collect::<Vec<_>>();
        children.sort_by_key(|child| child.created_at);
        if !children.is_empty() {
            lines.push(Line::from(""));
            push_section(&mut lines, "DELEGATION", theme::AQUA);
            for child in children {
                let (icon, status, color) = task_status_visual(child.status);
                let name = app.agent_for_owner(child.owner).map_or_else(
                    || short_agent_id(child.owner),
                    |agent| sanitize(&agent.name),
                );
                let parent_agent = child
                    .parent
                    .and_then(|id| app.tasks.iter().find(|task| task.id == id))
                    .and_then(|task| app.agent_for_owner(task.owner));
                let parent = parent_agent.map_or("Team Lead", |agent| agent.name.as_str());
                let parent_color = parent_agent.map_or(theme::TEXT, |agent| app.agent_color(agent));
                let child_color = app
                    .agent_for_owner(child.owner)
                    .map_or(theme::AQUA, |agent| app.agent_color(agent));
                let stage = if child.work.as_ref().is_some_and(|work| work.integration) {
                    " · integration"
                } else {
                    ""
                };
                lines.push(Line::from(vec![
                    Span::styled(format!(" {icon} "), Style::default().fg(color)),
                    Span::styled(
                        sanitize(parent),
                        Style::default()
                            .fg(parent_color)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::raw(" → "),
                    Span::styled(
                        name,
                        Style::default()
                            .fg(child_color)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(stage, Style::default().fg(theme::MUTED)),
                    Span::styled(format!(" · {status}"), Style::default().fg(color)),
                ]));
                lines.push(Line::from(format!(
                    "   {}",
                    truncate_text(&one_line(&sanitize(&child.objective)), 90)
                )));
            }
        }
        for artifact in app
            .artifacts
            .iter()
            .filter(|artifact| artifact.producer == task.id)
            .rev()
            .take(3)
        {
            lines.push(Line::from(Span::styled(
                format!(
                    " ▸ {} · {}",
                    sanitize(&artifact.location),
                    sanitize(&artifact.kind)
                ),
                Style::default().fg(theme::PURPLE),
            )));
        }
        return lines;
    }
    lines.push(Line::from(Span::styled(
        format!(" {}  ·  attempt {}", task.id, task.attempt),
        Style::default().fg(theme::TEXT),
    )));
    if let Some(previous) = task.retry_of {
        lines.push(Line::from(Span::styled(
            format!(" re-run of {previous}"),
            Style::default().fg(theme::AQUA),
        )));
    }
    if let Some(work) = &task.work {
        push_section(
            &mut lines,
            if work.integration {
                "INTEGRATION · waits for implementations"
            } else {
                "VERIFIED WORK · patch → test → acceptance"
            },
            theme::AQUA,
        );
        lines.push(Line::from(format!(" Turn {}/32", work.turns)));
        if !work.contract.is_empty() {
            push_section(&mut lines, "SHARED CONTRACT", theme::AQUA);
            push_text_lines(
                &mut lines,
                &compact_text(&sanitize(&work.contract), app.expanded_output, false),
                "  ",
            );
        }
    }
    lines.push(Line::from(""));
    if let Some(output) = app
        .scoped_events
        .iter()
        .rev()
        .find(|e| e.task_id == Some(task.id) && e.is_output)
        .filter(|output| !task_output_is_in_conversation(output))
    {
        push_section(
            &mut lines,
            "LATEST OUTPUT · preview, not applied",
            theme::AQUA,
        );
        push_text_lines(
            &mut lines,
            &compact_text(&output.text, app.expanded_output, true),
            "  ",
        );
        lines.push(Line::from(""));
    }
    push_section(&mut lines, "OBJECTIVE", theme::AQUA);
    push_text_lines(
        &mut lines,
        &compact_text(&sanitize(&task.objective), app.expanded_output, false),
        "  ",
    );

    lines.push(Line::from(""));
    push_section(&mut lines, "ACCEPTANCE", theme::AQUA);
    if task.acceptance_criteria.is_empty() {
        lines.push(Line::from(Span::styled(
            "  No explicit criteria",
            Style::default().fg(theme::TEXT),
        )));
    } else {
        let criteria = task
            .acceptance_criteria
            .iter()
            .map(|c| format!("• {}", sanitize(c)))
            .collect::<Vec<_>>()
            .join("\n");
        push_text_lines(
            &mut lines,
            &compact_text(&criteria, app.expanded_output, false),
            "  ",
        );
    }

    if let Some(result) = &task.result
        && (!task_result_is_in_conversation(app, task) || !result.evidence.is_empty())
    {
        lines.push(Line::from(""));
        push_section(&mut lines, "RESULT", theme::GREEN);
        if !task_result_is_in_conversation(app, task) {
            push_text_lines(
                &mut lines,
                &compact_text(&sanitize(&result.summary), app.expanded_output, false),
                "  ",
            );
        }
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
                Style::default().fg(theme::TEXT),
            )));
        }
    }

    let artifacts = app
        .artifacts
        .iter()
        .filter(|artifact| artifact.producer == task.id)
        .collect::<Vec<_>>();
    let has_live_test_output = app
        .scoped_events
        .iter()
        .any(|event| event.task_id == Some(task.id) && event.headline == Some("Test output"));
    if !artifacts.is_empty() {
        lines.push(Line::from(""));
        push_section(&mut lines, "ARTIFACTS", theme::PURPLE);
        for artifact in artifacts {
            lines.push(Line::from(vec![
                Span::styled("  ◆ ", Style::default().fg(theme::PURPLE)),
                Span::styled(
                    sanitize(&artifact.location),
                    Style::default().add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    format!("  {}", sanitize(&artifact.kind)),
                    Style::default().fg(theme::TEXT),
                ),
            ]));
            if !(artifact.preview.trim().is_empty()
                || has_live_test_output && artifact.kind == "test_report")
            {
                push_text_lines(
                    &mut lines,
                    &compact_text(&sanitize(&artifact.preview), app.expanded_output, false),
                    "    ",
                );
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
        Style::default().fg(theme::TEXT),
    )));
    lines
}

fn task_thread_ids(tasks: &[Task], selected: voff_core::TaskId) -> HashSet<voff_core::TaskId> {
    let mut ids = HashSet::from([selected]);
    loop {
        let before = ids.len();
        for task in tasks {
            if task.parent.is_some_and(|id| ids.contains(&id))
                || task.retry_of.is_some_and(|id| ids.contains(&id))
            {
                ids.insert(task.id);
            }
            if ids.contains(&task.id)
                && let Some(previous) = task.retry_of
            {
                ids.insert(previous);
            }
        }
        if ids.len() == before {
            return ids;
        }
    }
}

fn reporting_pair(app: &AppModel, selected: &Task) -> Option<(AgentId, AgentId)> {
    let owner = app.agent_for_owner(selected.owner)?;
    let parent = app
        .agents
        .iter()
        .find(|agent| agent.allowed_targets.contains(&owner.name))
        .map(|agent| agent.id)
        .or_else(|| {
            selected
                .parent
                .and_then(|id| app.tasks.iter().find(|task| task.id == id))
                .map(|task| task.owner)
        })?;
    (parent != selected.owner).then_some((selected.owner, parent))
}

fn conversation_heading(app: &AppModel, selected: &Task) -> Line<'static> {
    let mut spans = vec![Span::styled(
        " CONVERSATION",
        Style::default()
            .fg(theme::TEXT)
            .add_modifier(Modifier::BOLD),
    )];
    if let Some((worker, lead)) = reporting_pair(app, selected) {
        for (separator, id) in [(" · ", worker), (" ↔ ", lead)] {
            if let Some(agent) = app.agent_for_owner(id) {
                spans.push(Span::raw(separator));
                spans.push(Span::styled(
                    sanitize(&agent.name),
                    Style::default()
                        .fg(app.agent_color(agent))
                        .add_modifier(Modifier::BOLD),
                ));
            }
        }
    }
    Line::from(spans)
}

fn role_activity_lines(app: &AppModel, selected: &Task, width: u16) -> Vec<Line<'static>> {
    let ids = app
        .tasks
        .iter()
        .filter(|task| task.owner == selected.owner)
        .map(|task| task.id)
        .collect();
    let mut heading = conversation_heading(app, selected);
    heading.spans.push(Span::raw(" · [v] details"));
    let mut lines = vec![heading];
    lines.extend(conversation_lines(
        app,
        &ids,
        width,
        reporting_pair(app, selected),
    ));
    lines
}

fn conversation_lines(
    app: &AppModel,
    ids: &HashSet<voff_core::TaskId>,
    width: u16,
    pair: Option<(AgentId, AgentId)>,
) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    let mut previous_thread = None;
    let mut previous_entry = None;
    let completed_reviews: HashSet<_> = app
        .scoped_events
        .iter()
        .filter_map(|event| {
            event
                .review
                .as_ref()
                .filter(|review| review.decision.is_some())
                .and(event.approval_id.map(|id| (event.task_id, id)))
        })
        .collect();
    for event in &app.scoped_events {
        let Some(task_id) = event.task_id else {
            continue;
        };
        if !event.conversational || (!app.expanded_output && event.details_only) {
            continue;
        }
        // Expanding a conversation reveals full messages, not provider envelopes
        // or duplicate proposal/review bookkeeping. Raw events remain in `e`.
        if (event.is_output && event.details_only)
            || event.tool_call_id.is_some()
            || (event
                .review
                .as_ref()
                .is_some_and(|review| review.decision.is_none())
                && event
                    .approval_id
                    .is_some_and(|id| completed_reviews.contains(&(event.task_id, id))))
        {
            continue;
        }
        if (!ids.contains(&task_id) && !event.related_task.is_some_and(|id| ids.contains(&id)))
            || (!app.expanded_output && event.headline.is_none())
        {
            continue;
        }
        let Some(task) = app.tasks.iter().find(|task| task.id == task_id) else {
            continue;
        };
        let source = event
            .source_task
            .and_then(|id| app.tasks.iter().find(|task| task.id == id))
            .unwrap_or(task);
        if let Some((worker, lead)) = pair
            && (task.owner != worker
                || event
                    .review
                    .as_ref()
                    .is_some_and(|review| review.reviewer != lead)
                || task
                    .parent
                    .and_then(|id| app.tasks.iter().find(|parent| parent.id == id))
                    .is_some_and(|parent| parent.owner != lead)
                || (event.source_task.is_some() && source.owner != lead))
        {
            continue;
        }
        if event.headline == Some("✓ Result") && conversation_result_repeats_output(app, event) {
            continue;
        }
        let color = app
            .agent_for_owner(event.speaker.unwrap_or(source.owner))
            .map_or(event.accent, |agent| app.agent_color(agent));
        let owner = app.agent_for_owner(source.owner).map_or_else(
            || short_agent_id(source.owner),
            |agent| sanitize(&agent.name),
        );
        let related = event
            .related_task
            .and_then(|id| app.tasks.iter().find(|task| task.id == id));
        let approval_id = event.approval_id.or_else(|| {
            event.tool_call_id.and_then(|id| {
                app.approvals
                    .iter()
                    .find(|approval| approval.tool_call_id == id)
                    .map(|approval| approval.id)
            })
        });
        let thread = (
            task_id,
            if app.expanded_output {
                approval_id
            } else {
                None
            },
        );
        let connected = previous_thread == Some(thread);
        let action = approval_id
            .and_then(|id| app.approvals.iter().find(|approval| approval.id == id))
            .map_or("proposal", |approval| approval.action.as_str());
        if !connected {
            let heading = format!(" {owner}");
            let heading_color = app
                .agent_for_owner(source.owner)
                .map_or(theme::TEXT, |agent| app.agent_color(agent));
            for row in textwrap::wrap(&heading, usize::from(width.max(1))) {
                lines.push(Line::from(Span::styled(
                    row.into_owned(),
                    Style::default()
                        .fg(heading_color)
                        .add_modifier(Modifier::BOLD),
                )));
            }
        }
        let mut title = event.headline.unwrap_or("Details").to_owned();
        if event.is_output && event.headline != Some("Test output") {
            title.clear();
        } else if title == "✓ Result" {
            title = "✓".into();
        }
        let mut delegation_label = None;
        if let Some(target) = related.filter(|target| target.owner != source.owner) {
            let target_name = app.agent_for_owner(target.owner).map_or_else(
                || short_agent_id(target.owner),
                |agent| sanitize(&agent.name),
            );
            let label = format!("{owner} → {target_name}");
            title = format!("↳ {label}");
            delegation_label = Some((
                label,
                owner.len(),
                app.agent_for_owner(target.owner)
                    .map_or(theme::TEXT, |agent| app.agent_color(agent)),
            ));
        }
        if task.attempt > 1 && app.expanded_output {
            title.push_str(&format!(" · attempt {}", task.attempt));
        }
        let mut reviewer_highlight = None;
        let detail = if let Some(review) = &event.review {
            let reviewer = app.agent_for_owner(review.reviewer).map_or_else(
                || short_agent_id(review.reviewer),
                |agent| sanitize(&agent.name),
            );
            if review.reviewer != task.owner {
                let start = match &review.decision {
                    Some((true, _)) => 0,
                    Some((false, _)) => "↶ ".len(),
                    None => "↶ Waiting for ".len(),
                };
                reviewer_highlight = Some((
                    start..start + reviewer.len(),
                    app.agent_for_owner(review.reviewer)
                        .map_or(theme::TEXT, |agent| app.agent_color(agent)),
                ));
            }
            if let Some((approved, reason)) = &review.decision {
                title = if review.reviewer == task.owner {
                    if *approved {
                        "✓ Approved"
                    } else {
                        "↶ Changes requested"
                    }
                    .into()
                } else {
                    format!(
                        "{} {reviewer} {}",
                        if *approved { "✓" } else { "↶" },
                        if *approved {
                            "approved"
                        } else {
                            "requested changes"
                        }
                    )
                };
                if approval_id.is_some() {
                    title.push_str(&format!(" · {}", conversation_action(action)));
                }
                reason.clone()
            } else {
                title = if review.reviewer == task.owner {
                    "↶ Reviewing".into()
                } else {
                    format!("↶ Waiting for {reviewer}")
                };
                if approval_id.is_some() {
                    format!("Checking whether to allow {}.", conversation_action(action))
                } else {
                    "Diagnosing the failed attempt.".into()
                }
            }
        } else if let Some(related) = related {
            sanitize(&related.objective)
        } else if event.is_output {
            let output = sanitize(&event.text);
            if event.headline == Some("Test output") {
                process_output_preview(&output, width, if app.expanded_output { 79 } else { 3 })
            } else {
                output
            }
        } else {
            event
                .text
                .strip_prefix("result: ")
                .unwrap_or(&event.text)
                .to_owned()
        };
        let start = lines.len();
        push_activity_entry(
            &mut lines,
            &title,
            &detail,
            color,
            width,
            if app.expanded_output {
                usize::MAX
            } else if event.headline == Some("Test output") {
                4
            } else {
                3
            },
        );
        if let Some((label, owner_len, target_color)) = delegation_label {
            color_delegation_label(
                &mut lines[start..],
                &label,
                owner_len,
                color,
                target_color,
                width,
            );
        }
        if let Some(highlight) = reviewer_highlight {
            let label = title.strip_prefix('✓').map_or(title.as_str(), str::trim);
            color_activity_label(&mut lines[start..], label, &[highlight], width);
        }
        if connected && let Some(index) = previous_entry {
            let previous: &mut Line<'static> = &mut lines[index];
            let connector_style = previous
                .spans
                .first()
                .map_or(Style::default(), |span| span.style);
            if let Some(span) = previous.spans.first_mut() {
                span.content = span.content.replacen(" ╰", " ├", 1).into();
            }
            // Only entries with a following sibling carry a vertical line
            // through their wrapped body and the gap before that sibling.
            for line in &mut lines[index + 1..start - 1] {
                if let Some(span) = line.spans.first_mut() {
                    span.content = " │   ".into();
                }
            }
            lines[start - 1] = Line::from(Span::styled(" │", connector_style));
        }
        previous_entry = Some(start);
        previous_thread = Some(thread);
    }
    if lines.is_empty() {
        lines.push(Line::from(
            " No conversation messages yet. [v] work details",
        ));
    }
    lines
}

fn conversation_action(action: &str) -> &str {
    match action {
        "process.test" => "running checks",
        "file.patch" => "editing files",
        "file.read" => "reading files",
        "task.complete" => "accepting the result",
        "task.plan" | "task.delegate" => "delegating work",
        _ => "the next step",
    }
}

fn process_output_preview(output: &str, width: u16, visible_lines: usize) -> String {
    let inner = usize::from(width.saturating_sub(6).max(1));
    let tail_chars = inner.saturating_mul(visible_lines.max(1));
    let start = output
        .char_indices()
        .rev()
        .nth(tail_chars.saturating_sub(1))
        .map_or(0, |(index, _)| index);
    let wrapped = output[start..]
        .lines()
        .flat_map(|line| textwrap::wrap(line, inner))
        .map(|line| line.into_owned())
        .collect::<Vec<_>>();
    let start = wrapped.len().saturating_sub(visible_lines);
    wrapped[start..].join("\n")
}

fn push_activity_entry(
    lines: &mut Vec<Line<'static>>,
    title: &str,
    body: &str,
    color: Color,
    width: u16,
    limit: usize,
) {
    let inner = usize::from(width.saturating_sub(6).max(1));
    let (marker, label) = if title.is_empty() {
        ("•", "")
    } else if let Some(label) = title.strip_prefix("✓") {
        ("✓", label.trim())
    } else if let Some(label) = title.strip_prefix("↳") {
        ("↳", label.trim())
    } else {
        ("•", title)
    };
    // Bound preview work and preserve the beginning of a sentence, rather than
    // cutting into a word at the tail of the provider output.
    let end = body
        .char_indices()
        .nth(inner.saturating_mul(limit))
        .map_or(body.len(), |(i, _)| i);
    let content = if label.is_empty() {
        body[..end].trim().to_owned()
    } else {
        format!("{label}\n{}", body[..end].trim())
    };
    let wrapped = textwrap::wrap(&content, inner);
    let folded = end < body.len() || wrapped.len() > limit;
    let count = wrapped.len().min(limit);
    for (index, line) in wrapped.iter().take(limit).enumerate() {
        let mut text = line.to_string();
        if folded && index + 1 == count {
            text = truncate_text(&format!("{text} …"), inner);
        }
        lines.push(Line::from(vec![
            Span::styled(
                if index == 0 {
                    format!(" ╰─{marker} ")
                } else {
                    "     ".into()
                },
                Style::default().fg(color).add_modifier(Modifier::BOLD),
            ),
            Span::styled(text, Style::default().fg(theme::TEXT)),
        ]));
    }
    lines.push(Line::from(""));
}

fn color_delegation_label(
    lines: &mut [Line<'static>],
    label: &str,
    owner_len: usize,
    owner_color: Color,
    target_color: Color,
    width: u16,
) {
    color_activity_label(
        lines,
        label,
        &[
            (0..owner_len, owner_color),
            (owner_len + " → ".len()..label.len(), target_color),
        ],
        width,
    );
}

fn color_activity_label(
    lines: &mut [Line<'static>],
    label: &str,
    highlights: &[(std::ops::Range<usize>, Color)],
    width: u16,
) {
    let mut offset = 0;
    for (line, row) in lines.iter_mut().zip(textwrap::wrap(
        label,
        usize::from(width.saturating_sub(6).max(1)),
    )) {
        offset += label[offset..].find(row.as_ref()).unwrap_or(0);
        let Some(body) = line.spans.get(1) else { break };
        let text = body.content.to_string();
        line.spans.truncate(1);
        for (index, ch) in text.char_indices() {
            let position = offset + index;
            let color = if index < row.len() {
                highlights
                    .iter()
                    .find(|(range, _)| range.contains(&position))
                    .map_or(theme::TEXT, |(_, color)| *color)
            } else {
                theme::TEXT
            };
            line.spans.push(Span::styled(
                ch.to_string(),
                Style::default().fg(color).add_modifier(Modifier::BOLD),
            ));
        }
        offset += row.len();
    }
}

fn push_section(lines: &mut Vec<Line<'static>>, title: &str, color: Color) {
    lines.push(Line::from(Span::styled(
        format!(" {title}"),
        Style::default().fg(color).add_modifier(Modifier::BOLD),
    )));
}

// Display-only folding: full persisted output remains available with `v`.
fn compact_text(text: &str, expanded: bool, tail: bool) -> String {
    if expanded {
        return text.to_owned();
    }
    // Inspect only the visible edge, not megabytes of earlier code per frame.
    let bounded = if tail {
        let start = text
            .char_indices()
            .rev()
            .nth(239)
            .map_or(0, |(index, _)| index);
        &text[start..]
    } else {
        let end = text
            .char_indices()
            .nth(480)
            .map_or(text.len(), |(index, _)| index);
        &text[..end]
    };
    let lines = bounded.lines().collect::<Vec<_>>();
    let hidden = bounded.len() < text.len() || lines.len() > if tail { 3 } else { 5 };
    let excerpt = if tail {
        lines[lines.len().saturating_sub(3)..].join("\n")
    } else {
        lines.into_iter().take(5).collect::<Vec<_>>().join("\n")
    };
    if hidden {
        if tail {
            format!("… earlier output hidden · [v] expand\n{excerpt}")
        } else {
            format!("{excerpt}\n… more hidden · [v] expand")
        }
    } else {
        excerpt
    }
}

fn menu_tab(label: &str, active: bool) -> Span<'static> {
    Span::styled(
        format!(" {label} "),
        if active {
            Style::default()
                .fg(theme::TEXT)
                .bg(theme::BORDER)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme::TEXT).bg(theme::ELEVATED)
        },
    )
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
            .fg(theme::PURPLE)
            .add_modifier(Modifier::BOLD),
    ))
}

fn help_section(title: &str) -> Line<'static> {
    Line::from(Span::styled(
        format!("  {title} "),
        Style::default()
            .fg(theme::ORANGE)
            .add_modifier(Modifier::BOLD),
    ))
}

fn help_binding(key: &str, action: &str) -> Line<'static> {
    Line::from(vec![
        Span::styled(
            format!("    {key:<14}"),
            Style::default()
                .fg(theme::TEXT)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(action.to_owned(), Style::default().fg(theme::TEXT)),
    ])
}

fn help_lines(compact: bool) -> Vec<Line<'static>> {
    if compact {
        vec![
            help_section("NAVIGATION"),
            help_binding("1 / 2", "Team / tasks"),
            help_binding("i", "Inspect tasks"),
            help_binding("Tab", "Switch tabs"),
            help_binding("t / h", "Focus / toggle pane"),
            help_binding("j/k / click", "Browse / select"),
            help_binding("o / +", "Open agent tab"),
            help_binding("Ctrl+W", "Close tab"),
            help_section("TASKS & EVENTS"),
            help_binding("e / v", "Events / details"),
            help_binding("u", "Usage & cost"),
            help_binding("PgUp / Dn", "Scroll inspector"),
            help_binding("r / x", "Retry / cancel"),
            help_section("TEAM & PROMPT"),
            help_binding("a / Del", "Add / remove agent"),
            help_binding("c", "Configure / close tab"),
            help_binding("w", "Workspace"),
            help_binding("+/-/0/f", "Zoom / reset / fit"),
            help_binding("Ctrl+arrows", "Move agent card"),
            help_binding("Enter / Shift+Enter", "Message / new line"),
            help_binding("↑ / ↓", "Prompt history"),
            help_binding("Ctrl+Z / Y", "Undo / redo"),
            Line::from(""),
        ]
    } else {
        vec![
            help_section("NAVIGATION"),
            help_binding("1 / 2", "Team / tasks"),
            help_binding("i", "Inspect tasks"),
            help_binding("Tab", "Switch tabs"),
            help_binding("t", "Focus tasks / team"),
            help_binding("h", "Show / hide tasks"),
            help_binding("j / k", "Browse agents"),
            help_binding("Click", "Select agent"),
            help_binding("o / +", "Open agent tab"),
            help_binding("Ctrl+W", "Close agent tab"),
            help_section("TASKS & EVENTS"),
            help_binding("e", "Events"),
            help_binding("u", "Usage & cost"),
            help_binding("v", "Output details"),
            help_binding("PgUp / Dn", "Scroll inspector"),
            help_binding("r", "Retry task"),
            help_binding("x", "Cancel task"),
            help_section("TEAM & PROMPT"),
            help_binding("a", "Add agent"),
            help_binding("Del", "Remove agent"),
            help_binding("c", "Configure / close tab"),
            help_binding("w", "Workspace"),
            help_binding("+ / - / 0 / f", "Zoom / reset / fit"),
            help_binding("Ctrl+arrows", "Move agent card"),
            help_binding("Enter", "Message agent"),
            help_binding("Shift+Enter", "New line"),
            help_binding("↑ / ↓", "Prompt history"),
            help_binding("Ctrl+Z / Y", "Undo / redo"),
            Line::from(""),
        ]
    }
}

fn help_lines_width(lines: &[Line<'_>]) -> u16 {
    lines
        .iter()
        .map(|line| u16::try_from(line.width()).unwrap_or(u16::MAX))
        .max()
        .unwrap_or(0)
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

fn help_popup_area(area: Rect) -> Rect {
    help_popup_layout(area).0
}

fn help_popup_layout(area: Rect) -> (Rect, bool, bool) {
    let full_help = help_lines(false);
    let compact_help = help_lines(true);
    let logo = crate::onboarding::animated_logo_text_for(theme::SURFACE);
    let logo_width = logo.as_ref().map_or(0, |image| {
        image.lines.iter().map(Line::width).max().unwrap_or(0) as u16
    });
    let logo_height = logo.as_ref().map_or(0, |image| image.lines.len() as u16);
    let max_width = area.width.saturating_sub(2).min(104);
    let max_height = area.height.saturating_sub(2).min(34);
    let full_width = help_lines_width(&full_help);
    let compact_width = help_lines_width(&compact_help);
    let full_height = u16::try_from(full_help.len()).unwrap_or(u16::MAX);
    let use_compact = max_height.saturating_sub(2) < full_height;
    let help_width = if use_compact {
        compact_width
    } else {
        full_width
    };
    let can_show_logo = logo_width
        .saturating_add(2)
        .saturating_add(help_width)
        .saturating_add(4)
        <= max_width
        && logo_height.saturating_add(2) <= max_height;
    let width = if can_show_logo {
        logo_width
            .saturating_add(2)
            .saturating_add(help_width)
            .saturating_add(4)
    } else {
        help_width.saturating_add(4)
    }
    .min(max_width);
    let content_height = if can_show_logo {
        logo_height.max(if use_compact {
            u16::try_from(compact_help.len()).unwrap_or(u16::MAX)
        } else {
            full_height
        })
    } else if use_compact {
        u16::try_from(compact_help.len()).unwrap_or(u16::MAX)
    } else {
        full_height
    };
    let height = content_height.saturating_add(2).min(max_height);
    (
        centered_popup(area, width, height),
        use_compact,
        can_show_logo,
    )
}

fn delete_confirmation_area(area: Rect) -> Rect {
    centered_popup(area, area.width.saturating_sub(4).min(58), 7)
}

fn title_action_hitbox(area: Rect, title: &str, label: &str, width: u16, top: bool) -> Rect {
    let Some(index) = title.find(label) else {
        return Rect::default();
    };
    let x = area
        .x
        .saturating_add(1)
        .saturating_add(title[..index].chars().count() as u16);
    let available = area.right().saturating_sub(1).saturating_sub(x);
    Rect::new(
        x,
        if top {
            area.y
        } else {
            area.bottom().saturating_sub(1)
        },
        width.min(available),
        1,
    )
}

fn task_status_visual(status: TaskStatus) -> (&'static str, &'static str, Color) {
    match status {
        TaskStatus::Queued => ("○", "queued", theme::BLUE),
        TaskStatus::Ready => ("◇", "ready", theme::BLUE),
        TaskStatus::Running => ("▶", "running", theme::AQUA),
        TaskStatus::WaitingApproval => ("◆", "approval", theme::YELLOW),
        TaskStatus::Blocked => ("!", "blocked", theme::RED),
        TaskStatus::Completed => ("✓", "completed", theme::GREEN),
        TaskStatus::Failed => ("×", "failed", theme::RED),
        TaskStatus::Cancelled => ("–", "cancelled", theme::RED),
    }
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
    let task_pane_focused = app.task_pane_focused && !app.task_pane_hitbox.is_empty();
    let focus_hint = if app.task_pane_hitbox.is_empty() {
        if app.task_pane_visible {
            "[h] hide tasks"
        } else {
            "[h] show tasks"
        }
    } else if task_pane_focused {
        "[t] focus team"
    } else {
        "[t] focus tasks"
    };
    let title = format!(
        " Team · {handoff} · {focus_hint} · [o] open agent · [a] add · [c] config · +/- zoom · f fit "
    );
    app.team_edit_hitbox = title_action_hitbox(area, &title, "[c]", 8, true);
    app.team_add_hitbox = title_action_hitbox(area, &title, "[a]", 7, true);
    frame.render_widget(
        panel(&title, !task_pane_focused).border_type(BorderType::Rounded),
        area,
    );
    let inner = area.inner(Margin {
        horizontal: 2,
        vertical: 1,
    });
    if app.agents.is_empty() || inner.width < 8 || inner.height < 4 {
        frame.render_widget(
            Paragraph::new("No configured developers.").style(Style::default().fg(theme::TEXT)),
            inner,
        );
        return;
    }
    app.team_flow.sync(
        TeamSnapshot {
            agents: &app.agents,
            tasks: &app.tasks,
            approvals: &app.approvals,
            reviews: &app.lead_reviews,
            responses: &app.agent_responses,
            usage: &app.agent_usage,
            runtime_models: &app.agent_runtime_models,
            estimated_costs: &app.agent_estimated_costs,
            provider_kinds: &app.agent_provider_kinds,
            team_lead: app.team_lead,
        },
        app.selected_agent,
        inner,
    );
    app.team_flow.render(frame, inner);
}

#[derive(Clone)]
struct AgentUsageRow {
    name: String,
    provider: String,
    model: Option<String>,
    usage: TokenUsage,
    estimated_cost: Option<f64>,
    task_count: usize,
    completed_count: usize,
    color: Color,
}

impl AgentUsageRow {
    fn token_count(&self) -> u64 {
        self.usage
            .input_tokens
            .saturating_add(self.usage.output_tokens)
    }
}

fn agent_usage_rows(app: &AppModel) -> Vec<AgentUsageRow> {
    let mut profiles = app.agents.iter().collect::<Vec<_>>();
    profiles.extend(app.known_agents.values().filter(|agent| {
        app.agent_usage.contains_key(&agent.id)
            && !app.agents.iter().any(|current| current.id == agent.id)
    }));

    let mut rows = profiles
        .into_iter()
        .map(|agent| {
            let provider = app
                .agent_provider_kinds
                .get(&agent.id)
                .map_or(agent.provider.as_str(), String::as_str);
            let usage = app.agent_usage.get(&agent.id).copied().unwrap_or_default();
            let has_usage = usage.input_tokens > 0 || usage.output_tokens > 0;
            let task_count = app
                .tasks
                .iter()
                .filter(|task| task.owner == agent.id)
                .count();
            let completed_count = app
                .tasks
                .iter()
                .filter(|task| task.owner == agent.id && task.status == TaskStatus::Completed)
                .count();

            AgentUsageRow {
                name: sanitize(&agent.name),
                provider: match provider {
                    "codex_cli" => "Codex CLI",
                    "claude_cli" => "Claude CLI",
                    "ollama" => "Ollama",
                    "openai" => "OpenAI API",
                    "anthropic" => "Claude API",
                    other => other,
                }
                .to_owned(),
                model: app
                    .agent_runtime_models
                    .get(&agent.id)
                    .map(String::as_str)
                    .filter(|model| !model.trim().is_empty() && *model != "default")
                    .or_else(|| {
                        (!agent.model.trim().is_empty() && agent.model != "default")
                            .then_some(agent.model.as_str())
                    })
                    .map(sanitize),
                usage,
                estimated_cost: has_usage
                    .then(|| {
                        app.agent_estimated_costs
                            .get(&agent.id)
                            .copied()
                            .unwrap_or_else(|| {
                                let model = app
                                    .agent_runtime_models
                                    .get(&agent.id)
                                    .map_or(agent.model.as_str(), String::as_str);
                                estimate_cost(provider, model, usage)
                            })
                    })
                    .flatten(),
                task_count,
                completed_count,
                color: app.agent_color(agent),
            }
        })
        .collect::<Vec<_>>();
    rows.sort_by(|left, right| {
        right
            .token_count()
            .cmp(&left.token_count())
            .then_with(|| right.task_count.cmp(&left.task_count))
            .then_with(|| {
                right
                    .estimated_cost
                    .unwrap_or_default()
                    .total_cmp(&left.estimated_cost.unwrap_or_default())
            })
            .then_with(|| left.name.cmp(&right.name))
    });
    rows
}

fn usage_popup_area(area: Rect) -> Rect {
    centered_popup(
        area,
        area.width.saturating_sub(4).min(104),
        area.height.saturating_sub(2).min(30),
    )
}

fn usage_overview(rows: &[AgentUsageRow], tasks: &[Task], width: u16) -> Vec<Line<'static>> {
    let input_tokens = rows
        .iter()
        .map(|row| row.usage.input_tokens)
        .fold(0, u64::saturating_add);
    let output_tokens = rows
        .iter()
        .map(|row| row.usage.output_tokens)
        .fold(0, u64::saturating_add);
    let used_agents = rows.iter().filter(|row| row.token_count() > 0).count();
    let priced_agents = rows
        .iter()
        .filter(|row| row.token_count() > 0 && row.estimated_cost.is_some())
        .count();
    let total_cost = rows
        .iter()
        .filter_map(|row| row.estimated_cost)
        .sum::<f64>();
    let total_cost = (priced_agents > 0).then_some(total_cost);
    let pricing_coverage = format!("{priced_agents}/{used_agents}");
    let completed_tasks = tasks
        .iter()
        .filter(|task| task.status == TaskStatus::Completed)
        .count();
    let cost_text = total_cost.map_or_else(|| "unavailable".to_owned(), format_usd);

    if width < 68 {
        let mut lines = vec![Line::from(vec![
            Span::styled(
                "Estimated cost: ",
                Style::default()
                    .fg(theme::ORANGE)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(cost_text, Style::default().fg(theme::TEXT)),
            Span::styled(" | ", Style::default().fg(theme::MUTED)),
            Span::styled("Priced agents: ", Style::default().fg(theme::MUTED)),
            Span::styled(pricing_coverage, Style::default().fg(theme::TEXT)),
        ])];
        lines.push(Line::from(vec![
            Span::styled("Tokens in: ", Style::default().fg(theme::AQUA)),
            Span::styled(
                compact_count(input_tokens),
                Style::default().fg(theme::AQUA),
            ),
            Span::styled(" | ", Style::default().fg(theme::MUTED)),
            Span::styled("Tokens out: ", Style::default().fg(theme::PURPLE)),
            Span::styled(
                compact_count(output_tokens),
                Style::default().fg(theme::PURPLE),
            ),
            Span::styled(" | ", Style::default().fg(theme::MUTED)),
            Span::styled("Tasks: ", Style::default().fg(theme::ORANGE)),
            Span::styled(
                format!("{completed_tasks}/{}", tasks.len()),
                Style::default().fg(theme::TEXT),
            ),
        ]));
        return lines;
    }

    let mut lines = vec![Line::from(vec![
        Span::styled(
            "Estimated cost: ",
            Style::default()
                .fg(theme::ORANGE)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            cost_text,
            Style::default()
                .fg(theme::TEXT)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(" | ", Style::default().fg(theme::MUTED)),
        Span::styled("Priced agents: ", Style::default().fg(theme::MUTED)),
        Span::styled(pricing_coverage, Style::default().fg(theme::TEXT)),
    ])];
    lines.push(Line::from(vec![
        Span::styled("Tokens in: ", Style::default().fg(theme::AQUA)),
        Span::styled(
            compact_count(input_tokens),
            Style::default().fg(theme::AQUA),
        ),
        Span::styled(" | ", Style::default().fg(theme::MUTED)),
        Span::styled("Tokens out: ", Style::default().fg(theme::PURPLE)),
        Span::styled(
            compact_count(output_tokens),
            Style::default().fg(theme::PURPLE),
        ),
        Span::styled(" | ", Style::default().fg(theme::MUTED)),
        Span::styled("Tasks: ", Style::default().fg(theme::ORANGE)),
        Span::styled(
            format!("{completed_tasks}/{}", tasks.len()),
            Style::default().fg(theme::TEXT),
        ),
    ]));
    lines
}

fn usage_column(value: &str, width: usize, style: Style) -> Span<'static> {
    let value = truncate_text(value, width);
    let padding = width.saturating_sub(UnicodeWidthStr::width(value.as_str()));
    Span::styled(format!("{value}{}", " ".repeat(padding)), style)
}

fn usage_agent_lines(rows: &[AgentUsageRow], total_cost: f64, width: u16) -> Vec<Line<'static>> {
    let inner_width = usize::from(width);
    if inner_width < 52 {
        let mut lines = Vec::with_capacity(rows.len().saturating_mul(3));
        for row in rows {
            let cost = row.estimated_cost.map_or_else(
                || {
                    if row.token_count() > 0 {
                        "n/a".to_owned()
                    } else {
                        "no usage".to_owned()
                    }
                },
                format_usd,
            );
            let share = if total_cost > 0.0 {
                row.estimated_cost
                    .map(|cost| format!(" · {:.0}%", cost / total_cost * 100.0))
                    .unwrap_or_default()
            } else {
                String::new()
            };
            lines.push(Line::from(vec![
                Span::styled("● ", Style::default().fg(row.color)),
                Span::styled(
                    row.name.clone(),
                    Style::default().fg(row.color).add_modifier(Modifier::BOLD),
                ),
                row.model.as_ref().map_or_else(
                    || Span::raw(""),
                    |model| Span::styled(format!(" · {model}"), Style::default().fg(theme::MUTED)),
                ),
                Span::styled(
                    format!(" · {cost}{share}"),
                    Style::default().fg(if row.estimated_cost.is_none() && row.token_count() > 0 {
                        theme::MUTED
                    } else {
                        theme::TEXT
                    }),
                ),
            ]));
            lines.push(Line::from(vec![
                Span::styled("  ", Style::default().fg(theme::MUTED)),
                Span::styled(row.provider.clone(), Style::default().fg(theme::MUTED)),
            ]));
            lines.push(Line::from(vec![
                Span::styled(
                    format!("  In {}", compact_count(row.usage.input_tokens)),
                    Style::default().fg(theme::AQUA),
                ),
                Span::styled(
                    format!(" / Out {}", compact_count(row.usage.output_tokens)),
                    Style::default().fg(theme::PURPLE),
                ),
                Span::styled(
                    format!(" · Tasks {}/{}", row.completed_count, row.task_count),
                    Style::default().fg(theme::MUTED),
                ),
            ]));
        }
        if rows.is_empty() {
            lines.push(Line::from(Span::styled(
                "No agents configured",
                Style::default().fg(theme::MUTED),
            )));
        }
        return lines;
    }

    let cost_width = 10;
    let input_width = 8;
    let output_width = 8;
    let tasks_width = 10;
    let separators = 4;
    let agent_width = inner_width
        .saturating_sub(cost_width + input_width + output_width + tasks_width + separators)
        .max(12);
    let mut lines = vec![Line::from(vec![
        usage_column("Agent", agent_width, Style::default().fg(theme::MUTED)),
        Span::raw(" "),
        usage_column("Est. cost", cost_width, Style::default().fg(theme::ORANGE)),
        Span::raw(" "),
        usage_column("In", input_width, Style::default().fg(theme::AQUA)),
        Span::raw(" "),
        usage_column("Out", output_width, Style::default().fg(theme::PURPLE)),
        Span::raw(" "),
        usage_column("Tasks", tasks_width, Style::default().fg(theme::MUTED)),
    ])];

    for row in rows {
        let cost = row.estimated_cost.map_or_else(
            || {
                if row.token_count() > 0 {
                    "n/a".to_owned()
                } else {
                    "—".to_owned()
                }
            },
            format_usd,
        );
        let cost_share = if total_cost > 0.0 {
            row.estimated_cost
                .map(|cost| format!("{:.0}% share", cost / total_cost * 100.0))
                .unwrap_or_else(|| "not priced".to_owned())
        } else {
            String::new()
        };
        let task_count = format!("{}/{} done", row.completed_count, row.task_count);
        let cost_style = if row.estimated_cost.is_none() && row.token_count() > 0 {
            Style::default().fg(theme::MUTED)
        } else {
            Style::default().fg(theme::TEXT)
        };
        let agent_label = row.model.as_ref().map_or_else(
            || row.name.clone(),
            |model| format!("{} · {model}", row.name),
        );
        lines.push(Line::from(vec![
            usage_column(
                &agent_label,
                agent_width,
                Style::default().fg(row.color).add_modifier(Modifier::BOLD),
            ),
            Span::raw(" "),
            usage_column(&cost, cost_width, cost_style),
            Span::raw(" "),
            usage_column(
                &compact_count(row.usage.input_tokens),
                input_width,
                Style::default().fg(theme::AQUA),
            ),
            Span::raw(" "),
            usage_column(
                &compact_count(row.usage.output_tokens),
                output_width,
                Style::default().fg(theme::PURPLE),
            ),
            Span::raw(" "),
            usage_column(&task_count, tasks_width, Style::default().fg(theme::TEXT)),
        ]));
        lines.push(Line::from(vec![
            usage_column(
                &row.provider,
                agent_width,
                Style::default().fg(theme::MUTED),
            ),
            Span::raw(" "),
            usage_column(&cost_share, cost_width, Style::default().fg(theme::MUTED)),
        ]));
    }
    if rows.is_empty() {
        lines.push(Line::from(Span::styled(
            "No agents configured",
            Style::default().fg(theme::MUTED),
        )));
    }
    lines
}

#[derive(Clone, Copy)]
enum UsageChartMetric {
    Cost,
    Tokens,
    Tasks,
}

impl UsageChartMetric {
    fn title(self) -> &'static str {
        match self {
            Self::Cost => "COST SHARE",
            Self::Tokens => "TOKEN SHARE",
            Self::Tasks => "TASK SHARE",
        }
    }
}

fn usage_chart_slices(rows: &[AgentUsageRow]) -> (Option<UsageChartMetric>, Vec<PieSlice<'_>>) {
    let cost_slices = rows
        .iter()
        .filter_map(|row| {
            row.estimated_cost
                .filter(|cost| *cost > 0.0)
                .map(|cost| PieSlice::new(row.name.as_str(), cost, row.color))
        })
        .collect::<Vec<_>>();
    if !cost_slices.is_empty() {
        return (Some(UsageChartMetric::Cost), cost_slices);
    }

    let token_slices = rows
        .iter()
        .filter(|row| row.token_count() > 0)
        .map(|row| PieSlice::new(row.name.as_str(), row.token_count() as f64, row.color))
        .collect::<Vec<_>>();
    if !token_slices.is_empty() {
        return (Some(UsageChartMetric::Tokens), token_slices);
    }

    let task_slices = rows
        .iter()
        .filter(|row| row.task_count > 0)
        .map(|row| PieSlice::new(row.name.as_str(), row.task_count as f64, row.color))
        .collect::<Vec<_>>();
    if !task_slices.is_empty() {
        return (Some(UsageChartMetric::Tasks), task_slices);
    }

    (None, Vec::new())
}

fn render_usage_popup(frame: &mut ratatui::Frame<'_>, app: &AppModel) {
    let popup = usage_popup_area(frame.area());
    frame.render_widget(Clear, popup);
    let rows = agent_usage_rows(app);
    let total_cost = rows
        .iter()
        .filter_map(|row| row.estimated_cost)
        .sum::<f64>();
    let total_cost = if rows.iter().any(|row| row.estimated_cost.is_some()) {
        total_cost
    } else {
        0.0
    };
    let block = Block::default()
        .title_bottom(
            Line::from(" ↑/↓ scroll · catalog prices are estimates · [u] / Esc close ")
                .style(Style::default().fg(theme::AQUA))
                .alignment(Alignment::Center),
        )
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .style(theme::surface())
        .border_style(Style::default().fg(theme::BORDER));
    let inner = block.inner(popup);
    frame.render_widget(block, popup);
    if inner.is_empty() {
        return;
    }
    let overview = Paragraph::new(usage_overview(&rows, &app.tasks, inner.width))
        .wrap(Wrap { trim: true })
        .style(theme::surface());
    let overview_height = u16::try_from(overview.line_count(inner.width))
        .unwrap_or(inner.height)
        .min(inner.height);

    let sections = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(overview_height), Constraint::Min(0)])
        .split(inner);
    frame.render_widget(overview, sections[0]);
    let body = sections[1];
    let (metric, slices) = usage_chart_slices(&rows);
    let (chart_area, list_area) = if body.width >= 78 {
        let columns = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(30), Constraint::Percentage(70)])
            .split(body);
        (columns[0], columns[1])
    } else {
        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(body.height.min(if body.height >= 10 { 7 } else { 5 })),
                Constraint::Min(0),
            ])
            .split(body);
        (rows[0], rows[1])
    };

    let chart_title = metric.map_or("ACTIVITY", UsageChartMetric::title);
    let chart_block = Block::default()
        .title(
            Line::from(format!(" {chart_title} ")).style(
                Style::default()
                    .fg(theme::ORANGE)
                    .add_modifier(Modifier::BOLD),
            ),
        )
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .style(theme::surface())
        .border_style(Style::default().fg(theme::BORDER));
    let chart_inner = chart_block.inner(chart_area);
    frame.render_widget(chart_block, chart_area);
    if slices.is_empty() {
        frame.render_widget(
            Paragraph::new("Run tasks to build a breakdown")
                .alignment(Alignment::Center)
                .wrap(Wrap { trim: true })
                .style(theme::surface()),
            chart_inner,
        );
    } else if !chart_inner.is_empty() {
        frame.render_widget(
            PieChart::new(slices)
                .show_legend(false)
                .show_percentages(false)
                .high_resolution(chart_inner.width >= 8 && chart_inner.height >= 4)
                .pie_char('●')
                .style(theme::surface()),
            chart_inner,
        );
    }

    let list_block = Block::default()
        .title(
            Line::from(" AGENTS ").style(
                Style::default()
                    .fg(theme::ORANGE)
                    .add_modifier(Modifier::BOLD),
            ),
        )
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .style(theme::surface())
        .border_style(Style::default().fg(theme::BORDER));
    let list_inner = list_block.inner(list_area);
    frame.render_widget(list_block, list_area);
    if list_inner.is_empty() {
        return;
    }
    let list_width = list_inner.width.saturating_sub(1).max(1);
    let lines = usage_agent_lines(&rows, total_cost, list_width);
    let list = Paragraph::new(lines)
        .wrap(Wrap { trim: true })
        .style(theme::surface());
    let list_viewport = Rect::new(list_inner.x, list_inner.y, list_width, list_inner.height);
    let max_scroll = list
        .line_count(list_viewport.width)
        .saturating_sub(usize::from(list_inner.height))
        .min(usize::from(u16::MAX));
    let position = usize::from(app.usage_scroll).min(max_scroll);
    frame.render_widget(list.scroll((position as u16, 0)), list_viewport);
    if max_scroll > 0 && list_inner.width > 1 {
        let mut scrollbar = ScrollbarState::new(max_scroll.saturating_add(1))
            .position(position)
            .viewport_content_length(usize::from(list_inner.height));
        frame.render_stateful_widget(
            Scrollbar::new(ScrollbarOrientation::VerticalRight)
                .begin_symbol(None)
                .end_symbol(None)
                .track_style(Style::default().fg(theme::BORDER))
                .thumb_style(Style::default().fg(theme::AQUA)),
            Rect::new(
                list_inner.right().saturating_sub(1),
                list_inner.y,
                1,
                list_inner.height,
            ),
            &mut scrollbar,
        );
    }
}

fn render_overlays(frame: &mut ratatui::Frame<'_>, app: &AppModel) {
    if app.usage_viewer {
        render_usage_popup(frame, app);
        return;
    }
    if app.event_viewer {
        let area = frame.area();
        let popup = centered_popup(
            area,
            area.width.saturating_sub(4).min(100),
            area.height.saturating_sub(4).min(32),
        );
        let text = event_viewer_lines(app);
        frame.render_widget(Clear, popup);
        let (state, scroll_hint) = if app.event_follow {
            ("live · newest first", "↓/PgDn older")
        } else {
            ("paused", "↑/PgUp newer")
        };
        let output_hint = if app.expanded_output {
            "[v] compact"
        } else {
            "[v] details"
        };
        let title = format!(" Events · {state} · {output_hint} · {scroll_hint} · e/Esc close ");
        let block = Block::default()
            .title(Line::from(Span::styled(
                title,
                Style::default()
                    .fg(theme::AQUA)
                    .add_modifier(Modifier::BOLD),
            )))
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .style(theme::surface())
            .border_style(Style::default().fg(theme::AQUA));
        let inner = block.inner(popup);
        let paragraph = Paragraph::new(text).wrap(Wrap { trim: false });
        let max_scroll = paragraph
            .line_count(inner.width)
            .saturating_sub(usize::from(inner.height))
            .min(usize::from(u16::MAX));
        let position = usize::from(app.event_scroll).min(max_scroll);
        frame.render_widget(block, popup);
        frame.render_widget(paragraph.scroll((position as u16, 0)), inner);
        if max_scroll > 0 {
            let mut scrollbar = ScrollbarState::new(max_scroll.saturating_add(1))
                .position(position)
                .viewport_content_length(usize::from(inner.height));
            frame.render_stateful_widget(
                Scrollbar::new(ScrollbarOrientation::VerticalRight)
                    .begin_symbol(None)
                    .end_symbol(None)
                    .track_style(Style::default().fg(theme::BORDER))
                    .thumb_style(Style::default().fg(theme::AQUA)),
                popup.inner(Margin {
                    horizontal: 0,
                    vertical: 1,
                }),
                &mut scrollbar,
            );
        }
    }
    if app.help {
        let (popup, compact, show_logo) = help_popup_layout(frame.area());
        frame.render_widget(Clear, popup);
        let help = help_lines(compact);
        let content = popup.inner(Margin {
            horizontal: 2,
            vertical: 1,
        });
        let title = format!(" Help · voff v{} ", env!("CARGO_PKG_VERSION"));
        frame.render_widget(
            Block::default()
                .title(Line::from(Span::styled(
                    title,
                    Style::default()
                        .fg(theme::ORANGE)
                        .add_modifier(Modifier::BOLD),
                )))
                .title_bottom(
                    Line::from(Span::styled(
                        " [?] / [Esc] close · [q] safe exit ",
                        Style::default().fg(theme::AQUA),
                    ))
                    .alignment(Alignment::Center),
                )
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .style(theme::surface())
                .border_style(Style::default().fg(theme::BORDER))
                .title_style(
                    Style::default()
                        .fg(theme::ORANGE)
                        .add_modifier(Modifier::BOLD),
                ),
            popup,
        );
        let logo = crate::onboarding::animated_logo_text_for(theme::SURFACE);
        if show_logo && let Some(logo) = logo {
            let logo_width = logo.lines.iter().map(Line::width).max().unwrap_or(0) as u16;
            let logo_height = logo.lines.len() as u16;
            let logo_area = Rect::new(content.x, content.y, logo_width, logo_height);
            let help_area = Rect::new(
                content.x + logo_width + 2,
                content.y,
                content.width.saturating_sub(logo_width + 2),
                content.height,
            );
            frame.render_widget(Paragraph::new(logo), logo_area);
            frame.render_widget(Paragraph::new(help).wrap(Wrap { trim: false }), help_area);
        } else {
            frame.render_widget(Paragraph::new(help).wrap(Wrap { trim: false }), content);
        }
    }
    if let Some(approval) = app.pending_approval() {
        render_approval_popup(frame, app, approval);
    } else if let Some(notice) = &app.application_notice {
        let popup = compact_approval_area(frame.area());
        frame.render_widget(Clear, popup);
        frame.render_widget(
            Paragraph::new(notice.as_str())
                .wrap(Wrap { trim: true })
                .block(
                    Block::bordered()
                        .border_type(BorderType::Rounded)
                        .style(theme::surface())
                        .border_style(Style::default().fg(theme::RED))
                        .title(" Application needs attention ")
                        .title_bottom(if app.credential_target.is_some() {
                            " [e] events · [A] token · [Esc] dismiss "
                        } else {
                            " [e] events · [Esc] dismiss "
                        }),
                ),
            popup,
        );
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
                "Paste the provider API credential\n\n  {status}  \n\nStored in the local .voff SQLite database, never in logs.{error}"
            ))
            .alignment(ratatui::layout::Alignment::Center)
            .wrap(Wrap { trim: true })
            .block(
                Block::default()
                    .title(if app.credential_target.as_ref().is_some_and(|(_, slot)| slot.is_available()) {
                        " Replace provider token "
                    } else {
                        " Add provider token "
                    })
                    .title_bottom(
                        Line::from(" Enter save · Esc cancel ")
                            .style(Style::default().fg(theme::AQUA))
                            .alignment(ratatui::layout::Alignment::Center),
                    )
                    .borders(Borders::ALL)
                    .border_type(BorderType::Rounded)
                    .style(theme::surface())
                    .border_style(Style::default().fg(theme::BORDER)),
            ),
            popup,
        );
    }
    if let Some(id) = app.pending_delete {
        let popup = delete_confirmation_area(frame.area());
        let name = app
            .agents
            .iter()
            .find(|agent| agent.id == id)
            .map_or("this agent", |agent| agent.name.as_str());
        frame.render_widget(Clear, popup);
        frame.render_widget(
            Paragraph::new(format!(
                "Remove {} from the active team?\nHistorical tasks will remain.",
                sanitize(name)
            ))
            .wrap(Wrap { trim: true })
            .block(
                Block::default()
                    .title(" Delete agent ")
                    .title_bottom(
                        Line::from(" [y] Delete       [n] Keep ")
                            .style(Style::default().fg(theme::AQUA))
                            .alignment(Alignment::Center),
                    )
                    .borders(Borders::ALL)
                    .border_type(BorderType::Rounded)
                    .style(theme::surface())
                    .border_style(Style::default().fg(theme::RED)),
            ),
            popup,
        );
    }
}

fn compact_approval_area(area: Rect) -> Rect {
    let width = area.width.min(56);
    Rect::new(
        area.right().saturating_sub(width),
        area.y,
        width,
        area.height.min(5),
    )
}

fn render_compact_approval(frame: &mut ratatui::Frame<'_>, app: &AppModel, approval: &Approval) {
    let popup = compact_approval_area(frame.area());
    let owner = app
        .tasks
        .iter()
        .find(|task| task.id == approval.task_id)
        .and_then(|task| app.agent_for_owner(task.owner))
        .map_or("Task", |agent| agent.name.as_str());
    let caption = if approval.action == "provider.continue" {
        serde_json::from_str::<OutputContinuation>(&approval.reason)
            .map(|c| {
                format!(
                    "Output limit · {} → {} tokens. Continue?",
                    c.previous_tokens, c.next_tokens
                )
            })
            .unwrap_or_else(|_| "Output limit · continue or cancel?".into())
    } else {
        format!("{} · {}", sanitize(owner), sanitize(&approval.action))
    };
    let subtitle = if approval.action == "provider.continue" {
        "Regenerate output? y continue / n cancel".to_owned()
    } else {
        let summary = serde_json::from_str::<serde_json::Value>(&approval.reason)
            .ok()
            .and_then(|v| {
                v.get("path")
                    .or_else(|| v.get("target_agent"))
                    .or_else(|| v.get("command"))
                    .and_then(|v| v.as_str())
                    .map(str::to_owned)
            })
            .unwrap_or_else(|| short_task_id(approval.task_id));
        sanitize(&summary)
    };
    let title = if app.pending_approval_count() > 1 {
        format!(
            " Decision {}/{} · ↑/↓ ",
            app.approval_selected + 1,
            app.pending_approval_count()
        )
    } else {
        " Decision ".into()
    };
    frame.render_widget(Clear, popup);
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(Span::styled(
                truncate_text(&caption, usize::from(popup.width.saturating_sub(2))),
                Style::default()
                    .fg(theme::YELLOW)
                    .add_modifier(Modifier::BOLD),
            )),
            Line::from(truncate_text(
                &subtitle,
                usize::from(popup.width.saturating_sub(2)),
            )),
        ])
        .style(theme::surface())
        .block(
            Block::bordered()
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(theme::YELLOW))
                .title(title)
                .title_bottom(Line::from(vec![
                    Span::styled(
                        " [y] ",
                        Style::default()
                            .fg(theme::GREEN)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(
                        "[n] ",
                        Style::default().fg(theme::RED).add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(
                        "[d] details ",
                        Style::default()
                            .fg(theme::TEXT)
                            .add_modifier(Modifier::UNDERLINED),
                    ),
                ])),
        ),
        popup,
    );
}

fn render_approval_popup(frame: &mut ratatui::Frame<'_>, app: &AppModel, approval: &Approval) {
    if !app.approval_details {
        render_compact_approval(frame, app, approval);
        return;
    }
    if approval.action == "provider.continue" {
        render_continuation_popup(frame, app, approval);
        return;
    }
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
                .fg(theme::YELLOW)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(Span::styled(
            format!("  Task  {}", approval.task_id),
            Style::default().fg(theme::TEXT),
        )),
        Line::from(Span::styled(
            format!(
                "  Working directory  {}",
                sanitize(&approval.working_directory)
            ),
            Style::default().fg(theme::TEXT),
        )),
        Line::from(""),
        popup_section("REQUEST"),
    ];
    if let Some(proposal) = delegation {
        lines.push(Line::from(vec![
            Span::styled("  Target: ", Style::default().fg(theme::TEXT)),
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
            Style::default().fg(theme::TEXT),
        )));
        push_text_lines(&mut lines, &sanitize(&artifact.preview), "  ");
    }
    let area = frame.area();
    let popup = centered_popup(
        area,
        area.width.saturating_sub(2).max(1),
        area.height.saturating_sub(2).max(1),
    );
    let mut footer = vec![
        Span::styled(
            " [y] Approve",
            Style::default()
                .fg(theme::GREEN)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" · "),
        Span::styled(
            "[n] Deny ",
            Style::default().fg(theme::RED).add_modifier(Modifier::BOLD),
        ),
    ];
    footer.push(Span::raw(" · [d] minimize · PgUp/PgDn scroll "));
    if pending_count > 1 {
        footer.push(Span::styled(
            "· ↑/↓ request ",
            Style::default().fg(theme::AQUA),
        ));
    }
    frame.render_widget(Clear, popup);
    frame.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .scroll((app.approval_scroll, 0))
            .style(theme::surface())
            .block(
                Block::default()
                    .title(format!(
                        " Approval {} of {pending_count} · User decision ",
                        selected.saturating_add(1)
                    ))
                    .title_bottom(Line::from(footer).alignment(ratatui::layout::Alignment::Left))
                    .borders(Borders::ALL)
                    .border_type(BorderType::Rounded)
                    .border_style(Style::default().fg(theme::YELLOW)),
            ),
        popup,
    );
}

fn render_continuation_popup(frame: &mut ratatui::Frame<'_>, app: &AppModel, approval: &Approval) {
    let task = app.tasks.iter().find(|task| task.id == approval.task_id);
    let owner = task
        .and_then(|task| app.agent_for_owner(task.owner))
        .map_or("Agent", |agent| agent.name.as_str());
    let allowance = serde_json::from_str::<OutputContinuation>(&approval.reason)
        .map(|c| format!("{} → {} output tokens", c.previous_tokens, c.next_tokens))
        .unwrap_or_else(|_| "See the persisted continuation request".into());
    let lines = vec![
        Line::from(Span::styled(
            format!(" {} · {}", sanitize(owner), short_task_id(approval.task_id)),
            Style::default()
                .fg(theme::TEXT)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(task.map_or_else(String::new, |task| {
            truncate_text(&one_line(&sanitize(&task.objective)), 110)
        })),
        Line::from(""),
        Line::from("The model reached its output limit. No partial actions were applied."),
        Line::from(Span::styled(
            allowance,
            Style::default()
                .fg(theme::YELLOW)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from("Continue regenerates this response with a higher allowance."),
        Line::from("It may take longer. Tool approvals still apply."),
        Line::from("Cancel stops this task; its history and partial preview are kept."),
    ];
    let popup = centered_popup(
        frame.area(),
        frame.area().width.saturating_sub(2).max(1),
        frame.area().height.saturating_sub(2).max(1),
    );
    frame.render_widget(Clear, popup);
    frame.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .scroll((app.approval_scroll, 0))
            .style(theme::surface())
            .block(
                Block::bordered()
                    .border_type(BorderType::Rounded)
                    .border_style(Style::default().fg(theme::YELLOW))
                    .title(" Output limit · Continue task? ")
                    .title_bottom(Line::from(vec![
                        Span::styled(
                            " [y] Continue ",
                            Style::default()
                                .fg(theme::BACKGROUND)
                                .bg(theme::ORANGE)
                                .add_modifier(Modifier::BOLD),
                        ),
                        Span::styled(
                            " [n] Cancel ",
                            Style::default()
                                .fg(theme::TEXT)
                                .add_modifier(Modifier::BOLD),
                        ),
                        Span::raw(" · [d] minimize · PgUp/PgDn scroll "),
                        Span::raw(if app.pending_approval_count() > 1 {
                            " · ↑/↓ request "
                        } else {
                            ""
                        }),
                    ])),
            ),
        popup,
    );
}

fn panel(title: &str, active: bool) -> Block<'_> {
    Block::default()
        .title(title)
        .borders(Borders::ALL)
        .style(theme::surface())
        .border_style(if active {
            Style::default().fg(theme::AQUA)
        } else {
            Style::default().fg(theme::BORDER)
        })
}
struct TerminalGuard;
impl TerminalGuard {
    fn enter_with_handoff(
        startup_handoff: Option<crate::onboarding::TerminalHandoff>,
    ) -> io::Result<(Self, Option<Terminal<CrosstermBackend<io::Stdout>>>)> {
        let key_flags = PushKeyboardEnhancementFlags(
            KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES
                | KeyboardEnhancementFlags::REPORT_ALL_KEYS_AS_ESCAPE_CODES,
        );
        if startup_handoff.is_some() {
            execute!(io::stdout(), key_flags)?;
        } else {
            enable_raw_mode()?;
            execute!(
                io::stdout(),
                EnterAlternateScreen,
                EnableMouseCapture,
                EnableBracketedPaste,
                key_flags
            )?;
        }
        let startup_terminal = startup_handoff.map(crate::onboarding::TerminalHandoff::adopt);
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
        Ok((Self, startup_terminal))
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

    #[test]
    fn resize_redraws_immediately_and_preserves_input_with_help_open() {
        let mut app = AppModel::new(SessionId::new());
        app.help = true;
        app.composer_active = true;
        app.composer.insert_str("Keep my draft");
        let mut terminal = Terminal::new(TestBackend::new(120, 40)).unwrap();
        for (width, height) in [(68, 24), (140, 48)] {
            terminal.backend_mut().resize(width, height);
            let mut drawn = false;
            assert!(
                redraw_on_resize(&mut terminal, &Event::Resize(width, height), |frame| {
                    drawn = true;
                    assert_eq!(frame.area(), Rect::new(0, 0, width, height));
                    render(frame, &mut app);
                })
                .unwrap()
            );
            assert!(drawn);
            assert!(app.help && app.composer_active);
            assert_eq!(app.composer.lines(), &["Keep my draft"]);
        }
        assert!(
            redraw_on_resize(&mut terminal, &Event::Resize(0, 0), |_| {
                panic!("Do not draw an empty viewport");
            })
            .unwrap()
        );
        assert!(
            !redraw_on_resize(&mut terminal, &Event::FocusGained, |_| {
                panic!("Other events must reach their normal handlers");
            })
            .unwrap()
        );
    }

    #[test]
    fn automatic_approvals_do_not_prompt_but_application_limits_do() {
        let mut app = AppModel::new(SessionId::new());
        app.automatic_review = true;
        let approval = Approval {
            id: voff_core::ApprovalId::new(),
            task_id: voff_core::TaskId::new(),
            tool_call_id: voff_core::ToolCallId::new(),
            action: "file.patch".into(),
            working_directory: ".".into(),
            reason: "{}".into(),
            decision: ApprovalDecision::Pending,
            created_at: chrono::Utc::now(),
        };
        app.approvals.push(approval.clone());
        assert!(app.pending_approval().is_none());
        app.approvals.push(Approval {
            action: "provider.continue".into(),
            ..approval
        });
        assert_eq!(app.pending_approval_count(), 1);
        assert_eq!(app.pending_approval().unwrap().action, "provider.continue");
    }

    #[test]
    fn lead_review_reverses_the_live_edge_and_shows_review_cards() {
        let mut app = AppModel::new(SessionId::new());
        let lead = agent("Lead", "technical_lead", vec!["Backend".into()]);
        let worker = agent("Backend", "backend_developer", vec![]);
        app.team_lead = Some(lead.id);
        let root = Task::root(lead.id, "Build", vec![], Default::default());
        let mut child = Task::root(worker.id, "Implement", vec![], Default::default());
        child.parent = Some(root.id);
        child.status = TaskStatus::WaitingApproval;
        app.tasks = vec![root, child.clone()];
        app.agents = vec![lead.clone(), worker.clone()];
        app.lead_reviews
            .insert(child.id, "Reviewing proposal".into());
        let mut terminal = Terminal::new(TestBackend::new(120, 36)).unwrap();
        terminal.draw(|frame| render(frame, &mut app)).unwrap();
        let edge = &app.team_flow.edges()[0];
        assert_eq!(edge.source, worker.id.to_string());
        assert_eq!(edge.target, lead.id.to_string());
        assert!(edge.animated);
        assert!(edge.label.is_none());
        assert_eq!(
            app.team_flow.connection_positions(0),
            Some((
                rataflow::HandlePosition::Bottom,
                rataflow::HandlePosition::Bottom
            ))
        );
        assert_eq!(
            app.team_flow.connection_positions(1),
            Some((rataflow::HandlePosition::Top, rataflow::HandlePosition::Top))
        );
        let buffer = terminal.backend().buffer();
        let rendered = (0..36)
            .flat_map(|y| (0..120).map(move |x| buffer[(x, y)].symbol()))
            .collect::<String>();
        assert!(rendered.contains("REVIEW"), "{rendered}");
        app.lead_reviews.clear();
        terminal.draw(|frame| render(frame, &mut app)).unwrap();
        assert_eq!(app.team_flow.edges()[0].source, lead.id.to_string());
        assert_eq!(
            app.team_flow.connection_positions(0).unwrap().0,
            app.team_flow.connection_positions(0).unwrap().1
        );
    }

    #[test]
    fn nested_review_returns_to_direct_reporting_agent() {
        let mut app = AppModel::new(SessionId::new());
        let lead = agent("Team Lead", "team_lead", vec!["Backend Agent".into()]);
        let backend = agent("Backend Agent", "backend_agent", vec!["Helper".into()]);
        let helper = agent("Helper", "custom_agent", vec![]);
        app.team_lead = Some(lead.id);
        let root = Task::root(lead.id, "root", vec![], Default::default());
        let mut child = Task::root(backend.id, "backend", vec![], Default::default());
        child.parent = Some(root.id);
        let mut grandchild = Task::root(helper.id, "helper", vec![], Default::default());
        grandchild.parent = Some(child.id);
        app.lead_reviews
            .insert(grandchild.id, "Reviewing proposal".into());
        app.tasks = vec![root, child, grandchild];
        app.agents = vec![lead, backend.clone(), helper.clone()];
        let mut terminal = Terminal::new(TestBackend::new(120, 40)).unwrap();
        terminal.draw(|frame| render(frame, &mut app)).unwrap();
        assert!(
            app.team_flow
                .edges()
                .iter()
                .any(|edge| edge.source == helper.id.to_string()
                    && edge.target == backend.id.to_string())
        );
    }

    #[test]
    fn review_arrow_is_visible_at_shared_lead_port_with_other_assignments() {
        let mut app = AppModel::new(SessionId::new());
        let lead = agent(
            "Lead",
            "technical_lead",
            vec!["Backend".into(), "Frontend".into()],
        );
        let worker = agent("Backend", "backend_developer", vec![]);
        let other = agent("Frontend", "frontend_developer", vec![]);
        app.team_lead = Some(lead.id);
        let root = Task::root(lead.id, "Build", vec![], Default::default());
        let mut child = Task::root(worker.id, "Implement", vec![], Default::default());
        child.parent = Some(root.id);
        child.status = TaskStatus::WaitingApproval;
        let mut sibling = Task::root(other.id, "Other work", vec![], Default::default());
        sibling.parent = Some(root.id);
        sibling.status = TaskStatus::Completed;
        app.tasks = vec![root, child.clone(), sibling];
        app.agents = vec![lead.clone(), worker, other];
        app.lead_reviews
            .insert(child.id, "Reviewing proposal".into());
        let mut terminal = Terminal::new(TestBackend::new(120, 40)).unwrap();
        terminal.draw(|frame| render(frame, &mut app)).unwrap();
        assert!(
            app.team_flow
                .edges()
                .iter()
                .any(|edge| edge.target == lead.id.to_string())
        );
        let (left, top, right, bottom) = app.team_flow.node_terminal_rect(0).unwrap();
        let incoming = app
            .team_flow
            .edges()
            .iter()
            .find(|edge| edge.target == lead.id.to_string())
            .unwrap();
        let buffer = terminal.backend().buffer();
        let (x_start, x_end, y_start, y_end) = match incoming.target_handle.as_deref() {
            Some("left") => (left - 2, left, top, bottom),
            Some("right") => (right - 1, right + 2, top, bottom),
            Some("top") => (left, right, top - 2, top),
            _ => (left, right, bottom - 1, bottom + 2),
        };
        let port = (y_start..=y_end)
            .flat_map(|y| {
                (x_start..=x_end).map(move |x| buffer[(x.max(0) as u16, y.max(0) as u16)].symbol())
            })
            .collect::<String>();
        assert!(
            port.chars()
                .any(|symbol| matches!(symbol, '▲' | '▼' | '◀' | '▶')),
            "Lead's chosen port must show an incoming arrow: {port}"
        );
    }

    #[test]
    fn chat_follows_wrapped_content_and_allows_scrolling_back() {
        let mut terminal = Terminal::new(TestBackend::new(30, 8)).unwrap();
        let lines = (0..20)
            .map(|n| Line::from(format!("message {n}: a long wrapped message")))
            .collect::<Vec<_>>();
        let mut scroll = 0;
        terminal
            .draw(|frame| {
                render_chat(
                    frame,
                    lines.clone(),
                    panel("Chat", true),
                    frame.area(),
                    &mut scroll,
                )
            })
            .unwrap();
        let screen = |terminal: &Terminal<TestBackend>| {
            let buffer = terminal.backend().buffer();
            (0..8)
                .flat_map(|y| (0..30).map(move |x| buffer[(x, y)].symbol()))
                .collect::<String>()
        };
        assert!(screen(&terminal).contains("message 19"));
        assert!(!screen(&terminal).contains("message 0:"));
        assert_eq!(terminal.backend().buffer()[(29, 6)].fg, theme::AQUA);
        scroll = 10;
        terminal
            .draw(|frame| {
                render_chat(
                    frame,
                    lines.clone(),
                    panel("Chat", true),
                    frame.area(),
                    &mut scroll,
                )
            })
            .unwrap();
        assert!(!screen(&terminal).contains("message 19"));
        let mut app = AppModel::new(SessionId::new());
        app.role_scroll = 10;
        app.task_inspector_scroll = 10;
        app.push_event(&CanonicalEventKind::ProviderStarted);
        assert_eq!((app.role_scroll, app.task_inspector_scroll), (0, 0));
    }

    #[test]
    fn conversation_includes_delegated_work_and_folds_bookkeeping() {
        let mut app = AppModel::new(SessionId::new());
        let lead = agent("Lead", "technical_lead", vec!["Backend".into()]);
        let backend = agent("Backend", "backend_developer", vec![]);
        let root = Task::root(lead.id, "Build tracker", vec![], Default::default());
        let mut child = Task::root(backend.id, "Build API", vec![], Default::default());
        child.parent = Some(root.id);
        let unrelated = Task::root(backend.id, "Other app", vec![], Default::default());
        app.agents = vec![lead, backend];
        app.tasks = vec![root.clone(), child.clone(), unrelated.clone()];
        app.push_scoped_event(
            Some(child.id),
            &CanonicalEventKind::TaskDelegated {
                parent: root.id,
                child: child.id,
                owner: child.owner,
            },
        );
        app.push_scoped_event(
            Some(child.id),
            &CanonicalEventKind::Usage {
                input_tokens: 17,
                cached_input_tokens: 0,
                output_tokens: 9,
                model: None,
            },
        );
        app.push_scoped_event(
            Some(child.id),
            &CanonicalEventKind::ProviderFailed("Fix the failing check".into()),
        );
        app.push_scoped_event(
            Some(unrelated.id),
            &CanonicalEventKind::ProviderFailed("UNRELATED".into()),
        );
        let ids = task_thread_ids(&app.tasks, root.id);
        let render = |app: &AppModel| {
            conversation_lines(app, &ids, 80, None)
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("\n")
        };
        let text = render(&app);
        assert!(
            text.find("─↳ Lead → Backend").unwrap() < text.find("Fix the failing check").unwrap()
        );
        assert!(!text.contains("Delegated") && !text.contains("• ↳"));
        let lines = conversation_lines(&app, &ids, 80, None);
        let delegation = lines
            .iter()
            .find(|line| line.to_string().contains("Lead → Backend"))
            .unwrap();
        let colored = |color| {
            delegation
                .spans
                .iter()
                .filter(|span| span.style.fg == Some(color))
                .map(|span| span.content.as_ref())
                .collect::<String>()
        };
        assert!(colored(theme::ORANGE).contains("Lead"));
        assert_eq!(colored(theme::YELLOW), "Backend");
        assert!(
            text.contains("Lead → Backend") && text.contains("Fix the failing check"),
            "{text}"
        );
        assert!(
            !text.contains("usage:") && !text.contains("UNRELATED"),
            "{text}"
        );
        assert!(!task_thread_ids(&app.tasks, child.id).contains(&root.id));
        app.expanded_output = true;
        assert!(!render(&app).contains("usage: 17"));
    }

    #[test]
    fn review_cards_connect_only_the_same_review_and_use_actual_names() {
        let mut app = AppModel::new(SessionId::new());
        let lead = agent("pashaaaaa", "technical_lead", vec![]);
        let worker = agent("Backend Agent", "backend_developer", vec![]);
        let task = Task::root(worker.id, "Implement API", vec![], Default::default());
        let lead_id = lead.id;
        app.agents = vec![lead, worker];
        app.tasks = vec![task.clone()];
        let first_id = voff_core::ApprovalId::new();
        let first = Some(first_id);
        let second = Some(voff_core::ApprovalId::new());
        let tool_call_id = voff_core::ToolCallId::new();
        app.approvals.push(Approval {
            id: first_id,
            task_id: task.id,
            tool_call_id,
            action: "process.test".into(),
            working_directory: ".".into(),
            reason: "{}".into(),
            decision: ApprovalDecision::ApprovedOnce,
            created_at: chrono::Utc::now(),
        });
        for kind in [
            CanonicalEventKind::ToolProposed {
                tool_call_id,
                name: "process.test".into(),
            },
            CanonicalEventKind::LeadReviewStarted {
                lead: lead_id,
                approval_id: first,
            },
            CanonicalEventKind::LeadReviewFinished {
                lead: lead_id,
                approval_id: first,
                approved: true,
                reason: "The API checks passed.".into(),
            },
            CanonicalEventKind::LeadReviewStarted {
                lead: lead_id,
                approval_id: second,
            },
        ] {
            app.push_scoped_event(Some(task.id), &kind);
        }
        let compact = conversation_lines(&app, &HashSet::from([task.id]), 80, None)
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            !compact.contains("Review 1")
                && !compact.contains("approved")
                && !compact.contains("Review requested")
        );
        app.expanded_output = true;
        let lines = conversation_lines(&app, &HashSet::from([task.id]), 80, None);
        let text = lines
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(!text.contains("Review 1") && !text.contains("Review 2"));
        assert_eq!(text.matches("Waiting for pashaaaaa").count(), 1);
        assert_eq!(text.matches("✓ pashaaaaa approved").count(), 1);
        assert!(text.contains("approved · running checks"));
        assert!(!text.contains("process.test") && !text.contains("Action proposed"));
        assert!(
            !text.contains("Developer") && !text.contains("Sent for review"),
            "{text}"
        );
        assert_eq!(text.matches(" ╰─").count(), 2);
        assert!(
            text.find("The API checks passed.").unwrap()
                < text.find("Waiting for pashaaaaa").unwrap()
        );
        assert!(lines.iter().all(|line| line.width() <= 80));
    }

    #[test]
    fn reporting_conversation_filters_other_pairs_and_colors_each_speaker() {
        let mut app = AppModel::new(SessionId::new());
        let lead = agent("Lead", "technical_lead", vec!["Backend".into()]);
        let worker = agent("Backend", "backend_developer", vec!["Nested".into()]);
        let nested = agent("Nested", "frontend_developer", vec![]);
        let lead_id = lead.id;
        let worker_color = role_color(&worker.role);
        let lead_color = role_color(&lead.role);
        let root = Task::root(lead.id, "Build app", vec![], Default::default());
        let mut task = Task::root(worker.id, "Build API", vec![], Default::default());
        task.parent = Some(root.id);
        let mut subtask = Task::root(nested.id, "Nested job", vec![], Default::default());
        subtask.parent = Some(task.id);
        let other_reviewer = nested.id;
        app.agents = vec![lead, worker, nested];
        app.tasks = vec![root, task.clone(), subtask.clone()];
        app.push_scoped_event(
            Some(task.id),
            &CanonicalEventKind::TaskTransition {
                from: TaskStatus::WaitingApproval,
                to: TaskStatus::Running,
            },
        );
        app.push_scoped_event(
            Some(task.id),
            &CanonicalEventKind::TextDelta("WORKER_REPLY".into()),
        );
        for (id, reviewer, reason) in [
            (task.id, lead_id, "LEAD_FEEDBACK"),
            (subtask.id, task.owner, "SUBAGENT_FEEDBACK"),
            (task.id, other_reviewer, "WRONG_REVIEWER"),
        ] {
            app.push_scoped_event(
                Some(id),
                &CanonicalEventKind::LeadReviewFinished {
                    lead: reviewer,
                    approval_id: Some(voff_core::ApprovalId::new()),
                    approved: false,
                    reason: reason.into(),
                },
            );
        }
        let ids = task_thread_ids(&app.tasks, task.id);
        let pair = reporting_pair(&app, &task);
        assert_eq!(pair, Some((task.owner, lead_id)));
        for expanded in [false, true] {
            app.expanded_output = expanded;
            let lines = conversation_lines(&app, &ids, 80, pair);
            let text = lines
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("\n");
            assert!(text.contains("WORKER_REPLY") && text.contains("LEAD_FEEDBACK"));
            assert!(
                !text.contains("Build API"),
                "Assignment stays in the task list"
            );
            assert!(
                lines.iter().flat_map(|line| &line.spans).any(|span| {
                    span.content == " Backend" && span.style.fg == Some(worker_color)
                })
            );
            assert!(
                !text.contains("SUBAGENT_FEEDBACK")
                    && !text.contains("WRONG_REVIEWER")
                    && !text.contains("WaitingApproval"),
                "{text}"
            );
            for (message, color) in [("─•", worker_color), ("─•", lead_color)] {
                assert!(
                    lines
                        .iter()
                        .flat_map(|line| &line.spans)
                        .any(|span| span.content.contains(message) && span.style.fg == Some(color))
                );
            }
            for message in ["WORKER_REPLY", "LEAD_FEEDBACK"] {
                assert!(lines.iter().flat_map(|line| &line.spans).any(|span| {
                    span.content.contains(message) && span.style.fg == Some(theme::TEXT)
                }));
            }
            assert!(lines.iter().all(|line| line.style.bg.is_none()
                && line.spans.iter().all(|span| span.style.bg.is_none())));
        }
    }

    #[test]
    fn chat_hides_structured_generation_without_merging_it_into_replies() {
        let mut app = AppModel::new(SessionId::new());
        let task = Task::root(AgentId::new(), "Build app", vec![], Default::default());
        app.tasks = vec![task.clone()];
        for event in [
            CanonicalEventKind::TextDelta("I am building the API.".into()),
            CanonicalEventKind::GenerationDelta(
                r#"{"text":"The routes are ready.","tool_calls":[]}"#.into(),
            ),
            CanonicalEventKind::TextDelta("The routes are ready.".into()),
            CanonicalEventKind::ToolProposed {
                tool_call_id: voff_core::ToolCallId::new(),
                name: "file.patch".into(),
            },
        ] {
            app.push_scoped_event(Some(task.id), &event);
        }
        let ids = HashSet::from([task.id]);
        let render = |app: &AppModel| {
            conversation_lines(app, &ids, 80, None)
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("\n")
        };
        let chat = render(&app);
        assert!(chat.contains("I am building the API.") && chat.contains("The routes are ready."));
        assert!(!chat.contains("tool_calls") && !chat.contains("file.patch"));
        app.expanded_output = true;
        let details = render(&app);
        assert!(!details.contains("tool_calls") && !details.contains("file.patch"));
        assert_eq!(details.matches("The routes are ready.").count(), 1);
        assert!(!details.contains("preview"));
        assert!(app.events.iter().any(|event| event.contains("tool_calls")));
    }

    #[test]
    fn conversation_uses_bullets_and_a_completion_checkmark() {
        let mut app = AppModel::new(SessionId::new());
        let task = Task::root(AgentId::new(), "Build app", vec![], Default::default());
        app.tasks.push(task.clone());
        for event in [
            CanonicalEventKind::TextDelta("Patched the backend files.".into()),
            CanonicalEventKind::ProviderStarted,
            CanonicalEventKind::TextDelta("All eight tests passed.".into()),
            CanonicalEventKind::TaskResultPublished {
                summary: "Backend finished.".into(),
                evidence: vec![],
            },
        ] {
            app.push_scoped_event(Some(task.id), &event);
        }
        let lines = conversation_lines(&app, &HashSet::from([task.id]), 80, None);
        let text = lines
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("├─• Patched the backend files."), "{text}");
        assert!(text.contains("├─• All eight tests passed."), "{text}");
        assert!(text.contains("╰─✓ Backend finished."), "{text}");
        assert!(!text.contains("Reply") && !text.contains("Result"));
    }

    #[test]
    fn review_labels_color_only_the_reviewer_name_even_when_wrapped() {
        for decision in [None, Some(true), Some(false)] {
            let mut app = AppModel::new(SessionId::new());
            let lead = agent("pashaaaaa 開発", "technical_lead", vec![]);
            let worker = agent("Frontend Agent", "frontend_agent", vec![]);
            let task = Task::root(worker.id, "Build UI", vec![], Default::default());
            let event = match decision {
                Some(approved) => CanonicalEventKind::LeadReviewFinished {
                    lead: lead.id,
                    approval_id: Some(voff_core::ApprovalId::new()),
                    approved,
                    reason: "Review explanation stays neutral".into(),
                },
                None => CanonicalEventKind::LeadReviewStarted {
                    lead: lead.id,
                    approval_id: Some(voff_core::ApprovalId::new()),
                },
            };
            let reviewer_name = lead.name.clone();
            app.agents = vec![lead, worker];
            app.tasks.push(task.clone());
            app.expanded_output = true;
            app.push_scoped_event(Some(task.id), &event);
            for width in [22, 100] {
                let lines = conversation_lines(&app, &HashSet::from([task.id]), width, None);
                let colored_name: String = lines
                    .iter()
                    .flat_map(|line| line.spans.iter().skip(1))
                    .filter(|span| span.style.fg == Some(theme::ORANGE))
                    .map(|span| span.content.as_ref())
                    .collect();
                assert_eq!(
                    colored_name.replace(' ', ""),
                    reviewer_name.replace(' ', "")
                );
                assert!(lines.iter().all(|line| line.width() <= usize::from(width)));
            }
        }
    }

    #[test]
    fn delegation_names_keep_their_colors_when_wrapped() {
        let label = "Léåd → 開発 Backend";
        let mut lines = Vec::new();
        push_activity_entry(
            &mut lines,
            &format!("↳ {label}"),
            "Objective stays neutral",
            theme::ORANGE,
            18,
            usize::MAX,
        );
        color_delegation_label(
            &mut lines,
            label,
            "Léåd".len(),
            theme::ORANGE,
            theme::YELLOW,
            18,
        );
        assert!(lines.iter().all(|line| line.width() <= 18));
        let colored = |color| {
            lines
                .iter()
                .flat_map(|line| &line.spans)
                .filter(|span| span.style.fg == Some(color))
                .map(|span| span.content.as_ref())
                .collect::<String>()
        };
        assert!(colored(theme::ORANGE).contains("Léåd"));
        assert_eq!(colored(theme::YELLOW).replace(' ', ""), "開発Backend");
        assert!(!colored(theme::YELLOW).contains("Objective"));
    }

    #[test]
    fn last_thread_entry_has_no_connector_below_its_elbow() {
        for title in ["", "✓"] {
            for (body, width) in [
                ("First line\nSecond line\nThird line", 40),
                ("First line Second line Third line", 17),
            ] {
                let mut lines = Vec::new();
                push_activity_entry(&mut lines, title, body, theme::AQUA, width, 3);
                let rows: Vec<_> = lines.iter().map(ToString::to_string).collect();
                assert_eq!(
                    rows,
                    vec![
                        format!(" ╰─{} First line", if title.is_empty() { "•" } else { "✓" }),
                        "     Second line".into(),
                        "     Third line".into(),
                        "".into(),
                    ]
                );
            }
        }
    }

    #[test]
    fn thread_connectors_span_wrapped_siblings_but_stop_at_the_last_entry() {
        let mut app = AppModel::new(SessionId::new());
        let owner = agent("Backend", "backend_agent", vec![]);
        let task = Task::root(owner.id, "Build app", vec![], Default::default());
        app.agents.push(owner);
        app.tasks.push(task.clone());
        app.push_scoped_event(
            Some(task.id),
            &CanonicalEventKind::TextDelta("First line\nSecond line".into()),
        );
        // Invisible events must not break the visible thread.
        app.push_scoped_event(
            Some(task.id),
            &CanonicalEventKind::Usage {
                input_tokens: 1,
                cached_input_tokens: 0,
                output_tokens: 2,
                model: None,
            },
        );
        app.push_scoped_event(
            Some(task.id),
            &CanonicalEventKind::TaskResultPublished {
                summary: "Finished\nVerified".into(),
                evidence: vec![],
            },
        );
        for expanded in [false, true] {
            app.expanded_output = expanded;
            let lines = conversation_lines(&app, &HashSet::from([task.id]), 40, None);
            let rows: Vec<_> = lines.iter().map(ToString::to_string).collect();
            assert_eq!(
                rows,
                [
                    " Backend",
                    " ├─• First line",
                    " │   Second line",
                    " │",
                    " ╰─✓ Finished",
                    "     Verified",
                    ""
                ]
            );
        }
    }

    #[test]
    fn thread_connectors_do_not_join_different_tasks() {
        let mut app = AppModel::new(SessionId::new());
        let owner = agent("Backend", "backend_agent", vec![]);
        app.agents.push(owner.clone());
        let mut ids = HashSet::new();
        for _ in 0..2 {
            let task = Task::root(owner.id, "Build app", vec![], Default::default());
            ids.insert(task.id);
            app.tasks.push(task.clone());
            app.push_scoped_event(
                Some(task.id),
                &CanonicalEventKind::TaskResultPublished {
                    summary: "Finished\nVerified".into(),
                    evidence: vec![],
                },
            );
        }
        let rows: Vec<_> = conversation_lines(&app, &ids, 40, None)
            .iter()
            .map(ToString::to_string)
            .collect();
        assert_eq!(
            rows,
            [
                " Backend",
                " ╰─✓ Finished",
                "     Verified",
                "",
                " Backend",
                " ╰─✓ Finished",
                "     Verified",
                ""
            ]
        );
    }

    #[test]
    fn conversation_cards_wrap_unicode_and_bound_previews() {
        let mut lines = Vec::new();
        push_activity_entry(
            &mut lines,
            "Response · 🐶 開発者",
            &"長い出力を確認する ".repeat(40),
            theme::BLUE,
            32,
            3,
        );
        assert!(lines.iter().all(|line| line.width() <= 32));
        assert!(lines.iter().any(|line| line.to_string().contains('…')));
        assert!(lines.len() <= 9);
    }

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
        a.reduce(Action::ToggleTaskPane);
        assert!(!a.task_pane_visible);
        a.reduce(Action::ShowTasks);
        assert_eq!(a.panel, Panel::Tasks);
        a.reduce(Action::CloseTab);
        assert_eq!(a.panel, Panel::Tasks);
        a.reduce(Action::ShowTeam);
        assert_eq!(a.panel, Panel::Agents);
        a.reduce(Action::ToggleHelp);
        assert!(a.help)
    }
    #[test]
    fn keys_map_to_actions() {
        assert_eq!(action_for_key(KeyCode::Char('r')), Action::RetryTask);
        assert_eq!(action_for_key(KeyCode::Char('R')), Action::None);
        assert_eq!(action_for_key(KeyCode::Char('A')), Action::AddCredential);
        assert_eq!(action_for_key(KeyCode::Tab), Action::NextTab);
        assert_eq!(action_for_key(KeyCode::BackTab), Action::PreviousTab);
        assert_eq!(action_for_key(KeyCode::Enter), Action::FocusComposer);
        assert_eq!(action_for_key(KeyCode::Char('t')), Action::ToggleTaskFocus);
        assert_eq!(action_for_key(KeyCode::Char('h')), Action::ToggleTaskPane);
        assert_eq!(action_for_key(KeyCode::Char('u')), Action::ToggleUsage);
        assert_eq!(action_for_key(KeyCode::Char('b')), Action::None);
        assert_eq!(
            action_for_key(KeyCode::Char('o')),
            Action::OpenSelectedAgentTab
        );
        assert_eq!(action_for_key(KeyCode::Char('/')), Action::None);
        assert_eq!(action_for_key(KeyCode::Char('c')), Action::AdjustTeam);
        assert_eq!(action_for_key(KeyCode::Char('g')), Action::ChangeWorkspace);
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
        assert_eq!(action_for_key(KeyCode::Char('T')), Action::None);
        assert_eq!(action_for_key(KeyCode::Char('3')), Action::None);
        assert_eq!(action_for_key(KeyCode::Char('4')), Action::None);
        assert_eq!(action_for_key(KeyCode::Char('s')), Action::None);
        assert_eq!(action_for_key(KeyCode::Char('a')), Action::AddAgent);
        assert_eq!(
            action_for_key(KeyCode::Char('d')),
            Action::ToggleApprovalDetails
        );
        assert_eq!(action_for_key(KeyCode::Char('l')), Action::None);
        assert_eq!(
            action_for_event(Event::Key(crossterm::event::KeyEvent::new(
                KeyCode::F(4),
                KeyModifiers::CONTROL,
            ))),
            Action::None
        );
    }

    #[test]
    fn help_keys_are_white_and_ctrl_f4_is_not_advertised() {
        let binding = help_binding("Ctrl+W", "Close agent tab");
        assert_eq!(binding.spans[0].style.fg, Some(theme::TEXT));
        let help = help_lines(false)
            .iter()
            .map(Line::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(!help.contains("Ctrl+F4"));
        assert!(help.contains("↑ / ↓"));
        assert!(help.contains("Prompt history"));
        assert!(
            help_lines(false)
                .last()
                .is_some_and(|line| line.width() == 0)
        );
    }

    #[test]
    fn usage_chart_uses_cost_then_token_then_task_data() {
        let mut row = AgentUsageRow {
            name: "Worker".into(),
            provider: "Codex CLI".into(),
            model: None,
            usage: TokenUsage {
                input_tokens: 2_000,
                cached_input_tokens: 0,
                output_tokens: 400,
            },
            estimated_cost: None,
            task_count: 2,
            completed_count: 1,
            color: theme::AQUA,
        };

        let (metric, slices) = usage_chart_slices(std::slice::from_ref(&row));
        assert!(matches!(metric, Some(UsageChartMetric::Tokens)));
        assert_eq!(slices.len(), 1);

        row.estimated_cost = Some(0.01);
        let (metric, slices) = usage_chart_slices(std::slice::from_ref(&row));
        assert!(matches!(metric, Some(UsageChartMetric::Cost)));
        assert_eq!(slices.len(), 1);

        row.estimated_cost = None;
        row.usage = TokenUsage::default();
        let (metric, slices) = usage_chart_slices(std::slice::from_ref(&row));
        assert!(matches!(metric, Some(UsageChartMetric::Tasks)));
        assert_eq!(slices.len(), 1);

        row.task_count = 0;
        let (metric, slices) = usage_chart_slices(std::slice::from_ref(&row));
        assert!(metric.is_none());
        assert!(slices.is_empty());
    }

    #[test]
    fn usage_agents_sort_by_token_volume_before_priced_cost() {
        let mut app = AppModel::new(SessionId::new());
        let mut priced = agent("Priced", "priced_agent", vec![]);
        priced.provider = "openai".into();
        priced.model = "gpt-4o-mini".into();
        let mut high_usage = agent("High usage", "high_usage_agent", vec![]);
        high_usage.provider = "codex_cli".into();
        app.agents = vec![priced.clone(), high_usage.clone()];
        app.agent_provider_kinds.insert(priced.id, "openai".into());
        app.agent_provider_kinds
            .insert(high_usage.id, "codex_cli".into());
        app.agent_usage.insert(
            priced.id,
            TokenUsage {
                input_tokens: 1_000,
                cached_input_tokens: 0,
                output_tokens: 1_000,
            },
        );
        app.agent_usage.insert(
            high_usage.id,
            TokenUsage {
                input_tokens: 28_000,
                cached_input_tokens: 0,
                output_tokens: 118,
            },
        );

        let rows = agent_usage_rows(&app);
        assert_eq!(rows[0].name, "High usage");
        assert_eq!(rows[1].name, "Priced");
    }

    #[test]
    fn usage_popup_shows_agent_cost_tokens_and_task_activity() {
        let mut app = AppModel::new(SessionId::new());
        let mut backend = agent("Backend", "backend_agent", vec![]);
        backend.provider = "openai".into();
        backend.model = "gpt-4o-mini".into();
        let mut cli = agent("Local CLI", "custom_agent", vec![]);
        cli.provider = "codex_cli".into();
        cli.model = "default".into();
        app.agents = vec![backend.clone(), cli.clone()];
        app.agent_provider_kinds.insert(backend.id, "openai".into());
        app.agent_provider_kinds.insert(cli.id, "codex_cli".into());
        app.agent_usage.insert(
            backend.id,
            TokenUsage {
                input_tokens: 1_240,
                cached_input_tokens: 0,
                output_tokens: 318,
            },
        );
        app.agent_usage.insert(
            cli.id,
            TokenUsage {
                input_tokens: 400,
                cached_input_tokens: 0,
                output_tokens: 100,
            },
        );
        let mut task = Task::root(backend.id, "Implement endpoint", vec![], Default::default());
        task.status = TaskStatus::Completed;
        app.tasks.push(task);
        app.reduce(Action::ToggleUsage);
        assert!(app.usage_viewer);

        let mut terminal = Terminal::new(TestBackend::new(110, 32)).unwrap();
        terminal.draw(|frame| render(frame, &mut app)).unwrap();
        let buffer = terminal.backend().buffer();
        let rendered = (0..buffer.area.height)
            .flat_map(|y| (0..buffer.area.width).map(move |x| buffer[(x, y)].symbol()))
            .collect::<String>();
        assert!(rendered.contains("Estimated cost"), "{rendered}");
        assert!(rendered.contains("$0.0004"), "{rendered}");
        assert!(rendered.contains("Priced agents"), "{rendered}");
        assert!(rendered.contains("1/2"), "{rendered}");
        assert!(
            rendered.contains("Estimated cost: $0.0004 | Priced agents: 1/2"),
            "{rendered}"
        );
        assert!(rendered.contains("gpt-4o-mini"), "{rendered}");
        assert!(rendered.contains("Backend · gpt-4o-mini"), "{rendered}");
        assert!(!rendered.contains("OpenAI API · gpt-4o-mini"), "{rendered}");
        assert!(
            rendered.contains("Tokens in: 1.6k | Tokens out: 418 | Tasks: 1/1"),
            "{rendered}"
        );
        assert!(!rendered.contains("Most active"), "{rendered}");
        assert!(!rendered.contains("Cost & usage"), "{rendered}");
        assert!(rendered.contains("1.2k"), "{rendered}");
        assert!(rendered.contains("318"), "{rendered}");
        assert!(rendered.contains("1/1 done"), "{rendered}");
        assert!(rendered.contains("[u] usage"), "{rendered}");
        let overview = usage_overview(&agent_usage_rows(&app), &app.tasks, 100);
        assert!(
            overview[1].spans.iter().any(|span| {
                span.content.contains("1.6k") && span.style.fg == Some(theme::AQUA)
            })
        );
        assert!(
            overview[1].spans.iter().any(|span| {
                span.content.contains("418") && span.style.fg == Some(theme::PURPLE)
            })
        );
        assert!(
            rendered
                .chars()
                .any(|ch| ('\u{2800}'..='\u{28ff}').contains(&ch)),
            "the cost pie chart should be rendered"
        );

        let mut narrow = Terminal::new(TestBackend::new(44, 16)).unwrap();
        narrow.draw(|frame| render(frame, &mut app)).unwrap();

        app.reduce(Action::Escape);
        assert!(!app.usage_viewer);
        assert!(!app.quit);
    }

    #[test]
    fn workspace_picker_opens_independently_from_team_settings() {
        let workspace = tempfile::tempdir().unwrap();
        let mut app = AppModel::new(SessionId::new());
        app.workspace_root = workspace.path().to_path_buf();
        let area = Rect::new(0, 0, 100, 30);
        let mut terminal = Terminal::new(TestBackend::new(area.width, area.height)).unwrap();
        terminal.draw(|frame| render(frame, &mut app)).unwrap();
        let click = Event::Mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            app.workspace_hitbox.x + 1,
            app.workspace_hitbox.y,
        ));
        assert_eq!(app.handle_composer_event(&click), ComposerEvent::Ignored);
        let Event::Mouse(click) = click else {
            unreachable!()
        };
        assert_eq!(app.action_for_mouse(click, area), Action::ChangeWorkspace);
        app.reduce(Action::ChangeWorkspace);
        assert!(app.workspace_editor.is_some());
        assert!(app.setup_editor.is_none());
    }

    #[test]
    fn task_notifications_only_follow_important_status_transitions() {
        let mut app = AppModel::new(SessionId::new());
        let mut task = Task::root(
            AgentId::new(),
            "Build a feature",
            vec![],
            voff_core::TaskLimits::default(),
        );
        app.apply_event(EngineEvent::TaskUpdated(task.clone()));
        assert!(!app.notifications.has_notification());

        task.status = TaskStatus::Completed;
        app.apply_event(EngineEvent::TaskUpdated(task.clone()));
        assert!(app.notifications.has_notification());
        app.notifications.tick(Duration::from_millis(500));
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal.draw(|frame| render(frame, &mut app)).unwrap();
        let buffer = terminal.backend().buffer();
        let rendered = (0..buffer.area.height)
            .flat_map(|y| (0..buffer.area.width).map(move |x| buffer[(x, y)].symbol()))
            .collect::<String>();
        assert!(rendered.contains("Task completed"), "{rendered}");
        let notification_row = (0..buffer.area.height)
            .find(|&y| {
                (0..buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
                    .contains("Task completed")
            })
            .unwrap();
        assert!(notification_row >= 3, "notification overlaps the top bar");
        app.notifications.clear();

        app.apply_event(EngineEvent::TaskUpdated(task));
        assert!(!app.notifications.has_notification());
    }

    #[test]
    fn lead_approval_uses_a_notification_not_the_composer_border() {
        let mut app = AppModel::new(SessionId::new());
        let lead = agent("Team Lead", "team_lead", vec!["Backend Agent".into()]);
        let worker = agent("Backend Agent", "backend_agent", vec![]);
        let task = Task::root(worker.id, "Run tests", vec![], Default::default());
        app.team_lead = Some(lead.id);
        app.agents = vec![lead.clone(), worker];
        app.tasks.push(task.clone());
        app.apply_event(EngineEvent::Canonical(CanonicalEvent {
            id: voff_core::EventId::new(),
            session_id: app.session,
            task_id: Some(task.id),
            created_at: chrono::Utc::now(),
            kind: CanonicalEventKind::LeadReviewFinished {
                lead: lead.id,
                approval_id: None,
                approved: true,
                reason: "Scoped unittest discovery is approved".into(),
            },
        }));
        assert!(app.notifications.has_notification());
        app.notifications.tick(Duration::from_millis(500));
        let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
        terminal.draw(|frame| render(frame, &mut app)).unwrap();
        let buffer = terminal.backend().buffer();
        let rendered = (0..buffer.area.height)
            .flat_map(|y| (0..buffer.area.width).map(move |x| buffer[(x, y)].symbol()))
            .collect::<String>();
        assert!(rendered.contains("Team Lead approved"), "{rendered}");
        assert!(rendered.contains("Scoped unittest discovery"), "{rendered}");
        let border = (0..buffer.area.width)
            .map(|x| buffer[(x, buffer.area.height - 1)].symbol())
            .collect::<String>();
        assert!(!border.contains("approved"), "{border}");
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
        assert!(
            !rendered.contains("Historical Reviewer  ·  code reviewer"),
            "{rendered}"
        );
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
        app.select_tab(TabId::Tasks);
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
        assert!(rendered.contains("Tasks · 2 total"), "{rendered}");
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
        app.select_tab(TabId::Tasks);

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
        assert_eq!(icon_cell(lead_position), ("◆".into(), theme::ORANGE));
        assert_eq!(
            icon_cell(frontend_position),
            ("▣".into(), theme::agent_color(1))
        );
        assert_eq!(
            icon_cell(backend_position),
            ("⬢".into(), theme::agent_color(2))
        );

        let root_objective = position("root objective");
        let child_objective = position("child objective");
        let grandchild_objective = position("grandchild objective");
        assert_eq!(child_objective.0, root_objective.0 + 2);
        assert_eq!(grandchild_objective.0, root_objective.0 + 4);
    }

    #[test]
    fn task_tiles_resize_without_changing_selection_or_tab_order() {
        let mut app = AppModel::new(SessionId::new());
        app.reduce(Action::ShowTasks);
        let mut terminal = Terminal::new(TestBackend::new(120, 40)).unwrap();
        terminal.draw(|frame| render(frame, &mut app)).unwrap();
        let initial = app.task_list_hitbox;
        let key = |code| Event::Key(crossterm::event::KeyEvent::new(code, KeyModifiers::ALT));
        app.reduce(action_for_event(key(KeyCode::Down)));
        terminal.draw(|frame| render(frame, &mut app)).unwrap();
        assert!(app.task_list_hitbox.height > initial.height);
        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            10,
            app.task_list_hitbox.bottom() - 1,
        ));
        app.handle_mouse(mouse(
            MouseEventKind::Drag(MouseButton::Left),
            10,
            initial.y + 20,
        ));
        terminal.draw(|frame| render(frame, &mut app)).unwrap();
        assert!(app.task_list_hitbox.height > initial.height);
        assert_eq!(app.selected, 0);
        let resized = app.task_list_hitbox;
        app.reduce(Action::ShowTeam);
        assert!(!app.task_layout.dragging());
        app.reduce(action_for_event(key(KeyCode::Up)));
        app.reduce(Action::ShowTasks);
        terminal.draw(|frame| render(frame, &mut app)).unwrap();
        assert_eq!(app.task_list_hitbox, resized);
        app.help = true;
        app.reduce(action_for_event(key(KeyCode::Up)));
        app.help = false;
        terminal.draw(|frame| render(frame, &mut app)).unwrap();
        assert_eq!(app.task_list_hitbox, resized);
        app.reduce(action_for_event(key(KeyCode::Char('0'))));
        terminal.draw(|frame| render(frame, &mut app)).unwrap();
        assert_eq!(app.task_list_hitbox, initial);
        assert_eq!(app.active_tab(), TabId::Tasks);
    }

    #[test]
    fn task_workspace_stacks_wide_panes_and_scrolls_the_task_list() {
        let mut app = AppModel::new(SessionId::new());
        let owner = agent("Backend", "backend_agent", vec![]);
        app.tasks = (0..20)
            .map(|index| {
                Task::root(
                    owner.id,
                    format!("Task {index}"),
                    vec![],
                    Default::default(),
                )
            })
            .collect();
        app.agents.push(owner);
        app.selected = 19;
        app.panel = Panel::Tasks;
        let mut terminal = Terminal::new(TestBackend::new(120, 36)).unwrap();
        terminal
            .draw(|frame| render_tasks_workspace(frame, &mut app, frame.area()))
            .unwrap();
        let browser = app.task_list_hitbox;
        let inspector = app.task_inspector_hitbox;
        assert_eq!(browser.width, inspector.width);
        assert_eq!(browser.bottom(), inspector.y);
        assert!(app.task_window_start > 0);
        let buffer = terminal.backend().buffer();
        assert!(
            (browser.y + 1..browser.bottom() - 1)
                .any(|y| { buffer[(browser.right() - 1, y)].fg == theme::AQUA })
        );
        app.handle_mouse(mouse(
            MouseEventKind::ScrollUp,
            browser.x + 1,
            browser.y + 1,
        ));
        assert_eq!(app.selected, 18);
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
        assert!(rendered.contains("CONVERSATION"), "{rendered}");
        assert!(rendered.contains("API Engineer"), "{rendered}");
        assert!(rendered.contains("voff · ses_"), "{rendered}");
        assert!(!rendered.contains("Tasks view"), "{rendered}");
        assert!(rendered.contains("Inspector"), "{rendered}");
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
                id: voff_core::EventId::new(),
                session_id: app.session,
                task_id: Some(task_id),
                created_at: chrono::Utc::now(),
                kind: CanonicalEventKind::ProviderFailed(message.into()),
            }));
        }
        app.reduce(Action::ShowTasks);

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
        assert_eq!(event_text_color("tool proposed: file.patch"), theme::YELLOW);
        assert_eq!(event_text_color("provider failed: timeout"), theme::RED);
        assert_eq!(event_text_color("provider completed"), theme::GREEN);
        assert_eq!(event_text_color("task created"), theme::AQUA);
        assert_eq!(
            event_text_color("re-run attempt 2: abc -> def"),
            theme::AQUA
        );
        assert_eq!(event_text_color("usage: 10 in / 2 out"), theme::PURPLE);

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

    fn editable_team() -> AppModel {
        let mut app = AppModel::new(SessionId::new());
        app.agents = vec![agent("Team Lead", "technical_lead", vec![])];
        app.team_setup = Some(crate::onboarding::SetupDraft {
            developers: vec![crate::onboarding::DeveloperDraft::new(
                "Team Lead",
                crate::onboarding::DeveloperRole::Lead,
                crate::onboarding::ProviderChoice::CodexCli,
                "default",
                16384,
            )],
            working_directory: ".".into(),
        });
        app
    }

    #[test]
    fn add_agent_uses_selected_map_agent_as_reporting_parent() {
        let mut app = editable_team();
        let backend = agent("Backend Agent", "backend_agent", vec![]);
        app.agents.push(backend);
        let mut draft = crate::onboarding::DeveloperDraft::new(
            "Backend Agent",
            crate::onboarding::DeveloperRole::Backend,
            crate::onboarding::ProviderChoice::CodexCli,
            "default",
            16384,
        );
        draft.reports_to = Some(0);
        app.team_setup.as_mut().unwrap().developers.push(draft);
        app.selected_agent = 1;
        app.reduce(Action::AddAgent);
        let draft = app.setup_editor.take().unwrap().into_draft();
        assert_eq!(draft.developers.len(), 3);
        assert_eq!(draft.developers[2].reports_to, Some(1));
    }

    #[test]
    fn delete_agent_requires_confirmation_and_keeps_historical_tasks() {
        let mut app = editable_team();
        let lead = app.agents[0].id;
        app.team_lead = Some(lead);
        let specialist = agent("Backend Agent", "backend_agent", vec![]);
        let specialist_id = specialist.id;
        app.agents.push(specialist);
        let mut draft = crate::onboarding::DeveloperDraft::new(
            "Backend Agent",
            crate::onboarding::DeveloperRole::Backend,
            crate::onboarding::ProviderChoice::CodexCli,
            "default",
            16384,
        );
        draft.reports_to = Some(0);
        app.team_setup.as_mut().unwrap().developers.push(draft);
        let mut historical = Task::root(specialist_id, "Previous work", vec![], Default::default());
        historical.status = TaskStatus::Completed;
        app.tasks.push(historical);

        app.reduce(Action::DeleteAgent);
        assert!(app.pending_delete.is_none(), "the lead cannot be deleted");
        assert!(app.notifications.has_notification());
        app.notifications.tick(Duration::from_millis(500));
        let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
        terminal.draw(|frame| render(frame, &mut app)).unwrap();
        let buffer = terminal.backend().buffer();
        let warning = (0..buffer.area.height)
            .flat_map(|y| (0..buffer.area.width).map(move |x| buffer[(x, y)].symbol()))
            .collect::<String>();
        assert!(
            warning.contains("The Team Lead cannot be deleted."),
            "{warning}"
        );
        app.selected_agent = 1;
        app.reduce(Action::DeleteAgent);
        assert_eq!(app.pending_delete, Some(specialist_id));
        assert!(app.handle_delete_event(
            &Event::Key(crossterm::event::KeyEvent::new(
                KeyCode::Char('n'),
                KeyModifiers::NONE
            )),
            Rect::new(0, 0, 100, 30),
        ));
        assert!(app.setup_result.is_none());

        app.reduce(Action::DeleteAgent);
        terminal.draw(|frame| render(frame, &mut app)).unwrap();
        let buffer = terminal.backend().buffer();
        let rendered = (0..buffer.area.height)
            .flat_map(|y| (0..buffer.area.width).map(move |x| buffer[(x, y)].symbol()))
            .collect::<String>();
        assert!(rendered.contains("Historical tasks will remain"));
        assert!(rendered.contains("[y] Delete"));

        assert!(app.handle_delete_event(
            &Event::Key(crossterm::event::KeyEvent::new(
                KeyCode::Char('y'),
                KeyModifiers::NONE
            )),
            Rect::new(0, 0, 100, 30),
        ));
        assert!(app.quit);
        assert_eq!(app.setup_result.as_ref().unwrap().developers.len(), 1);
        assert_eq!(app.tasks.len(), 1);
        assert_eq!(
            app.setup_notice.as_deref(),
            Some("Backend Agent removed from the team")
        );
    }

    #[test]
    fn delete_agent_rejects_reports_and_active_work() {
        let mut app = editable_team();
        app.team_lead = Some(app.agents[0].id);
        let specialist = agent("Backend Agent", "backend_agent", vec!["Helper".into()]);
        let specialist_id = specialist.id;
        app.agents.push(specialist);
        let mut draft = crate::onboarding::DeveloperDraft::new(
            "Backend Agent",
            crate::onboarding::DeveloperRole::Backend,
            crate::onboarding::ProviderChoice::CodexCli,
            "default",
            16384,
        );
        draft.reports_to = Some(0);
        app.team_setup.as_mut().unwrap().developers.push(draft);
        let mut helper = crate::onboarding::DeveloperDraft::new(
            "Helper",
            crate::onboarding::DeveloperRole::Custom,
            crate::onboarding::ProviderChoice::CodexCli,
            "default",
            16384,
        );
        helper.reports_to = Some(1);
        app.team_setup.as_mut().unwrap().developers.push(helper);
        app.selected_agent = 1;
        app.reduce(Action::DeleteAgent);
        assert!(app.pending_delete.is_none());
        assert!(app.notifications.has_notification());

        app.team_setup.as_mut().unwrap().developers.pop();
        app.tasks.push(Task::root(
            specialist_id,
            "Active",
            vec![],
            Default::default(),
        ));
        app.reduce(Action::DeleteAgent);
        assert!(app.pending_delete.is_none());
        assert!(app.notifications.has_notification());
    }

    #[test]
    fn team_button_opens_the_common_intro_with_continue_control() {
        let mut app = editable_team();
        let mut terminal = Terminal::new(TestBackend::new(100, 32)).unwrap();
        terminal.draw(|frame| render(frame, &mut app)).unwrap();
        let before_header = (0..100)
            .map(|x| terminal.backend().buffer()[(x, 0)].symbol())
            .collect::<String>();

        app.reduce(Action::AdjustTeam);
        assert!(app.setup_editor.is_some());
        terminal.draw(|frame| render(frame, &mut app)).unwrap();
        let buffer = terminal.backend().buffer();
        let after_header = (0..100)
            .map(|x| buffer[(x, 0)].symbol())
            .collect::<String>();
        assert_eq!(after_header, before_header);
        let rendered = (0..buffer.area.height)
            .flat_map(|y| (0..buffer.area.width).map(move |x| buffer[(x, y)].symbol()))
            .collect::<String>();
        assert!(!rendered.contains("Agent settings"));
        assert!(rendered.contains("🐶"), "{rendered}");
        assert!(rendered.contains("voff · ses_"), "{rendered}");
        assert!(rendered.contains("[Enter] save"));
        assert!(rendered.contains("Team Lead"));
        assert!(!rendered.contains("quick defaults"));
        assert!(!rendered.contains("Adjust team"));
    }

    #[test]
    fn team_builder_is_the_only_team_config_entry_point() {
        let mut app = editable_team();
        app.reduce(action_for_key(KeyCode::Char('b')));
        assert!(app.setup_editor.is_none());
        app.reduce(action_for_key(KeyCode::Char('c')));
        assert!(app.setup_editor.is_some());
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal.draw(|frame| render(frame, &mut app)).unwrap();
        let buffer = terminal.backend().buffer();
        let rendered = (0..buffer.area.height)
            .flat_map(|y| (0..buffer.area.width).map(move |x| buffer[(x, y)].symbol()))
            .collect::<String>();
        assert!(!rendered.contains("Agent settings"));
        assert!(rendered.contains("voff · ses_"), "{rendered}");
        assert!(rendered.contains("Team Lead"));
        assert!(!rendered.contains("[c] config"));
    }

    #[test]
    fn help_shows_the_bundled_logo_and_version() {
        let mut app = AppModel::new(SessionId::new());
        app.help = true;
        let mut terminal = Terminal::new(TestBackend::new(112, 32)).unwrap();
        terminal.draw(|frame| render(frame, &mut app)).unwrap();
        let buffer = terminal.backend().buffer();
        let rendered = (0..buffer.area.height)
            .flat_map(|y| (0..buffer.area.width).map(move |x| buffer[(x, y)].symbol()))
            .collect::<String>();
        assert!(
            rendered.contains(&format!("voff v{}", env!("CARGO_PKG_VERSION"))),
            "{rendered}"
        );
        let popup = help_popup_area(Rect::new(0, 0, 112, 32));
        let content = popup.inner(Margin {
            horizontal: 2,
            vertical: 1,
        });
        let colored_logo_cells = (content.y..content.bottom())
            .flat_map(|y| (content.x..content.x + 40).map(move |x| buffer[(x, y)].fg))
            .filter(|color| *color != theme::TEXT && *color != theme::SURFACE)
            .count();
        assert!(
            colored_logo_cells > 10,
            "ANSI logo should retain its colors"
        );
        let (_, compact, _) = help_popup_layout(Rect::new(0, 0, 112, 32));
        assert!(rendered.contains(if compact {
            "Add / remove agent"
        } else {
            "Add agent"
        }));
        assert!(popup.width < 104, "popup should fit its content: {popup:?}");
        assert!(popup.width > 60, "logo and help list should fit: {popup:?}");
        assert!(
            usize::from(popup.height) >= help_lines(compact).len() + 2,
            "popup should leave room for the help list and its padding: {popup:?}"
        );
        assert!(rendered.contains("[?] / [Esc] close · [q] safe exit"));
        let title_row = (0..112)
            .map(|x| buffer[(x, popup.y)].symbol())
            .collect::<String>();
        let title_x = title_row[..title_row.find("Help · voff").unwrap()]
            .chars()
            .count() as u16;
        assert_eq!(
            buffer[(title_x, popup.y)].fg,
            theme::ORANGE,
            "title_row={title_row:?}, title_x={title_x}, popup={popup:?}"
        );
        let heading = (content.y..content.bottom())
            .find_map(|y| {
                let row = (0..112)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>();
                row.find("NAVIGATION")
                    .map(|byte| (row[..byte].chars().count() as u16, y))
            })
            .unwrap();
        assert_eq!(buffer[heading].fg, theme::ORANGE);
        assert!(matches!(
            buffer[(content.x, content.y)].bg,
            ratatui::style::Color::Reset | ratatui::style::Color::Rgb(_, _, _)
        ));
    }

    #[test]
    fn role_tabs_are_clickable_closable_and_route_prompts_via_the_lead() {
        let mut app = AppModel::new(SessionId::new());
        let lead = agent("Ada", "technical_lead", vec!["Linus".into()]);
        let specialist = agent("Linus", "backend_developer", vec![]);
        let specialist_id = specialist.id;
        app.team_lead = Some(lead.id);
        app.agents = vec![lead, specialist];
        app.selected_agent = 1;
        app.open_role_tab(specialist_id);
        assert_eq!(app.active_tab(), TabId::Role(specialist_id));
        assert!(
            app.routed_objective("Build an API")
                .contains("User request:\nBuild an API")
        );
        assert!(app.routed_objective("Build an API").contains("Team Lead"));

        let mut terminal = Terminal::new(TestBackend::new(110, 30)).unwrap();
        terminal.draw(|frame| render(frame, &mut app)).unwrap();
        let buffer = terminal.backend().buffer();
        let rendered = (0..buffer.area.height)
            .flat_map(|y| (0..buffer.area.width).map(move |x| buffer[(x, y)].symbol()))
            .collect::<String>();
        let header = (0..buffer.area.width)
            .map(|x| buffer[(x, 0)].symbol())
            .collect::<String>();
        assert!(
            header.contains("Team")
                && header.contains("Tasks")
                && header.contains("Linus ×")
                && !header.contains('|'),
            "{header}"
        );
        assert!(rendered.contains("Ask Linus to do anything"), "{rendered}");
        assert!(rendered.contains("Activity") && !rendered.contains("Inspector"));
        let role = app
            .tab_hitboxes
            .iter()
            .find(|tab| tab.id == TabId::Role(specialist_id))
            .copied()
            .unwrap();
        let close = role.close.unwrap();
        app.reduce(Action::Mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            close.x,
            close.y,
        )));
        assert_eq!(app.active_tab(), TabId::Team);
        assert!(app.role_tabs.is_empty());
        app.reduce(Action::CloseTab);
        assert_eq!(app.active_tab(), TabId::Team);
    }

    #[test]
    fn tasks_tab_is_always_open_and_cannot_be_closed() {
        let mut app = AppModel::new(SessionId::new());
        let lead = agent("Ada", "technical_lead", vec![]);
        let lead_id = lead.id;
        app.team_lead = Some(lead_id);
        app.agents = vec![lead];
        app.open_role_tab(lead_id);
        app.reduce(Action::ShowTeam);
        app.reduce(Action::ShowTasks);
        assert_eq!(app.active_tab(), TabId::Tasks);
        app.reduce(Action::CloseTab);
        assert_eq!(app.active_tab(), TabId::Tasks);
        assert!(app.role_tabs.contains(&lead_id));
        app.reduce(Action::ShowTeam);
        assert_eq!(app.active_tab(), TabId::Team);
    }

    #[test]
    fn fixed_task_tab_and_role_tabs_preserve_rendered_order() {
        let mut app = AppModel::new(SessionId::new());
        let lead = agent("pashaaaaa", "technical_lead", vec![]);
        let frontend = agent("Frontend Agent", "frontend_developer", vec![]);
        let order = [
            TabId::Team,
            TabId::Tasks,
            TabId::Role(lead.id),
            TabId::Role(frontend.id),
        ];
        app.agents = vec![lead, frontend];
        app.select_tab(TabId::Tasks);
        app.open_role_tab(app.agents[0].id);
        app.open_role_tab(app.agents[1].id);
        app.select_tab(order[2]);
        let mut terminal = Terminal::new(TestBackend::new(120, 30)).unwrap();
        terminal.draw(|frame| render(frame, &mut app)).unwrap();
        let positions = app
            .tab_hitboxes
            .iter()
            .map(|tab| (tab.id, tab.area))
            .collect::<Vec<_>>();
        assert_eq!(
            positions.iter().map(|(id, _)| *id).collect::<Vec<_>>(),
            order
        );
        for expected in [order[3], order[0], order[1], order[2]] {
            app.select_tab(expected);
            terminal.draw(|frame| render(frame, &mut app)).unwrap();
            assert_eq!(app.active_tab(), expected);
            assert_eq!(
                app.tab_hitboxes
                    .iter()
                    .map(|tab| (tab.id, tab.area))
                    .collect::<Vec<_>>(),
                positions
            );
        }
    }

    #[test]
    fn switching_tabs_preserves_each_unsent_prompt_draft() {
        let mut app = AppModel::new(SessionId::new());
        let specialist = agent("Linus", "backend_developer", vec![]);
        let id = specialist.id;
        app.agents = vec![specialist];
        app.composer.insert_str("Team Lead draft");
        app.open_role_tab(id);
        assert_eq!(app.composer.lines(), [""]);
        app.composer.insert_str("Backend draft");
        app.reduce(Action::ShowTeam);
        assert_eq!(app.composer.lines(), ["Team Lead draft"]);
        app.close_role_tab(id);
        app.open_role_tab(id);
        assert_eq!(app.composer.lines(), ["Backend draft"]);
    }

    #[test]
    fn tasks_tab_is_always_shown() {
        let mut app = AppModel::new(SessionId::new());
        let mut terminal = Terminal::new(TestBackend::new(40, 20)).unwrap();
        terminal.draw(|frame| render(frame, &mut app)).unwrap();
        let buffer = terminal.backend().buffer();
        let header = (0..40).map(|x| buffer[(x, 0)].symbol()).collect::<String>();
        assert!(header.contains("Team"), "{header}");
        assert!(header.contains("Tasks"), "{header}");
        assert!(!app.team_tab_hitbox.is_empty());
        assert!(!app.tasks_tab_hitbox.is_empty());
    }

    #[test]
    fn plus_tab_opens_the_selected_role() {
        let mut app = AppModel::new(SessionId::new());
        let specialist = agent("Linus", "backend_developer", vec![]);
        let id = specialist.id;
        app.agents = vec![specialist];
        let mut terminal = Terminal::new(TestBackend::new(90, 24)).unwrap();
        terminal.draw(|frame| render(frame, &mut app)).unwrap();
        let plus = app.add_role_tab_hitbox;
        assert!(!plus.is_empty());
        app.reduce(Action::Mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            plus.x + 1,
            plus.y,
        )));
        assert_eq!(app.active_tab(), TabId::Role(id));
    }

    #[test]
    fn team_changes_are_blocked_until_pending_work_is_settled() {
        let mut app = editable_team();
        app.tasks.push(Task::root(
            app.agents[0].id,
            "pending work",
            vec![],
            voff_core::TaskLimits::default(),
        ));
        app.reduce(Action::AdjustTeam);
        assert!(app.setup_editor.is_none());
        assert!(app.notifications.has_notification());
    }

    #[test]
    fn fresh_automatic_review_session_can_edit_the_team() {
        let mut app = editable_team();
        app.automatic_review = true;
        assert!(!has_unsettled_tasks(&app.tasks));
        app.reduce(Action::AdjustTeam);
        assert!(app.setup_editor.is_some());
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
    fn composer_up_down_navigates_prompt_history_and_restores_the_draft() {
        let mut app = AppModel::new(SessionId::new());
        let enter = Event::Key(crossterm::event::KeyEvent::new(
            KeyCode::Enter,
            KeyModifiers::NONE,
        ));
        for prompt in ["first request", "second request"] {
            app.composer_active = true;
            app.handle_composer_event(&Event::Paste(prompt.into()));
            assert_eq!(
                app.handle_composer_event(&enter),
                ComposerEvent::Submit(prompt.into())
            );
        }

        app.composer_active = true;
        app.handle_composer_event(&Event::Paste("unsent draft".into()));
        let up = Event::Key(crossterm::event::KeyEvent::new(
            KeyCode::Up,
            KeyModifiers::NONE,
        ));
        let down = Event::Key(crossterm::event::KeyEvent::new(
            KeyCode::Down,
            KeyModifiers::NONE,
        ));

        assert_eq!(app.handle_composer_event(&up), ComposerEvent::Consumed);
        assert_eq!(app.composer.lines(), ["second request"]);
        app.handle_composer_event(&up);
        assert_eq!(app.composer.lines(), ["first request"]);
        app.handle_composer_event(&up);
        assert_eq!(app.composer.lines(), ["first request"]);
        app.handle_composer_event(&down);
        assert_eq!(app.composer.lines(), ["second request"]);
        app.handle_composer_event(&down);
        assert_eq!(app.composer.lines(), ["unsent draft"]);

        app.handle_composer_event(&Event::Paste("\nsecond line".into()));
        assert_eq!(app.composer.cursor().0, 1);
        app.handle_composer_event(&up);
        assert_eq!(app.composer.cursor().0, 0);
        app.handle_composer_event(&down);
        assert_eq!(app.composer.cursor().0, 1);
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
    fn mouse_switches_tabs_and_scrolls_the_inspector_independently() {
        let mut app = AppModel::new(SessionId::new());
        app.tasks.push(Task::root(
            AgentId::new(),
            "Inspect this task",
            vec![],
            voff_core::TaskLimits::default(),
        ));
        let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
        terminal.draw(|frame| render(frame, &mut app)).unwrap();
        assert_eq!(app.panel, Panel::Agents);
        app.composer_active = true;
        assert_eq!(
            app.handle_composer_event(&Event::Mouse(mouse(
                MouseEventKind::Down(MouseButton::Left),
                app.help_hitbox.x + 2,
                app.help_hitbox.y,
            ))),
            ComposerEvent::Ignored
        );
        app.composer_active = false;
        app.help = true;
        assert_eq!(
            app.action_for_mouse(
                mouse(
                    MouseEventKind::Down(MouseButton::Left),
                    app.tasks_tab_hitbox.x + 1,
                    app.tasks_tab_hitbox.y,
                ),
                Rect::new(0, 0, 100, 30),
            ),
            Action::ToggleHelp
        );
        app.help = false;
        assert_eq!(
            app.action_for_mouse(
                mouse(
                    MouseEventKind::Down(MouseButton::Left),
                    app.team_edit_hitbox.x,
                    app.team_edit_hitbox.y,
                ),
                Rect::new(0, 0, 100, 30),
            ),
            Action::AdjustTeam
        );
        app.reduce(Action::Mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            app.tasks_tab_hitbox.x + 1,
            app.tasks_tab_hitbox.y,
        )));
        assert_eq!(app.panel, Panel::Tasks);
        terminal.draw(|frame| render(frame, &mut app)).unwrap();
        assert_eq!(
            app.action_for_mouse(
                mouse(
                    MouseEventKind::Down(MouseButton::Left),
                    app.task_expand_hitbox.x + 1,
                    app.task_expand_hitbox.y,
                ),
                Rect::new(0, 0, 100, 30),
            ),
            Action::ToggleExpanded
        );
        assert_eq!(
            app.action_for_mouse(
                mouse(
                    MouseEventKind::Down(MouseButton::Left),
                    app.task_retry_hitbox.x + 1,
                    app.task_retry_hitbox.y,
                ),
                Rect::new(0, 0, 100, 30),
            ),
            Action::RetryTask
        );
        let point = (
            app.task_inspector_hitbox.x + 2,
            app.task_inspector_hitbox.y + 2,
        );
        app.reduce(Action::Mouse(mouse(
            MouseEventKind::ScrollUp,
            point.0,
            point.1,
        )));
        assert_eq!(app.task_inspector_scroll, 3);
        assert_eq!(app.selected, 0);
        app.reduce(Action::Mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            app.team_tab_hitbox.x + 2,
            app.team_tab_hitbox.y,
        )));
        assert_eq!(app.panel, Panel::Agents);
    }

    #[test]
    fn approval_buttons_are_mouse_accessible_without_click_through() {
        let mut app = AppModel::new(SessionId::new());
        app.approvals.push(Approval {
            id: voff_core::ApprovalId::new(),
            task_id: voff_core::TaskId::new(),
            tool_call_id: voff_core::ToolCallId::new(),
            action: "provider.continue".into(),
            working_directory: ".".into(),
            reason: "continue".into(),
            decision: ApprovalDecision::Pending,
            created_at: chrono::Utc::now(),
        });
        let area = Rect::new(0, 0, 80, 24);
        let popup = compact_approval_area(area);
        let bottom = popup.bottom() - 1;
        assert_eq!(
            app.action_for_mouse(
                mouse(MouseEventKind::Down(MouseButton::Left), popup.x + 2, bottom),
                area
            ),
            Action::Approve
        );
        assert_eq!(
            app.action_for_mouse(
                mouse(MouseEventKind::Down(MouseButton::Left), popup.x + 7, bottom),
                area
            ),
            Action::Deny
        );
        assert_eq!(
            app.action_for_mouse(
                mouse(
                    MouseEventKind::Down(MouseButton::Left),
                    popup.x + 12,
                    bottom
                ),
                area
            ),
            Action::ToggleApprovalDetails
        );
        assert_eq!(
            app.action_for_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 1, 1), area),
            Action::None
        );
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
    fn attention_popup_can_replace_an_existing_token_while_composing() {
        let mut app = AppModel::new(SessionId::new());
        let slot = crate::credential::CredentialSlot::new(Some("expired".into()));
        app.credential_target = Some(("OPENAI_API_KEY".into(), slot.clone()));
        app.application_notice = Some("Provider needs attention".into());
        app.composer_active = true;
        let event = Event::Key(crossterm::event::KeyEvent::new(
            KeyCode::Char('A'),
            KeyModifiers::SHIFT,
        ));
        assert_eq!(app.handle_composer_event(&event), ComposerEvent::Ignored);
        app.reduce(action_for_event(event));
        assert!(app.credential_input.is_some());
        assert!(!app.composer_active);
        assert_eq!(slot.get().as_deref(), Some("expired"));
        app.handle_credential_event(&Event::Paste("replacement".into()));
        let (_, submission) = app.handle_credential_event(&Event::Key(
            crossterm::event::KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        ));
        let (reference, _, token) = submission.unwrap();
        assert_eq!(reference, "OPENAI_API_KEY");
        assert_eq!(token, "replacement");
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
    fn interleaved_task_streams_are_not_merged() {
        let mut app = AppModel::new(SessionId::new());
        let first = voff_core::TaskId::new();
        let second = voff_core::TaskId::new();
        app.push_scoped_event(Some(first), &CanonicalEventKind::TextDelta("hello".into()));
        app.push_scoped_event(
            Some(second),
            &CanonicalEventKind::GenerationDelta("{\"path\":".into()),
        );
        app.push_scoped_event(
            Some(second),
            &CanonicalEventKind::GenerationDelta("\"a.rs\"}".into()),
        );
        app.push_scoped_event(Some(first), &CanonicalEventKind::TextDelta(" world".into()));
        assert_eq!(app.events, vec!["hello", "{\"path\":\"a.rs\"}", " world"]);
        assert_eq!(
            app.event_task_ids,
            vec![Some(first), Some(second), Some(first)]
        );
        assert_eq!(app.scoped_events.len(), 3);
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
    fn usage_events_are_recorded() {
        let mut app = AppModel::new(SessionId::new());
        app.apply_event(EngineEvent::Canonical(voff_core::CanonicalEvent {
            id: voff_core::EventId::new(),
            session_id: app.session,
            task_id: None,
            created_at: chrono::Utc::now(),
            kind: CanonicalEventKind::Usage {
                input_tokens: 1_000,
                cached_input_tokens: 0,
                output_tokens: 500,
                model: None,
            },
        }));
        assert!(!app.events.is_empty());
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
        assert!(!rendered.contains("4096 token limit"));
        assert!(!rendered.contains("no delegation"));
        assert!(rendered.contains("Team"), "{rendered}");
        assert!(rendered.contains("Tasks · 0"), "{rendered}");
        assert!(rendered.contains("Tasks"), "{rendered}");
        assert!(!rendered.contains("[3 Events]"), "{rendered}");
        assert!(rendered.contains("voff · ses_"), "{rendered}");
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
        assert!(!composer.contains("Ask Team Lead"), "{composer}");
        assert!(
            composer.contains("🐶") && composer.contains("Ask Ada to do anything"),
            "{composer}"
        );
        assert!(!composer.contains("/task assign"), "{composer}");
        assert!(!composer.contains("/tasks browse"), "{composer}");
        // The wide emoji occupies a continuation cell; the next cell is the visible gap.
        assert!(
            composer.contains("🐶  Ask Ada to do anything"),
            "{composer}"
        );
        assert!(!composer.contains("[R]"), "{composer}");
        assert!(!composer.contains("1/2/3 views"), "{composer}");
        let bottom_border = (0..buffer.area.width)
            .map(|x| buffer[(x, buffer.area.height - 1)].symbol())
            .collect::<String>();
        assert!(
            bottom_border.contains("[?] help · [u] usage · [w]"),
            "{bottom_border}"
        );
        assert!(!bottom_border.contains("Prompt"), "{bottom_border}");
        assert!(!composer.contains("Prompt · Team Lead"), "{composer}");
        let cwd = bottom_border
            .find("[w] /workspaces/voff-demo")
            .expect("cwd should be visible on the bottom border");
        assert!(cwd > usize::from(buffer.area.width / 2), "{bottom_border}");
    }

    #[test]
    fn role_cards_render_one_icon_labeled_identity_inside_the_border() {
        let mut app = AppModel::new(SessionId::new());
        app.agents = vec![agent("Backend Agent 2", "backend_agent", vec![])];
        let backend = TestBackend::new(120, 24);
        let mut terminal = Terminal::new(backend).unwrap();

        terminal.draw(|frame| render(frame, &mut app)).unwrap();

        let buffer = terminal.backend().buffer();
        let rendered = (0..buffer.area.height)
            .flat_map(|y| (0..buffer.area.width).map(move |x| buffer[(x, y)].symbol()))
            .collect::<String>();
        assert!(rendered.contains("⬢ Backend Agent 2"), "{rendered}");
        assert!(!rendered.contains("Backend Agent ·"), "{rendered}");
        assert_eq!(
            rendered.matches("⬢ Backend Agent 2").count(),
            1,
            "{rendered}"
        );
    }

    #[test]
    fn team_cards_end_at_their_content_after_zoom_changes() {
        let mut app = AppModel::new(SessionId::new());
        let lead = agent("Team Lead", "team_lead", vec![]);
        app.team_lead = Some(lead.id);
        app.agents.push(lead);
        app.agents
            .push(agent("Backend Agent", "backend_agent", vec![]));
        let mut terminal = Terminal::new(TestBackend::new(120, 34)).unwrap();
        for action in [
            Action::ResetTeamZoom,
            Action::ZoomTeamOut,
            Action::ZoomTeamOut,
        ] {
            app.reduce(action);
            terminal.draw(|frame| render(frame, &mut app)).unwrap();
            let buffer = terminal.backend().buffer();
            for index in 0..app.agents.len() {
                let (left, _, right, bottom) = app.team_flow.node_terminal_rect(index).unwrap();
                let last_content_row = bottom - 2;
                if last_content_row < 0 || last_content_row >= i32::from(buffer.area.height) {
                    continue;
                }
                assert!(
                    ((left + 1)..(right - 1)).any(|x| {
                        x >= 0
                            && x < i32::from(buffer.area.width)
                            && buffer[(x as u16, last_content_row as u16)].symbol() != " "
                    }),
                    "card {index} ended with an empty content row at zoom {}",
                    app.team_flow.zoom_level()
                );
            }
        }
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
        assert!(rendered.contains("Codex CLI"), "{rendered}");
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

        terminal.draw(|frame| render(frame, &mut app)).unwrap();
        let buffer = terminal.backend().buffer();
        let input_row = app.composer_hitbox.y.saturating_add(1);
        let hint_start = (0..buffer.area.width)
            .find(|&column| buffer[(column, input_row)].symbol() == "A")
            .expect("placeholder begins with Ask");

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
            composer.contains("🐶") && !composer.contains("Ask Team Lead to do anything"),
            "{composer}"
        );
        assert!(
            composer.contains("Enter send · Shift+Enter newline"),
            "{composer}"
        );
        assert!(!composer.contains("Ctrl+Z/Y undo/redo"), "{composer}");
        assert!(!composer.contains("/task"), "{composer}");
        assert!(!composer.contains("/tasks"), "{composer}");
        let cursor = (hint_start, input_row);
        assert_eq!(buffer[cursor].bg, theme::AQUA);
        assert_eq!(buffer[cursor].fg, theme::BACKGROUND);

        app.composer.insert_str("Build a test app");
        terminal.draw(|frame| render(frame, &mut app)).unwrap();
        let buffer = terminal.backend().buffer();
        let composer = (buffer.area.height.saturating_sub(3)..buffer.area.height)
            .flat_map(|y| (0..buffer.area.width).map(move |x| buffer[(x, y)].symbol()))
            .collect::<String>();
        assert_eq!(buffer[cursor].symbol(), "B");
        assert!(composer.contains("🐶  Build a test app"), "{composer}");
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
        assert!(rendered.contains("2 handoff"), "{rendered}");
        assert!(rendered.contains("Backend"), "{rendered}");
        assert!(app.agents.iter().any(|agent| agent.name == "Frontend"));
        assert!(rendered.contains("Decision 1/2"), "{rendered}");
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
                .all(|edge| { edge.animated && edge.label.is_none() })
        );
    }

    #[test]
    fn approval_popup_navigates_one_pending_request_at_a_time() {
        let mut app = AppModel::new(SessionId::new());
        app.approval_details = true;
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
        app.approval_details = true;
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
        assert_eq!(buffer[action_position].fg, theme::YELLOW);
    }

    #[test]
    fn live_delegation_draws_a_continuous_edge_and_spins_on_working_roles() {
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
        assert!(app.team_flow.edges().iter().any(|edge| edge.animated));
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
    fn team_card_side_borders_have_no_handle_gaps() {
        let mut app = AppModel::new(SessionId::new());
        app.agents = vec![agent("Ada", "technical_lead", vec![])];
        let mut terminal = Terminal::new(TestBackend::new(120, 36)).unwrap();
        terminal.draw(|frame| render(frame, &mut app)).unwrap();
        let (left, top, right, bottom) = app.team_flow.node_terminal_rect(0).unwrap();
        let middle = u16::try_from((top + bottom) / 2).unwrap();
        let buffer = terminal.backend().buffer();
        assert_eq!(buffer[(u16::try_from(left).unwrap(), middle)].symbol(), "│");
        assert_eq!(
            buffer[(u16::try_from(right - 1).unwrap(), middle)].symbol(),
            "│"
        );
    }

    #[test]
    fn connector_ports_change_when_a_specialist_moves_beside_the_lead() {
        let mut app = AppModel::new(SessionId::new());
        let lead = agent("Ada", "technical_lead", vec!["Linus".into()]);
        let specialist = agent("Linus", "backend_developer", vec![]);
        app.team_lead = Some(lead.id);
        app.agents = vec![lead, specialist];
        let mut terminal = Terminal::new(TestBackend::new(120, 36)).unwrap();
        terminal.draw(|frame| render(frame, &mut app)).unwrap();
        let lead_position = app.team_flow.node_position(0).unwrap();
        app.team_flow.set_node_position_for_test(
            1,
            rataflow::Position::new(lead_position.x + 100.0, lead_position.y),
        );
        terminal.draw(|frame| render(frame, &mut app)).unwrap();
        let edge = &app.team_flow.edges()[0];
        assert_eq!(edge.source_handle.as_deref(), Some("right"));
        assert_eq!(edge.target_handle.as_deref(), Some("left"));
    }

    #[test]
    fn mouse_selecting_a_role_draws_its_lead_connection_solid_and_yellow() {
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
        assert_eq!(app.active_role_tab, None);
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
        assert_eq!(app.active_role_tab, Some(specialist_id));
        app.reduce(Action::ShowTeam);
        terminal.draw(|frame| render(frame, &mut app)).unwrap();

        assert_eq!(app.team_flow.mouse_highlight(), Some(specialist_id));
        let buffer = terminal.backend().buffer();
        let yellow_route_cells = (0..buffer.area.height)
            .flat_map(|row| (0..buffer.area.width).map(move |column| &buffer[(column, row)]))
            .filter(|cell| {
                cell.fg == theme::YELLOW
                    && matches!(
                        cell.symbol(),
                        "─" | "│" | "┌" | "┐" | "└" | "┘" | "◆" | "▲" | "△" | "●"
                    )
            })
            .count();
        assert!(yellow_route_cells > 0);
    }
    #[test]
    fn compact_output_keeps_latest_text_and_expands_without_loss() {
        let text = (0..50)
            .map(|n| format!("line {n} 🦀\n"))
            .collect::<String>();
        let collapsed = compact_text(&text, false, true);
        assert!(collapsed.contains("line 49"));
        assert!(!collapsed.contains("line 0 "));
        assert!(collapsed.contains("[v] expand"));
        assert_eq!(compact_text(&text, true, true), text);
        let mut app = AppModel::new(SessionId::new());
        app.reduce(Action::ToggleExpanded);
        assert!(app.expanded_output);
        assert_eq!(menu_tab("summary", false).style.fg, Some(theme::TEXT));
        assert_eq!(menu_tab("summary", true).style.bg, Some(theme::BORDER));
        assert_eq!(
            menu_tab("summary", true).content,
            menu_tab("summary", false).content
        );
    }

    #[test]
    fn continuation_choices_remain_on_border_and_override_composer_input() {
        let mut app = AppModel::new(SessionId::new());
        let approval = Approval {
            id: voff_core::ApprovalId::new(),
            task_id: voff_core::TaskId::new(),
            tool_call_id: voff_core::ToolCallId::new(),
            action: "provider.continue".into(),
            working_directory: "current task only".into(),
            reason: serde_json::to_string(&OutputContinuation {
                previous_tokens: 4096,
                next_tokens: 8192,
                conversation: false,
            })
            .unwrap(),
            decision: ApprovalDecision::Pending,
            created_at: chrono::Utc::now(),
        };
        app.approvals.push(approval.clone());
        app.composer_active = true;
        assert_eq!(
            app.handle_composer_event(&Event::Key(crossterm::event::KeyEvent::new(
                KeyCode::Char('y'),
                KeyModifiers::NONE
            ))),
            ComposerEvent::Ignored
        );
        for (width, height) in [(80, 24), (40, 10)] {
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            app.approval_details = false;
            terminal
                .draw(|frame| render_approval_popup(frame, &app, &approval))
                .unwrap();
            let buffer = terminal.backend().buffer();
            let rendered = (0..height)
                .flat_map(|y| (0..width).map(move |x| buffer[(x, y)].symbol()))
                .collect::<String>();
            assert!(rendered.contains("[y] [n] [d] details"), "{rendered}");
            let area = compact_approval_area(buffer.area);
            assert_eq!(area.right(), width);
            assert_eq!(area.y, 0);
            assert_eq!(area.height, 5);
            assert_eq!(buffer[(area.x, 0)].symbol(), "╭");
            assert_eq!(buffer[(area.x, 5)].symbol(), " ");
            app.reduce(Action::ToggleApprovalDetails);
            assert!(app.approval_details);
            terminal
                .draw(|frame| render_approval_popup(frame, &app, &approval))
                .unwrap();
            let buffer = terminal.backend().buffer();
            let rendered = (0..height)
                .flat_map(|y| (0..width).map(move |x| buffer[(x, y)].symbol()))
                .collect::<String>();
            assert!(rendered.contains("[y] Continue"), "{rendered}");
            assert!(rendered.contains("[n] Cancel"), "{rendered}");
            app.reduce(Action::PageDown);
            assert_eq!(app.approval_scroll, 10);
            app.reduce(Action::Escape);
            assert!(!app.approval_details);
            assert!(!app.quit);
        }
    }
}
