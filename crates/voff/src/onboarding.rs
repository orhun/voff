//! Interactive workspace setup and team builder.

use crate::model_catalog::ModelCatalog;
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
    backend::CrosstermBackend,
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, BorderType, Borders, Clear, FrameExt as _, Paragraph, Wrap},
};
use ratatui_explorer::{FileExplorer, FileExplorerBuilder, Theme};
use ratatui_textarea::{TextArea, WrapMode};
use serde::{Deserialize, Serialize};
use std::{
    io, panic,
    path::{Path, PathBuf},
    time::Duration,
};
use voff_core::MAX_OUTPUT_TOKENS;

const DEFAULT_OUTPUT_TOKENS: u64 = voff_core::DEFAULT_MAX_OUTPUT_TOKENS;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum ProviderChoice {
    Ollama,
    OpenAi,
    CodexCli,
    ClaudeCli,
}

impl ProviderChoice {
    const ALL: [Self; 4] = [Self::Ollama, Self::OpenAi, Self::CodexCli, Self::ClaudeCli];

    #[must_use]
    pub const fn id(self) -> &'static str {
        match self {
            Self::Ollama => "ollama",
            Self::OpenAi => "openai",
            Self::CodexCli => "codex_cli",
            Self::ClaudeCli => "claude_cli",
        }
    }

    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Ollama => "Ollama · local",
            Self::OpenAi => "OpenAI · cloud",
            Self::CodexCli => "Codex · CLI",
            Self::ClaudeCli => "Claude · CLI",
        }
    }

    fn cycle(self, delta: isize) -> Self {
        cycle(Self::ALL, self, delta)
    }
}

fn preferred_provider() -> ProviderChoice {
    let installed = |binary: &str| {
        std::env::var_os("PATH").is_some_and(|paths| {
            std::env::split_paths(&paths).any(|dir| dir.join(binary).is_file())
        })
    };
    choose_preferred_provider(installed)
}

fn choose_preferred_provider(installed: impl Fn(&str) -> bool) -> ProviderChoice {
    if installed("codex") {
        ProviderChoice::CodexCli
    } else if installed("claude") {
        ProviderChoice::ClaudeCli
    } else if installed("ollama") {
        ProviderChoice::Ollama
    } else {
        ProviderChoice::OpenAi
    }
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
    pub const fn prompt(self) -> &'static str {
        match self {
            Self::Lead => {
                "You are voff's Team Lead. Own coordination, integration, and the final answer.\n\n- Classify the request. Answer simple questions yourself; do not delegate work that needs no specialist.\n- Before implementation, read relevant instructions and code. Create bounded tasks with dependencies, scope, and acceptance criteria.\n- Delegate only to your direct reports. Give each an objective, editable files, integration contract, required checks, and return format; they may delegate to their own direct reports under the same rules.\n- Parallelize only independent work; never assign simultaneous writers to the same file. Route communication through the reporting tree.\n- Review each direct report's result, reconcile interfaces, run approved checks, and give precise feedback for revisions. Do not claim success when tests or acceptance criteria fail.\n- Respect runtime approvals and permissions. Surface blockers and residual risks to the user. Report the integrated result with evidence."
            }
            Self::Frontend => {
                "You are the Frontend Agent. Complete the task assigned by your reporting agent.\n\n- Read the requirements and existing UI patterns. Stay within your file and permission scope.\n- Follow the agreed API and data contract; return missing or inconsistent details to your reporting agent.\n- Build accessible, responsive behavior with loading, empty, validation, and error states.\n- If you have direct reports, delegate bounded subtasks only to them, review their results, and integrate their work.\n- Run focused approved checks. Return changed files, behavior, evidence, assumptions, and blockers to your reporting agent. Never claim unverified work is complete."
            }
            Self::Backend => {
                "You are the Backend Agent. Complete the task assigned by your reporting agent.\n\n- Read requirements and existing backend patterns. Stay within your file and permission scope.\n- Define or follow the shared API contract: routes or functions, request and response shapes, validation, errors, and ownership boundaries. Return contract questions to your reporting agent.\n- Implement reliable behavior with bounded resources and success and failure-path tests. Preserve existing data and user changes.\n- If you have direct reports, delegate bounded subtasks only to them, review their results, and integrate their work.\n- Run focused approved checks. Return changed files, contract details, evidence, and blockers to your reporting agent."
            }
            Self::Reviewer => {
                "You are the Reviewer Agent. Review work assigned by your reporting agent independently.\n\n- Compare the result with the user goal, acceptance criteria, instructions, and shared contracts.\n- Inspect changes and relevant tests. With approval, run checks that expose integration gaps and failure paths.\n- Report blocking findings with severity, file reference, evidence, and a concrete fix.\n- If you have direct reports, delegate bounded review subtasks only to them and integrate their findings.\n- Return findings, checks, and remaining uncertainty to your reporting agent. Do not edit files unless explicitly assigned that scope."
            }
            Self::Custom => "",
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
            system_prompt: role.prompt().into(),
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
            system_prompt: role.prompt().into(),
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
    Prompt(usize),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Field {
    Role,
    Name,
    ReportsTo,
    Provider,
    Model,
    Token,
    OutputLimit,
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
            Self::OutputLimit => Self::Model,
            Self::Token => Self::OutputLimit,
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
    pub(crate) fn edit(catalog: ModelCatalog, mut draft: SetupDraft) -> Self {
        for developer in &mut draft.developers {
            if developer.system_prompt.trim().is_empty() {
                developer.system_prompt = developer.role.prompt().into();
            }
        }
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
        let provider = preferred_provider();
        let model = wizard.catalog.first_model(provider.id());
        let mut agent =
            DeveloperDraft::suggested(index, index + 1, provider, model, DEFAULT_OUTPUT_TOKENS);
        agent.role = DeveloperRole::Backend;
        agent.name = unique_name(agent.role.label(), &wizard.developers, None);
        agent.system_prompt = agent.role.prompt().into();
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
        if wizard.step == Step::Developer(0) {
            wizard.field = Field::Name;
        }
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
        let available = if self.editing {
            area.inner(ratatui::layout::Margin::new(
                if area.width >= 12 { 2 } else { 0 },
                if area.height >= 16 { 2 } else { 0 },
            ))
        } else {
            area
        };
        let shell = setup_shell(available, self.step);
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
        if self.editing || self.mode != WizardMode::Startup || self.step != Step::Developer(0) {
            return None;
        }
        let logo = logo_text()?;
        let width = u16::try_from(logo.lines.iter().map(Line::width).max().unwrap_or(0)).ok()?;
        let height = u16::try_from(logo.lines.len()).ok()?;
        let shell = setup_shell(area, self.step);
        let total = height.saturating_add(1).saturating_add(shell.height);
        if width > area.width || total.saturating_add(2) > area.height {
            return None;
        }
        Some(Rect::new(
            area.x + area.width.saturating_sub(width) / 2,
            area.y + area.height.saturating_sub(total) / 2,
            width,
            height,
        ))
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
            let key = if matches!(self.step, Step::Prompt(_)) {
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
                if cols[1].contains((mouse.column, mouse.row).into()) {
                    if matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left)) {
                        self.field = Field::Prompt;
                        self.open_prompt_editor(index);
                    }
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
                );
                if let Some(field) = field {
                    self.field = field;
                    if field == Field::Model {
                        self.model_touched = false;
                    }
                    match mouse.kind {
                        MouseEventKind::Down(MouseButton::Left) => {
                            if field == Field::Prompt {
                                self.open_prompt_editor(index);
                            } else if field == Field::Model {
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
                if let Some(editor) = &mut self.prompt_editor {
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
                    self.limit_touched = false;
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
            .with_block(Block::default())
            .with_highlight_dir_style(
                Style::default()
                    .fg(theme::AQUA)
                    .add_modifier(Modifier::BOLD),
            )
            .with_highlight_symbol("› ");
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
            KeyCode::Char('e')
                if self.field == Field::Prompt
                    && !key
                        .modifiers
                        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                self.open_prompt_editor(index);
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
                    Field::Role
                    | Field::ReportsTo
                    | Field::Provider
                    | Field::OutputLimit
                    | Field::Prompt => {}
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
                } else if developer.max_output_tokens == 0 {
                    self.error = Some("Output limit must be at least 1 token.".into());
                } else if developer.role != DeveloperRole::Custom
                    && developer.system_prompt.trim().is_empty()
                {
                    self.error = Some("System prompt cannot be empty.".into());
                } else if self.mode != WizardMode::Startup {
                    self.complete = true;
                } else if index + 1 < self.developers.len() {
                    self.step = Step::Developer(index + 1);
                    self.field = Field::Role;
                    self.limit_touched = false;
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
        editor.set_wrap_mode(WrapMode::WordOrGlyph);
        editor.set_style(theme::canvas());
        editor.set_block(
            Block::default()
                .title(format!(" System prompt · {} ", self.developers[index].name))
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
            if prompt.is_empty() && self.developers[index].role != DeveloperRole::Custom {
                self.error = Some("System prompt cannot be empty.".into());
            } else if prompt.len() > 16_000 {
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
            if !((index == 0 && matches!(self.field, Field::Role | Field::ReportsTo))
                || (self.field == Field::Token
                    && self.developers[index].provider != ProviderChoice::OpenAi))
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
        if let Step::Prompt(_) = self.step {
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
            && self.developers[index].provider == ProviderChoice::OpenAi
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
                self.developers[index].system_prompt = role.prompt().into();
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
    for suffix in 2..=32 {
        let candidate = format!("{base} {suffix}");
        if !developers
            .iter()
            .enumerate()
            .any(|(index, agent)| Some(index) != exclude && agent.name == candidate)
        {
            return candidate;
        }
    }
    format!("{base} 33")
}

/// Run workspace setup. `None` means the user cancelled without writing.
pub fn run(
    catalog: ModelCatalog,
    credential_available: bool,
    initial_workspace: &Path,
) -> io::Result<Option<SetupDraft>> {
    let _guard = TerminalGuard::enter()?;
    let mut terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;
    let _ = credential_available;
    let mut wizard = Wizard::startup(catalog, initial_workspace);
    terminal.draw(render_intro)?;
    loop {
        match event::read()? {
            Event::Key(key) if key.kind == KeyEventKind::Press && key.code == KeyCode::Esc => {
                return Ok(None);
            }
            event if starts_setup(&event) => break,
            _ => {}
        }
    }
    while !wizard.cancelled && !wizard.complete {
        terminal.draw(|frame| render(frame, &wizard))?;
        if event::poll(Duration::from_millis(100))? {
            wizard.handle_event(&event::read()?, terminal.size()?.into());
        }
    }
    if wizard.complete {
        Ok(Some(SetupDraft {
            developers: wizard.developers,
            working_directory: wizard.working_directory,
        }))
    } else {
        Ok(None)
    }
}

fn starts_setup(event: &Event) -> bool {
    matches!(event, Event::Key(key) if key.kind == KeyEventKind::Press && key.code == KeyCode::Enter)
        || matches!(event, Event::Mouse(mouse) if matches!(mouse.kind, MouseEventKind::Down(_)))
}

fn render_intro(frame: &mut ratatui::Frame<'_>) {
    let area = frame.area();
    let splash_style = theme::canvas();
    frame.render_widget(Block::default().style(splash_style), area);
    let logo = logo_text();
    let logo_height = logo.as_ref().map_or(0, |text| text.lines.len() as u16);
    let logo_width = logo.as_ref().map_or(0, |text| {
        text.lines.iter().map(Line::width).max().unwrap_or(0) as u16
    });
    if let Some(logo) = logo.filter(|_| area.width >= logo_width && area.height >= logo_height + 2)
    {
        let caption_height = if area.height >= logo_height + 3 { 3 } else { 2 };
        let box_area = centered(area, logo_width, logo_height + caption_height);
        frame.render_widget(
            Paragraph::new(logo),
            Rect::new(box_area.x, box_area.y, box_area.width, logo_height),
        );
        frame.render_widget(
            Paragraph::new(splash_caption(caption_height == 3))
                .alignment(Alignment::Center)
                .style(splash_style),
            Rect::new(area.x, box_area.y + logo_height, area.width, caption_height),
        );
    } else {
        let box_area = centered(area, area.width.min(54), 3);
        frame.render_widget(
            Paragraph::new(splash_caption(true))
                .alignment(Alignment::Center)
                .wrap(Wrap { trim: true })
                .style(splash_style),
            box_area,
        );
    }
}

fn splash_caption(spaced: bool) -> Text<'static> {
    let mut lines = vec![Line::from(vec![
        Span::raw("Welcome to "),
        Span::styled("voff", Style::default().fg(theme::ORANGE)),
        Span::raw("!"),
    ])];
    if spaced {
        lines.push(Line::default());
    }
    lines.push(Line::from(Span::styled(
        "Enter/click to set up",
        Style::default().add_modifier(Modifier::ITALIC),
    )));
    Text::from(lines)
}

fn logo_text() -> Option<Text<'static>> {
    logo_text_for(theme::BACKGROUND)
}

pub(crate) fn logo_text_for(background: Color) -> Option<Text<'static>> {
    let bytes: &'static [u8] = include_bytes!("../assets/voff-logo.ansi");
    let bytes = bytes.strip_prefix(b"\x1b[?25l").unwrap_or(bytes);
    let bytes = bytes.strip_suffix(b"\x1b[?25h").unwrap_or(bytes);
    let logo = bytes.into_text().ok()?;
    // The ANSI image uses black *foreground* pixels for its outlines. Treating
    // black as transparent removes parts of the dog, so crop only blank cells.
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
            return Some(logo);
        }
    }
    if left == usize::MAX {
        return None;
    }
    let mut lines = Vec::new();
    for line in logo.lines.into_iter().skip(top).take(bottom - top) {
        let mut row_left = usize::MAX;
        let mut row_right = 0;
        let mut row_column = 0;
        for span in &line.spans {
            for character in span.content.chars() {
                if logo_art_cell(character, span.style) {
                    row_left = row_left.min(row_column);
                    row_right = row_column + 1;
                }
                row_column += 1;
            }
        }
        let mut column = 0;
        let mut spans = Vec::new();
        for span in line.spans {
            for character in span.content.chars() {
                if (left..right).contains(&column) {
                    let style = if column < row_left || column >= row_right {
                        Style::default().bg(background)
                    } else {
                        span.style
                    };
                    spans.push(Span::styled(character.to_string(), style));
                }
                column += 1;
            }
        }
        if column < right {
            spans.push(Span::raw(" ".repeat(right - column)));
        }
        lines.push(Line::from(spans).style(line.style));
    }
    Some(Text::from(lines))
}

fn logo_art_cell(character: char, style: Style) -> bool {
    !character.is_whitespace()
        || style.bg.is_some_and(|background| {
            !matches!(
                background,
                Color::Rgb(0, 0, 0) | Color::Black | Color::Reset
            )
        })
}

pub(crate) fn render(frame: &mut ratatui::Frame<'_>, wizard: &Wizard) {
    let area = frame.area();
    frame.render_widget(Block::default().style(theme::canvas()), area);
    let shell = wizard.shell(area);
    if let Some(logo_area) = wizard.logo_area(area)
        && let Some(logo) = logo_text()
    {
        frame.render_widget(Paragraph::new(logo), logo_area);
    }
    render_setup(frame, wizard, shell);
    render_model_browser(frame, wizard, shell);
}

pub(crate) fn render_popup(frame: &mut ratatui::Frame<'_>, wizard: &Wizard) {
    let shell = wizard.shell(frame.area());
    frame.render_widget(Clear, shell);
    render_setup(frame, wizard, shell);
    render_model_browser(frame, wizard, shell);
}

fn model_browser_area(shell: Rect) -> Rect {
    centered(
        shell,
        shell.width.saturating_sub(2).min(64),
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
    frame.render_widget(Clear, area);
    frame.render_widget(
        Block::default()
            .title(" Models ")
            .title_bottom(" ↑↓ move · Enter select · Esc close ")
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

fn render_setup(frame: &mut ratatui::Frame<'_>, wizard: &Wizard, shell: Rect) {
    let workspace_title = wizard
        .workspace_explorer
        .as_ref()
        .map(|explorer| format!(" Choose workspace: {} ", explorer.cwd().display()));
    frame.render_widget(
        Block::default()
            .title(Line::from(vec![
                Span::styled(" 🐶 ", Style::default().fg(theme::ORANGE)),
                Span::styled(
                    match wizard.mode {
                        WizardMode::Startup => "voff team builder ",
                        WizardMode::Add => "Add agent ",
                        WizardMode::Configure => "Agent settings ",
                        WizardMode::Workspace => {
                            workspace_title.as_deref().unwrap_or("Choose workspace ")
                        }
                    },
                    Style::default()
                        .fg(theme::TEXT)
                        .add_modifier(Modifier::BOLD),
                ),
            ]))
            .title_bottom(Line::from(match wizard.step {
                Step::Basics => " [c] Continue · Esc cancel ",
                Step::Prompt(_) => " Ctrl+S save prompt · Esc discard ",
                Step::WorkspacePicker => "",
                _ if wizard.mode != WizardMode::Startup => " Enter save · Esc cancel ",
                _ => "",
            }))
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .style(theme::surface())
            .border_style(Style::default().fg(theme::BORDER)),
        shell,
    );
    let rows = setup_rows(shell, wizard.step, wizard.mode);
    match wizard.step {
        Step::Basics => render_basics(frame, wizard, rows[1]),
        Step::WorkspacePicker => render_workspace_picker(frame, wizard, rows[1]),
        Step::Developer(index) => render_developer(frame, wizard, index, rows[1]),
        Step::Prompt(_) => {
            if let Some(editor) = &wizard.prompt_editor {
                frame.render_widget(editor, rows[1]);
            }
        }
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
        ])
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
        frame.render_widget_ref(explorer.widget(), area);
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
    if index != 0 {
        form.push(form_line(
            "Type",
            developer.role.label(),
            wizard.field == Field::Role,
            true,
        ));
        if !compact {
            form.push(Line::from(""));
        }
    }
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
            " (f to browse)",
            Style::default().fg(theme::MUTED),
        ));
    }
    form.push(model_line);
    if !compact {
        form.push(Line::from(""));
    }
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
    if developer.provider == ProviderChoice::OpenAi {
        let token = openai_token_label(
            &developer.api_token,
            std::env::var_os("OPENAI_API_KEY").is_some_and(|token| !token.is_empty()),
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
    form.push(form_line(
        "Prompt",
        "Press [e] to edit",
        wizard.field == Field::Prompt,
        false,
    ));
    frame.render_widget(
        Paragraph::new(form).block(
            Block::default()
                .title(if index == 0 {
                    " Configure your lead agent ".to_owned()
                } else {
                    format!(" Configure {} ", developer.name)
                })
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .style(theme::surface())
                .border_style(Style::default().fg(theme::BORDER)),
        ),
        cols[0],
    );
    render_prompt_preview(frame, developer, cols[1]);
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

fn openai_token_label(entered: &str, env_available: bool) -> String {
    if !entered.is_empty() {
        "•".repeat(entered.chars().count().min(36))
    } else if env_available {
        "Configured via OPENAI_API_KEY".into()
    } else {
        "Required".into()
    }
}

fn render_prompt_preview(frame: &mut ratatui::Frame<'_>, developer: &DeveloperDraft, area: Rect) {
    let prompt = if developer.system_prompt.is_empty() {
        "No system prompt yet. Press [e] to write one."
    } else {
        developer.system_prompt.as_str()
    };
    frame.render_widget(
        Paragraph::new(prompt).wrap(Wrap { trim: false }).block(
            Block::default()
                .title(" System prompt · preview ")
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .style(theme::surface())
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
) -> Option<Field> {
    let mut fields = Vec::new();
    if !lead {
        fields.push(Field::Role);
    }
    fields.push(Field::Name);
    if !lead {
        fields.push(Field::ReportsTo);
    }
    fields.extend([Field::Provider, Field::Model, Field::OutputLimit]);
    if provider == ProviderChoice::OpenAi {
        fields.push(Field::Token);
    }
    fields.push(Field::Prompt);
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
        Step::Basics => "Enter continue · c choose workspace".into(),
        Step::WorkspacePicker => {
            if wizard.workspace_filtering {
                format!(
                    "Filter: {}█ · type to refine · ↑/↓ move · Enter highlighted · Esc finish",
                    wizard.workspace_filter
                )
            } else if wizard.workspace_filter.is_empty() {
                "↑↓/jk move · ←→/hl browse · / filter · Enter select · Space current".into()
            } else {
                format!(
                    "Filter: {} · ↑↓/jk move · Enter highlighted · Space current · Esc clear",
                    wizard.workspace_filter
                )
            }
        }
        Step::Developer(_) if wizard.mode != WizardMode::Startup => {
            "Tab/↑↓ fields · ←→ choices · type to edit · [e] prompt · Enter save".into()
        }
        Step::Developer(_) => "Press Enter to voff".into(),
        Step::Prompt(_) => "Edit freely · Enter new line · Ctrl+S save · Esc discard".into(),
    }
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

struct TerminalGuard;

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
        Ok(Self)
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), DisableBracketedPaste, LeaveAlternateScreen);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
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
        assert_eq!(openai_token_label("", false), "Required");
        assert_eq!(
            openai_token_label("", true),
            "Configured via OPENAI_API_KEY"
        );
        assert_eq!(openai_token_label("secret", false), "••••••");
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
        assert!(model.contains("‹ gpt-5-mini › (f to browse)"), "{model}");
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
                .any(|line| line.contains("Choose workspace:"))
        );
        assert_eq!(rendered.join("").matches('╭').count(), 1);
        assert!(rendered.iter().any(|line| line.contains("Enter select")));
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
        wizard.field = Field::Role;
        for _ in 0..3 {
            wizard.handle_key(key(KeyCode::Right));
        }
        assert_eq!(wizard.developers[2].role, DeveloperRole::Custom);
        assert!(wizard.developers[2].system_prompt.is_empty());
        assert_eq!(wizard.developers[2].name, "Custom Agent");
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
            developer_field_at_row(2, wizard.developers[0].provider, false, true),
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
    fn every_builtin_role_has_a_delegation_prompt() {
        assert_eq!(DeveloperRole::Lead.config_role(), "team_lead");
        assert_eq!(DeveloperRole::Backend.config_role(), "backend_agent");
        assert_eq!(DeveloperRole::Frontend.config_role(), "frontend_agent");
        assert_eq!(DeveloperRole::Reviewer.config_role(), "reviewer_agent");
        for role in [
            DeveloperRole::Lead,
            DeveloperRole::Backend,
            DeveloperRole::Frontend,
            DeveloperRole::Reviewer,
        ] {
            let prompt = role.prompt();
            assert!(
                prompt.contains("report") || prompt.contains("Team Lead"),
                "{}",
                role.label()
            );
            assert!(prompt.len() > 250, "{}", role.label());
        }
        let mut wizard = Wizard::default();
        wizard.handle_key(key(KeyCode::Enter));
        assert!(
            wizard
                .developers
                .iter()
                .all(|developer| developer.system_prompt == developer.role.prompt())
        );
    }

    #[test]
    fn prompt_editor_starts_prefilled_and_saves_multiline_text() {
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
                .contains("Team Lead")
        );
        wizard.handle_key(key(KeyCode::Enter));
        wizard.handle_paste("Keep task ownership explicit.");
        wizard.handle_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL));
        assert_eq!(wizard.step, Step::Developer(0));
        assert!(
            wizard.developers[0]
                .system_prompt
                .contains("\nKeep task ownership explicit.")
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
        assert!(rendered.contains("System prompt · Team Lead"), "{rendered}");
        assert!(rendered.contains("You are voff's Team Lead"), "{rendered}");
        assert!(rendered.contains("Ctrl+S save prompt"), "{rendered}");
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
        assert!(rendered.contains("System prompt · preview"), "{rendered}");
        assert!(rendered.contains("Classify the request."), "{rendered}");
        assert!(rendered.contains("Press [e] to edit"), "{rendered}");
        assert!(!rendered.contains("[e] edit full prompt"), "{rendered}");

        wizard.handle_key(key(KeyCode::Enter));
        assert!(wizard.complete());
        assert!(wizard.prompt_editor.is_none());
    }

    #[test]
    fn cloud_prompt_field_is_clickable_in_a_narrow_editor() {
        let mut initial = Wizard::default();
        initial.handle_key(key(KeyCode::Enter));
        let mut wizard = Wizard::edit(ModelCatalog::default(), initial.into_draft());
        wizard.handle_key(key(KeyCode::Enter));
        wizard.developers[0].provider = ProviderChoice::OpenAi;
        let area = Rect::new(0, 0, 80, 24);
        let content = setup_rows(wizard.shell(area), wizard.step, wizard.mode)[1];
        let cols = developer_columns(content);
        assert!(cols[0].height >= 16);
        assert_eq!(
            developer_field_at_row(12, ProviderChoice::OpenAi, false, true),
            Some(Field::Prompt)
        );
        let mut terminal = Terminal::new(ratatui::backend::TestBackend::new(80, 24)).unwrap();
        terminal.draw(|frame| render(frame, &wizard)).unwrap();
        let buffer = terminal.backend().buffer();
        let rendered = (0..buffer.area.height)
            .flat_map(|y| (0..buffer.area.width).map(move |x| buffer[(x, y)].symbol()))
            .collect::<String>();
        assert!(rendered.contains("API token"), "{rendered}");
        assert!(rendered.contains("Prompt"), "{rendered}");
        wizard.handle_event(
            &Event::Mouse(MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: cols[0].x + 4,
                row: cols[0].y + 1 + 12,
                modifiers: KeyModifiers::NONE,
            }),
            area,
        );
        assert_eq!(wizard.step, Step::Prompt(0));
    }

    #[test]
    fn prompt_editor_discard_and_invalid_save_preserve_the_previous_prompt() {
        let mut wizard = Wizard::default();
        wizard.handle_key(key(KeyCode::Enter));
        let original = wizard.developers[0].system_prompt.clone();
        wizard.field = Field::Prompt;
        wizard.handle_key(key(KeyCode::Char('e')));
        wizard.prompt_editor = Some(TextArea::default());
        wizard.handle_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL));
        assert_eq!(wizard.step, Step::Prompt(0));
        assert!(wizard.error.is_some());
        wizard.handle_paste("unsaved text");
        wizard.handle_key(key(KeyCode::Esc));
        assert_eq!(wizard.developers[0].system_prompt, original);
    }

    #[test]
    fn changing_role_loads_its_matching_builtin_prompt() {
        let mut wizard = Wizard::default();
        wizard.handle_key(key(KeyCode::Enter));
        let draft = wizard.into_draft();
        let mut wizard = Wizard::add_agent(ModelCatalog::default(), draft, 0);
        assert_eq!(wizard.step, Step::Developer(1));
        wizard.developers[1].system_prompt = "Previous role text".into();
        wizard.field = Field::Role;
        wizard.handle_key(key(KeyCode::Right));
        assert_eq!(wizard.developers[1].role, DeveloperRole::Frontend);
        assert_eq!(
            wizard.developers[1].system_prompt,
            DeveloperRole::Frontend.prompt()
        );
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
    fn output_token_limit_is_set_per_developer() {
        let mut wizard = Wizard::default();
        wizard.handle_key(key(KeyCode::Enter));
        wizard.field = Field::OutputLimit;
        for _ in 0..DEFAULT_OUTPUT_TOKENS.to_string().len() {
            wizard.handle_key(key(KeyCode::Backspace));
        }
        for value in "8192".chars() {
            wizard.handle_key(key(KeyCode::Char(value)));
        }
        wizard.handle_key(key(KeyCode::Enter));

        assert_eq!(wizard.developers[0].max_output_tokens, 8192);
        assert!(wizard.complete());
    }

    #[test]
    fn zero_output_tokens_do_not_advance() {
        let mut wizard = Wizard::default();
        wizard.handle_key(key(KeyCode::Enter));
        wizard.field = Field::OutputLimit;
        for _ in 0..DEFAULT_OUTPUT_TOKENS.to_string().len() {
            wizard.handle_key(key(KeyCode::Backspace));
        }
        wizard.handle_key(key(KeyCode::Enter));

        assert_eq!(wizard.step, Step::Developer(0));
        assert!(wizard.error.is_some());
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
            choose_preferred_provider(|name| name == "codex" || name == "ollama"),
            ProviderChoice::CodexCli
        );
        assert_eq!(
            choose_preferred_provider(|name| name == "claude"),
            ProviderChoice::ClaudeCli
        );
    }

    #[test]
    fn welcome_screen_has_a_clear_start_action() {
        let logo = logo_text().expect("bundled ANSI logo must be parseable");
        assert!(logo.lines.len() <= 22, "logo must fit a 24-row terminal");
        assert!(logo.lines.iter().all(|line| line.width() == 40));
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
        terminal.draw(render_intro).unwrap();
        let buffer = terminal.backend().buffer();
        let rendered = (0..buffer.area.height)
            .flat_map(|y| (0..buffer.area.width).map(move |x| buffer[(x, y)].symbol()))
            .collect::<String>();
        assert!(rendered.contains("Welcome to voff!"), "{rendered}");
        assert!(rendered.contains("Enter/click to set up"), "{rendered}");
        assert!(rendered.contains('▂') || rendered.contains('█'));
        let black_outline_cells = (0..buffer.area.height)
            .flat_map(|y| (0..buffer.area.width).map(move |x| (x, y)))
            .filter(|&(x, y)| {
                buffer[(x, y)].fg == Color::Rgb(0, 0, 0) && buffer[(x, y)].symbol() != " "
            })
            .count();
        assert!(
            black_outline_cells > 10,
            "black outline was lost during rendering"
        );
        assert_eq!(buffer[(0, 0)].bg, theme::BACKGROUND);
        let logo_x = (80 - logo.lines[0].width() as u16) / 2;
        let logo_y = (24 - logo.lines.len() as u16 - 2) / 2;
        assert_eq!(
            buffer[(logo_x, logo_y)].bg,
            theme::BACKGROUND,
            "empty logo margin must blend into splash background"
        );

        let mut terminal = Terminal::new(ratatui::backend::TestBackend::new(80, 26)).unwrap();
        terminal.draw(render_intro).unwrap();
        let buffer = terminal.backend().buffer();
        let row_text = |y| (0..80).map(|x| buffer[(x, y)].symbol()).collect::<String>();
        let welcome_y = (0..26)
            .find(|&y| row_text(y).contains("Welcome to voff!"))
            .unwrap();
        assert!(row_text(welcome_y + 1).trim().is_empty());
        assert!(row_text(welcome_y + 2).contains("Enter/click to set up"));
        let action_x = row_text(welcome_y + 2).find("Enter/click").unwrap() as u16;
        assert!(
            buffer[(action_x, welcome_y + 2)]
                .modifier
                .contains(Modifier::ITALIC)
        );
        let voff_x = row_text(welcome_y).find("voff").unwrap() as u16;
        assert_eq!(buffer[(voff_x, welcome_y)].fg, theme::ORANGE);
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
        let title = row_text(shell.y);
        let title_x = title.find("voff team builder").unwrap() as u16;
        assert_eq!(buffer[(title_x, shell.y)].fg, theme::TEXT);
        let all_text = (0..36).map(row_text).collect::<Vec<_>>().join("\n");
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
    fn welcome_accepts_enter_and_clicks_anywhere() {
        assert!(starts_setup(&Event::Key(key(KeyCode::Enter))));
        for (button, column, row) in [
            (MouseButton::Left, 0, 0),
            (MouseButton::Right, 79, 0),
            (MouseButton::Middle, 79, 23),
        ] {
            assert!(starts_setup(&Event::Mouse(MouseEvent {
                kind: MouseEventKind::Down(button),
                column,
                row,
                modifiers: KeyModifiers::NONE,
            })));
        }
        assert!(!starts_setup(&Event::Mouse(MouseEvent {
            kind: MouseEventKind::Moved,
            column: 0,
            row: 0,
            modifiers: KeyModifiers::NONE,
        })));
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
        assert_eq!(wizard.field, Field::OutputLimit);
        wizard.field = Field::Provider;
        wizard.handle_key(key(KeyCode::Right));
        wizard.field = Field::Token;
        wizard.handle_paste("secret");
        wizard.field = Field::Provider;
        wizard.handle_key(key(KeyCode::Right));
        assert_eq!(wizard.developers[0].provider, ProviderChoice::CodexCli);
        assert!(wizard.developers[0].api_token.is_empty());
        wizard.field = Field::Model;
        wizard.handle_key(key(KeyCode::Tab));
        assert_eq!(wizard.field, Field::OutputLimit);
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
        assert!(footer_text(&wizard).contains("Filter: hell█"));
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
