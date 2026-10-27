//! Interactive workspace setup and team builder.

use crate::model_catalog::ModelCatalog;
use crate::terminal_colors::ColorFallbackBackend;
use crate::theme;
use ansi_to_tui::IntoText as _;
use crossterm::{
    event::{
        self, DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
        Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent,
        MouseEventKind,
    },
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{
    Terminal,
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{
        Block, BorderType, Borders, Clear, FrameExt as _, Paragraph, Scrollbar,
        ScrollbarOrientation, ScrollbarState, Wrap,
    },
};
use ratatui_explorer::{FileExplorer, FileExplorerBuilder, Theme};
use ratatui_textarea::TextArea;
use serde::{Deserialize, Serialize};
use std::{
    cell::Cell,
    fs,
    io::{self, IsTerminal, Read as _},
    panic,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tokio_util::sync::CancellationToken;
use voff_core::{MAX_OUTPUT_TOKENS, MessageId, TaskId};
use voff_prompts::setup::{DEFAULT_BUILD_SETUP_PROMPT, DEFAULT_SCAN_SETUP_PROMPT};
use voff_provider::{
    CanonicalMessage, CanonicalRequest, MessageRole, ModelProvider, ProviderEvent, ToolChoice,
    ToolDefinition, collect_response,
};

const DEFAULT_OUTPUT_TOKENS: u64 = voff_core::DEFAULT_MAX_OUTPUT_TOKENS;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default)]
pub(crate) struct SetupPrompts {
    pub scan_workspace: String,
    pub build_from_description: String,
}

impl Default for SetupPrompts {
    fn default() -> Self {
        Self {
            scan_workspace: DEFAULT_SCAN_SETUP_PROMPT.into(),
            build_from_description: DEFAULT_BUILD_SETUP_PROMPT.into(),
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WorkspaceTeamPlan {
    summary: String,
    agents: Vec<WorkspaceAgentPlan>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WorkspaceAgentPlan {
    name: String,
    instructions: String,
    reports_to: Option<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum ProviderChoice {
    Ollama,
    OpenAi,
    ClaudeCloud,
    CodexCli,
    ClaudeCli,
}

impl ProviderChoice {
    const ALL: [Self; 5] = [
        Self::Ollama,
        Self::OpenAi,
        Self::ClaudeCloud,
        Self::CodexCli,
        Self::ClaudeCli,
    ];

    fn is_available(self) -> bool {
        match self {
            Self::Ollama => executable_available("ollama"),
            Self::OpenAi => {
                std::env::var_os("OPENAI_API_KEY").is_some_and(|value| !value.is_empty())
            }
            Self::ClaudeCloud => {
                std::env::var_os("ANTHROPIC_API_KEY").is_some_and(|value| !value.is_empty())
            }
            Self::CodexCli => executable_available("codex"),
            Self::ClaudeCli => executable_available("claude"),
        }
    }

    fn available_ordered() -> Vec<Self> {
        [
            Self::CodexCli,
            Self::ClaudeCli,
            Self::Ollama,
            Self::OpenAi,
            Self::ClaudeCloud,
        ]
        .into_iter()
        .filter(|provider| provider.is_available())
        .collect()
    }

    pub(crate) fn default_provider() -> Self {
        preferred_provider()
    }

    pub(crate) fn from_config_name(value: &str) -> Result<Self, String> {
        match value.to_ascii_lowercase().as_str() {
            "ollama" => Ok(Self::Ollama),
            "openai" | "open_ai" => Ok(Self::OpenAi),
            "anthropic" | "claude_api" | "claude_cloud" => Ok(Self::ClaudeCloud),
            "codex" | "codex_cli" => Ok(Self::CodexCli),
            "claude" | "claude_cli" => Ok(Self::ClaudeCli),
            _ => Err(format!("unsupported provider `{value}`")),
        }
    }

    #[must_use]
    pub const fn id(self) -> &'static str {
        match self {
            Self::Ollama => "ollama",
            Self::OpenAi => "openai",
            Self::ClaudeCloud => "anthropic",
            Self::CodexCli => "codex_cli",
            Self::ClaudeCli => "claude_cli",
        }
    }

    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Ollama => "Ollama · local",
            Self::OpenAi => "OpenAI · cloud",
            Self::ClaudeCloud => "Claude · cloud",
            Self::CodexCli => "Codex · CLI",
            Self::ClaudeCli => "Claude · CLI",
        }
    }

    pub(crate) fn credential_env(self) -> Option<&'static str> {
        match self {
            Self::OpenAi => Some("OPENAI_API_KEY"),
            Self::ClaudeCloud => Some("ANTHROPIC_API_KEY"),
            Self::Ollama | Self::CodexCli | Self::ClaudeCli => None,
        }
    }

    pub(crate) fn uses_api_key(self) -> bool {
        self.credential_env().is_some()
    }

    fn cycle(self, delta: isize) -> Self {
        cycle(Self::ALL, self, delta)
    }
}

fn preferred_provider() -> ProviderChoice {
    ProviderChoice::available_ordered()
        .into_iter()
        .next()
        .unwrap_or(ProviderChoice::CodexCli)
}

fn executable_available(binary: &str) -> bool {
    std::env::var_os("PATH").is_some_and(|paths| {
        std::env::split_paths(&paths).any(|directory| {
            let candidate = directory.join(binary);
            if !candidate.is_file() {
                return false;
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt as _;
                candidate
                    .metadata()
                    .is_ok_and(|metadata| metadata.permissions().mode() & 0o111 != 0)
            }
            #[cfg(not(unix))]
            {
                true
            }
        })
    })
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum DeveloperRole {
    Lead,
    Frontend,
    Backend,
    Reviewer,
    Custom,
}

impl DeveloperRole {
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Lead => "Team Lead",
            Self::Frontend => "Frontend Agent",
            Self::Backend => "Backend Agent",
            Self::Reviewer => "Reviewer Agent",
            Self::Custom => "Custom Agent",
        }
    }

    #[must_use]
    pub const fn config_role(self) -> &'static str {
        match self {
            Self::Lead => "team_lead",
            Self::Frontend => "frontend_agent",
            Self::Backend => "backend_agent",
            Self::Reviewer => "reviewer_agent",
            Self::Custom => "custom_agent",
        }
    }

    #[must_use]
    pub fn tools(self) -> Vec<String> {
        let values: &[&str] = match self {
            Self::Lead => &[
                "file.read",
                "search.text",
                "task.delegate",
                "review.request",
            ],
            Self::Reviewer => &["file.read", "search.text", "process.test"],
            Self::Frontend | Self::Backend | Self::Custom => {
                &["file.read", "search.text", "file.patch", "process.test"]
            }
        };
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    fn cycle_specialist(self, delta: isize) -> Self {
        const SPECIALISTS: [DeveloperRole; 4] = [
            DeveloperRole::Backend,
            DeveloperRole::Frontend,
            DeveloperRole::Reviewer,
            DeveloperRole::Custom,
        ];
        let current = if self == Self::Lead {
            Self::Backend
        } else {
            self
        };
        cycle(SPECIALISTS, current, delta)
    }
}

fn cycle<T: Copy + Eq, const N: usize>(values: [T; N], current: T, delta: isize) -> T {
    let current = values
        .iter()
        .position(|value| *value == current)
        .unwrap_or(0);
    let next = (current as isize + delta).rem_euclid(N as isize) as usize;
    values[next]
}

#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
pub struct DeveloperDraft {
    pub name: String,
    pub role: DeveloperRole,
    #[serde(default)]
    pub reports_to: Option<usize>,
    #[serde(default)]
    pub system_prompt: String,
    pub provider: ProviderChoice,
    pub model: String,
    pub max_output_tokens: u64,
    /// Optional replacement token; never serialized into project configuration.
    #[serde(skip)]
    pub api_token: String,
    /// Existing credential lookup to retain when no replacement is entered.
    pub credential_reference: Option<String>,
}

impl std::fmt::Debug for DeveloperDraft {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DeveloperDraft")
            .field("name", &self.name)
            .field("role", &self.role)
            .field("reports_to", &self.reports_to)
            .field("system_prompt", &"[redacted]")
            .field("provider", &self.provider)
            .field("model", &self.model)
            .field("max_output_tokens", &self.max_output_tokens)
            .field("api_token", &"[redacted]")
            .finish()
    }
}

impl DeveloperDraft {
    #[cfg(test)]
    #[must_use]
    pub fn new(
        name: impl Into<String>,
        role: DeveloperRole,
        provider: ProviderChoice,
        model: impl Into<String>,
        max_output_tokens: u64,
    ) -> Self {
        Self {
            name: name.into(),
            role,
            reports_to: None,
            system_prompt: String::new(),
            provider,
            model: model.into(),
            max_output_tokens,
            api_token: String::new(),
            credential_reference: None,
        }
    }

    fn suggested(
        index: usize,
        count: usize,
        provider: ProviderChoice,
        model: String,
        max_output_tokens: u64,
    ) -> Self {
        let role = suggested_role(index, count);
        Self {
            name: suggested_name(role, index),
            role,
            reports_to: (index != 0).then_some(0),
            system_prompt: String::new(),
            provider,
            model,
            max_output_tokens,
            api_token: String::new(),
            credential_reference: None,
        }
    }
}

#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
pub struct SetupDraft {
    pub developers: Vec<DeveloperDraft>,
    pub working_directory: PathBuf,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Step {
    Basics,
    WorkspacePicker,
    Developer(usize),
    #[allow(dead_code)]
    Prompt(usize),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Field {
    Role,
    Name,
    ReportsTo,
    Provider,
    Model,
    OutputLimit,
    Token,
    Prompt,
}

impl Field {
    fn next(self) -> Self {
        match self {
            Self::Role => Self::Name,
            Self::Name => Self::ReportsTo,
            Self::ReportsTo => Self::Provider,
            Self::Provider => Self::Model,
            Self::Model => Self::OutputLimit,
            Self::OutputLimit => Self::Token,
            Self::Token => Self::Prompt,
            Self::Prompt => Self::Role,
        }
    }

    fn previous(self) -> Self {
        match self {
            Self::Role => Self::Prompt,
            Self::Name => Self::Role,
            Self::ReportsTo => Self::Name,
            Self::Provider => Self::ReportsTo,
            Self::Model => Self::Provider,
            Self::Token => Self::OutputLimit,
            Self::OutputLimit => Self::Model,
            Self::Prompt => Self::Token,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum WizardMode {
    Startup,
    Add,
    Configure,
    Workspace,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum BasicsField {
    WorkingDirectory,
}

pub(crate) struct Wizard {
    step: Step,
    basics_field: BasicsField,
    working_directory: PathBuf,
    workspace_explorer: Option<FileExplorer>,
    workspace_filter: String,
    workspace_filtering: bool,
    developers: Vec<DeveloperDraft>,
    field: Field,
    prompt_editor: Option<TextArea<'static>>,
    model_browser: Option<ModelBrowser>,
    model_touched: bool,
    limit_touched: bool,
    error: Option<String>,
    cancelled: bool,
    complete: bool,
    catalog: ModelCatalog,
    editing: bool,
    mode: WizardMode,
}

pub(crate) enum StartupChoice {
    Scan {
        workspace: PathBuf,
        popup_size: (u16, u16),
        provider: ProviderChoice,
        model: String,
        api_token: Option<String>,
        terminal: Option<TerminalHandoff>,
    },
    Build {
        workspace: PathBuf,
        popup_size: (u16, u16),
        provider: ProviderChoice,
        model: String,
        api_token: Option<String>,
        description: String,
        terminal: Option<TerminalHandoff>,
    },
    Quit,
}

pub(crate) struct TerminalHandoff {
    guard: TerminalGuard,
    terminal: Terminal<ColorFallbackBackend<io::Stdout>>,
}

impl TerminalHandoff {
    pub(crate) fn adopt(mut self) -> Terminal<ColorFallbackBackend<io::Stdout>> {
        self.guard.handoff();
        self.terminal
    }
}

enum StartupPickerResult<T> {
    Quit,
    Selection(Option<T>),
}

fn is_ctrl_c(event: &Event) -> bool {
    matches!(event, Event::Key(key)
        if key.kind == KeyEventKind::Press
            && key.code == KeyCode::Char('c')
            && key.modifiers.contains(KeyModifiers::CONTROL))
}

impl StartupChoice {
    pub(crate) fn popup_size(&self) -> Option<(u16, u16)> {
        match self {
            Self::Scan { popup_size, .. } | Self::Build { popup_size, .. } => Some(*popup_size),
            Self::Quit => None,
        }
    }

    pub(crate) fn workspace(&self) -> Option<&Path> {
        match self {
            Self::Scan { workspace, .. } | Self::Build { workspace, .. } => Some(workspace),
            Self::Quit => None,
        }
    }

    pub(crate) fn set_workspace(&mut self, selected: PathBuf) {
        match self {
            Self::Scan { workspace, .. } | Self::Build { workspace, .. } => {
                *workspace = selected;
            }
            Self::Quit => {}
        }
    }

    pub(crate) fn take_terminal_handoff(&mut self) -> Option<TerminalHandoff> {
        match self {
            Self::Scan { terminal, .. } | Self::Build { terminal, .. } => terminal.take(),
            Self::Quit => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum StartupField {
    Workspace,
    Provider,
    Model,
    Goal,
    ApiKey,
}

impl StartupField {
    fn next(self, model_picker_available: bool, api_key_field: bool) -> Self {
        match self {
            Self::Workspace => Self::Provider,
            Self::Provider if model_picker_available => Self::Model,
            Self::Provider => Self::Goal,
            Self::Model if api_key_field => Self::ApiKey,
            Self::Model | Self::ApiKey => Self::Goal,
            Self::Goal => Self::Workspace,
        }
    }

    fn previous(self, model_picker_available: bool, api_key_field: bool) -> Self {
        match self {
            Self::Workspace => Self::Goal,
            Self::Provider => Self::Workspace,
            Self::Model => Self::Provider,
            Self::Goal if api_key_field => Self::ApiKey,
            Self::Goal if model_picker_available => Self::Model,
            Self::Goal => Self::Provider,
            Self::ApiKey if model_picker_available => Self::Model,
            Self::ApiKey => Self::Provider,
        }
    }
}

#[derive(Clone, Copy)]
struct StartupView<'a> {
    workspace: &'a Path,
    show_setup: bool,
    selected_provider: ProviderChoice,
    model: &'a str,
    model_picker_available: bool,
    api_token: &'a str,
    focused: StartupField,
    description: &'a str,
    error: Option<&'a str>,
}

#[derive(Debug)]
struct ModelBrowser {
    query: String,
    selected: usize,
}

impl ModelBrowser {
    fn matches(&self, catalog: &ModelCatalog, provider: &str) -> Vec<usize> {
        let query = self.query.to_lowercase();
        catalog
            .models(provider)
            .iter()
            .enumerate()
            .filter_map(|(index, model)| model.id.to_lowercase().contains(&query).then_some(index))
            .collect()
    }
}

fn key_hint_line(hints: &[(&str, &str)]) -> Line<'static> {
    let mut spans = Vec::with_capacity(hints.len() * 3);
    for (index, (key, action)) in hints.iter().enumerate() {
        if index > 0 {
            spans.push(Span::styled("· ", Style::default().fg(theme::MUTED)));
        }
        spans.push(Span::styled(
            format!("[{key}]"),
            Style::default()
                .fg(theme::AQUA)
                .add_modifier(Modifier::BOLD),
        ));
        spans.push(Span::styled(
            format!(" {action} "),
            Style::default().fg(theme::MUTED),
        ));
    }
    Line::from(spans)
}

fn model_picker_title(title: &str) -> Line<'static> {
    Line::from(Span::styled(
        format!(" {title} "),
        Style::default()
            .fg(theme::AQUA)
            .add_modifier(Modifier::BOLD),
    ))
}

fn model_picker_footer(close_action: &str) -> Line<'static> {
    key_hint_line(&[("↑/↓", "move"), ("Enter", "select"), ("Esc", close_action)])
        .alignment(Alignment::Center)
}

fn model_picker_width(
    area: Rect,
    title: &str,
    minimum: u16,
    maximum: u16,
    close_action: &str,
) -> u16 {
    let title = model_picker_title(title);
    let footer = model_picker_footer(close_action);
    picker_popup_width(area, &title, minimum, maximum)
        .max(picker_popup_width(area, &footer, minimum, maximum))
}

fn picker_popup_width(area: Rect, title: &Line<'_>, minimum: u16, maximum: u16) -> u16 {
    let desired = u16::try_from(title.width().saturating_add(4))
        .unwrap_or(u16::MAX)
        .max(minimum);
    desired.min(maximum).min(area.width)
}

impl std::fmt::Debug for Wizard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Wizard")
            .field("step", &self.step)
            .field("developer_count", &self.developers.len())
            .field("prompt_editor_open", &self.prompt_editor.is_some())
            .field("editing", &self.editing)
            .finish()
    }
}

impl Default for Wizard {
    fn default() -> Self {
        Self::new(ModelCatalog::default(), false)
    }
}

impl Wizard {
    fn new(catalog: ModelCatalog, _credential_available: bool) -> Self {
        Self {
            step: Step::Basics,
            basics_field: BasicsField::WorkingDirectory,
            working_directory: ".".into(),
            workspace_explorer: None,
            workspace_filter: String::new(),
            workspace_filtering: false,
            developers: vec![],
            field: Field::Role,
            prompt_editor: None,
            model_browser: None,
            model_touched: false,
            limit_touched: false,
            error: None,
            cancelled: false,
            complete: false,
            catalog,
            editing: false,
            mode: WizardMode::Startup,
        }
    }

    #[cfg(test)]
    fn startup(catalog: ModelCatalog, workspace: &Path) -> Self {
        let provider = preferred_provider();
        let model = catalog.first_model(provider.id());
        let mut wizard = Self::new(catalog, false);
        wizard.working_directory = workspace.to_path_buf();
        wizard.developers = vec![DeveloperDraft::suggested(
            0,
            1,
            provider,
            model,
            DEFAULT_OUTPUT_TOKENS,
        )];
        wizard.step = Step::Developer(0);
        wizard.field = Field::Name;
        wizard
    }

    pub(crate) fn workspace(workspace: &Path) -> Self {
        let mut wizard = Self::new(ModelCatalog::default(), false);
        wizard.working_directory = workspace.to_path_buf();
        wizard.mode = WizardMode::Workspace;
        wizard.open_workspace_picker();
        wizard
    }

    pub(crate) fn selected_workspace(&self) -> &Path {
        &self.working_directory
    }

    /// Reuse startup setup with current team values; the running workspace is fixed.
    pub(crate) fn edit(catalog: ModelCatalog, draft: SetupDraft) -> Self {
        Self {
            developers: draft.developers,
            working_directory: draft.working_directory,
            editing: true,
            ..Self::new(catalog, false)
        }
    }

    pub(crate) fn add_agent(catalog: ModelCatalog, draft: SetupDraft, parent: usize) -> Self {
        let mut wizard = Self::edit(catalog, draft);
        let index = wizard.developers.len();
        let lead = &wizard.developers[0];
        let provider = lead.provider;
        let model = lead.model.clone();
        let mut agent =
            DeveloperDraft::suggested(index, index + 1, provider, model, DEFAULT_OUTPUT_TOKENS);
        agent.role = DeveloperRole::Custom;
        agent.name = unique_name(&format!("Agent {}", index + 1), &wizard.developers, None);
        agent.credential_reference = lead.credential_reference.clone();
        agent.reports_to = Some(parent.min(index.saturating_sub(1)));
        wizard.developers.push(agent);
        wizard.mode = WizardMode::Add;
        wizard.step = Step::Developer(index);
        wizard
    }

    pub(crate) fn configure_agent(catalog: ModelCatalog, draft: SetupDraft, index: usize) -> Self {
        let mut wizard = Self::edit(catalog, draft);
        wizard.mode = WizardMode::Configure;
        wizard.step = Step::Developer(index.min(wizard.developers.len().saturating_sub(1)));
        wizard.field = Field::Prompt;
        wizard
    }

    pub(crate) fn handle_event(&mut self, event: &Event, area: Rect) {
        match event {
            Event::Key(key) => self.handle_key(*key),
            Event::Mouse(mouse) => self.handle_mouse(*mouse, area),
            Event::Paste(value) => self.handle_paste(value),
            _ => {}
        }
    }

    fn shell(&self, area: Rect) -> Rect {
        let available = self.available_shell_area(area);
        let shell = self.layout_shell(available);
        self.logo_area(available).map_or(shell, |logo| {
            Rect::new(
                shell.x,
                logo.bottom().saturating_add(1),
                shell.width,
                shell.height,
            )
        })
    }

    fn logo_area(&self, area: Rect) -> Option<Rect> {
        if self.editing
            || self.mode != WizardMode::Startup
            || !matches!(self.step, Step::Developer(0) | Step::Prompt(0))
        {
            return None;
        }
        let logo = logo_text()?;
        let width = u16::try_from(logo.lines.iter().map(Line::width).max().unwrap_or(0)).ok()?;
        let height = u16::try_from(logo.lines.len()).ok()?;
        let shell = self.layout_shell(area);
        let total = height.saturating_add(1).saturating_add(shell.height);
        let prompt_edit = self.prompt_editor.is_some();
        if width > area.width
            || (total.saturating_add(2) > area.height && !prompt_edit)
            || shell.height < 12
        {
            return None;
        }
        Some(Rect::new(
            area.x + area.width.saturating_sub(width) / 2,
            area.y + area.height.saturating_sub(total) / 2,
            width,
            height,
        ))
    }

    fn available_shell_area(&self, area: Rect) -> Rect {
        if self.editing {
            area.inner(ratatui::layout::Margin::new(
                if area.width >= 12 { 2 } else { 0 },
                if area.height >= 16 { 2 } else { 0 },
            ))
        } else {
            area
        }
    }

    fn layout_shell(&self, area: Rect) -> Rect {
        let step = match self.step {
            Step::Prompt(index) => Step::Developer(index),
            step => step,
        };
        let shell = setup_shell(area, step);
        if self.mode == WizardMode::Startup
            && self.prompt_editor.is_some()
            && step == Step::Developer(0)
            && let Some(logo) = logo_text()
        {
            let logo_height = u16::try_from(logo.lines.len()).unwrap_or(u16::MAX);
            let available_height = area.height.saturating_sub(logo_height.saturating_add(1));
            if available_height >= 12 {
                return centered(area, shell.width, shell.height.min(available_height));
            }
        }
        shell
    }

    fn handle_mouse(&mut self, mouse: MouseEvent, area: Rect) {
        let shell = self.shell(area);
        if self.model_browser.is_some() {
            self.handle_model_browser_mouse(mouse, shell);
            return;
        }
        if !shell.contains((mouse.column, mouse.row).into()) {
            return;
        }
        let content = setup_rows(shell, self.step, self.mode)[1];
        if matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left))
            && mouse.row == shell.bottom().saturating_sub(1)
        {
            let key = if self.prompt_editor.is_some() || matches!(self.step, Step::Prompt(_)) {
                KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL)
            } else {
                KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)
            };
            self.handle_key(key);
            return;
        }
        match self.step {
            Step::Basics => {
                let fields = centered(content, content.width.min(72), 1);
                if matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left))
                    && mouse.row == fields.y
                {
                    self.basics_field = BasicsField::WorkingDirectory;
                    self.open_workspace_picker();
                }
            }
            Step::Developer(index) => {
                let cols = developer_columns(content);
                if self.prompt_editor.is_some() {
                    if cols[1].contains((mouse.column, mouse.row).into())
                        && let Some(editor) = &mut self.prompt_editor
                    {
                        editor.input(mouse);
                    }
                    return;
                }
                if cols[1].contains((mouse.column, mouse.row).into()) {
                    return;
                }
                if !cols[0].contains((mouse.column, mouse.row).into()) {
                    return;
                }
                let row = mouse.row.saturating_sub(cols[0].y.saturating_add(1));
                let field = developer_field_at_row(
                    row,
                    self.developers[index].provider,
                    cols[0].height < 16,
                    index == 0,
                    self.mode == WizardMode::Configure,
                );
                if let Some(field) = field {
                    self.field = field;
                    if field == Field::Model {
                        self.model_touched = false;
                    }
                    match mouse.kind {
                        MouseEventKind::Down(MouseButton::Left) => {
                            if field == Field::Model {
                                self.open_model_browser(index);
                            } else if matches!(
                                field,
                                Field::Role | Field::Provider | Field::ReportsTo
                            ) {
                                self.cycle_choice(index, 1);
                            }
                        }
                        MouseEventKind::ScrollUp => self.cycle_choice(index, 1),
                        MouseEventKind::ScrollDown => self.cycle_choice(index, -1),
                        _ => {}
                    }
                }
            }
            Step::Prompt(_) => {
                let cols = developer_columns(content);
                if cols[1].contains((mouse.column, mouse.row).into())
                    && let Some(editor) = &mut self.prompt_editor
                {
                    editor.input(mouse);
                }
            }
            Step::WorkspacePicker => {
                if let Some(explorer) = &mut self.workspace_explorer {
                    let _ = explorer.handle(&Event::Mouse(mouse));
                }
            }
        }
    }

    pub(crate) fn cancelled(&self) -> bool {
        self.cancelled
    }
    pub(crate) fn complete(&self) -> bool {
        self.complete
    }

    pub(crate) fn into_draft(self) -> SetupDraft {
        SetupDraft {
            developers: self.developers,
            working_directory: self.working_directory,
        }
    }

    fn handle_key(&mut self, key: KeyEvent) {
        if key.kind != KeyEventKind::Press {
            return;
        }
        self.error = None;
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            self.cancelled = true;
            return;
        }
        if self.prompt_editor.is_some()
            && let Step::Developer(index) | Step::Prompt(index) = self.step
        {
            self.handle_prompt_key(index, key);
            return;
        }
        if let Step::Developer(index) = self.step
            && self.model_browser.is_some()
        {
            self.handle_model_browser_key(index, key);
            return;
        }
        match self.step {
            Step::Basics => self.handle_basics(key),
            Step::WorkspacePicker => self.handle_workspace_picker(key),
            Step::Developer(index) => self.handle_developer(index, key),
            Step::Prompt(index) => self.handle_prompt_key(index, key),
        }
    }

    fn handle_basics(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => self.cancelled = true,
            KeyCode::Tab | KeyCode::BackTab | KeyCode::Up | KeyCode::Down => {}
            KeyCode::Char('q') => self.cancelled = true,
            KeyCode::Char(value) if value.is_ascii_digit() => {}
            KeyCode::Backspace
            | KeyCode::Right
            | KeyCode::Left
            | KeyCode::Char('+')
            | KeyCode::Char('-') => {}
            KeyCode::Enter | KeyCode::Char('c') => {
                if key.code == KeyCode::Char('c')
                    && self.basics_field == BasicsField::WorkingDirectory
                {
                    self.open_workspace_picker();
                } else if self.valid_working_directory().is_some() {
                    let provider = preferred_provider();
                    let model = self.catalog.first_model(provider.id());
                    let previous = std::mem::take(&mut self.developers);
                    self.developers = vec![previous.into_iter().next().unwrap_or_else(|| {
                        DeveloperDraft::suggested(0, 1, provider, model, DEFAULT_OUTPUT_TOKENS)
                    })];
                    self.step = Step::Developer(0);
                    self.field = Field::Name;
                } else {
                    self.error = Some("Choose an existing working directory.".into());
                }
            }
            _ => {}
        }
    }

    fn open_workspace_picker(&mut self) {
        if self.editing {
            self.error = Some("The active workspace stays fixed while editing its team.".into());
            return;
        }
        let start = match self.working_directory.canonicalize() {
            Ok(path) => path,
            Err(error) => {
                self.error = Some(format!("Cannot open that workspace: {error}"));
                return;
            }
        };
        let theme = Theme::default()
            .with_block(Block::default().style(theme::surface()))
            .with_highlight_dir_style(
                Style::default()
                    .fg(theme::ORANGE)
                    .add_modifier(Modifier::BOLD),
            )
            .with_highlight_item_style(
                Style::default()
                    .fg(theme::ORANGE)
                    .add_modifier(Modifier::BOLD),
            )
            .with_highlight_symbol("");
        match FileExplorerBuilder::default()
            .working_dir(start)
            .filter_map(|file| file.is_dir.then_some(file))
            .theme(theme)
            .build()
        {
            Ok(explorer) => {
                self.workspace_explorer = Some(explorer);
                self.workspace_filter.clear();
                self.workspace_filtering = false;
                self.step = Step::WorkspacePicker;
            }
            Err(error) => self.error = Some(format!("Cannot browse that workspace: {error}")),
        }
    }

    fn handle_workspace_picker(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.cancelled = true;
            }
            KeyCode::Esc => {
                if self.workspace_filtering {
                    self.workspace_filtering = false;
                } else if !self.workspace_filter.is_empty() {
                    self.workspace_filter.clear();
                    self.refresh_workspace_filter();
                } else {
                    self.workspace_explorer = None;
                    if self.mode == WizardMode::Workspace {
                        self.cancelled = true;
                    } else {
                        self.step = Step::Basics;
                    }
                }
            }
            KeyCode::Enter => {
                if let Some(explorer) = &self.workspace_explorer {
                    self.working_directory = explorer.current().path.clone();
                }
                self.workspace_explorer = None;
                self.workspace_filter.clear();
                self.workspace_filtering = false;
                if self.mode == WizardMode::Workspace {
                    self.complete = true;
                } else {
                    self.step = Step::Basics;
                }
            }
            KeyCode::Char(' ') => {
                if let Some(explorer) = &self.workspace_explorer {
                    self.working_directory = explorer.cwd().clone();
                }
                self.workspace_explorer = None;
                self.workspace_filter.clear();
                self.workspace_filtering = false;
                if self.mode == WizardMode::Workspace {
                    self.complete = true;
                } else {
                    self.step = Step::Basics;
                }
            }
            KeyCode::Char('/') if !self.workspace_filtering => {
                self.workspace_filtering = true;
            }
            KeyCode::Backspace if self.workspace_filtering => {
                self.workspace_filter.pop();
                self.refresh_workspace_filter();
            }
            KeyCode::Char('u')
                if self.workspace_filtering && key.modifiers.contains(KeyModifiers::CONTROL) =>
            {
                self.workspace_filter.clear();
                self.refresh_workspace_filter();
            }
            KeyCode::Char(value)
                if self.workspace_filtering
                    && !key
                        .modifiers
                        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                self.push_workspace_filter(value);
            }
            _ => {
                let previous_cwd = self
                    .workspace_explorer
                    .as_ref()
                    .map(|explorer| explorer.cwd().clone());
                if let Some(explorer) = &mut self.workspace_explorer
                    && let Err(error) = explorer.handle(&Event::Key(key))
                {
                    self.error = Some(format!("Cannot open that directory: {error}"));
                }
                let changed_directory = previous_cwd.is_some_and(|previous| {
                    self.workspace_explorer
                        .as_ref()
                        .is_some_and(|explorer| explorer.cwd() != &previous)
                });
                if changed_directory {
                    self.workspace_filter.clear();
                    self.workspace_filtering = false;
                    self.refresh_workspace_filter();
                }
            }
        }
    }

    fn push_workspace_filter(&mut self, value: char) {
        let previous = self.workspace_filter.clone();
        self.workspace_filter.push(value);
        if !self.refresh_workspace_filter() {
            self.workspace_filter = previous;
            self.refresh_workspace_filter();
            self.error = Some("No matching directories.".into());
        }
    }

    fn refresh_workspace_filter(&mut self) -> bool {
        let query = self.workspace_filter.to_lowercase();
        let Some(explorer) = &mut self.workspace_explorer else {
            return false;
        };
        match explorer.set_filter_map(move |file| {
            let matches =
                query.is_empty() || file.name == "../" || file.name.to_lowercase().contains(&query);
            (file.is_dir && matches).then_some(file)
        }) {
            Ok(()) => !explorer.files().is_empty(),
            Err(error) => {
                self.error = Some(format!("Cannot filter that directory: {error}"));
                false
            }
        }
    }

    fn handle_developer(&mut self, index: usize, key: KeyEvent) {
        match key.code {
            KeyCode::Char('e') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.open_prompt_editor(index);
            }
            KeyCode::Char('e')
                if !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
                    && !matches!(self.field, Field::Name | Field::Model | Field::Token) =>
            {
                self.open_prompt_editor(index);
            }
            KeyCode::Char('f')
                if self.field == Field::Model
                    && !self.model_touched
                    && !key
                        .modifiers
                        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                self.open_model_browser(index);
            }
            KeyCode::Esc => {
                if self.mode != WizardMode::Startup || index == 0 {
                    self.cancelled = true;
                } else {
                    self.step = Step::Developer(index - 1);
                }
            }
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.cancelled = true;
            }
            KeyCode::Tab | KeyCode::Down => self.move_field(index, true),
            KeyCode::BackTab | KeyCode::Up => self.move_field(index, false),
            KeyCode::Char('j')
                if !matches!(self.field, Field::Name | Field::Model | Field::Token) =>
            {
                self.move_field(index, true)
            }
            KeyCode::Char('k')
                if !matches!(self.field, Field::Name | Field::Model | Field::Token) =>
            {
                self.move_field(index, false)
            }
            KeyCode::Left => self.cycle_choice(index, -1),
            KeyCode::Right => self.cycle_choice(index, 1),
            KeyCode::Char('h')
                if !matches!(self.field, Field::Name | Field::Model | Field::Token) =>
            {
                self.cycle_choice(index, -1)
            }
            KeyCode::Char('l')
                if !matches!(self.field, Field::Name | Field::Model | Field::Token) =>
            {
                self.cycle_choice(index, 1)
            }
            KeyCode::Backspace => match self.field {
                Field::Name => {
                    self.developers[index].name.pop();
                }
                Field::Model => {
                    self.developers[index].model.pop();
                    self.model_touched = true;
                }
                Field::Token => {
                    self.developers[index].api_token.pop();
                }
                Field::OutputLimit => {
                    self.limit_touched = true;
                    self.developers[index].max_output_tokens /= 10;
                }
                Field::Role | Field::ReportsTo | Field::Provider | Field::Prompt => {}
            },
            KeyCode::Char(value)
                if !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
                    && !value.is_control() =>
            {
                match self.field {
                    Field::Name => self.developers[index].name.push(value),
                    Field::Model => {
                        if !self.model_touched {
                            self.developers[index].model.clear();
                            self.model_touched = true;
                        }
                        self.developers[index].model.push(value);
                    }
                    Field::Token => {
                        self.developers[index].api_token.push(value);
                    }
                    Field::OutputLimit if value.is_ascii_digit() => {
                        if !self.limit_touched {
                            self.developers[index].max_output_tokens = 0;
                            self.limit_touched = true;
                        }
                        let digit = value.to_digit(10).map_or(0, u64::from);
                        self.developers[index].max_output_tokens = self.developers[index]
                            .max_output_tokens
                            .saturating_mul(10)
                            .saturating_add(digit)
                            .min(MAX_OUTPUT_TOKENS);
                    }
                    Field::Role | Field::ReportsTo | Field::Provider | Field::Prompt => {}
                    Field::OutputLimit => {}
                }
            }
            KeyCode::Enter => {
                let developer = &self.developers[index];
                if developer.name.trim().is_empty() {
                    self.error = Some("Agent name cannot be empty.".into());
                } else if self
                    .developers
                    .iter()
                    .enumerate()
                    .any(|(other, candidate)| other != index && candidate.name == developer.name)
                {
                    self.error = Some("Agent names must be unique.".into());
                } else if index > 0 && developer.reports_to.is_none() {
                    self.error = Some("Choose who this agent reports to.".into());
                } else if developer.model.trim().is_empty() {
                    self.error = Some("Model cannot be empty.".into());
                } else if self.mode == WizardMode::Configure && developer.max_output_tokens == 0 {
                    self.error = Some("Output limit must be at least 1 token.".into());
                } else if self.mode != WizardMode::Startup {
                    self.complete = true;
                } else if index + 1 < self.developers.len() {
                    self.step = Step::Developer(index + 1);
                    self.field = Field::Role;
                } else {
                    self.complete = true;
                }
            }
            _ => {}
        }
    }

    fn open_model_browser(&mut self, index: usize) {
        let developer = &self.developers[index];
        let models = self.catalog.models(developer.provider.id());
        if models.len() < 2 {
            return;
        }
        let selected = models
            .iter()
            .position(|model| model.id == developer.model)
            .unwrap_or(0);
        self.model_browser = Some(ModelBrowser {
            query: String::new(),
            selected,
        });
    }

    fn model_browser_matches(&self, index: usize) -> Vec<usize> {
        self.model_browser
            .as_ref()
            .map_or_else(Vec::new, |browser| {
                browser.matches(&self.catalog, self.developers[index].provider.id())
            })
    }

    fn move_model_browser_selection(&mut self, index: usize, delta: isize) {
        let len = self.model_browser_matches(index).len();
        if let Some(browser) = &mut self.model_browser
            && len > 0
        {
            browser.selected = (browser.selected as isize + delta)
                .clamp(0, len.saturating_sub(1) as isize) as usize;
        }
    }

    fn handle_model_browser_key(&mut self, index: usize, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => self.model_browser = None,
            KeyCode::Up => self.move_model_browser_selection(index, -1),
            KeyCode::Down => self.move_model_browser_selection(index, 1),
            KeyCode::PageUp => self.move_model_browser_selection(index, -8),
            KeyCode::PageDown => self.move_model_browser_selection(index, 8),
            KeyCode::Home => {
                if let Some(browser) = &mut self.model_browser {
                    browser.selected = 0;
                }
            }
            KeyCode::End => {
                let last = self.model_browser_matches(index).len().saturating_sub(1);
                if let Some(browser) = &mut self.model_browser {
                    browser.selected = last;
                }
            }
            KeyCode::Enter => {
                let matches = self.model_browser_matches(index);
                if let Some(browser) = &self.model_browser
                    && let Some(&model_index) = matches.get(browser.selected)
                    && let Some(model) = self
                        .catalog
                        .models(self.developers[index].provider.id())
                        .get(model_index)
                {
                    self.developers[index].model = model.id.clone();
                    self.model_touched = false;
                    self.model_browser = None;
                }
            }
            KeyCode::Backspace => {
                if let Some(browser) = &mut self.model_browser {
                    browser.query.pop();
                    browser.selected = 0;
                }
            }
            KeyCode::Char(value)
                if !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
                    && !value.is_control() =>
            {
                if let Some(browser) = &mut self.model_browser {
                    browser.query.push(value);
                    browser.selected = 0;
                }
            }
            _ => {}
        }
    }

    fn handle_model_browser_mouse(&mut self, mouse: MouseEvent, shell: Rect) {
        let Step::Developer(index) = self.step else {
            return;
        };
        let area = model_browser_area(shell);
        match mouse.kind {
            MouseEventKind::ScrollUp => self.move_model_browser_selection(index, -1),
            MouseEventKind::ScrollDown => self.move_model_browser_selection(index, 1),
            MouseEventKind::Down(MouseButton::Left) => {
                if !area.contains((mouse.column, mouse.row).into()) {
                    self.model_browser = None;
                    return;
                }
                let row = mouse.row.saturating_sub(area.y.saturating_add(2)) as usize;
                let visible = area.height.saturating_sub(3) as usize;
                let start = self.model_browser.as_ref().map_or(0, |browser| {
                    browser.selected / visible.max(1) * visible.max(1)
                });
                let matches = self.model_browser_matches(index);
                if mouse.row >= area.y.saturating_add(2)
                    && let Some(&model_index) = matches.get(start + row)
                    && let Some(model) = self
                        .catalog
                        .models(self.developers[index].provider.id())
                        .get(model_index)
                {
                    self.developers[index].model = model.id.clone();
                    self.model_touched = false;
                    self.model_browser = None;
                }
            }
            _ => {}
        }
    }

    fn open_prompt_editor(&mut self, index: usize) {
        let mut editor = TextArea::default();
        editor.insert_str(&self.developers[index].system_prompt);
        editor.set_style(theme::surface());
        editor.set_block(
            Block::default()
                .title(Line::from(vec![
                    Span::styled(
                        " Agent prompt · ",
                        Style::default()
                            .fg(theme::AQUA)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(
                        self.developers[index].name.clone(),
                        Style::default()
                            .fg(theme::agent_color(index))
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::raw(" "),
                ]))
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .style(theme::surface())
                .border_style(Style::default().fg(theme::AQUA)),
        );
        editor.set_cursor_style(
            Style::default()
                .fg(theme::BACKGROUND)
                .bg(theme::AQUA)
                .add_modifier(Modifier::BOLD),
        );
        self.prompt_editor = Some(editor);
        self.step = Step::Prompt(index);
    }

    fn handle_prompt_key(&mut self, index: usize, key: KeyEvent) {
        if key.code == KeyCode::Esc {
            self.prompt_editor = None;
            self.step = Step::Developer(index);
            return;
        }
        if key.code == KeyCode::Char('s') && key.modifiers.contains(KeyModifiers::CONTROL) {
            let Some(editor) = &self.prompt_editor else {
                return;
            };
            let prompt = editor.lines().join("\n").trim().to_owned();
            if prompt.len() > 16_000 {
                self.error = Some("Keep the system prompt under 16,000 bytes.".into());
            } else {
                self.developers[index].system_prompt = prompt;
                self.prompt_editor = None;
                self.step = Step::Developer(index);
            }
            return;
        }
        if let Some(editor) = &mut self.prompt_editor {
            editor.input(key);
        }
    }

    fn move_field(&mut self, index: usize, forward: bool) {
        loop {
            self.field = if forward {
                self.field.next()
            } else {
                self.field.previous()
            };
            if !(self.field == Field::Role
                || (self.field == Field::Prompt && self.mode != WizardMode::Configure)
                || (index == 0 && self.field == Field::ReportsTo)
                || (self.field == Field::OutputLimit && self.mode != WizardMode::Configure)
                || (self.field == Field::Token && !self.developers[index].provider.uses_api_key()))
            {
                break;
            }
        }
        if self.field == Field::OutputLimit {
            self.limit_touched = false;
        }
        if self.field == Field::Model {
            self.model_touched = false;
        }
    }

    fn handle_paste(&mut self, value: &str) {
        if let Some(browser) = &mut self.model_browser {
            browser
                .query
                .extend(value.chars().filter(|character| !character.is_control()));
            browser.selected = 0;
            return;
        }
        if self.prompt_editor.is_some() || matches!(self.step, Step::Prompt(_)) {
            if let Some(editor) = &mut self.prompt_editor {
                let current_len = editor.lines().iter().map(String::len).sum::<usize>();
                if current_len.saturating_add(value.len()) > 16_000 {
                    self.error = Some("Keep the system prompt under 16,000 bytes.".into());
                } else {
                    editor.insert_str(value);
                }
            }
            return;
        }
        if let Step::Developer(index) = self.step
            && self.field == Field::Name
        {
            self.developers[index].name.push_str(
                &value
                    .chars()
                    .filter(|c| !c.is_control())
                    .collect::<String>(),
            );
            return;
        }
        if let Step::Developer(index) = self.step
            && self.field == Field::Model
        {
            self.developers[index].model =
                value.trim().chars().filter(|c| !c.is_control()).collect();
            self.model_touched = true;
            return;
        }
        if let Step::Developer(index) = self.step
            && self.field == Field::Token
            && self.developers[index].provider.uses_api_key()
        {
            self.developers[index]
                .api_token
                .extend(value.trim().chars().filter(|c| !c.is_control()));
        }
    }

    fn valid_working_directory(&self) -> Option<PathBuf> {
        let path = self.working_directory.clone();
        (!path.as_os_str().is_empty() && path.is_dir()).then_some(path)
    }

    fn cycle_choice(&mut self, index: usize, delta: isize) {
        if self.field == Field::ReportsTo {
            if index == 0 {
                return;
            }
            let candidates = (0..self.developers.len())
                .filter(|candidate| *candidate != index && !self.is_descendant(*candidate, index))
                .collect::<Vec<_>>();
            if !candidates.is_empty() {
                let position = self.developers[index]
                    .reports_to
                    .and_then(|current| {
                        candidates
                            .iter()
                            .position(|candidate| *candidate == current)
                    })
                    .unwrap_or(0);
                let next =
                    (position as isize + delta).rem_euclid(candidates.len() as isize) as usize;
                self.developers[index].reports_to = Some(candidates[next]);
            }
            return;
        }
        if self.field == Field::Role {
            if index != 0 {
                let role = self.developers[index].role.cycle_specialist(delta);
                let name = unique_name(role.label(), &self.developers, Some(index));
                self.developers[index].role = role;
                self.developers[index].name = name;
            }
            return;
        }
        let developer = &mut self.developers[index];
        match self.field {
            Field::Token | Field::OutputLimit | Field::Prompt | Field::Name => {}
            Field::Role => unreachable!(),
            Field::Provider => {
                developer.provider = developer.provider.cycle(delta);
                developer.model = self.catalog.first_model(developer.provider.id());
                developer.api_token.clear();
                developer.credential_reference = None;
                self.model_touched = false;
            }
            Field::Model => {
                developer.model =
                    self.catalog
                        .cycle_model(developer.provider.id(), &developer.model, delta);
                self.model_touched = false;
            }
            Field::ReportsTo => unreachable!(),
        }
    }

    fn is_descendant(&self, mut candidate: usize, ancestor: usize) -> bool {
        for _ in 0..self.developers.len() {
            if candidate == ancestor {
                return true;
            }
            let Some(parent) = self
                .developers
                .get(candidate)
                .and_then(|agent| agent.reports_to)
            else {
                return false;
            };
            candidate = parent;
        }
        true
    }
}

fn suggested_role(index: usize, count: usize) -> DeveloperRole {
    if count == 1 {
        return DeveloperRole::Lead;
    }
    match index {
        0 => DeveloperRole::Lead,
        _ => [
            DeveloperRole::Backend,
            DeveloperRole::Frontend,
            DeveloperRole::Reviewer,
        ][(index - 1) % 3],
    }
}

fn suggested_name(role: DeveloperRole, index: usize) -> String {
    let base = match role {
        DeveloperRole::Lead => "Team Lead",
        DeveloperRole::Frontend => "Frontend Agent",
        DeveloperRole::Backend => "Backend Agent",
        DeveloperRole::Reviewer => "Reviewer Agent",
        DeveloperRole::Custom => "Custom Agent",
    };
    if index < 4 {
        base.into()
    } else {
        format!("{base} {}", index + 1)
    }
}

fn unique_name(base: &str, developers: &[DeveloperDraft], exclude: Option<usize>) -> String {
    if !developers
        .iter()
        .enumerate()
        .any(|(index, agent)| Some(index) != exclude && agent.name == base)
    {
        return base.into();
    }
    for suffix in 2..=developers.len().saturating_add(2) {
        let candidate = format!("{base} {suffix}");
        if !developers
            .iter()
            .enumerate()
            .any(|(index, agent)| Some(index) != exclude && agent.name == candidate)
        {
            return candidate;
        }
    }
    unreachable!("one of the generated suffixes must be unused")
}

/// Set up a project team from the workspace or a short user description.
pub(crate) fn startup_choice(workspace: &Path) -> io::Result<StartupChoice> {
    let guard = TerminalGuard::enter()?;
    let mut terminal = Terminal::new(ColorFallbackBackend::new(io::stdout()))?;
    let choices = startup_provider_choices();
    let catalog = ModelCatalog::default();
    let mut provider = ProviderChoice::default_provider();
    let mut model = catalog.first_model(provider.id());
    let mut workspace = workspace.to_path_buf();
    let mut api_token = String::new();
    let mut focused = StartupField::Workspace;
    let mut description = String::new();
    let mut error: Option<String> = None;
    let popup_size = Cell::new(None);
    loop {
        terminal.draw(|frame| {
            if let Some(popup) = render_startup_splash_with_popup(
                frame,
                StartupView {
                    workspace: &workspace,
                    show_setup: true,
                    selected_provider: provider,
                    model: &model,
                    model_picker_available: catalog.models(provider.id()).len() > 1,
                    api_token: &api_token,
                    focused,
                    description: &description,
                    error: error.as_deref(),
                },
            ) {
                popup_size.set(Some((popup.width, popup.height)));
            }
        })?;
        if !event::poll(Duration::from_millis(50))? {
            continue;
        }
        let event = event::read()?;
        if crate::tui::redraw_on_resize(&mut terminal, &event, |frame| {
            if let Some(popup) = render_startup_splash_with_popup(
                frame,
                StartupView {
                    workspace: &workspace,
                    show_setup: true,
                    selected_provider: provider,
                    model: &model,
                    model_picker_available: catalog.models(provider.id()).len() > 1,
                    api_token: &api_token,
                    focused,
                    description: &description,
                    error: error.as_deref(),
                },
            ) {
                popup_size.set(Some((popup.width, popup.height)));
            }
        })? {
            continue;
        }
        match event {
            Event::Key(key)
                if key.kind == KeyEventKind::Press
                    && key.code == KeyCode::Char('c')
                    && key.modifiers.contains(KeyModifiers::CONTROL) =>
            {
                return Ok(StartupChoice::Quit);
            }
            Event::Key(key) if key.kind == KeyEventKind::Press => match key.code {
                KeyCode::Right | KeyCode::Char('f' | 'F') if focused == StartupField::Workspace => {
                    match browse_startup_workspace(
                        &mut terminal,
                        StartupView {
                            workspace: &workspace,
                            show_setup: true,
                            selected_provider: provider,
                            model: &model,
                            model_picker_available: catalog.models(provider.id()).len() > 1,
                            api_token: &api_token,
                            focused,
                            description: &description,
                            error: error.as_deref(),
                        },
                    )? {
                        StartupPickerResult::Quit => return Ok(StartupChoice::Quit),
                        StartupPickerResult::Selection(Some(selected)) => workspace = selected,
                        StartupPickerResult::Selection(None) => {}
                    }
                    error = None;
                }
                KeyCode::Enter if !key.modifiers.contains(KeyModifiers::SHIFT) => {
                    if provider.uses_api_key() && api_token.is_empty() && !provider.is_available() {
                        error = Some(format!(
                            "Enter a {} API key to use this provider.",
                            if provider == ProviderChoice::ClaudeCloud {
                                "Claude"
                            } else {
                                "OpenAI"
                            }
                        ));
                    } else if !description.trim().is_empty() {
                        return Ok(StartupChoice::Build {
                            workspace: workspace.clone(),
                            popup_size: popup_size.get().unwrap_or((42, 8)),
                            provider,
                            model,
                            api_token: (!api_token.is_empty()).then_some(api_token),
                            description: description.trim().to_owned(),
                            terminal: Some(TerminalHandoff { guard, terminal }),
                        });
                    } else {
                        return Ok(StartupChoice::Scan {
                            workspace: workspace.clone(),
                            popup_size: popup_size.get().unwrap_or((42, 8)),
                            provider,
                            model,
                            api_token: (!api_token.is_empty()).then_some(api_token),
                            terminal: Some(TerminalHandoff { guard, terminal }),
                        });
                    }
                }
                KeyCode::Enter
                    if focused == StartupField::Goal
                        && key.modifiers.contains(KeyModifiers::SHIFT) =>
                {
                    if description.chars().count() < 1200 {
                        description.push('\n');
                    }
                    error = None;
                }
                KeyCode::Up => {
                    focused = focused.previous(
                        catalog.models(provider.id()).len() > 1,
                        provider.uses_api_key() && !provider.is_available(),
                    );
                    error = None;
                }
                KeyCode::Down => {
                    focused = focused.next(
                        catalog.models(provider.id()).len() > 1,
                        provider.uses_api_key() && !provider.is_available(),
                    );
                    error = None;
                }
                KeyCode::Left | KeyCode::Right if focused == StartupField::Provider => {
                    provider = cycle_provider(
                        &choices,
                        provider,
                        if key.code == KeyCode::Left { -1 } else { 1 },
                    );
                    model = catalog.first_model(provider.id());
                    error = None;
                }
                KeyCode::Left | KeyCode::Right if focused == StartupField::Model => {
                    model = catalog.cycle_model(
                        provider.id(),
                        &model,
                        if key.code == KeyCode::Left { -1 } else { 1 },
                    );
                    error = None;
                }
                KeyCode::Char('f' | 'F')
                    if focused == StartupField::Model
                        && catalog.models(provider.id()).len() > 1 =>
                {
                    match browse_startup_model(
                        &mut terminal,
                        StartupView {
                            workspace: &workspace,
                            show_setup: true,
                            selected_provider: provider,
                            model: &model,
                            model_picker_available: catalog.models(provider.id()).len() > 1,
                            api_token: &api_token,
                            focused: StartupField::Model,
                            description: &description,
                            error: error.as_deref(),
                        },
                        &catalog,
                    )? {
                        StartupPickerResult::Quit => return Ok(StartupChoice::Quit),
                        StartupPickerResult::Selection(Some(selected)) => model = selected,
                        StartupPickerResult::Selection(None) => {}
                    }
                    error = None;
                }
                KeyCode::Backspace if focused == StartupField::Goal => {
                    description.pop();
                    error = None;
                }
                KeyCode::Backspace if focused == StartupField::ApiKey => {
                    api_token.pop();
                    error = None;
                }
                KeyCode::Char(character)
                    if focused == StartupField::Goal
                        && !key.modifiers.contains(KeyModifiers::CONTROL)
                        && !character.is_control() =>
                {
                    if description.chars().count() < 1200 {
                        description.push(character);
                    }
                    error = None;
                }
                KeyCode::Char(character)
                    if focused == StartupField::ApiKey
                        && !key.modifiers.contains(KeyModifiers::CONTROL)
                        && !character.is_control() =>
                {
                    if api_token.chars().count() < 4096 {
                        api_token.push(character);
                    }
                    error = None;
                }
                KeyCode::Char('q' | 'Q')
                    if !matches!(focused, StartupField::Goal | StartupField::ApiKey) =>
                {
                    return Ok(StartupChoice::Quit);
                }
                KeyCode::Esc => return Ok(StartupChoice::Quit),
                _ => {}
            },
            Event::Paste(value) if focused == StartupField::Goal => {
                let remaining = 1200usize.saturating_sub(description.chars().count());
                let value = value.replace("\r\n", "\n").replace('\r', "\n");
                description.extend(
                    value
                        .chars()
                        .filter(|character| *character == '\n' || !character.is_control())
                        .take(remaining),
                );
                error = None;
            }
            Event::Paste(value) if focused == StartupField::ApiKey => {
                let remaining = 4096usize.saturating_sub(api_token.chars().count());
                api_token.extend(
                    value
                        .chars()
                        .filter(|character| !character.is_control())
                        .take(remaining),
                );
                error = None;
            }
            _ => {}
        }
    }
}

fn browse_startup_workspace(
    terminal: &mut Terminal<ColorFallbackBackend<io::Stdout>>,
    view: StartupView<'_>,
) -> io::Result<StartupPickerResult<PathBuf>> {
    let mut wizard = Wizard::workspace(view.workspace);
    loop {
        terminal.draw(|frame| render_startup_workspace_browser(frame, view, &wizard))?;
        let input = event::read()?;
        if crate::tui::redraw_on_resize(terminal, &input, |frame| {
            render_startup_workspace_browser(frame, view, &wizard)
        })? {
            continue;
        }
        if is_ctrl_c(&input) {
            return Ok(StartupPickerResult::Quit);
        }
        wizard.handle_event(&input, terminal.size()?.into());
        if wizard.cancelled() {
            return Ok(StartupPickerResult::Selection(None));
        }
        if wizard.complete() {
            return Ok(StartupPickerResult::Selection(Some(
                wizard.selected_workspace().to_path_buf(),
            )));
        }
    }
}

fn render_startup_workspace_browser(
    frame: &mut ratatui::Frame<'_>,
    view: StartupView<'_>,
    wizard: &Wizard,
) {
    render_startup_splash(frame, view);
    let area = frame.area();
    let directory = wizard.workspace_explorer.as_ref().map_or_else(
        || "Workspace".to_owned(),
        |explorer| explorer.cwd().display().to_string(),
    );
    let title = workspace_path_title("Workspace", &directory);
    let footer = workspace_picker_footer(wizard);
    let popup_width =
        picker_popup_width(area, &title, 72, 112).max(picker_popup_width(area, &footer, 72, 112));
    let popup = centered_lower(area, popup_width, area.height.min(16));
    frame.render_widget(Clear, popup);
    let block = Block::default()
        .title(title)
        .title_bottom(footer)
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .style(theme::surface())
        .border_style(Style::default().fg(theme::BORDER));
    let inner = block.inner(popup);
    frame.render_widget(block, popup);
    let show_filter = wizard.workspace_filtering || !wizard.workspace_filter.is_empty();
    let explorer_area = if show_filter && inner.height > 0 {
        frame.render_widget(
            Paragraph::new(format!(
                "Search: {}{}",
                wizard.workspace_filter,
                if wizard.workspace_filtering {
                    "█"
                } else {
                    ""
                }
            ))
            .style(theme::surface()),
            Rect::new(inner.x, inner.y, inner.width, 1),
        );
        Rect::new(
            inner.x,
            inner.y.saturating_add(1),
            inner.width,
            inner.height.saturating_sub(1),
        )
    } else {
        inner
    };
    if explorer_area.height > 0
        && let Some(explorer) = &wizard.workspace_explorer
    {
        frame.render_widget_ref(explorer.widget(), explorer_area);
    }
}

fn browse_startup_model(
    terminal: &mut Terminal<ColorFallbackBackend<io::Stdout>>,
    view: StartupView<'_>,
    catalog: &ModelCatalog,
) -> io::Result<StartupPickerResult<String>> {
    let provider = view.selected_provider;
    let models = catalog.models(provider.id());
    let mut browser = ModelBrowser {
        query: String::new(),
        selected: models
            .iter()
            .position(|model| model.id == view.model)
            .unwrap_or(0),
    };
    loop {
        terminal.draw(|frame| {
            render_startup_model_browser(frame, view, provider, catalog, &browser);
        })?;
        let input = event::read()?;
        if crate::tui::redraw_on_resize(terminal, &input, |frame| {
            render_startup_model_browser(frame, view, provider, catalog, &browser);
        })? {
            continue;
        }
        if is_ctrl_c(&input) {
            return Ok(StartupPickerResult::Quit);
        }
        let matches = browser.matches(catalog, provider.id());
        match input {
            Event::Key(key) if key.kind == KeyEventKind::Press => match key.code {
                KeyCode::Esc => return Ok(StartupPickerResult::Selection(None)),
                KeyCode::Enter => {
                    return Ok(StartupPickerResult::Selection(
                        matches
                            .get(browser.selected)
                            .and_then(|index| models.get(*index))
                            .map(|model| model.id.clone()),
                    ));
                }
                KeyCode::Up => {
                    browser.selected = browser.selected.saturating_sub(1);
                }
                KeyCode::Down => {
                    browser.selected = browser
                        .selected
                        .saturating_add(1)
                        .min(matches.len().saturating_sub(1));
                }
                KeyCode::Backspace => {
                    browser.query.pop();
                    browser.selected = 0;
                }
                KeyCode::Char(character)
                    if !key
                        .modifiers
                        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
                        && !character.is_control() =>
                {
                    browser.query.push(character);
                    browser.selected = 0;
                }
                _ => {}
            },
            Event::Paste(value) => {
                browser
                    .query
                    .extend(value.chars().filter(|character| !character.is_control()));
                browser.selected = 0;
            }
            _ => {}
        }
    }
}

fn render_startup_model_browser(
    frame: &mut ratatui::Frame<'_>,
    view: StartupView<'_>,
    provider: ProviderChoice,
    catalog: &ModelCatalog,
    browser: &ModelBrowser,
) {
    render_startup_splash(frame, view);
    let area = frame.area();
    let title = model_picker_title("Browse models");
    let footer = model_picker_footer("cancel");
    let popup = centered_lower(
        area,
        model_picker_width(area, "Browse models", 72, 88, "cancel"),
        area.height.min(16),
    );
    frame.render_widget(Clear, popup);
    let block = Block::default()
        .title(title)
        .title_bottom(footer)
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .style(theme::surface())
        .border_style(Style::default().fg(theme::BORDER));
    let inner = block.inner(popup);
    frame.render_widget(block, popup);
    frame.render_widget(
        Paragraph::new(format!("Search: {}", browser.query)).style(theme::surface()),
        Rect::new(inner.x, inner.y, inner.width, 1),
    );
    let matches = browser.matches(catalog, provider.id());
    let visible = inner.height.saturating_sub(1).max(1) as usize;
    let start = browser.selected.saturating_sub(visible.saturating_sub(1));
    for (row, model_index) in matches.iter().skip(start).take(visible).enumerate() {
        let selected = start + row == browser.selected;
        let style = if selected {
            Style::default()
                .fg(theme::BACKGROUND)
                .bg(theme::AQUA)
                .add_modifier(Modifier::BOLD)
        } else {
            theme::surface()
        };
        let model = &catalog.models(provider.id())[*model_index].id;
        frame.render_widget(
            Paragraph::new(model.as_str()).style(style),
            Rect::new(inner.x, inner.y + 1 + row as u16, inner.width, 1),
        );
    }
}

pub(crate) fn show_splash() -> io::Result<()> {
    if !io::stdout().is_terminal() {
        return Ok(());
    }
    let _guard = TerminalGuard::enter()?;
    let mut terminal = Terminal::new(ColorFallbackBackend::new(io::stdout()))?;
    terminal.draw(|frame| {
        let provider = ProviderChoice::default_provider();
        let catalog = ModelCatalog::default();
        let model = catalog.first_model(provider.id());
        render_startup_splash(
            frame,
            StartupView {
                workspace: Path::new("."),
                show_setup: false,
                selected_provider: provider,
                model: &model,
                model_picker_available: catalog.models(provider.id()).len() > 1,
                api_token: "",
                focused: StartupField::Workspace,
                description: "",
                error: None,
            },
        )
    })?;
    Ok(())
}

fn render_startup_splash(frame: &mut ratatui::Frame<'_>, view: StartupView<'_>) {
    let _ = render_startup_splash_with_popup(frame, view);
}

fn render_startup_splash_with_popup(
    frame: &mut ratatui::Frame<'_>,
    view: StartupView<'_>,
) -> Option<Rect> {
    let StartupView {
        workspace,
        show_setup,
        selected_provider,
        model,
        model_picker_available,
        api_token,
        focused,
        description,
        error,
    } = view;
    render_startup_background(frame);
    let logo = logo_text();
    let api_key_field = selected_provider.uses_api_key() && !selected_provider.is_available();
    let active_style = Style::default()
        .fg(theme::ORANGE)
        .add_modifier(Modifier::BOLD);
    let label_style = |field| {
        if focused == field {
            active_style
        } else {
            Style::default().fg(theme::MUTED)
        }
    };
    let value_style = |field| {
        if focused == field {
            Style::default()
                .fg(theme::TEXT)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme::TEXT)
        }
    };
    let mut workspace_line = vec![
        Span::styled("Workspace  ", label_style(StartupField::Workspace)),
        Span::styled(
            workspace.display().to_string(),
            value_style(StartupField::Workspace),
        ),
    ];
    if focused == StartupField::Workspace {
        workspace_line.push(Span::styled(
            "  [f]",
            Style::default()
                .fg(theme::AQUA)
                .add_modifier(Modifier::BOLD),
        ));
    }
    let mut lines = vec![Line::from(workspace_line)];
    let provider_name_style = if focused == StartupField::Provider {
        Style::default()
            .fg(theme::TEXT)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(theme::TEXT)
    };
    lines.push(Line::from(vec![
        Span::styled("Provider  ", label_style(StartupField::Provider)),
        Span::styled("‹ ", Style::default().fg(theme::AQUA)),
        Span::styled(selected_provider.label(), provider_name_style),
        Span::styled(" ›", Style::default().fg(theme::AQUA)),
    ]));
    if model_picker_available || model != "default" {
        let model_style = if focused == StartupField::Model {
            Style::default()
                .fg(theme::TEXT)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme::TEXT)
        };
        let mut model_line = vec![Span::styled("Model     ", label_style(StartupField::Model))];
        if model_picker_available {
            model_line.push(Span::styled("‹ ", Style::default().fg(theme::AQUA)));
        }
        model_line.push(Span::styled(model, model_style));
        if model_picker_available {
            model_line.push(Span::styled(" ›", Style::default().fg(theme::AQUA)));
        }
        if model_picker_available && focused == StartupField::Model {
            model_line.push(Span::styled(
                "  [f]",
                Style::default()
                    .fg(theme::AQUA)
                    .add_modifier(Modifier::BOLD),
            ));
        }
        lines.push(Line::from(model_line));
    }
    let api_key_line_index = if api_key_field {
        let index = lines.len();
        lines.push(Line::from(vec![
            Span::styled("API key   ", label_style(StartupField::ApiKey)),
            Span::styled(
                if api_token.is_empty() {
                    "Enter key…".to_owned()
                } else {
                    "•".repeat(api_token.chars().count().min(48))
                },
                if focused == StartupField::ApiKey {
                    Style::default().fg(theme::TEXT)
                } else {
                    Style::default().fg(theme::MUTED)
                },
            ),
        ]));
        Some(index)
    } else {
        None
    };
    lines.push(Line::from(""));
    lines.push(
        Line::from(vec![Span::styled(
            "What do you want to build?",
            if focused == StartupField::Goal {
                active_style
            } else {
                Style::default().fg(theme::TEXT)
            },
        )])
        .alignment(Alignment::Center),
    );
    let input_line_index = lines.len();
    lines.push(Line::from(""));
    let input_hint = "Leave empty to scan the workspace";
    if let Some(error) = error {
        lines.push(Line::from(error).style(Style::default().fg(theme::YELLOW)));
    }
    let content_width = lines.iter().map(Line::width).max().unwrap_or(0);
    let popup_max_width = frame
        .area()
        .width
        .saturating_sub(4)
        .max(frame.area().width.min(1));
    let popup_width = popup_max_width.min((content_width as u16).saturating_add(4).clamp(42, 88));
    let inner_width = popup_width.saturating_sub(2).max(1);
    let input_text = if description.is_empty() {
        input_hint
    } else {
        description
    };
    let input_rows = Paragraph::new(input_text)
        .line_count(popup_width.saturating_sub(4).max(1))
        .clamp(1, 5);
    let input_area_height = (input_rows as u16).saturating_add(2);
    lines.splice(
        input_line_index..input_line_index + 1,
        std::iter::repeat_n(Line::from(""), input_area_height as usize),
    );
    let popup_content_height = lines
        .iter()
        .map(|line| (line.width() as u16).div_ceil(inner_width).max(1))
        .sum::<u16>()
        .max(1);
    let popup_height = popup_content_height.saturating_add(2);
    let input_y = lines
        .iter()
        .take(input_line_index)
        .map(|line| (line.width() as u16).div_ceil(inner_width).max(1))
        .sum::<u16>();
    let api_key_y = api_key_line_index.map(|index| {
        lines
            .iter()
            .take(index)
            .map(|line| (line.width() as u16).div_ceil(inner_width).max(1))
            .sum::<u16>()
    });
    let (logo_width, logo_height) = logo.as_ref().map_or((0, 0), |logo| {
        (
            logo.lines.iter().map(Line::width).max().unwrap_or(0) as u16,
            logo.lines.len() as u16,
        )
    });
    let logo_content_height = logo_height;
    let setup_tail_height = if show_setup { 2 } else { 0 };
    let logo_fits = logo.is_some()
        && logo_width <= frame.area().width
        && (!show_setup
            || logo_content_height + popup_height + setup_tail_height < frame.area().height);
    let splash_height = if logo_fits { logo_content_height } else { 0 };
    let visible_popup_height = if show_setup {
        popup_height.min(
            frame
                .area()
                .height
                .saturating_sub(splash_height + setup_tail_height),
        )
    } else {
        0
    };
    let action_height = u16::from(show_setup && visible_popup_height > 0);
    let logo_overlap = u16::from(show_setup && visible_popup_height > 0 && logo_fits);
    let group_height =
        splash_height.saturating_sub(logo_overlap) + visible_popup_height + action_height;
    let centered_y = frame.area().y + frame.area().height.saturating_sub(group_height) / 2;
    let group_y = centered_y
        .saturating_add(1)
        .min(frame.area().bottom().saturating_sub(group_height));
    let splash_area = Rect::new(frame.area().x, group_y, frame.area().width, splash_height);
    let mut setup_popup = None;

    if show_setup && visible_popup_height > 0 {
        let popup = centered(frame.area(), popup_width, visible_popup_height);
        let popup_y = group_y + splash_height.saturating_sub(logo_overlap);
        let popup = Rect::new(popup.x, popup_y, popup.width, popup.height);
        setup_popup = Some(popup);
        frame.render_widget(Clear, popup);
        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .style(theme::surface())
            .border_style(Style::default().fg(theme::BORDER));
        let body = block.inner(popup);
        frame.render_widget(block, popup);
        frame.render_widget(
            Paragraph::new(lines)
                .alignment(Alignment::Left)
                .wrap(Wrap { trim: true })
                .style(theme::surface()),
            body,
        );
        let input_area_y = body.y.saturating_add(input_y);
        let visible_input_area_height = body
            .bottom()
            .saturating_sub(input_area_y)
            .min(input_area_height);
        let input_area = Rect::new(body.x, input_area_y, body.width, visible_input_area_height);
        let input_block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .style(theme::surface())
            .border_style(Style::default().fg(if focused == StartupField::Goal {
                theme::AQUA
            } else {
                theme::BORDER
            }));
        let input_inner = input_block.inner(input_area);
        frame.render_widget(input_block, input_area);
        let shown_input = if description.is_empty() {
            input_hint
        } else {
            description
        };
        let input_paragraph = Paragraph::new(shown_input)
            .wrap(Wrap { trim: false })
            .style(
                Style::default()
                    .fg(if description.is_empty() {
                        theme::MUTED
                    } else {
                        theme::TEXT
                    })
                    .bg(theme::SURFACE),
            );
        let wrapped_lines = input_paragraph.line_count(input_inner.width.max(1));
        let scroll_y = wrapped_lines
            .saturating_sub(input_inner.height as usize)
            .min(u16::MAX as usize) as u16;
        frame.render_widget(input_paragraph.scroll((scroll_y, 0)), input_inner);
        if focused == StartupField::Goal && input_inner.width > 0 && input_inner.height > 0 {
            let last_line_len = description
                .rsplit('\n')
                .next()
                .map_or(0, |line| line.chars().count());
            let cursor_column = (last_line_len % input_inner.width as usize) as u16;
            let cursor_row = wrapped_lines
                .saturating_sub(1)
                .saturating_sub(scroll_y as usize)
                .min(input_inner.height.saturating_sub(1) as usize)
                as u16;
            frame.set_cursor_position((
                input_inner.x.saturating_add(cursor_column),
                input_inner.y.saturating_add(cursor_row),
            ));
        } else if focused == StartupField::ApiKey && api_key_field {
            let masked_length = api_token.chars().count().min(body.width as usize);
            let key_y = body.y.saturating_add(api_key_y.unwrap_or_default());
            let key_x = body
                .x
                .saturating_add("API key   ".chars().count() as u16 + masked_length as u16);
            frame.set_cursor_position((
                key_x.min(body.right().saturating_sub(1)),
                key_y.min(body.bottom().saturating_sub(1)),
            ));
        }
        let action = Line::from(vec![
            Span::raw("Press "),
            Span::styled(
                "Enter",
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(" to "),
            Span::styled(
                "voff",
                Style::default()
                    .fg(theme::ORANGE)
                    .add_modifier(Modifier::ITALIC),
            ),
            Span::raw(" 🐶"),
        ]);
        frame.render_widget(
            Paragraph::new(action)
                .alignment(Alignment::Center)
                .style(theme::canvas()),
            Rect::new(popup.x, popup.bottom(), popup.width, action_height),
        );
    }

    if logo_fits && let Some(logo) = logo {
        let content = centered(
            splash_area,
            splash_area.width.min(logo_width.max(48)),
            logo_content_height,
        );
        let transparent_row = if logo_overlap > 0 {
            setup_popup.map(|popup| popup.y)
        } else {
            None
        };
        render_logo_overlay(frame, &logo, content, transparent_row);
    }
    setup_popup
}

pub(crate) struct TeamBuildingView<'a> {
    pub(crate) popup_size: Option<(u16, u16)>,
    pub(crate) setup_prompt: &'a str,
    pub(crate) team: Option<&'a SetupDraft>,
    pub(crate) summary: Option<&'a str>,
    pub(crate) frame_index: usize,
    pub(crate) scroll: u16,
    pub(crate) error: Option<&'a str>,
}

pub(crate) fn render_team_building_screen(
    frame: &mut ratatui::Frame<'_>,
    view: TeamBuildingView<'_>,
) {
    let TeamBuildingView {
        popup_size,
        setup_prompt,
        team,
        summary,
        frame_index,
        scroll,
        error,
    } = view;
    render_startup_background(frame);
    let area = frame.area();
    let logo = logo_text();
    let (logo_width, logo_height) = logo.as_ref().map_or((0, 0), |image| {
        (
            image.lines.iter().map(Line::width).max().unwrap_or(0) as u16,
            image.lines.len() as u16,
        )
    });
    let requested_size = popup_size.unwrap_or((42, 8));
    let width = requested_size.0.saturating_add(16).min(area.width).max(1);
    let after_height = u16::from(team.is_some() || error.is_none());
    let max_panel_height = area.height.saturating_sub(after_height).max(1);
    let panel_height = requested_size
        .1
        .saturating_add(4)
        .min(max_panel_height)
        .max(1);
    let group_height = logo_height
        .saturating_sub(u16::from(logo_height > 0))
        .saturating_add(panel_height)
        .saturating_add(after_height);
    let show_logo = logo.is_some() && logo_width <= area.width && group_height <= area.height;
    let visible_logo_height = if show_logo { logo_height } else { 0 };
    let overlap = u16::from(visible_logo_height > 0);
    let mut lines = Vec::new();
    if team.is_none() {
        lines.push(Line::from(Span::styled(
            "Setup prompt",
            Style::default()
                .fg(theme::AQUA)
                .add_modifier(Modifier::BOLD),
        )));
    }
    if let Some(team) = team {
        lines.push(Line::from(Span::styled(
            "Project summary",
            Style::default()
                .fg(theme::AQUA)
                .add_modifier(Modifier::BOLD),
        )));
        if let Some(summary) = summary {
            lines.extend(
                summary
                    .lines()
                    .map(|line| Line::from(line).style(Style::default().fg(theme::TEXT))),
            );
        }
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            format!("Team · {} agents", team.developers.len()),
            Style::default()
                .fg(theme::AQUA)
                .add_modifier(Modifier::BOLD),
        )));
        let members = team
            .developers
            .iter()
            .enumerate()
            .map(|(index, developer)| {
                let name = crate::tui::sanitize(&developer.name).replace(['\n', '\r'], " ");
                let parent = developer.reports_to.map(|parent| {
                    team.developers.get(parent).map_or_else(
                        || "team".to_owned(),
                        |parent| crate::tui::sanitize(&parent.name).replace(['\n', '\r'], " "),
                    )
                });
                let role = parent
                    .map(|parent| format!("reports to {parent}"))
                    .or_else(|| (index == 0).then(|| "lead".to_owned()));
                (name, role, theme::agent_color(index))
            })
            .collect::<Vec<_>>();
        let name_width = members
            .iter()
            .map(|(name, _, _)| Line::from(name.as_str()).width())
            .max()
            .unwrap_or_default();
        for (name, role, color) in members {
            let name_padding = name_width.saturating_sub(Line::from(name.as_str()).width()) + 1;
            let mut member = vec![
                Span::styled(
                    name,
                    Style::default().fg(color).add_modifier(Modifier::BOLD),
                ),
                Span::raw(" ".repeat(name_padding)),
            ];
            if let Some(role) = role {
                member.push(Span::styled(
                    format!("·  {role}"),
                    Style::default().fg(theme::TEXT),
                ));
            }
            lines.push(Line::from(member));
        }
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "Setup prompt",
            Style::default()
                .fg(theme::AQUA)
                .add_modifier(Modifier::BOLD),
        )));
    }
    lines.extend(
        setup_prompt
            .lines()
            .map(|line| Line::from(line).style(Style::default().fg(theme::TEXT))),
    );
    if let Some(error) = error {
        lines.push(Line::from(""));
        lines.push(
            Line::from("Voff could not build this team.").style(Style::default().fg(theme::YELLOW)),
        );
        lines.push(Line::from(error).style(Style::default().fg(theme::MUTED)));
    }
    let content_width = width.saturating_sub(3).max(1);
    let paragraph = Paragraph::new(lines)
        .wrap(Wrap { trim: false })
        .style(theme::surface());
    let content_height = paragraph.line_count(content_width);
    let group_height = visible_logo_height
        .saturating_sub(overlap)
        .saturating_add(panel_height)
        .saturating_add(after_height);
    let group_y = area
        .y
        .saturating_add(area.height.saturating_sub(group_height) / 2)
        .min(area.bottom().saturating_sub(group_height));
    let popup = Rect::new(
        area.x + area.width.saturating_sub(width) / 2,
        group_y + visible_logo_height.saturating_sub(overlap),
        width,
        panel_height,
    );
    let footer = if error.is_some() {
        Some(Line::from(vec![
            Span::styled(
                "Enter",
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(" or Esc to close"),
        ]))
    } else {
        None
    }
    .map(|footer| footer.alignment(Alignment::Center));
    let after_lines = if team.is_some() {
        vec![Line::from(vec![
            Span::styled(
                "Enter",
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(" to open your team"),
            Span::raw(" 🐶"),
        ])]
    } else if error.is_none() {
        let icon = if (frame_index / 5) % 2 == 0 {
            "🐶"
        } else {
            "🦴"
        };
        vec![Line::from(vec![
            Span::styled(
                "Building the team ",
                Style::default()
                    .fg(theme::TEXT)
                    .bg(theme::BACKGROUND)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                icon,
                Style::default().fg(theme::ORANGE).bg(theme::BACKGROUND),
            ),
        ])]
    } else {
        Vec::new()
    }
    .into_iter()
    .map(|line| line.alignment(Alignment::Center))
    .collect::<Vec<_>>();
    let mut block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .style(theme::surface())
        .border_style(Style::default().fg(theme::BORDER));
    if let Some(footer) = footer {
        block = block.title_bottom(footer);
    }
    let inner = block.inner(popup);
    let text_area = Rect {
        width: inner.width.saturating_sub(1),
        ..inner
    };
    let max_scroll = content_height.saturating_sub(text_area.height as usize);
    let scroll = usize::from(scroll).min(max_scroll);
    frame.render_widget(Clear, popup);
    frame.render_widget(block, popup);
    frame.render_widget(paragraph.scroll((scroll as u16, 0)), text_area);
    if content_height > text_area.height as usize && inner.width > 1 {
        let mut state = ScrollbarState::new(max_scroll.saturating_add(1))
            .position(scroll)
            .viewport_content_length(text_area.height as usize);
        frame.render_stateful_widget(
            Scrollbar::new(ScrollbarOrientation::VerticalRight),
            Rect {
                x: inner.x + inner.width.saturating_sub(1),
                width: 1,
                ..inner
            },
            &mut state,
        );
    }
    if !after_lines.is_empty() {
        let after_area = Rect::new(popup.x, popup.bottom(), popup.width, after_height);
        frame.render_widget(Block::default().style(theme::canvas()), after_area);
        frame.render_widget(
            Paragraph::new(after_lines)
                .alignment(Alignment::Center)
                .style(theme::canvas()),
            after_area,
        );
    }
    if show_logo && let Some(logo) = logo {
        let logo_area = Rect::new(
            area.x + area.width.saturating_sub(logo_width) / 2,
            group_y,
            logo_width,
            visible_logo_height,
        );
        render_logo_overlay(frame, &logo, logo_area, Some(popup.y));
    }
}

fn render_startup_background(frame: &mut ratatui::Frame<'_>) {
    let area = frame.area();
    frame.render_widget(Block::default().style(theme::canvas()), area);
    render_starfield(frame, area, theme::BACKGROUND);
}

fn render_starfield(frame: &mut ratatui::Frame<'_>, area: Rect, background: Color) {
    if area.width < 24 || area.height < 10 {
        return;
    }

    static START: OnceLock<Instant> = OnceLock::new();
    let elapsed = START.get_or_init(Instant::now).elapsed().as_millis();
    for y in (area.y + 1..area.bottom().saturating_sub(1)).step_by(4) {
        for x in (area.x + 2..area.right().saturating_sub(2)).step_by(9) {
            let seed =
                u64::from(x).wrapping_mul(73_856_093) ^ u64::from(y).wrapping_mul(19_349_663);
            if seed % 3 != 0 {
                continue;
            }
            let phase = (elapsed + u128::from(seed % 12_000)) % 12_000;
            let (symbol, color) = match phase {
                0..=1_499 => ("✦", Color::Rgb(178, 81, 30)),
                1_500..=2_499 => ("✧", Color::Rgb(116, 59, 31)),
                _ => continue,
            };
            frame.render_widget(
                Paragraph::new(symbol).style(Style::default().fg(color).bg(background)),
                Rect::new(x, y, 1, 1),
            );
        }
    }
}

fn render_logo_overlay(
    frame: &mut ratatui::Frame<'_>,
    logo: &Text<'_>,
    area: Rect,
    transparent_row: Option<u16>,
) {
    let bounds = frame.area();
    let buffer = frame.buffer_mut();
    for (row, line) in logo.lines.iter().enumerate() {
        let y = area.y.saturating_add(row as u16);
        if y >= area.bottom() || y < bounds.y || y >= bounds.bottom() {
            continue;
        }
        let line_width = line.width().min(u16::MAX as usize) as u16;
        let mut x = area
            .x
            .saturating_add(area.width.saturating_sub(line_width) / 2);
        for span in &line.spans {
            let style = logo.style.patch(line.style).patch(span.style);
            for character in span.content.chars() {
                let is_transparent = Some(y) == transparent_row
                    && character.is_whitespace()
                    && style.bg == Some(theme::BACKGROUND);
                if !is_transparent
                    && x >= bounds.x
                    && x < bounds.right()
                    && let Some(cell) = buffer.cell_mut((x, y))
                {
                    cell.set_char(character).set_style(style);
                }
                x = x.saturating_add(1);
            }
        }
    }
}

fn startup_provider_choices() -> Vec<ProviderChoice> {
    [
        ProviderChoice::CodexCli,
        ProviderChoice::ClaudeCli,
        ProviderChoice::Ollama,
        ProviderChoice::OpenAi,
    ]
    .into()
}

fn cycle_provider(
    providers: &[ProviderChoice],
    current: ProviderChoice,
    delta: isize,
) -> ProviderChoice {
    if providers.is_empty() {
        return current;
    }
    let index = providers
        .iter()
        .position(|provider| *provider == current)
        .unwrap_or(0);
    let next = (index as isize + delta).rem_euclid(providers.len() as isize) as usize;
    providers[next]
}

/// Inspect project filenames and create a small, project-specific starter team.
pub(crate) fn scan_workspace(
    workspace: &Path,
    previous: Option<&SetupDraft>,
    catalog: ModelCatalog,
) -> io::Result<(SetupDraft, String)> {
    let mut scan = WorkspaceScan::default();
    scan.visit(workspace, workspace, 0)?;
    let previous_provider = previous
        .and_then(|draft| draft.developers.first())
        .filter(|lead| lead.provider.is_available());
    let provider = previous_provider.map_or_else(preferred_provider, |lead| lead.provider);
    let model = previous_provider.map_or_else(
        || catalog.first_model(provider.id()),
        |lead| lead.model.clone(),
    );
    let project_name = workspace
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .unwrap_or("this project");
    let evidence = scan.summary();
    let mut developers = Vec::with_capacity(3);
    for index in 0..3 {
        let role = if index == 0 {
            DeveloperRole::Lead
        } else {
            DeveloperRole::Custom
        };
        let mut developer =
            DeveloperDraft::suggested(index, 3, provider, model.clone(), DEFAULT_OUTPUT_TOKENS);
        developer.role = role;
        developer.name = if index == 0 {
            "Team Lead".into()
        } else {
            format!("Agent {}", index + 1)
        };
        developer.reports_to = (index > 0).then_some(0);
        developer.system_prompt = if index == 0 {
            voff_prompts::setup::generated_lead_prompt(project_name, &evidence)
        } else {
            voff_prompts::setup::generated_agent_prompt(project_name, &evidence)
        };
        developers.push(developer);
    }
    let notice = format!(
        "Scanned {project_name}: {evidence}. Created {} agents using {}.",
        developers.len(),
        provider.label()
    );
    Ok((
        SetupDraft {
            developers,
            working_directory: workspace.to_path_buf(),
        },
        notice,
    ))
}

/// Build a team from repository evidence using the workspace's selected model provider.
pub(crate) async fn build_workspace_team(
    workspace: PathBuf,
    provider_choice: ProviderChoice,
    model: String,
    credential_reference: Option<String>,
    goal: Option<String>,
    setup_prompts: SetupPrompts,
    provider: Arc<dyn ModelProvider>,
) -> Result<(SetupDraft, String), String> {
    let scan = tokio::task::spawn_blocking({
        let workspace = workspace.clone();
        move || {
            let mut scan = WorkspaceScan::default();
            scan.visit(&workspace, &workspace, 0)
                .map(|()| scan)
                .map_err(|error| error.to_string())
        }
    })
    .await
    .map_err(|error| format!("workspace scan failed: {error}"))??;
    if model.trim().is_empty() {
        return Err(format!(
            "No model is configured for {}. Choose a model in setup first.",
            provider_choice.label()
        ));
    }
    let user_goal = goal
        .as_deref()
        .map(str::trim)
        .filter(|goal| !goal.is_empty());
    let system_prompt = if user_goal.is_some() {
        setup_prompts.build_from_description
    } else {
        setup_prompts.scan_workspace
    };
    if system_prompt.trim().is_empty() || system_prompt.len() > 16_000 {
        return Err("the configured setup prompt must contain 1–16,000 bytes".into());
    }
    let project_context = scan.prompt_context(&workspace);
    let user_context = voff_prompts::setup::user_context(user_goal, &project_context);
    let request = CanonicalRequest {
        task_id: TaskId::new(),
        model: model.clone(),
        messages: vec![
            CanonicalMessage {
                id: MessageId::new(),
                role: MessageRole::System,
                content: system_prompt,
            },
            CanonicalMessage {
                id: MessageId::new(),
                role: MessageRole::User,
                content: user_context,
            },
        ],
        tools: vec![ToolDefinition {
            name: "create_team".into(),
            description: voff_prompts::setup::CREATE_TEAM_TOOL_DESCRIPTION.into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "summary": { "type": "string", "minLength": 1, "maxLength": 500 },
                    "agents": {
                        "type": "array",
                        "minItems": 1,
                        "maxItems": voff_prompts::setup::MAX_GENERATED_AGENTS,
                        "items": {
                            "type": "object",
                            "properties": {
                                "name": { "type": "string", "minLength": 1, "maxLength": 80 },
                                "instructions": { "type": "string", "minLength": 1, "maxLength": 16000 },
                                "reports_to": { "type": ["string", "null"] }
                            },
                            "required": ["name", "instructions", "reports_to"],
                            "additionalProperties": false
                        }
                    }
                },
                "required": ["summary", "agents"],
                "additionalProperties": false
            })
            .to_string(),
        }],
        tool_choice: ToolChoice::Specific(vec!["create_team".into()]),
        max_output_tokens: Some(team_plan_output_budget(&scan)),
    };
    let events = collect_response(
        provider.as_ref(),
        request,
        CancellationToken::new(),
        Duration::from_secs(180),
    )
    .await
    .map_err(|error| format!("{} team design failed: {error}", provider_choice.label()))?;
    let arguments = events
        .into_iter()
        .find_map(|event| match event {
            ProviderEvent::ToolRequest(call) if call.name == "create_team" => {
                Some(call.arguments_json)
            }
            _ => None,
        })
        .ok_or_else(|| {
            "The model returned no team plan. Try again or choose another provider.".to_owned()
        })?;
    let plan: WorkspaceTeamPlan = serde_json::from_str(&arguments)
        .map_err(|error| format!("the model returned an invalid team plan: {error}"))?;
    let summary = plan.summary.trim().to_owned();
    let developers = plan_to_draft(plan, provider_choice, &model, credential_reference)?;
    let notice = summary;
    Ok((
        SetupDraft {
            developers,
            working_directory: workspace,
        },
        notice,
    ))
}

fn plan_to_draft(
    plan: WorkspaceTeamPlan,
    provider: ProviderChoice,
    model: &str,
    credential_reference: Option<String>,
) -> Result<Vec<DeveloperDraft>, String> {
    if plan.agents.is_empty() {
        return Err("the generated team must include a Team Lead".into());
    }
    if plan.agents.len() > voff_prompts::setup::MAX_GENERATED_AGENTS {
        return Err(format!(
            "the generated team exceeds the limit of {} agents",
            voff_prompts::setup::MAX_GENERATED_AGENTS
        ));
    }
    let names = plan
        .agents
        .iter()
        .map(|agent| agent.name.trim().to_owned())
        .collect::<Vec<_>>();
    if names
        .iter()
        .enumerate()
        .any(|(index, name)| name.is_empty() || name.len() > 80 || names[..index].contains(name))
    {
        return Err("the generated team has empty, overlong, or duplicate agent names".into());
    }
    if plan.summary.trim().is_empty() || plan.summary.len() > 500 {
        return Err("the generated project summary is empty or too long".into());
    }
    let agent_count = plan.agents.len();
    let mut developers = Vec::with_capacity(agent_count);
    for (index, agent) in plan.agents.into_iter().enumerate() {
        let reports_to = if index == 0 {
            if agent.reports_to.is_some() {
                return Err("the first generated agent must be the team root".into());
            }
            None
        } else {
            let parent = agent
                .reports_to
                .as_deref()
                .ok_or_else(|| format!("agent {} must name its reporting parent", agent.name))?;
            Some(
                names
                    .iter()
                    .position(|name| name == parent)
                    .filter(|parent_index| *parent_index < index)
                    .ok_or_else(|| {
                        format!("agent {} has an invalid reporting target", agent.name)
                    })?,
            )
        };
        let instructions = agent.instructions.trim();
        if instructions.is_empty() || instructions.len() > 16_000 {
            return Err(format!(
                "agent {} has empty or overlong instructions",
                agent.name
            ));
        }
        let mut developer = DeveloperDraft::suggested(
            index,
            agent_count,
            provider,
            model.to_owned(),
            DEFAULT_OUTPUT_TOKENS,
        );
        developer.name = agent.name.trim().to_owned();
        developer.role = if index == 0 {
            DeveloperRole::Lead
        } else {
            DeveloperRole::Custom
        };
        developer.reports_to = reports_to;
        developer.system_prompt = instructions.to_owned();
        developer.credential_reference = credential_reference.clone();
        developers.push(developer);
    }
    for (index, developer) in developers.iter().enumerate().skip(1) {
        let mut parent = developer.reports_to;
        for _ in 0..4 {
            if let Some(parent_index) = parent {
                if parent_index >= developers.len() || parent_index == index {
                    return Err("the generated team has an invalid reporting tree".into());
                }
                parent = developers[parent_index].reports_to;
            } else {
                break;
            }
        }
        if parent.is_some() {
            return Err("the generated team reporting tree is too deep or cyclic".into());
        }
    }
    Ok(developers)
}

fn team_plan_output_budget(scan: &WorkspaceScan) -> u64 {
    const MIN_TOKENS: u64 = 4_096;
    const MAX_TOKENS: u64 = 16_384;
    let additional_files = scan.files_seen.saturating_sub(40);
    let extra_tokens = u64::try_from(additional_files / 8)
        .unwrap_or(u64::MAX)
        .saturating_mul(512);
    MIN_TOKENS.saturating_add(extra_tokens).min(MAX_TOKENS)
}

pub(crate) fn scan_placeholder(
    workspace: &Path,
    previous: Option<&SetupDraft>,
    catalog: ModelCatalog,
) -> SetupDraft {
    let previous_lead = previous
        .and_then(|draft| draft.developers.first())
        .filter(|lead| lead.provider.is_available());
    let provider = previous_lead.map_or_else(preferred_provider, |lead| lead.provider);
    let model = previous_lead.map_or_else(
        || catalog.first_model(provider.id()),
        |lead| lead.model.clone(),
    );
    let mut lead = DeveloperDraft::suggested(0, 1, provider, model, DEFAULT_OUTPUT_TOKENS);
    lead.role = DeveloperRole::Lead;
    lead.name = "Team Lead".into();
    lead.system_prompt = voff_prompts::setup::SCAN_PLACEHOLDER_PROMPT.into();
    SetupDraft {
        developers: vec![lead],
        working_directory: workspace.to_path_buf(),
    }
}

#[derive(Default)]
struct WorkspaceScan {
    files_seen: usize,
    entries_seen: usize,
    has_tests: bool,
    languages: std::collections::BTreeSet<&'static str>,
    paths: Vec<String>,
    context_files: Vec<(String, String)>,
}

impl WorkspaceScan {
    fn visit(&mut self, directory: &Path, root: &Path, depth: usize) -> io::Result<()> {
        if depth > 4 || self.entries_seen >= 10_000 {
            return Ok(());
        }
        let entries = match fs::read_dir(directory) {
            Ok(entries) => entries,
            Err(error) if depth > 0 && error.kind() == io::ErrorKind::PermissionDenied => {
                return Ok(());
            }
            Err(error) => return Err(error),
        };
        let mut entries = entries.filter_map(Result::ok).collect::<Vec<_>>();
        entries.sort_by_key(std::fs::DirEntry::file_name);
        for entry in entries {
            if self.entries_seen >= 10_000 {
                break;
            }
            self.entries_seen += 1;
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.starts_with('.')
                || matches!(
                    name.as_ref(),
                    "node_modules" | "target" | "vendor" | "dist" | "build" | ".venv"
                )
            {
                continue;
            }
            let kind = match entry.file_type() {
                Ok(kind) => kind,
                Err(_) => continue,
            };
            if kind.is_dir() {
                self.visit(&entry.path(), root, depth + 1)?;
            } else if kind.is_file() {
                let path = entry.path();
                let relative = path
                    .strip_prefix(root)
                    .unwrap_or(&path)
                    .to_string_lossy()
                    .replace('\\', "/");
                if is_sensitive_path(&relative) {
                    continue;
                }
                self.paths.push(relative.clone());
                self.inspect_file(&name, &path);
                self.capture_context_file(&relative, &path);
            }
        }
        Ok(())
    }

    fn capture_context_file(&mut self, relative: &str, path: &Path) {
        const MAX_CONTEXT_FILES: usize = 12;
        const MAX_CONTEXT_BYTES: usize = 24_000;
        const MAX_FILE_BYTES: usize = 4_000;
        let documentation_files = self
            .context_files
            .iter()
            .filter(|(path, _)| is_project_document(path))
            .count();
        let source_files = self.context_files.len() - documentation_files;
        if self.context_files.len() >= MAX_CONTEXT_FILES
            || self
                .context_files
                .iter()
                .map(|(_, content)| content.len())
                .sum::<usize>()
                >= MAX_CONTEXT_BYTES
            || !is_context_file(relative, documentation_files, source_files)
        {
            return;
        }
        let Ok(file) = fs::File::open(path) else {
            return;
        };
        let mut bytes = Vec::new();
        if file
            .take((MAX_FILE_BYTES + 1) as u64)
            .read_to_end(&mut bytes)
            .is_err()
        {
            return;
        }
        if bytes.len() > MAX_FILE_BYTES {
            return;
        }
        let Ok(contents) = String::from_utf8(bytes) else {
            return;
        };
        let redacted = contents
            .lines()
            .map(|line| {
                let lower = line.to_ascii_lowercase();
                if [
                    "api_key",
                    "apikey",
                    "token",
                    "password",
                    "secret",
                    "private_key",
                    "authorization",
                    "credential",
                ]
                .iter()
                .any(|needle| lower.contains(needle))
                {
                    "[redacted possible credential]"
                } else {
                    line
                }
            })
            .collect::<Vec<_>>()
            .join("\n");
        let used = self
            .context_files
            .iter()
            .map(|(_, content)| content.len())
            .sum::<usize>();
        let remaining = MAX_CONTEXT_BYTES.saturating_sub(used);
        if remaining == 0 {
            return;
        }
        self.context_files.push((
            relative.to_owned(),
            redacted.chars().take(remaining).collect(),
        ));
    }

    fn prompt_context(&self, workspace: &Path) -> String {
        let name = workspace
            .file_name()
            .and_then(|value| value.to_str())
            .filter(|value| !value.is_empty())
            .unwrap_or("workspace");
        let summary = self.summary();
        let mut context =
            voff_prompts::setup::project_evidence_header(name, &summary, self.files_seen);
        for path in self.paths.iter().take(160) {
            context.push_str("- ");
            context.push_str(path);
            context.push('\n');
        }
        if self.paths.len() > 160 {
            context.push_str(&format!("- … and {} more paths\n", self.paths.len() - 160));
        }
        for (path, contents) in &self.context_files {
            context.push_str(&format!("\n--- {path} ---\n{contents}\n"));
        }
        context
    }

    fn inspect_file(&mut self, name: &str, path: &Path) {
        self.files_seen += 1;
        let extension = path
            .extension()
            .and_then(|extension| extension.to_str())
            .unwrap_or("");
        match extension {
            "rs" => {
                self.languages.insert("Rust");
            }
            "py" => {
                self.languages.insert("Python");
            }
            "go" => {
                self.languages.insert("Go");
            }
            "java" => {
                self.languages.insert("Java");
            }
            "cs" => {
                self.languages.insert("C#");
            }
            "js" | "jsx" | "ts" | "tsx" | "vue" | "svelte" | "html" | "css" => {
                self.languages.insert("web");
            }
            "sql" => {
                self.languages.insert("SQL");
            }
            _ => {}
        }
        let lower = name.to_ascii_lowercase();
        self.has_tests |= lower.contains("test") || lower.contains("spec");
        if lower == "cargo.toml" {
            self.languages.insert("Rust workspace");
        }
        if lower == "package.json" {
            self.languages.insert("Node project");
        }
        if lower == "pyproject.toml" || lower == "requirements.txt" {
            self.languages.insert("Python project");
        }
    }

    fn summary(&self) -> String {
        let mut indicators = self.languages.iter().copied().collect::<Vec<_>>();
        if self.has_tests {
            indicators.push("tests");
        }
        if indicators.is_empty() {
            format!("{} project files", self.files_seen)
        } else {
            indicators.sort_unstable();
            indicators.dedup();
            indicators.join(", ")
        }
    }
}

fn is_sensitive_path(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    let file = lower.rsplit('/').next().unwrap_or(&lower);
    [".env", "secret", "credential", "private", "id_rsa"]
        .iter()
        .any(|needle| lower.contains(needle))
        || [".pem", ".key", ".p12", ".pfx"]
            .iter()
            .any(|extension| file.ends_with(extension))
}

fn is_project_document(path: &str) -> bool {
    let file = path.rsplit('/').next().unwrap_or(path).to_ascii_lowercase();
    file.starts_with("readme")
        || matches!(
            file.as_str(),
            "agents.md"
                | "cargo.toml"
                | "package.json"
                | "pyproject.toml"
                | "go.mod"
                | "requirements.txt"
                | "makefile"
                | "justfile"
                | "cmakelists.txt"
                | "pom.xml"
                | "build.gradle"
                | "build.gradle.kts"
        )
}

fn is_context_file(path: &str, documentation_files: usize, source_files: usize) -> bool {
    if is_project_document(path) {
        return documentation_files < 4;
    }
    let file = path.rsplit('/').next().unwrap_or(path).to_ascii_lowercase();
    let extension = file.rsplit_once('.').map(|(_, extension)| extension);
    matches!(
        extension,
        Some("rs" | "py" | "go" | "ts" | "tsx" | "js" | "jsx")
    ) && (path.starts_with("src/")
        || path.contains("/src/")
        || path.starts_with("app/")
        || path.contains("/app/"))
        && source_files < 6
}

fn logo_text() -> Option<Text<'static>> {
    animated_logo_text_for(theme::BACKGROUND)
}

pub(crate) fn animated_logo_text_for(background: Color) -> Option<Text<'static>> {
    const OPEN: &[u8] = include_bytes!("../assets/voff-logo.ansi");
    const HALF_OPEN: &[u8] = include_bytes!("../assets/logo-blink/half-open-block.ansi");
    const NEARLY_CLOSED: &[u8] = include_bytes!("../assets/logo-blink/nearly-closed-block.ansi");
    const CLOSED: &[u8] = include_bytes!("../assets/logo-blink/closed-block.ansi");
    const OPEN_CELLS: &[u8] = include_bytes!("../assets/logo-blink/open.rgba");
    const HALF_OPEN_CELLS: &[u8] = include_bytes!("../assets/logo-blink/half-open.rgba");
    const NEARLY_CLOSED_CELLS: &[u8] = include_bytes!("../assets/logo-blink/nearly-closed.rgba");
    const CLOSED_CELLS: &[u8] = include_bytes!("../assets/logo-blink/closed.rgba");
    static BLINK: OnceLock<Mutex<LogoBlink>> = OnceLock::new();

    let mut blink = BLINK
        .get_or_init(|| Mutex::new(LogoBlink::new()))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let frame = blink.frame(OPEN, HALF_OPEN, NEARLY_CLOSED, CLOSED);
    let cells = if std::ptr::eq(frame.as_ptr(), OPEN.as_ptr()) {
        OPEN_CELLS
    } else if std::ptr::eq(frame.as_ptr(), HALF_OPEN.as_ptr()) {
        HALF_OPEN_CELLS
    } else if std::ptr::eq(frame.as_ptr(), NEARLY_CLOSED.as_ptr()) {
        NEARLY_CLOSED_CELLS
    } else {
        CLOSED_CELLS
    };
    logo_text_from_bytes(frame, cells, background)
}

struct LogoBlink {
    random_state: u64,
    last_blink: Instant,
    next_blink_after: Duration,
    blink_started: Option<Instant>,
}

impl LogoBlink {
    fn new() -> Self {
        let now = Instant::now();
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        Self {
            random_state: (u64::try_from(nanos).unwrap_or(u64::MAX)
                ^ u64::from(std::process::id()))
            .max(1),
            last_blink: now,
            next_blink_after: Duration::from_millis(500),
            blink_started: None,
        }
    }

    fn frame(
        &mut self,
        open: &'static [u8],
        half_open: &'static [u8],
        nearly_closed: &'static [u8],
        closed: &'static [u8],
    ) -> &'static [u8] {
        let now = Instant::now();
        if let Some(started) = self.blink_started {
            let elapsed = now.duration_since(started).as_millis();
            if elapsed >= 660 {
                self.blink_started = None;
                self.last_blink = now;
                self.next_blink_after = self.random_delay();
                return open;
            }
            return match elapsed {
                0..=129 | 530..=659 => half_open,
                130..=259 | 400..=529 => nearly_closed,
                _ => closed,
            };
        }

        if now.duration_since(self.last_blink) >= self.next_blink_after {
            self.blink_started = Some(now);
            half_open
        } else {
            open
        }
    }

    fn random_delay(&mut self) -> Duration {
        // Xorshift seeded per process so blink intervals vary from 5 to 15 seconds.
        self.random_state ^= self.random_state << 13;
        self.random_state ^= self.random_state >> 7;
        self.random_state ^= self.random_state << 17;
        Duration::from_millis(5_000 + self.random_state % 10_001)
    }
}

fn logo_text_from_bytes(
    bytes: &'static [u8],
    cell_colors: &'static [u8],
    background: Color,
) -> Option<Text<'static>> {
    let bytes = bytes.strip_prefix(b"\x1b[?25l").unwrap_or(bytes);
    let bytes = bytes.strip_suffix(b"\x1b[?25h").unwrap_or(bytes);
    let logo = bytes.into_text().ok()?;
    // ANSI block glyphs retain the original shading. A softened per-cell map
    // from the GIF fills blank glyphs and smooths the helmet's darkest edge.
    if cell_colors.len() != 40 * 22 * 4 {
        return Some(fill_logo_background(logo, background));
    }
    let mut left = usize::MAX;
    let mut right = 0;
    let mut top = usize::MAX;
    let mut bottom = 0;
    for (row, line) in logo.lines.iter().enumerate() {
        let mut column = 0;
        for span in &line.spans {
            for character in span.content.chars() {
                if logo_art_cell(character, span.style) {
                    left = left.min(column);
                    right = right.max(column + 1);
                    top = top.min(row);
                    bottom = row + 1;
                }
                column += 1;
            }
        }
        // The bundled artwork uses one-cell block characters. If that ever
        // changes, render the original text instead of cropping at wrong cells.
        if column != line.width() {
            return Some(fill_logo_background(logo, background));
        }
        // Keep the full 40-column source canvas, including transparent edge
        // cells, so the logo retains its intended layout width.
        right = right.max(column);
    }
    if left == usize::MAX {
        return None;
    }
    let mut lines = Vec::new();
    for (row, line) in logo
        .lines
        .into_iter()
        .enumerate()
        .skip(top)
        .take(bottom - top)
    {
        let mut column = 0;
        let mut spans = Vec::new();
        for span in line.spans {
            for character in span.content.chars() {
                if (left..right).contains(&column) {
                    let cell_offset = (row * 40 + column) * 4;
                    let cell = &cell_colors[cell_offset..cell_offset + 4];
                    let mut style = span.style;
                    if character.is_whitespace() && cell[3] != 0 {
                        style = style.bg(Color::Rgb(cell[0], cell[1], cell[2]));
                    } else if matches!(
                        style.bg,
                        Some(Color::Rgb(0, 0, 0) | Color::Black | Color::Reset) | None
                    ) {
                        style = style.bg(if cell[3] == 0 {
                            background
                        } else {
                            Color::Rgb(1, 1, 1)
                        });
                    }
                    if row < 14 && cell[3] != 0 && cell[..3].iter().all(|channel| *channel < 80) {
                        if style.fg.is_some_and(|foreground| {
                            matches!(
                                foreground,
                                Color::Rgb(red, green, blue)
                                    if red < 32 && green < 32 && blue < 32
                            )
                        }) {
                            style = style.fg(Color::Rgb(cell[0], cell[1], cell[2]));
                        }
                        if style.bg == Some(Color::Rgb(1, 1, 1)) {
                            style = style.bg(Color::Rgb(cell[0], cell[1], cell[2]));
                        }
                    }
                    spans.push(Span::styled(character.to_string(), style));
                }
                column += 1;
            }
        }
        if column < right {
            spans.push(Span::raw(" ".repeat(right - column)));
        }
        lines.push(Line::from(spans).style(line.style.bg(background)));
    }
    Some(Text::from(lines))
}

fn fill_logo_background(mut logo: Text<'static>, background: Color) -> Text<'static> {
    for line in &mut logo.lines {
        line.style = line.style.bg(background);
        for span in &mut line.spans {
            if matches!(
                span.style.bg,
                None | Some(Color::Rgb(0, 0, 0) | Color::Black | Color::Reset)
            ) {
                span.style = span.style.bg(background);
            }
        }
    }
    logo
}

fn logo_art_cell(character: char, style: Style) -> bool {
    !character.is_whitespace()
        || style.add_modifier.contains(Modifier::REVERSED)
        || style.bg.is_some_and(|background| {
            !matches!(
                background,
                Color::Rgb(0, 0, 0) | Color::Black | Color::Reset
            )
        })
}

#[cfg(test)]
pub(crate) fn render(frame: &mut ratatui::Frame<'_>, wizard: &Wizard) {
    let area = frame.area();
    frame.render_widget(Block::default().style(theme::canvas()), area);
    let shell = wizard.shell(area);
    if let Some(logo_area) = wizard.logo_area(area)
        && let Some(logo) = logo_text()
    {
        frame.render_widget(Paragraph::new(logo).style(theme::canvas()), logo_area);
    }
    render_setup(frame, wizard, shell, false);
    render_model_browser(frame, wizard, shell);
}

pub(crate) fn render_popup(frame: &mut ratatui::Frame<'_>, wizard: &Wizard) {
    let shell = wizard.shell(frame.area());
    frame.render_widget(Clear, shell);
    let mut block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .style(theme::surface())
        .border_style(Style::default().fg(theme::BORDER));
    if let Some(title) = setup_title(wizard) {
        block = block.title(title);
    }
    frame.render_widget(block, shell);
    render_setup(frame, wizard, shell, true);
    render_model_browser(frame, wizard, shell);
}

fn model_browser_area(shell: Rect) -> Rect {
    centered(
        shell,
        model_picker_width(shell, "Models", 72, 88, "close"),
        shell.height.saturating_sub(2).min(12),
    )
}

fn render_model_browser(frame: &mut ratatui::Frame<'_>, wizard: &Wizard, shell: Rect) {
    let (Step::Developer(index), Some(browser)) = (wizard.step, &wizard.model_browser) else {
        return;
    };
    let area = model_browser_area(shell);
    if area.width < 8 || area.height < 5 {
        return;
    }
    let title = model_picker_title("Models");
    let footer = model_picker_footer("close");
    frame.render_widget(Clear, area);
    frame.render_widget(
        Block::default()
            .title(title)
            .title_bottom(footer)
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .style(theme::surface())
            .border_style(Style::default().fg(theme::AQUA)),
        area,
    );
    frame.render_widget(
        Paragraph::new(format!("  Search: {}", browser.query)).style(theme::surface()),
        Rect::new(area.x + 1, area.y + 1, area.width - 2, 1),
    );
    let matches = wizard.model_browser_matches(index);
    let visible = area.height.saturating_sub(3) as usize;
    let start = browser.selected / visible.max(1) * visible.max(1);
    let models = wizard
        .catalog
        .models(wizard.developers[index].provider.id());
    if matches.is_empty() {
        frame.render_widget(
            Paragraph::new("  No matching models").style(Style::default().fg(theme::MUTED)),
            Rect::new(area.x + 1, area.y + 2, area.width - 2, 1),
        );
    } else {
        for (row, &model_index) in matches.iter().skip(start).take(visible).enumerate() {
            let style = if start + row == browser.selected {
                Style::default()
                    .fg(theme::BACKGROUND)
                    .bg(theme::AQUA)
                    .add_modifier(Modifier::BOLD)
            } else {
                theme::surface()
            };
            frame.render_widget(
                Paragraph::new(format!("  {}", models[model_index].id)).style(style),
                Rect::new(area.x + 1, area.y + 2 + row as u16, area.width - 2, 1),
            );
        }
    }
    let cursor_x = area
        .x
        .saturating_add(11)
        .saturating_add(browser.query.chars().count() as u16)
        .min(area.right().saturating_sub(2));
    frame.set_cursor_position((cursor_x, area.y + 1));
}

fn setup_title(wizard: &Wizard) -> Option<Line<'static>> {
    if wizard.step == Step::WorkspacePicker {
        let directory = wizard.workspace_explorer.as_ref().map_or_else(
            || wizard.working_directory.display().to_string(),
            |explorer| explorer.cwd().display().to_string(),
        );
        let mut title = workspace_path_title("Choose workspace", &directory);
        if wizard.mode == WizardMode::Workspace {
            title
                .spans
                .insert(0, Span::styled(" 🐶 ", Style::default().fg(theme::ORANGE)));
        }
        Some(title)
    } else {
        match wizard.mode {
            WizardMode::Startup | WizardMode::Configure => None,
            WizardMode::Add => Some(Line::from(vec![
                Span::styled(" 🐶 ", Style::default().fg(theme::ORANGE)),
                Span::styled(
                    "Add agent",
                    Style::default()
                        .fg(theme::AQUA)
                        .add_modifier(Modifier::BOLD),
                ),
            ])),
            WizardMode::Workspace => Some(Line::from(vec![
                Span::styled(" 🐶 ", Style::default().fg(theme::ORANGE)),
                Span::styled(
                    "Choose workspace",
                    Style::default()
                        .fg(theme::AQUA)
                        .add_modifier(Modifier::BOLD),
                ),
            ])),
        }
    }
}

fn render_setup(
    frame: &mut ratatui::Frame<'_>,
    wizard: &Wizard,
    shell: Rect,
    title_in_block: bool,
) {
    if !title_in_block && let Some(title) = setup_title(wizard) {
        frame.render_widget(
            Paragraph::new(title).style(theme::surface()),
            Rect::new(shell.x, shell.y, shell.width, 1),
        );
    }
    let rows = setup_rows(shell, wizard.step, wizard.mode);
    match wizard.step {
        Step::Basics => render_basics(frame, wizard, rows[1]),
        Step::WorkspacePicker => render_workspace_picker(frame, wizard, rows[1]),
        Step::Developer(index) => render_developer(frame, wizard, index, rows[1]),
        Step::Prompt(index) => render_developer(frame, wizard, index, rows[1]),
    }
    let footer = wizard
        .error
        .as_deref()
        .map_or_else(|| footer_text(wizard), |error| format!("! {error}"));
    let footer_style = if wizard.error.is_some() {
        Style::default().fg(theme::RED).bg(theme::SURFACE)
    } else {
        theme::surface()
    };
    let footer_line = if wizard.mode == WizardMode::Startup
        && matches!(wizard.step, Step::Developer(_))
        && wizard.error.is_none()
    {
        Line::from(vec![
            Span::raw("Press Enter to "),
            Span::styled(
                "voff",
                Style::default()
                    .fg(theme::ORANGE)
                    .add_modifier(Modifier::ITALIC),
            ),
            Span::raw(" 🐶"),
        ])
    } else if wizard.step == Step::WorkspacePicker && wizard.error.is_none() {
        workspace_picker_footer(wizard)
    } else {
        Line::from(footer)
    };
    frame.render_widget(
        Paragraph::new(footer_line)
            .alignment(Alignment::Center)
            .style(footer_style),
        rows[2],
    );
}

fn render_basics(frame: &mut ratatui::Frame<'_>, wizard: &Wizard, area: Rect) {
    let working_directory = wizard.working_directory.display().to_string();
    let lines = vec![form_line("Workspace", &working_directory, true, true)];
    frame.render_widget(
        Paragraph::new(lines)
            .alignment(Alignment::Center)
            .wrap(Wrap { trim: true }),
        centered(area, area.width.min(72), 1),
    );
}

fn render_workspace_picker(frame: &mut ratatui::Frame<'_>, wizard: &Wizard, area: Rect) {
    if let Some(explorer) = &wizard.workspace_explorer {
        let mut list_y = area.y;
        let mut list_height = area.height;
        if (wizard.workspace_filtering || !wizard.workspace_filter.is_empty()) && list_height > 0 {
            frame.render_widget(
                Paragraph::new(format!(
                    "Search: {}{}",
                    wizard.workspace_filter,
                    if wizard.workspace_filtering {
                        "█"
                    } else {
                        ""
                    }
                ))
                .style(theme::surface()),
                Rect::new(area.x, list_y, area.width, 1),
            );
            list_y = list_y.saturating_add(1);
            list_height = list_height.saturating_sub(1);
        }
        if list_height > 0 {
            frame.render_widget_ref(
                explorer.widget(),
                Rect::new(area.x, list_y, area.width, list_height),
            );
        }
    } else {
        frame.render_widget(
            Paragraph::new("Workspace picker is unavailable.")
                .alignment(Alignment::Center)
                .style(Style::default().fg(theme::RED)),
            area,
        );
    }
}

fn render_developer(frame: &mut ratatui::Frame<'_>, wizard: &Wizard, index: usize, area: Rect) {
    let developer = &wizard.developers[index];
    let model_label = &developer.model;
    let cols = developer_columns(area);
    let compact = cols[0].height < 16;
    let mut form = Vec::new();
    let mut cursor = None;
    if wizard.field == Field::Name {
        cursor = Some((form.len(), 15 + Line::from(developer.name.as_str()).width()));
    }
    form.push(form_line(
        "Name",
        &developer.name,
        wizard.field == Field::Name,
        false,
    ));
    if !compact {
        form.push(Line::from(""));
    }
    let parent = if index == 0 {
        None
    } else {
        Some(
            developer
                .reports_to
                .and_then(|parent| wizard.developers.get(parent))
                .map_or("Team Lead", |parent| parent.name.as_str()),
        )
    };
    if let Some(parent) = parent {
        form.push(form_line(
            "Reports to",
            parent,
            wizard.field == Field::ReportsTo,
            true,
        ));
        if !compact {
            form.push(Line::from(""));
        }
    }
    form.push(form_line(
        "Provider",
        developer.provider.label(),
        wizard.field == Field::Provider,
        true,
    ));
    if !compact {
        form.push(Line::from(""));
    }
    let model_choice = wizard.catalog.models(developer.provider.id()).len() > 1;
    if wizard.field == Field::Model {
        cursor = Some((
            form.len(),
            15 + usize::from(model_choice) * 2 + Line::from(model_label.as_str()).width(),
        ));
    }
    let mut model_line = form_line(
        "Model",
        model_label,
        wizard.field == Field::Model,
        model_choice,
    );
    if wizard.field == Field::Model && model_choice && !wizard.model_touched {
        model_line.spans.push(Span::styled(
            " [f]",
            Style::default()
                .fg(theme::AQUA)
                .add_modifier(Modifier::BOLD),
        ));
    }
    form.push(model_line);
    if !compact {
        form.push(Line::from(""));
    }
    if wizard.mode == WizardMode::Configure {
        if wizard.field == Field::OutputLimit {
            cursor = Some((
                form.len(),
                15 + developer.max_output_tokens.to_string().len(),
            ));
        }
        form.push(form_line(
            "Token limit",
            &developer.max_output_tokens.to_string(),
            wizard.field == Field::OutputLimit,
            false,
        ));
        if !compact {
            form.push(Line::from(""));
        }
    }
    if developer.provider.uses_api_key() {
        let token = api_token_label(
            developer.provider,
            &developer.api_token,
            developer.provider.is_available(),
        );
        if !compact {
            form.push(Line::from(""));
        }
        if wizard.field == Field::Token {
            cursor = Some((form.len(), 15 + Line::from(token.as_str()).width()));
        }
        form.push(form_line_with_value_style(
            "API token",
            &token,
            wizard.field == Field::Token,
            false,
            if token == "Required" {
                Style::default().fg(theme::RED)
            } else {
                Style::default()
            },
        ));
    }
    if !compact {
        form.push(Line::from(""));
    }
    let form_title = if wizard.mode == WizardMode::Configure {
        Line::from(vec![Span::styled(
            format!(" Configure {} ", developer.name),
            Style::default()
                .fg(theme::AQUA)
                .add_modifier(Modifier::BOLD),
        )])
    } else {
        Line::from(Span::styled(
            if index == 0 {
                " Configure your lead agent ".to_owned()
            } else {
                format!(" Configure {} ", developer.name)
            },
            Style::default()
                .fg(theme::AQUA)
                .add_modifier(Modifier::BOLD),
        ))
    };
    form.push(form_line(
        "Setup prompt",
        "Press e to edit",
        wizard.field == Field::Prompt,
        false,
    ));
    frame.render_widget(
        Paragraph::new(form).block(
            Block::default()
                .title(form_title)
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .style(Style::default().fg(theme::TEXT))
                .border_style(Style::default().fg(theme::BORDER)),
        ),
        cols[0],
    );
    render_prompt_preview(frame, wizard, developer, cols[1]);
    if let Some((row, column)) = cursor {
        let x = cols[0]
            .x
            .saturating_add(1)
            .saturating_add(u16::try_from(column).unwrap_or(u16::MAX))
            .min(cols[0].right().saturating_sub(2));
        let y = cols[0]
            .y
            .saturating_add(1)
            .saturating_add(u16::try_from(row).unwrap_or(u16::MAX));
        if y < cols[0].bottom().saturating_sub(1) {
            frame.set_cursor_position((x, y));
        }
    }
}

fn api_token_label(provider: ProviderChoice, entered: &str, env_available: bool) -> String {
    if !entered.is_empty() {
        "•".repeat(entered.chars().count().min(36))
    } else if env_available {
        format!(
            "Configured via {}",
            provider.credential_env().unwrap_or("API key")
        )
    } else {
        "Required".into()
    }
}

fn render_prompt_preview(
    frame: &mut ratatui::Frame<'_>,
    wizard: &Wizard,
    developer: &DeveloperDraft,
    area: Rect,
) {
    if let Some(editor) = &wizard.prompt_editor {
        frame.render_widget(editor, area);
        return;
    }
    let prompt = if developer.system_prompt.is_empty() {
        "The setup prompt will be generated from the project and your goal."
    } else {
        developer.system_prompt.as_str()
    };
    frame.render_widget(
        Paragraph::new(prompt).wrap(Wrap { trim: false }).block(
            Block::default()
                .title(Line::from(Span::styled(
                    " Agent prompt · [e] edit ",
                    Style::default()
                        .fg(theme::AQUA)
                        .add_modifier(Modifier::BOLD),
                )))
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .style(Style::default().fg(theme::TEXT))
                .border_style(Style::default().fg(theme::BORDER)),
        ),
        area,
    );
}

fn developer_columns(area: Rect) -> std::rc::Rc<[Rect]> {
    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(area)
}

fn developer_field_at_row(
    row: u16,
    provider: ProviderChoice,
    compact: bool,
    lead: bool,
    show_output_limit: bool,
) -> Option<Field> {
    let mut fields = vec![Field::Name];
    if !lead {
        fields.push(Field::ReportsTo);
    }
    fields.extend([Field::Provider, Field::Model]);
    if show_output_limit {
        fields.push(Field::OutputLimit);
    }
    if provider.uses_api_key() {
        fields.push(Field::Token);
    }
    let mut field_row = 0;
    for field in fields {
        if field_row == row {
            return Some(field);
        }
        field_row += if compact {
            1
        } else if field == Field::Token {
            4
        } else {
            2
        };
    }
    None
}

fn form_line(label: &str, value: &str, selected: bool, choice: bool) -> Line<'static> {
    form_line_with_value_style(label, value, selected, choice, Style::default())
}

fn form_line_with_value_style(
    label: &str,
    value: &str,
    selected: bool,
    choice: bool,
    value_style: Style,
) -> Line<'static> {
    let value = if choice {
        format!("‹ {value} ›")
    } else {
        value.to_owned()
    };
    Line::from(vec![
        Span::styled(
            format!("  {label:<13}"),
            if selected {
                Style::default()
                    .fg(theme::BACKGROUND)
                    .bg(theme::BORDER)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(theme::TEXT)
            },
        ),
        Span::styled(value, value_style),
    ])
}

fn footer_text(wizard: &Wizard) -> String {
    match wizard.step {
        Step::Basics => "[Enter] continue · [c] choose workspace".into(),
        Step::WorkspacePicker => String::new(),
        Step::Developer(_) if wizard.mode != WizardMode::Startup => {
            "[Tab]/[↑/↓] move · [←/→] choose · [e]/[Ctrl+E] prompt · [Enter] save · [Esc] cancel"
                .into()
        }
        Step::Developer(_) => "[e]/[Ctrl+E] prompt · [Enter] start · [Esc] go back".into(),
        Step::Prompt(_) => "Edit freely · [Enter] new line · [Ctrl+S] save · [Esc] discard".into(),
    }
}

fn workspace_path_title(label: &str, directory: &str) -> Line<'static> {
    Line::from(vec![
        Span::styled(
            format!(" {label} · "),
            Style::default()
                .fg(theme::AQUA)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(directory.to_owned(), Style::default().fg(theme::TEXT)),
        Span::raw(" "),
    ])
}

fn workspace_picker_footer(wizard: &Wizard) -> Line<'static> {
    let hints = if wizard.workspace_filtering {
        &[("Enter", "select"), ("Esc", "finish filter")][..]
    } else if !wizard.workspace_filter.is_empty() {
        &[
            ("↑/↓", "move"),
            ("←/→", "browse"),
            ("Enter", "select"),
            ("Space", "current"),
            ("Esc", "clear"),
        ][..]
    } else {
        &[
            ("↑/↓", "move"),
            ("←/→", "browse"),
            ("/", "filter"),
            ("Enter", "select"),
            ("Space", "current"),
            (
                "Esc",
                if wizard.mode == WizardMode::Workspace {
                    "cancel"
                } else {
                    "back"
                },
            ),
        ][..]
    };
    key_hint_line(hints).alignment(Alignment::Center)
}

fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let width = width.min(area.width);
    let height = height.min(area.height);
    Rect::new(
        area.x + area.width.saturating_sub(width) / 2,
        area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    )
}

fn centered_lower(area: Rect, width: u16, height: u16) -> Rect {
    let popup = centered(area, width, height);
    let offset = area.height / 10;
    Rect::new(
        popup.x,
        popup
            .y
            .saturating_add(offset)
            .min(area.bottom().saturating_sub(popup.height)),
        popup.width,
        popup.height,
    )
}

fn setup_shell(area: Rect, step: Step) -> Rect {
    if step == Step::Basics {
        centered(area, area.width.min(70), area.height.min(15))
    } else {
        centered(area, area.width.min(120), area.height.min(27))
    }
}

fn setup_rows(shell: Rect, step: Step, mode: WizardMode) -> [Rect; 3] {
    let inner = shell.inner(ratatui::layout::Margin {
        horizontal: 2,
        vertical: 1,
    });
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(0),
            Constraint::Min(if step == Step::Basics { 3 } else { 10 }),
            Constraint::Length(if mode == WizardMode::Workspace { 2 } else { 1 }),
        ])
        .split(inner);
    [rows[0], rows[1], rows[2]]
}

struct TerminalGuard {
    handed_off: bool,
}

impl TerminalGuard {
    fn enter() -> io::Result<Self> {
        enable_raw_mode()?;
        execute!(
            io::stdout(),
            EnterAlternateScreen,
            EnableBracketedPaste,
            EnableMouseCapture
        )?;
        let old = panic::take_hook();
        panic::set_hook(Box::new(move |info| {
            let _ = disable_raw_mode();
            let _ = execute!(
                io::stdout(),
                DisableMouseCapture,
                DisableBracketedPaste,
                LeaveAlternateScreen
            );
            old(info);
        }));
        Ok(Self { handed_off: false })
    }

    fn handoff(&mut self) {
        self.handed_off = true;
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        if self.handed_off {
            return;
        }
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

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn generated_agent(name: &str, reports_to: Option<&str>) -> WorkspaceAgentPlan {
        WorkspaceAgentPlan {
            name: name.into(),
            instructions: format!("Own the {name} workstream."),
            reports_to: reports_to.map(str::to_owned),
        }
    }

    #[test]
    fn generated_team_preserves_nested_reporting_ownership() {
        let agents = plan_to_draft(
            WorkspaceTeamPlan {
                summary: "A project with separate service areas.".into(),
                agents: vec![
                    generated_agent("Team Lead", None),
                    generated_agent("Platform Coordinator", Some("Team Lead")),
                    generated_agent("Storage Specialist", Some("Platform Coordinator")),
                ],
            },
            ProviderChoice::CodexCli,
            "default",
            None,
        )
        .unwrap();

        assert_eq!(agents[1].reports_to, Some(0));
        assert_eq!(agents[2].reports_to, Some(1));
    }

    #[test]
    fn generated_team_rejects_missing_or_forward_reporting_parent() {
        for agents in [
            vec![
                generated_agent("Team Lead", None),
                generated_agent("Worker", None),
            ],
            vec![
                generated_agent("Team Lead", None),
                generated_agent("Worker", Some("Coordinator")),
                generated_agent("Coordinator", Some("Team Lead")),
            ],
        ] {
            let result = plan_to_draft(
                WorkspaceTeamPlan {
                    summary: "A project team.".into(),
                    agents,
                },
                ProviderChoice::CodexCli,
                "default",
                None,
            );
            assert!(result.is_err());
        }
    }

    #[test]
    fn generated_team_size_is_bounded_for_large_workspace_plans() {
        let agents = std::iter::once(generated_agent("Team Lead", None))
            .chain(
                (1..=voff_prompts::setup::MAX_GENERATED_AGENTS)
                    .map(|index| generated_agent(&format!("Worker {index}"), Some("Team Lead"))),
            )
            .collect();
        let result = plan_to_draft(
            WorkspaceTeamPlan {
                summary: "A broad project team.".into(),
                agents,
            },
            ProviderChoice::CodexCli,
            "default",
            None,
        );

        assert!(result.unwrap_err().contains("exceeds the limit"));
    }

    #[test]
    fn team_plan_output_budget_scales_with_project_evidence_and_is_bounded() {
        let small = WorkspaceScan {
            files_seen: 12,
            ..WorkspaceScan::default()
        };
        let large = WorkspaceScan {
            files_seen: 2_000,
            ..WorkspaceScan::default()
        };

        assert_eq!(team_plan_output_budget(&small), 4_096);
        assert!(team_plan_output_budget(&large) > team_plan_output_budget(&small));
        assert_eq!(team_plan_output_budget(&large), 16_384);
    }

    #[test]
    fn local_cli_providers_use_compact_labels() {
        assert_eq!(ProviderChoice::CodexCli.label(), "Codex · CLI");
        assert_eq!(ProviderChoice::ClaudeCli.label(), "Claude · CLI");
    }

    #[test]
    fn cli_model_and_cloud_token_labels_are_concise() {
        let mut wizard = Wizard::startup(ModelCatalog::default(), Path::new("."));
        wizard.developers[0].provider = ProviderChoice::CodexCli;
        let mut terminal = Terminal::new(ratatui::backend::TestBackend::new(100, 32)).unwrap();
        terminal.draw(|frame| render(frame, &wizard)).unwrap();
        let buffer = terminal.backend().buffer();
        let rendered = (0..buffer.area.height)
            .flat_map(|y| (0..buffer.area.width).map(move |x| buffer[(x, y)].symbol()))
            .collect::<String>();
        assert!(rendered.contains("Model        default"), "{rendered}");
        assert!(!rendered.contains("default  (1/1)"), "{rendered}");
        assert_eq!(
            api_token_label(ProviderChoice::OpenAi, "", false),
            "Required"
        );
        assert_eq!(
            api_token_label(ProviderChoice::OpenAi, "", true),
            "Configured via OPENAI_API_KEY"
        );
        assert_eq!(
            api_token_label(ProviderChoice::OpenAi, "secret", false),
            "••••••"
        );
    }

    #[test]
    fn model_picker_omits_type_hint_and_required_token_is_red() {
        let mut wizard = Wizard::startup(ModelCatalog::default(), Path::new("."));
        wizard.developers[0].provider = ProviderChoice::OpenAi;
        wizard.developers[0].model = "gpt-5-mini".into();
        wizard.field = Field::Model;
        let mut terminal = Terminal::new(ratatui::backend::TestBackend::new(100, 32)).unwrap();
        terminal.draw(|frame| render(frame, &wizard)).unwrap();
        let buffer = terminal.backend().buffer();
        let rendered = (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>();
        let model = rendered
            .iter()
            .find(|line| line.contains("gpt-5-mini"))
            .unwrap();
        assert!(model.contains("‹ gpt-5-mini ›"), "{model}");
        assert!(model.contains("‹ gpt-5-mini › [f]"), "{model}");
        assert!(!model.contains('●'), "{model}");
        assert!(!model.contains("[i] type"), "{model}");
        let token_row = rendered
            .iter()
            .position(|line| line.contains("Required"))
            .unwrap() as u16;
        let token_column = rendered[usize::from(token_row)].find("Required").unwrap() as u16;
        assert_eq!(buffer[(token_column, token_row)].fg, theme::RED);
    }

    #[test]
    fn model_browser_filters_selects_and_cancels_without_starting_setup() {
        let mut wizard = Wizard::startup(ModelCatalog::default(), Path::new("."));
        wizard.developers[0].provider = ProviderChoice::OpenAi;
        wizard.developers[0].model = wizard.catalog.first_model("openai");
        wizard.field = Field::Model;
        let original = wizard.developers[0].model.clone();
        wizard.handle_key(key(KeyCode::Char('f')));
        assert!(wizard.model_browser.is_some());
        wizard.handle_key(key(KeyCode::Down));
        wizard.handle_key(key(KeyCode::Enter));
        assert!(wizard.model_browser.is_none());
        assert_ne!(wizard.developers[0].model, original);
        assert!(!wizard.complete());

        let selected = wizard.developers[0].model.clone();
        wizard.handle_key(key(KeyCode::Char('f')));
        wizard.handle_key(key(KeyCode::Char('~')));
        assert!(wizard.model_browser_matches(0).is_empty());
        wizard.handle_key(key(KeyCode::Esc));
        assert_eq!(wizard.developers[0].model, selected);
        assert!(wizard.model_browser.is_none());
    }

    #[test]
    fn model_browser_supports_search_and_keeps_direct_model_typing() {
        let mut wizard = Wizard::startup(ModelCatalog::default(), Path::new("."));
        wizard.developers[0].provider = ProviderChoice::OpenAi;
        wizard.field = Field::Model;
        let wanted = wizard.catalog.models("openai")[1].id.clone();
        wizard.handle_key(key(KeyCode::Char('f')));
        wizard.handle_paste(&wanted);
        let matches = wizard.model_browser_matches(0);
        assert!(
            matches
                .iter()
                .any(|&index| { wizard.catalog.models("openai")[index].id == wanted })
        );
        wizard.handle_key(key(KeyCode::Enter));
        assert_eq!(wizard.developers[0].model, wanted);

        wizard.handle_key(key(KeyCode::Char('x')));
        wizard.handle_key(key(KeyCode::Char('f')));
        assert_eq!(wizard.developers[0].model, "xf");
        assert!(wizard.model_browser.is_none());
    }

    #[test]
    fn startup_logo_only_appears_when_there_is_room_above_builder() {
        let wizard = Wizard::startup(ModelCatalog::default(), Path::new("."));
        let tall = Rect::new(0, 0, 120, 60);
        let logo = wizard.logo_area(tall).expect("logo fits above form");
        assert!(logo.bottom() < wizard.shell(tall).y);
        assert!(wizard.logo_area(Rect::new(0, 0, 120, 36)).is_none());
    }

    #[test]
    fn workspace_picker_uses_one_frame_and_one_key_guide() {
        let directory = tempfile::tempdir().unwrap();
        let wizard = Wizard::workspace(directory.path());
        let mut terminal = Terminal::new(ratatui::backend::TestBackend::new(100, 32)).unwrap();
        terminal.draw(|frame| render(frame, &wizard)).unwrap();
        let buffer = terminal.backend().buffer();
        let rendered = (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>();
        assert!(
            rendered
                .iter()
                .any(|line| line.contains("Choose workspace"))
        );
        assert_eq!(rendered.join("").matches("Choose workspace").count(), 1);
        assert!(rendered.iter().any(|line| line.contains("[Enter] select")));
    }

    #[test]
    fn workspace_picker_popup_restores_its_rounded_border() {
        let directory = tempfile::tempdir().unwrap();
        let wizard = Wizard::workspace(directory.path());
        let area = Rect::new(0, 0, 100, 32);
        let shell = wizard.shell(area);
        let mut terminal = Terminal::new(ratatui::backend::TestBackend::new(100, 32)).unwrap();
        terminal.draw(|frame| render_popup(frame, &wizard)).unwrap();
        let buffer = terminal.backend().buffer();
        assert_eq!(buffer[(shell.x, shell.y)].symbol(), "╭");
        assert_eq!(buffer[(shell.right() - 1, shell.y)].symbol(), "╮");
        assert_eq!(buffer[(shell.x, shell.bottom() - 1)].symbol(), "╰");
        assert_eq!(buffer[(shell.x, shell.y)].fg, theme::BORDER);
    }

    #[test]
    fn startup_configures_only_team_lead() {
        let mut wizard = Wizard::default();
        wizard.handle_key(key(KeyCode::Enter));
        assert_eq!(wizard.step, Step::Developer(0));
        assert_eq!(wizard.developers.len(), 1);
        assert_eq!(wizard.developers[0].role, DeveloperRole::Lead);
    }

    #[test]
    fn first_run_uses_supplied_workspace_and_one_enter_to_launch() {
        let workspace = tempfile::tempdir().unwrap();
        let mut wizard = Wizard::startup(ModelCatalog::default(), workspace.path());
        assert_eq!(wizard.step, Step::Developer(0));
        assert_eq!(wizard.working_directory, workspace.path());
        assert_eq!(wizard.developers.len(), 1);
        wizard.handle_key(key(KeyCode::Enter));
        assert!(wizard.complete());
    }

    #[test]
    fn separate_workspace_picker_selects_and_cancels() {
        let workspace = tempfile::tempdir().unwrap();
        let mut picker = Wizard::workspace(workspace.path());
        assert_eq!(picker.step, Step::WorkspacePicker);
        picker.handle_key(key(KeyCode::Char(' ')));
        assert!(picker.complete());
        assert_eq!(picker.selected_workspace(), workspace.path());

        let mut picker = Wizard::workspace(workspace.path());
        picker.handle_key(key(KeyCode::Esc));
        assert!(picker.cancelled());
    }

    #[test]
    fn add_agent_defaults_to_selected_parent_and_custom_prompt_is_blank() {
        let mut startup = Wizard::default();
        startup.handle_key(key(KeyCode::Enter));
        let lead = startup.into_draft();
        let backend = Wizard::add_agent(ModelCatalog::default(), lead, 0).into_draft();
        let mut wizard = Wizard::add_agent(ModelCatalog::default(), backend, 1);
        assert_eq!(wizard.developers[2].reports_to, Some(1));
        assert_eq!(wizard.developers[2].role, DeveloperRole::Custom);
        assert!(wizard.developers[2].system_prompt.is_empty());
        assert_eq!(wizard.developers[2].name, "Agent 3");
        wizard.handle_key(key(KeyCode::Enter));
        assert!(wizard.complete());
    }

    #[test]
    fn reporting_parent_choice_excludes_descendants() {
        let mut startup = Wizard::default();
        startup.handle_key(key(KeyCode::Enter));
        let backend =
            Wizard::add_agent(ModelCatalog::default(), startup.into_draft(), 0).into_draft();
        let grandchild = Wizard::add_agent(ModelCatalog::default(), backend, 1).into_draft();
        let mut wizard = Wizard::configure_agent(ModelCatalog::default(), grandchild, 1);
        wizard.field = Field::ReportsTo;
        wizard.handle_key(key(KeyCode::Right));
        assert_eq!(wizard.developers[1].reports_to, Some(0));
        assert_ne!(wizard.developers[1].reports_to, Some(2));
    }

    #[test]
    fn team_lead_setup_has_no_reporting_parent_field() {
        let mut wizard = Wizard::default();
        wizard.handle_key(key(KeyCode::Enter));
        let mut terminal = Terminal::new(ratatui::backend::TestBackend::new(100, 36)).unwrap();
        terminal.draw(|frame| render(frame, &wizard)).unwrap();
        let buffer = terminal.backend().buffer();
        let rendered = (0..buffer.area.height)
            .flat_map(|y| (0..buffer.area.width).map(move |x| buffer[(x, y)].symbol()))
            .collect::<String>();
        assert!(!rendered.contains("Reports to"), "{rendered}");
        wizard.field = Field::Name;
        wizard.handle_key(key(KeyCode::Tab));
        assert_eq!(wizard.field, Field::Provider);
        wizard.handle_key(key(KeyCode::BackTab));
        assert_eq!(wizard.field, Field::Name);
        assert_eq!(
            developer_field_at_row(2, wizard.developers[0].provider, false, true, false),
            Some(Field::Provider)
        );

        let specialist = Wizard::add_agent(ModelCatalog::default(), wizard.into_draft(), 0);
        terminal.draw(|frame| render(frame, &specialist)).unwrap();
        let buffer = terminal.backend().buffer();
        let rendered = (0..buffer.area.height)
            .flat_map(|y| (0..buffer.area.width).map(move |x| buffer[(x, y)].symbol()))
            .collect::<String>();
        assert!(rendered.contains("Reports to"), "{rendered}");
    }

    #[test]
    fn larger_teams_only_use_the_four_builtin_roles() {
        let roles = (0..7)
            .map(|index| suggested_role(index, 7))
            .collect::<Vec<_>>();

        assert_eq!(
            roles,
            vec![
                DeveloperRole::Lead,
                DeveloperRole::Backend,
                DeveloperRole::Frontend,
                DeveloperRole::Reviewer,
                DeveloperRole::Backend,
                DeveloperRole::Frontend,
                DeveloperRole::Reviewer,
            ]
        );
    }

    #[test]
    fn role_presets_start_without_prompts() {
        assert_eq!(DeveloperRole::Lead.config_role(), "team_lead");
        assert_eq!(DeveloperRole::Backend.config_role(), "backend_agent");
        assert_eq!(DeveloperRole::Frontend.config_role(), "frontend_agent");
        assert_eq!(DeveloperRole::Reviewer.config_role(), "reviewer_agent");
        let mut wizard = Wizard::default();
        wizard.handle_key(key(KeyCode::Enter));
        assert!(
            wizard
                .developers
                .iter()
                .all(|developer| developer.system_prompt.is_empty())
        );
    }

    #[test]
    fn prompt_editor_starts_empty_and_saves_multiline_text() {
        let mut wizard = Wizard::default();
        wizard.handle_key(key(KeyCode::Enter));
        wizard.field = Field::Prompt;
        wizard.handle_key(key(KeyCode::Char('e')));
        assert_eq!(wizard.step, Step::Prompt(0));
        assert!(
            wizard
                .prompt_editor
                .as_ref()
                .unwrap()
                .lines()
                .join("\n")
                .is_empty()
        );
        wizard.handle_paste("Keep task ownership explicit.\nReport the checks you ran.");
        wizard.handle_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL));
        assert_eq!(wizard.step, Step::Developer(0));
        assert_eq!(
            wizard.developers[0].system_prompt,
            "Keep task ownership explicit.\nReport the checks you ran."
        );
    }

    #[test]
    fn text_fields_accept_shortcut_letters_without_entering_edit_mode() {
        let mut wizard = Wizard::startup(ModelCatalog::default(), Path::new("."));
        wizard.field = Field::Model;
        for character in "model-e-hjkl".chars() {
            wizard.handle_key(key(KeyCode::Char(character)));
        }
        assert_eq!(wizard.developers[0].model, "model-e-hjkl");
        assert_eq!(wizard.step, Step::Developer(0));
        wizard.handle_key(key(KeyCode::Char('e')));
        assert_eq!(wizard.step, Step::Developer(0));
        assert_eq!(wizard.developers[0].model, "model-e-hjkle");
        wizard.field = Field::Prompt;
        wizard.handle_key(key(KeyCode::Char('e')));
        assert_eq!(wizard.step, Step::Prompt(0));
    }

    #[test]
    fn e_edits_name_and_token_but_only_opens_the_selected_prompt() {
        let mut wizard = Wizard::startup(ModelCatalog::default(), Path::new("."));
        wizard.field = Field::Name;
        wizard.handle_key(key(KeyCode::Char('e')));
        assert!(wizard.developers[0].name.ends_with('e'));
        assert_eq!(wizard.step, Step::Developer(0));

        wizard.developers[0].provider = ProviderChoice::OpenAi;
        wizard.field = Field::Token;
        wizard.handle_key(key(KeyCode::Char('e')));
        assert_eq!(wizard.developers[0].api_token, "e");
        assert_eq!(wizard.step, Step::Developer(0));

        wizard.field = Field::Prompt;
        wizard.handle_key(key(KeyCode::Char('e')));
        assert_eq!(wizard.step, Step::Prompt(0));
    }

    #[test]
    fn hjkl_navigate_choices_but_type_directly_in_text_fields() {
        let mut wizard = Wizard::startup(ModelCatalog::default(), Path::new("."));
        wizard.handle_key(key(KeyCode::Down));
        assert_eq!(wizard.field, Field::Provider);
        let original = wizard.developers[0].provider;
        wizard.handle_key(key(KeyCode::Char('l')));
        assert_ne!(wizard.developers[0].provider, original);
        wizard.handle_key(key(KeyCode::Char('h')));
        assert_eq!(wizard.developers[0].provider, original);
        wizard.handle_key(key(KeyCode::Char('k')));
        assert_eq!(wizard.field, Field::Name);
        for character in "hjkl".chars() {
            wizard.handle_key(key(KeyCode::Char(character)));
        }
        assert!(wizard.developers[0].name.ends_with("hjkl"));
        assert_eq!(wizard.field, Field::Name);
    }

    #[test]
    fn prompt_editor_is_visible_and_prefilled_in_setup() {
        let mut wizard = Wizard::default();
        wizard.handle_key(key(KeyCode::Enter));
        wizard.field = Field::Prompt;
        wizard.handle_key(key(KeyCode::Char('e')));
        let mut terminal = Terminal::new(ratatui::backend::TestBackend::new(100, 36)).unwrap();
        terminal.draw(|frame| render(frame, &wizard)).unwrap();
        let buffer = terminal.backend().buffer();
        let rendered = (0..buffer.area.height)
            .flat_map(|y| (0..buffer.area.width).map(move |x| buffer[(x, y)].symbol()))
            .collect::<String>();
        assert!(rendered.contains("Agent prompt · Team Lead"), "{rendered}");
        assert!(rendered.contains("Ctrl+S"), "{rendered}");
        assert!(!rendered.contains("You are voff's Team Lead"), "{rendered}");
    }

    #[test]
    fn setup_previews_the_prompt_and_enter_does_not_open_its_editor() {
        let mut wizard = Wizard::default();
        wizard.handle_key(key(KeyCode::Enter));
        wizard.field = Field::Prompt;
        let mut terminal = Terminal::new(ratatui::backend::TestBackend::new(110, 36)).unwrap();
        terminal.draw(|frame| render(frame, &wizard)).unwrap();
        let buffer = terminal.backend().buffer();
        let rendered = (0..buffer.area.height)
            .flat_map(|y| (0..buffer.area.width).map(move |x| buffer[(x, y)].symbol()))
            .collect::<String>();
        assert!(rendered.contains("Setup prompt"), "{rendered}");
        assert!(
            rendered.contains("The setup prompt will be generated"),
            "{rendered}"
        );
        assert!(rendered.contains("Agent prompt · [e] edit"), "{rendered}");
        assert!(!rendered.contains("[e] edit full prompt"), "{rendered}");

        wizard.handle_key(key(KeyCode::Enter));
        assert!(wizard.complete());
        assert!(wizard.prompt_editor.is_none());
    }

    #[test]
    fn cloud_prompt_editor_is_accessible_in_a_narrow_setup() {
        let mut initial = Wizard::default();
        initial.handle_key(key(KeyCode::Enter));
        let mut wizard = Wizard::edit(ModelCatalog::default(), initial.into_draft());
        wizard.handle_key(key(KeyCode::Enter));
        wizard.developers[0].provider = ProviderChoice::OpenAi;
        let area = Rect::new(0, 0, 80, 24);
        assert!(setup_rows(wizard.shell(area), wizard.step, wizard.mode)[1].height >= 16);
        let mut terminal = Terminal::new(ratatui::backend::TestBackend::new(80, 24)).unwrap();
        terminal.draw(|frame| render(frame, &wizard)).unwrap();
        let buffer = terminal.backend().buffer();
        let rendered = (0..buffer.area.height)
            .flat_map(|y| (0..buffer.area.width).map(move |x| buffer[(x, y)].symbol()))
            .collect::<String>();
        assert!(rendered.contains("API token"), "{rendered}");
        assert!(rendered.contains("Agent prompt"), "{rendered}");
        wizard.field = Field::Prompt;
        wizard.handle_key(key(KeyCode::Char('e')));
        assert_eq!(wizard.step, Step::Prompt(0));
    }

    #[test]
    fn prompt_editor_allows_empty_prompt_and_discard_preserves_previous_prompt() {
        let mut wizard = Wizard::default();
        wizard.handle_key(key(KeyCode::Enter));
        let original = wizard.developers[0].system_prompt.clone();
        wizard.field = Field::Prompt;
        wizard.open_prompt_editor(0);
        wizard.handle_paste("unsaved text");
        wizard.handle_key(key(KeyCode::Esc));
        assert_eq!(wizard.developers[0].system_prompt, original);

        wizard.open_prompt_editor(0);
        wizard.prompt_editor = Some(TextArea::default());
        wizard.handle_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL));
        assert_eq!(wizard.step, Step::Developer(0));
        assert!(wizard.developers[0].system_prompt.is_empty());
    }

    #[test]
    fn changing_role_keeps_the_user_prompt_unchanged() {
        let mut wizard = Wizard::default();
        wizard.handle_key(key(KeyCode::Enter));
        let draft = wizard.into_draft();
        let mut wizard = Wizard::add_agent(ModelCatalog::default(), draft, 0);
        assert_eq!(wizard.step, Step::Developer(1));
        wizard.developers[1].system_prompt = "Previous role text".into();
        wizard.field = Field::Role;
        wizard.handle_key(key(KeyCode::Right));
        assert_eq!(wizard.developers[1].role, DeveloperRole::Backend);
        assert_eq!(wizard.developers[1].system_prompt, "Previous role text");
    }

    #[test]
    fn setup_keeps_exactly_one_required_team_lead() {
        let mut wizard = Wizard::default();
        wizard.handle_key(key(KeyCode::Enter));

        wizard.field = Field::Role;
        wizard.handle_key(key(KeyCode::Right));
        assert_eq!(wizard.developers[0].role, DeveloperRole::Lead);

        let draft = wizard.into_draft();
        let mut wizard = Wizard::add_agent(ModelCatalog::default(), draft, 0);
        wizard.field = Field::Role;
        for _ in 0..12 {
            wizard.handle_key(key(KeyCode::Right));
            assert_ne!(wizard.developers[1].role, DeveloperRole::Lead);
        }
    }

    #[test]
    fn solo_setup_still_requires_a_team_lead() {
        let mut wizard = Wizard::default();

        wizard.handle_key(key(KeyCode::Enter));

        assert_eq!(wizard.developers.len(), 1);
        assert_eq!(wizard.developers[0].role, DeveloperRole::Lead);
    }

    #[test]
    fn each_developer_keeps_an_independent_model() {
        let catalog = ModelCatalog::default();
        let first = catalog.models("openai")[0].id.clone();
        let second = catalog.models("openai")[1].id.clone();
        let mut wizard = Wizard::new(catalog.clone(), false);
        wizard.handle_key(key(KeyCode::Enter));
        for developer in &mut wizard.developers {
            developer.provider = ProviderChoice::OpenAi;
            developer.model = first.clone();
        }
        wizard.developers[0].model = second.clone();
        let draft = wizard.into_draft();
        let mut wizard = Wizard::add_agent(catalog, draft, 0);
        wizard.developers[1].provider = ProviderChoice::OpenAi;
        wizard.developers[1].model = first;
        wizard.field = Field::Model;
        wizard.handle_key(key(KeyCode::Right));
        assert_eq!(wizard.developers[0].model, second);
        assert_eq!(wizard.developers[1].model, second);
    }

    #[test]
    fn changing_provider_updates_an_untouched_default_model() {
        let mut wizard = Wizard::default();
        wizard.handle_key(key(KeyCode::Enter));
        wizard.developers[0].provider = ProviderChoice::Ollama;
        wizard.field = Field::Provider;
        wizard.handle_key(key(KeyCode::Right));
        assert_eq!(wizard.developers[0].provider, ProviderChoice::OpenAi);
        assert_eq!(
            wizard.developers[0].model,
            wizard.catalog.first_model("openai")
        );
    }

    #[test]
    fn unavailable_ollama_keeps_a_custom_model_fallback() {
        let mut wizard = Wizard::default();
        wizard.handle_key(key(KeyCode::Enter));
        wizard.developers[0].provider = ProviderChoice::Ollama;
        wizard.developers[0].model = "qwen3:4b".into();
        wizard.field = Field::Model;
        for value in "local-code".chars() {
            wizard.handle_key(key(KeyCode::Char(value)));
        }

        assert_eq!(wizard.developers[0].model, "local-code");
    }

    #[test]
    fn enter_accepts_all_suggested_developers_without_extra_fields() {
        let mut wizard = Wizard::default();
        wizard.handle_key(key(KeyCode::Enter));
        for _ in 0..wizard.developers.len() {
            wizard.handle_key(key(KeyCode::Enter));
        }
        assert!(wizard.complete());
    }

    #[test]
    fn installed_cli_is_the_enter_only_default() {
        assert_eq!(
            ProviderChoice::from_config_name("codex_cli").unwrap(),
            ProviderChoice::CodexCli
        );
        assert_eq!(
            ProviderChoice::from_config_name("claude").unwrap(),
            ProviderChoice::ClaudeCli
        );
    }

    #[test]
    fn bundled_logo_frames_keep_black_pixels_and_canvas_width() {
        const FRAMES: [(&[u8], &[u8]); 4] = [
            (
                include_bytes!("../assets/voff-logo.ansi"),
                include_bytes!("../assets/logo-blink/open.rgba"),
            ),
            (
                include_bytes!("../assets/logo-blink/half-open-block.ansi"),
                include_bytes!("../assets/logo-blink/half-open.rgba"),
            ),
            (
                include_bytes!("../assets/logo-blink/nearly-closed-block.ansi"),
                include_bytes!("../assets/logo-blink/nearly-closed.rgba"),
            ),
            (
                include_bytes!("../assets/logo-blink/closed-block.ansi"),
                include_bytes!("../assets/logo-blink/closed.rgba"),
            ),
        ];

        for (frame, cell_colors) in FRAMES {
            let logo = logo_text_from_bytes(frame, cell_colors, theme::SURFACE)
                .expect("bundled ANSI frame should parse");
            assert!(logo.lines.len() <= 22);
            assert!(logo.lines.iter().all(|line| line.width() == 40));
            assert!(
                logo.lines
                    .iter()
                    .flat_map(|line| &line.spans)
                    .any(|span| span.style.fg == Some(Color::Rgb(0, 0, 0))),
                "black foreground pixels must remain visible"
            );
            assert!(
                logo.lines.iter().flat_map(|line| &line.spans).any(|span| {
                    span.content.chars().any(char::is_whitespace)
                        && span.style.bg.is_some_and(|color| {
                            matches!(color, Color::Rgb(red, green, blue) if red > 0 || green > 0 || blue > 0)
                        })
                }),
                "opaque blank cells must be filled from the source image"
            );
        }
    }

    #[test]
    fn welcome_screen_has_a_clear_start_action() {
        let logo = logo_text().expect("bundled ANSI logo must be parseable");
        assert!(logo.lines.len() <= 22, "logo must fit a 24-row terminal");
        assert!(
            logo.lines.iter().all(|line| line.width() == 40),
            "logo widths: {:?}",
            logo.lines.iter().map(Line::width).collect::<Vec<_>>()
        );
        assert!(
            logo.lines
                .iter()
                .all(|line| line.spans.iter().any(|span| span
                    .content
                    .chars()
                    .any(|character| !character.is_whitespace()))),
            "only surrounding blank rows may be removed"
        );
        assert!(
            logo.lines
                .iter()
                .flat_map(|line| &line.spans)
                .any(|span| span.style.bg.is_some()),
            "logo must retain its original shaded blocks"
        );
        assert!(
            logo.lines
                .iter()
                .flat_map(|line| &line.spans)
                .any(|span| span.style.fg == Some(Color::Rgb(0, 0, 0))),
            "black outline pixels must not be made transparent"
        );
        assert!(
            logo.lines
                .iter()
                .flat_map(|line| &line.spans)
                .any(|span| matches!(span.style.fg, Some(Color::Rgb(_, _, _))))
        );
        let mut terminal = Terminal::new(ratatui::backend::TestBackend::new(80, 24)).unwrap();
        terminal
            .draw(|frame| {
                let provider = ProviderChoice::OpenAi;
                let catalog = ModelCatalog::default();
                let model = catalog.first_model(provider.id());
                render_startup_splash(
                    frame,
                    StartupView {
                        workspace: Path::new("."),
                        show_setup: true,
                        selected_provider: provider,
                        model: &model,
                        model_picker_available: catalog.models(provider.id()).len() > 1,
                        api_token: "",
                        focused: StartupField::Provider,
                        description: "",
                        error: None,
                    },
                )
            })
            .unwrap();
        let buffer = terminal.backend().buffer();
        let rendered = (0..buffer.area.height)
            .flat_map(|y| (0..buffer.area.width).map(move |x| buffer[(x, y)].symbol()))
            .collect::<String>();
        assert!(rendered.contains("Provider"), "{rendered}");
        assert!(rendered.contains("‹"), "{rendered}");
        assert!(rendered.contains("scan"), "{rendered}");
        assert!(
            rendered.contains("Leave empty to scan the workspace"),
            "{rendered}"
        );
        assert!(rendered.contains("Model"), "{rendered}");
        assert!(rendered.contains("Press Enter to voff"), "{rendered}");
        assert!(!rendered.contains("not detected"), "{rendered}");
    }

    #[test]
    fn lead_setup_uses_a_wide_short_form_and_styled_start_action() {
        let wizard = Wizard::startup(ModelCatalog::default(), Path::new("."));
        let area = Rect::new(0, 0, 120, 36);
        let shell = wizard.shell(area);
        assert_eq!(shell.width, 120);
        assert_eq!(shell.height, 27);
        let mut terminal = Terminal::new(ratatui::backend::TestBackend::new(120, 36)).unwrap();
        terminal.draw(|frame| render(frame, &wizard)).unwrap();
        let buffer = terminal.backend().buffer();
        let row_text = |y| {
            (0..120)
                .map(|x| buffer[(x, y)].symbol())
                .collect::<String>()
        };
        let all_text = (0..36).map(row_text).collect::<Vec<_>>().join("\n");
        assert!(!all_text.contains("voff team builder"));
        assert!(all_text.contains("Configure your lead agent"));
        assert!(!all_text.contains("Type      Team Lead"));
        assert!(!all_text.contains('▏'), "{all_text}");
        let footer_y = shell.bottom() - 2;
        let footer = row_text(footer_y);
        let start_x = footer.find("Press Enter to voff").unwrap() as u16;
        assert_eq!(buffer[(start_x + 15, footer_y)].fg, theme::ORANGE);
        assert!(
            buffer[(start_x + 15, footer_y)]
                .modifier
                .contains(Modifier::ITALIC)
        );
        assert!(start_x > shell.x + shell.width / 4);
    }

    #[test]
    fn setup_fields_and_continue_are_clickable() {
        let mut wizard = Wizard::default();
        let area = Rect::new(0, 0, 100, 30);
        let shell = setup_shell(area, Step::Basics);
        let fields = centered(
            setup_rows(shell, Step::Basics, WizardMode::Startup)[1],
            72,
            1,
        );
        let click = |row| {
            Event::Mouse(MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: fields.x + 4,
                row,
                modifiers: KeyModifiers::NONE,
            })
        };
        wizard.handle_event(&click(fields.y), area);
        assert_eq!(wizard.step, Step::WorkspacePicker);
        wizard.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert_eq!(wizard.step, Step::Basics);
        assert_eq!(wizard.basics_field, BasicsField::WorkingDirectory);
        wizard.handle_event(&click(shell.bottom() - 1), area);
        assert_eq!(wizard.step, Step::Developer(0));

        let content = setup_rows(setup_shell(area, wizard.step), wizard.step, wizard.mode)[1];
        let provider_row = content.y + 1 + 2;
        let before = wizard.developers[0].provider;
        wizard.handle_event(&click(provider_row), area);
        assert_ne!(wizard.developers[0].provider, before);
    }

    #[test]
    fn reopened_team_builder_keeps_a_margin_and_clickable_footer() {
        let mut initial = Wizard::default();
        initial.handle_key(key(KeyCode::Char('c')));
        let mut wizard = Wizard::edit(ModelCatalog::default(), initial.into_draft());
        let area = Rect::new(0, 0, 80, 24);
        let shell = wizard.shell(area);
        assert!(shell.x > area.x && shell.y > area.y + 1);
        assert!(shell.right() < area.right() && shell.bottom() < area.bottom());
        wizard.handle_event(
            &Event::Mouse(MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: shell.x + 4,
                row: shell.bottom() - 1,
                modifiers: KeyModifiers::NONE,
            }),
            area,
        );
        assert_eq!(wizard.step, Step::Developer(0));
    }

    #[test]
    fn cloud_tokens_are_per_role_masked_and_debug_redacted() {
        let mut wizard = Wizard::default();
        wizard.handle_key(key(KeyCode::Enter));
        wizard.developers[0].system_prompt = "private prompt marker".into();
        assert!(!format!("{wizard:?}").contains("private prompt marker"));
        let draft = wizard.into_draft();
        let mut wizard = Wizard::add_agent(ModelCatalog::default(), draft, 0);
        for index in 0..2 {
            wizard.step = Step::Developer(index);
            wizard.developers[index].provider = ProviderChoice::OpenAi;
            wizard.field = Field::Token;
            wizard.handle_paste(&format!("sk-role-{index}"));
            let mut terminal = Terminal::new(ratatui::backend::TestBackend::new(100, 32)).unwrap();
            terminal.draw(|frame| render(frame, &wizard)).unwrap();
            let buffer = terminal.backend().buffer();
            let rendered = (0..buffer.area.height)
                .flat_map(|y| (0..buffer.area.width).map(move |x| buffer[(x, y)].symbol()))
                .collect::<String>();
            assert!(rendered.contains("API token"));
            assert!(rendered.contains('•'));
            assert!(!rendered.contains("sk-role"));
            assert!(!format!("{wizard:?}").contains("sk-role"));
        }
        assert_eq!(wizard.developers[0].api_token, "sk-role-0");
        assert_eq!(wizard.developers[1].api_token, "sk-role-1");
    }

    #[test]
    fn local_providers_skip_token_field_and_discard_cloud_draft() {
        let mut wizard = Wizard::default();
        wizard.handle_key(key(KeyCode::Enter));
        wizard.developers[0].provider = ProviderChoice::Ollama;
        wizard.field = Field::Model;
        wizard.handle_key(key(KeyCode::Tab));
        assert_eq!(wizard.field, Field::Name);
        wizard.field = Field::Provider;
        wizard.handle_key(key(KeyCode::Right));
        wizard.field = Field::Token;
        wizard.handle_paste("secret");
        wizard.field = Field::Provider;
        wizard.handle_key(key(KeyCode::Right));
        wizard.handle_key(key(KeyCode::Right));
        assert_eq!(wizard.developers[0].provider, ProviderChoice::CodexCli);
        assert!(wizard.developers[0].api_token.is_empty());
        wizard.field = Field::Model;
        wizard.handle_key(key(KeyCode::Tab));
        assert_eq!(wizard.field, Field::Name);
    }

    #[test]
    fn common_editor_preserves_current_roles_and_continue_works_from_workspace() {
        let mut initial = Wizard::default();
        initial.handle_key(key(KeyCode::Enter));
        initial.developers[0].provider = ProviderChoice::ClaudeCli;
        initial.developers[0].model = "custom-model".into();
        let draft = initial.into_draft();
        let mut wizard = Wizard::edit(ModelCatalog::default(), draft.clone());
        assert_eq!(wizard.step, Step::Basics);
        wizard.basics_field = BasicsField::WorkingDirectory;
        wizard.handle_key(key(KeyCode::Enter));
        assert_eq!(wizard.step, Step::Developer(0));
        assert_eq!(wizard.developers, draft.developers);
    }

    #[test]
    fn startup_basics_only_has_workspace() {
        let mut wizard = Wizard::default();
        assert_eq!(wizard.basics_field, BasicsField::WorkingDirectory);

        wizard.handle_key(key(KeyCode::Tab));
        assert_eq!(wizard.basics_field, BasicsField::WorkingDirectory);
        wizard.handle_key(key(KeyCode::Tab));
        assert_eq!(wizard.basics_field, BasicsField::WorkingDirectory);
    }

    #[test]
    fn workspace_picker_enter_selects_the_highlighted_directory() {
        let directory = tempfile::tempdir().unwrap();
        let child = directory.path().join("calculator");
        std::fs::create_dir(&child).unwrap();
        let mut wizard = Wizard {
            basics_field: BasicsField::WorkingDirectory,
            working_directory: directory.path().into(),
            ..Wizard::default()
        };

        wizard.handle_key(key(KeyCode::Char('c')));
        let expected = directory.path().canonicalize().unwrap();
        assert_eq!(wizard.step, Step::WorkspacePicker);
        assert_eq!(wizard.workspace_explorer.as_ref().unwrap().cwd(), &expected);

        wizard.handle_key(key(KeyCode::Down));
        wizard.handle_key(key(KeyCode::Enter));

        assert_eq!(wizard.step, Step::Basics);
        assert_eq!(wizard.working_directory, child.canonicalize().unwrap());
        assert!(wizard.workspace_explorer.is_none());
    }

    #[test]
    fn workspace_picker_space_selects_the_displayed_directory() {
        let directory = tempfile::tempdir().unwrap();
        let mut wizard = Wizard {
            basics_field: BasicsField::WorkingDirectory,
            working_directory: directory.path().into(),
            ..Wizard::default()
        };

        wizard.handle_key(key(KeyCode::Enter));
        wizard.handle_key(key(KeyCode::Char(' ')));

        assert_eq!(
            wizard.working_directory,
            directory.path().canonicalize().unwrap()
        );
    }

    #[test]
    fn workspace_picker_filters_directories_as_the_user_types() {
        let directory = tempfile::tempdir().unwrap();
        let backend = directory.path().join("backend");
        let frontend = directory.path().join("frontend");
        std::fs::create_dir(&backend).unwrap();
        std::fs::create_dir(&frontend).unwrap();
        std::fs::write(directory.path().join("frontend.txt"), "not a directory").unwrap();
        let mut wizard = Wizard {
            basics_field: BasicsField::WorkingDirectory,
            working_directory: directory.path().into(),
            ..Wizard::default()
        };

        wizard.handle_key(key(KeyCode::Char('c')));
        wizard.handle_key(key(KeyCode::Char('/')));
        for value in "front".chars() {
            wizard.handle_key(key(KeyCode::Char(value)));
        }

        let explorer = wizard.workspace_explorer.as_ref().unwrap();
        assert!(wizard.workspace_filtering);
        assert_eq!(wizard.workspace_filter, "front");
        assert_eq!(
            explorer
                .files()
                .iter()
                .map(|file| file.name.as_str())
                .collect::<Vec<_>>(),
            vec!["../", "frontend/"]
        );

        wizard.handle_key(key(KeyCode::Down));
        wizard.handle_key(key(KeyCode::Enter));

        assert_eq!(wizard.step, Step::Basics);
        assert_eq!(wizard.working_directory, frontend.canonicalize().unwrap());
    }

    #[test]
    fn workspace_picker_supports_hjkl_navigation() {
        let directory = tempfile::tempdir().unwrap();
        let child = directory.path().join("child");
        std::fs::create_dir(&child).unwrap();
        let mut wizard = Wizard {
            basics_field: BasicsField::WorkingDirectory,
            working_directory: directory.path().into(),
            ..Wizard::default()
        };

        wizard.handle_key(key(KeyCode::Char('c')));
        wizard.handle_key(key(KeyCode::Char('j')));
        wizard.handle_key(key(KeyCode::Char('l')));
        wizard.handle_key(key(KeyCode::Char(' ')));

        assert_eq!(wizard.working_directory, child.canonicalize().unwrap());
    }

    #[test]
    fn slash_filter_mode_accepts_hjkl_as_query_text() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::create_dir(directory.path().join("hello")).unwrap();
        let mut wizard = Wizard {
            basics_field: BasicsField::WorkingDirectory,
            working_directory: directory.path().into(),
            ..Wizard::default()
        };

        wizard.handle_key(key(KeyCode::Char('c')));
        wizard.handle_key(key(KeyCode::Char('/')));
        for value in "hell".chars() {
            wizard.handle_key(key(KeyCode::Char(value)));
        }

        assert_eq!(wizard.workspace_filter, "hell");
        assert!(wizard.workspace_filtering);
    }

    #[test]
    fn missing_working_directory_does_not_advance() {
        let mut wizard = Wizard {
            basics_field: BasicsField::WorkingDirectory,
            working_directory: "/definitely/missing/voff-workspace".into(),
            ..Wizard::default()
        };

        wizard.handle_key(key(KeyCode::Enter));

        assert_eq!(wizard.step, Step::Basics);
        assert!(
            wizard
                .error
                .as_deref()
                .is_some_and(|error| error.starts_with("Choose an existing working directory."))
        );
    }
}
