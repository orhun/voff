//! First-launch team builder.

use crate::{config::MAX_OUTPUT_TOKENS, model_catalog::ModelCatalog};
use crossterm::{
    event::{
        self, DisableBracketedPaste, EnableBracketedPaste, Event, KeyCode, KeyEvent, KeyEventKind,
        KeyModifiers,
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
    widgets::{Block, BorderType, Borders, Clear, FrameExt as _, Gauge, Paragraph, Wrap},
};
use ratatui_explorer::{FileExplorer, FileExplorerBuilder, Theme};
use std::{io, panic, path::PathBuf};

const MAX_DEVELOPERS: usize = 32;
const DEFAULT_OUTPUT_TOKENS: u64 = 4096;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProviderChoice {
    Ollama,
    OpenAi,
}

impl ProviderChoice {
    const ALL: [Self; 2] = [Self::Ollama, Self::OpenAi];

    #[must_use]
    pub const fn id(self) -> &'static str {
        match self {
            Self::Ollama => "ollama",
            Self::OpenAi => "openai",
        }
    }

    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Ollama => "Ollama · local",
            Self::OpenAi => "OpenAI · cloud",
        }
    }

    fn cycle(self, delta: isize) -> Self {
        cycle(Self::ALL, self, delta)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DeveloperRole {
    Lead,
    FullStack,
    Frontend,
    Backend,
    Reviewer,
    Tester,
    DevOps,
}

impl DeveloperRole {
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Lead => "Technical lead",
            Self::FullStack => "Full-stack developer",
            Self::Frontend => "Frontend developer",
            Self::Backend => "Backend developer",
            Self::Reviewer => "Code reviewer",
            Self::Tester => "Test engineer",
            Self::DevOps => "DevOps engineer",
        }
    }

    #[must_use]
    pub const fn config_role(self) -> &'static str {
        match self {
            Self::Lead => "technical_lead",
            Self::FullStack => "full_stack_developer",
            Self::Frontend => "frontend_developer",
            Self::Backend => "backend_developer",
            Self::Reviewer => "code_reviewer",
            Self::Tester => "test_engineer",
            Self::DevOps => "devops_engineer",
        }
    }

    #[must_use]
    pub const fn icon(self) -> &'static str {
        match self {
            Self::Lead => "◆",
            Self::FullStack => "●",
            Self::Frontend => "▣",
            Self::Backend => "⬢",
            Self::Reviewer => "◇",
            Self::Tester => "✓",
            Self::DevOps => "▲",
        }
    }

    #[must_use]
    pub const fn prompt(self) -> &'static str {
        match self {
            Self::Lead => {
                "Own every team decision. Plan bounded work, delegate every required specialty as an explicit child task, prevent overlapping ownership, and integrate only verified results."
            }
            Self::FullStack => {
                "Implement small end-to-end changes, follow repository rules, and return test evidence to the Team Lead without coordinating peers directly."
            }
            Self::Frontend => {
                "Build accessible user-facing changes, preserve established patterns, verify behavior, and return the result to the Team Lead."
            }
            Self::Backend => {
                "Implement reliable backend changes within Team Lead-assigned boundaries and return verifiable tests."
            }
            Self::Reviewer => {
                "Review Team Lead-routed artifacts against acceptance criteria and return concrete blocking findings."
            }
            Self::Tester => {
                "Design focused checks, exercise failure paths, and return reproducible evidence to the Team Lead."
            }
            Self::DevOps => {
                "Improve Team Lead-assigned build and runtime workflows without broadening permissions or hiding risk."
            }
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
            Self::Reviewer | Self::Tester => &["file.read", "search.text", "process.test"],
            Self::FullStack | Self::Frontend | Self::Backend | Self::DevOps => {
                &["file.read", "search.text", "file.patch", "process.test"]
            }
        };
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    fn cycle_specialist(self, delta: isize) -> Self {
        const SPECIALISTS: [DeveloperRole; 6] = [
            DeveloperRole::FullStack,
            DeveloperRole::Frontend,
            DeveloperRole::Backend,
            DeveloperRole::Reviewer,
            DeveloperRole::Tester,
            DeveloperRole::DevOps,
        ];
        let current = if self == Self::Lead {
            Self::FullStack
        } else {
            self
        };
        cycle(SPECIALISTS, current, delta)
    }

    fn color(self) -> Color {
        match self {
            Self::Lead => Color::Magenta,
            Self::FullStack => Color::Cyan,
            Self::Frontend => Color::LightCyan,
            Self::Backend => Color::Blue,
            Self::Reviewer => Color::Yellow,
            Self::Tester => Color::Green,
            Self::DevOps => Color::LightRed,
        }
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

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DeveloperDraft {
    pub name: String,
    pub role: DeveloperRole,
    pub provider: ProviderChoice,
    pub model: String,
    pub max_output_tokens: u64,
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
            provider,
            model: model.into(),
            max_output_tokens,
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
            provider,
            model,
            max_output_tokens,
        }
    }
}

#[derive(Clone, Eq, PartialEq)]
pub struct SetupDraft {
    pub developers: Vec<DeveloperDraft>,
    pub working_directory: PathBuf,
    pub openai_token: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Step {
    Basics,
    WorkspacePicker,
    Developer(usize),
    Credential,
    Review,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Field {
    Role,
    Provider,
    Model,
}

impl Field {
    fn next(self) -> Self {
        match self {
            Self::Role => Self::Provider,
            Self::Provider => Self::Model,
            Self::Model => Self::Role,
        }
    }

    fn previous(self) -> Self {
        match self {
            Self::Role => Self::Model,
            Self::Provider => Self::Role,
            Self::Model => Self::Provider,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum BasicsField {
    Count,
    WorkingDirectory,
    Tokens,
}

impl BasicsField {
    const fn next(self) -> Self {
        match self {
            Self::Count => Self::WorkingDirectory,
            Self::WorkingDirectory => Self::Tokens,
            Self::Tokens => Self::Count,
        }
    }

    const fn previous(self) -> Self {
        match self {
            Self::Count => Self::Tokens,
            Self::WorkingDirectory => Self::Count,
            Self::Tokens => Self::WorkingDirectory,
        }
    }
}

#[derive(Debug)]
struct Wizard {
    step: Step,
    basics_field: BasicsField,
    count_input: String,
    count_touched: bool,
    working_directory: PathBuf,
    workspace_explorer: Option<FileExplorer>,
    workspace_filter: String,
    workspace_filtering: bool,
    default_max_output_tokens: u64,
    tokens_touched: bool,
    developers: Vec<DeveloperDraft>,
    field: Field,
    error: Option<String>,
    cancelled: bool,
    complete: bool,
    catalog: ModelCatalog,
    credential_available: bool,
    openai_token: String,
}

impl Default for Wizard {
    fn default() -> Self {
        Self::new(ModelCatalog::default(), false)
    }
}

impl Wizard {
    fn new(catalog: ModelCatalog, credential_available: bool) -> Self {
        Self {
            step: Step::Basics,
            basics_field: BasicsField::Count,
            count_input: "3".into(),
            count_touched: false,
            working_directory: ".".into(),
            workspace_explorer: None,
            workspace_filter: String::new(),
            workspace_filtering: false,
            default_max_output_tokens: DEFAULT_OUTPUT_TOKENS,
            tokens_touched: false,
            developers: vec![],
            field: Field::Role,
            error: None,
            cancelled: false,
            complete: false,
            catalog,
            credential_available,
            openai_token: String::new(),
        }
    }

    fn handle_key(&mut self, key: KeyEvent) {
        if key.kind != KeyEventKind::Press {
            return;
        }
        self.error = None;
        match self.step {
            Step::Basics => self.handle_basics(key),
            Step::WorkspacePicker => self.handle_workspace_picker(key),
            Step::Developer(index) => self.handle_developer(index, key),
            Step::Credential => self.handle_credential(key),
            Step::Review => self.handle_review(key),
        }
    }

    fn handle_basics(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => self.cancelled = true,
            KeyCode::Tab => self.basics_field = self.basics_field.next(),
            KeyCode::BackTab => self.basics_field = self.basics_field.previous(),
            KeyCode::Up => self.basics_field = self.basics_field.previous(),
            KeyCode::Down => self.basics_field = self.basics_field.next(),
            KeyCode::Char('q') => self.cancelled = true,
            KeyCode::Char(value) if value.is_ascii_digit() => match self.basics_field {
                BasicsField::Count => {
                    if !self.count_touched {
                        self.count_input.clear();
                        self.count_touched = true;
                    }
                    if self.count_input.len() < 2 {
                        self.count_input.push(value);
                    }
                }
                BasicsField::Tokens => {
                    if !self.tokens_touched {
                        self.default_max_output_tokens = 0;
                        self.tokens_touched = true;
                    }
                    let digit = value.to_digit(10).map_or(0, u64::from);
                    self.default_max_output_tokens = self
                        .default_max_output_tokens
                        .saturating_mul(10)
                        .saturating_add(digit)
                        .min(MAX_OUTPUT_TOKENS);
                }
                BasicsField::WorkingDirectory => {}
            },
            KeyCode::Backspace => match self.basics_field {
                BasicsField::Count => {
                    self.count_touched = true;
                    self.count_input.pop();
                }
                BasicsField::Tokens => {
                    self.tokens_touched = true;
                    self.default_max_output_tokens /= 10;
                }
                BasicsField::WorkingDirectory => {}
            },
            KeyCode::Right | KeyCode::Char('+') => self.adjust_basic_choice(1),
            KeyCode::Left | KeyCode::Char('-') => self.adjust_basic_choice(-1),
            KeyCode::Enter => {
                if self.basics_field == BasicsField::WorkingDirectory {
                    self.open_workspace_picker();
                } else if let Some(count) = self.count()
                    && self.default_max_output_tokens > 0
                    && self.valid_working_directory().is_some()
                {
                    let model = self.catalog.first_model(ProviderChoice::Ollama.id());
                    self.developers = (0..count)
                        .map(|index| {
                            DeveloperDraft::suggested(
                                index,
                                count,
                                ProviderChoice::Ollama,
                                model.clone(),
                                self.default_max_output_tokens,
                            )
                        })
                        .collect();
                    self.step = Step::Developer(0);
                    self.field = Field::Role;
                } else {
                    self.error = Some(if self.valid_working_directory().is_none() {
                        "Choose an existing working directory.".into()
                    } else {
                        format!("Choose 1–{MAX_DEVELOPERS} developers and at least 1 output token.")
                    });
                }
            }
            _ => {}
        }
    }

    fn open_workspace_picker(&mut self) {
        let start = match self.working_directory.canonicalize() {
            Ok(path) => path,
            Err(error) => {
                self.error = Some(format!("Cannot open that workspace: {error}"));
                return;
            }
        };
        let theme = Theme::default()
            .with_block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_type(BorderType::Rounded)
                    .border_style(Style::default().fg(Color::LightCyan)),
            )
            .add_default_title()
            .with_highlight_dir_style(
                Style::default()
                    .fg(Color::LightCyan)
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
                    self.step = Step::Basics;
                }
            }
            KeyCode::Enter => {
                if let Some(explorer) = &self.workspace_explorer {
                    self.working_directory = explorer.current().path.clone();
                }
                self.workspace_explorer = None;
                self.workspace_filter.clear();
                self.workspace_filtering = false;
                self.step = Step::Basics;
            }
            KeyCode::Char(' ') => {
                if let Some(explorer) = &self.workspace_explorer {
                    self.working_directory = explorer.cwd().clone();
                }
                self.workspace_explorer = None;
                self.workspace_filter.clear();
                self.workspace_filtering = false;
                self.step = Step::Basics;
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
            KeyCode::Esc => {
                if index == 0 {
                    self.step = Step::Basics;
                } else {
                    self.step = Step::Developer(index - 1);
                }
            }
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.cancelled = true;
            }
            KeyCode::Tab => self.field = self.field.next(),
            KeyCode::BackTab => self.field = self.field.previous(),
            KeyCode::Up => self.field = self.field.previous(),
            KeyCode::Down => self.field = self.field.next(),
            KeyCode::Left => self.cycle_choice(index, -1),
            KeyCode::Right => self.cycle_choice(index, 1),
            KeyCode::Backspace => match self.field {
                Field::Model
                    if self
                        .catalog
                        .model_is_editable(self.developers[index].provider.id()) =>
                {
                    self.developers[index].model.pop();
                }
                Field::Model => {}
                Field::Role | Field::Provider => {}
            },
            KeyCode::Char(value) => match self.field {
                Field::Model
                    if !key.modifiers.contains(KeyModifiers::CONTROL)
                        && self
                            .catalog
                            .model_is_editable(self.developers[index].provider.id()) =>
                {
                    self.developers[index].model.push(value);
                }
                Field::Role | Field::Provider | Field::Model => {}
            },
            KeyCode::Enter => {
                let developer = &self.developers[index];
                if developer.model.trim().is_empty() {
                    self.error = Some("Model cannot be empty.".into());
                } else if index + 1 < self.developers.len() {
                    self.step = Step::Developer(index + 1);
                    self.field = Field::Role;
                } else if self.needs_openai_credential() {
                    self.step = Step::Credential;
                } else {
                    self.step = Step::Review;
                }
            }
            _ => {}
        }
    }

    fn handle_credential(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => {
                self.openai_token.clear();
                self.step = Step::Developer(self.developers.len().saturating_sub(1));
                self.field = Field::Role;
            }
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.cancelled = true;
            }
            KeyCode::Backspace => {
                self.openai_token.pop();
            }
            KeyCode::Char(value) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.openai_token.push(value);
            }
            KeyCode::Enter => self.step = Step::Review,
            _ => {}
        }
    }

    fn handle_paste(&mut self, value: &str) {
        match self.step {
            Step::Credential => self.openai_token.push_str(value.trim()),
            Step::Basics | Step::WorkspacePicker | Step::Developer(_) | Step::Review => {}
        }
    }

    fn handle_review(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Enter => self.complete = true,
            KeyCode::Esc => {
                self.step = Step::Developer(self.developers.len().saturating_sub(1));
                self.field = Field::Role;
            }
            KeyCode::Char('q') => self.cancelled = true,
            _ => {}
        }
    }

    fn count(&self) -> Option<usize> {
        self.count_input
            .parse()
            .ok()
            .filter(|count| (1..=MAX_DEVELOPERS).contains(count))
    }

    fn valid_working_directory(&self) -> Option<PathBuf> {
        let path = self.working_directory.clone();
        (!path.as_os_str().is_empty() && path.is_dir()).then_some(path)
    }

    fn adjust_count(&mut self, delta: isize) {
        let current = self.count().unwrap_or(1);
        let next = (current as isize + delta).clamp(1, MAX_DEVELOPERS as isize);
        self.count_input = next.to_string();
        self.count_touched = true;
    }

    fn adjust_basic_choice(&mut self, delta: isize) {
        match self.basics_field {
            BasicsField::Count => self.adjust_count(delta),
            BasicsField::WorkingDirectory => {}
            BasicsField::Tokens => {
                let signed = i64::try_from(self.default_max_output_tokens).unwrap_or(i64::MAX);
                self.default_max_output_tokens = if delta.is_negative() {
                    signed.saturating_sub(1024).max(1) as u64
                } else {
                    signed.saturating_add(1024).min(MAX_OUTPUT_TOKENS as i64) as u64
                };
                self.tokens_touched = true;
            }
        }
    }

    fn needs_openai_credential(&self) -> bool {
        !self.credential_available
            && self
                .developers
                .iter()
                .any(|developer| developer.provider == ProviderChoice::OpenAi)
    }

    fn cycle_choice(&mut self, index: usize, delta: isize) {
        let developer = &mut self.developers[index];
        match self.field {
            Field::Role => {
                if index == 0 {
                    developer.role = DeveloperRole::Lead;
                    developer.name = suggested_name(developer.role, index);
                    return;
                }
                developer.role = developer.role.cycle_specialist(delta);
                developer.name = suggested_name(developer.role, index);
            }
            Field::Provider => {
                developer.provider = developer.provider.cycle(delta);
                developer.model = self.catalog.first_model(developer.provider.id());
            }
            Field::Model => {
                if !self.catalog.model_is_editable(developer.provider.id()) {
                    developer.model =
                        self.catalog
                            .cycle_model(developer.provider.id(), &developer.model, delta);
                }
            }
        }
    }
}

fn suggested_role(index: usize, count: usize) -> DeveloperRole {
    if count == 1 {
        return DeveloperRole::Lead;
    }
    match index {
        0 => DeveloperRole::Lead,
        1 => DeveloperRole::Backend,
        2 => DeveloperRole::Frontend,
        3 => DeveloperRole::Reviewer,
        4 => DeveloperRole::Tester,
        _ => DeveloperRole::FullStack,
    }
}

fn suggested_name(role: DeveloperRole, index: usize) -> String {
    let base = match role {
        DeveloperRole::Lead => "Team Lead",
        DeveloperRole::FullStack => "Full-stack Developer",
        DeveloperRole::Frontend => "Frontend Developer",
        DeveloperRole::Backend => "Backend Developer",
        DeveloperRole::Reviewer => "Reviewer",
        DeveloperRole::Tester => "Test Engineer",
        DeveloperRole::DevOps => "DevOps Engineer",
    };
    if index < 5 {
        base.into()
    } else {
        format!("{base} {}", index + 1)
    }
}

/// Run the first-launch setup. `None` means the user cancelled without writing.
pub fn run(catalog: ModelCatalog, credential_available: bool) -> io::Result<Option<SetupDraft>> {
    let _guard = TerminalGuard::enter()?;
    let mut terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;
    let mut wizard = Wizard::new(catalog, credential_available);
    while !wizard.cancelled && !wizard.complete {
        terminal.draw(|frame| render(frame, &wizard))?;
        match event::read()? {
            Event::Key(key) => wizard.handle_key(key),
            Event::Paste(value) => wizard.handle_paste(&value),
            _ => {}
        }
    }
    if wizard.complete {
        Ok(Some(SetupDraft {
            developers: wizard.developers,
            working_directory: wizard.working_directory,
            openai_token: (!wizard.openai_token.is_empty()).then_some(wizard.openai_token),
        }))
    } else {
        Ok(None)
    }
}

fn render(frame: &mut ratatui::Frame<'_>, wizard: &Wizard) {
    let area = frame.area();
    frame.render_widget(Clear, area);
    let shell = centered(area, area.width.min(92), area.height.min(34));
    frame.render_widget(
        Block::default()
            .title(Line::from(vec![
                Span::styled(" ◆ ", Style::default().fg(Color::Magenta)),
                Span::styled(
                    "voff team builder ",
                    Style::default().add_modifier(Modifier::BOLD),
                ),
            ]))
            .title_bottom(Line::from(
                " Your team stays editable in .voff/config.toml ",
            ))
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(Color::DarkGray)),
        shell,
    );
    let inner = shell.inner(ratatui::layout::Margin {
        horizontal: 2,
        vertical: 1,
    });
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(10),
            Constraint::Length(3),
        ])
        .split(inner);
    render_progress(frame, wizard, rows[0]);
    match wizard.step {
        Step::Basics => render_basics(frame, wizard, rows[1]),
        Step::WorkspacePicker => render_workspace_picker(frame, wizard, rows[1]),
        Step::Developer(index) => render_developer(frame, wizard, index, rows[1]),
        Step::Credential => render_credential(frame, wizard, rows[1]),
        Step::Review => render_review(frame, wizard, rows[1]),
    }
    let footer = wizard
        .error
        .as_deref()
        .map_or_else(|| footer_text(wizard), |error| format!("! {error}"));
    let footer_style = if wizard.error.is_some() {
        Style::default().fg(Color::LightRed)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    frame.render_widget(
        Paragraph::new(footer)
            .alignment(Alignment::Center)
            .style(footer_style),
        rows[2],
    );
}

fn render_progress(frame: &mut ratatui::Frame<'_>, wizard: &Wizard, area: Rect) {
    let (label, ratio) = match wizard.step {
        Step::Basics => ("1 · quick defaults", 0.15),
        Step::WorkspacePicker => ("1 · choose workspace", 0.15),
        Step::Developer(index) => {
            let total = wizard.developers.len().max(1);
            (
                "2 · roles & models",
                0.2 + 0.6 * (index + 1) as f64 / total as f64,
            )
        }
        Step::Credential => ("3 · secure access", 0.9),
        Step::Review => ("ready to launch", 1.0),
    };
    frame.render_widget(
        Gauge::default()
            .block(Block::default().borders(Borders::BOTTOM))
            .gauge_style(Style::default().fg(Color::Magenta))
            .label(label)
            .ratio(ratio),
        area,
    );
}

fn render_basics(frame: &mut ratatui::Frame<'_>, wizard: &Wizard, area: Rect) {
    let tokens = wizard.default_max_output_tokens.to_string();
    let working_directory = wizard.working_directory.display().to_string();
    let lines = vec![
        Line::from(Span::styled(
            "Start with sensible defaults. You can launch without changing anything.",
            Style::default().add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        form_line(
            "Developers",
            &wizard.count_input,
            wizard.basics_field == BasicsField::Count,
            false,
        ),
        Line::from(""),
        form_line(
            "Workspace",
            &working_directory,
            wizard.basics_field == BasicsField::WorkingDirectory,
            true,
        ),
        Line::from(""),
        form_line(
            "Token limit",
            &tokens,
            wizard.basics_field == BasicsField::Tokens,
            false,
        ),
        Line::from(""),
        Line::from(
            "These apply to the whole team. Choose provider and model on each developer next.",
        ),
    ];
    frame.render_widget(
        Paragraph::new(lines)
            .alignment(Alignment::Center)
            .wrap(Wrap { trim: true }),
        centered(area, area.width.min(72), 12),
    );
}

fn render_workspace_picker(frame: &mut ratatui::Frame<'_>, wizard: &Wizard, area: Rect) {
    if let Some(explorer) = &wizard.workspace_explorer {
        frame.render_widget_ref(explorer.widget(), area);
    } else {
        frame.render_widget(
            Paragraph::new("Workspace picker is unavailable.")
                .alignment(Alignment::Center)
                .style(Style::default().fg(Color::LightRed)),
            area,
        );
    }
}

fn render_credential(frame: &mut ratatui::Frame<'_>, wizard: &Wizard, area: Rect) {
    let bullets = "•".repeat(wizard.openai_token.chars().count().min(36));
    let lines = vec![
        Line::from(Span::styled(
            "Add your OpenAI API token now? (optional)",
            Style::default().add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from(Span::styled(
            if bullets.is_empty() {
                "  token goes here  ".into()
            } else {
                format!("  {bullets}  ")
            },
            Style::default()
                .fg(Color::Black)
                .bg(Color::LightCyan)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from("Paste it now, or press Enter to launch and add it later with a."),
        Line::from("Tokens are stored in the local .voff SQLite database—not config or logs."),
        Line::from(Span::styled(
            "OPENAI_API_KEY remains the non-interactive override.",
            Style::default().fg(Color::DarkGray),
        )),
    ];
    frame.render_widget(
        Paragraph::new(lines)
            .alignment(Alignment::Center)
            .wrap(Wrap { trim: true }),
        centered(area, area.width.min(72), 11),
    );
}

fn render_developer(frame: &mut ratatui::Frame<'_>, wizard: &Wizard, index: usize, area: Rect) {
    let developer = &wizard.developers[index];
    let option = wizard
        .catalog
        .option(developer.provider.id(), &developer.model);
    let editable_model = wizard.catalog.model_is_editable(developer.provider.id());
    let model_label = if editable_model {
        developer.model.clone()
    } else {
        format!(
            "{}  ({})",
            developer.model,
            wizard
                .catalog
                .selection_label(developer.provider.id(), &developer.model)
        )
    };
    let pricing = option.map_or_else(
        || "price unavailable".into(),
        crate::model_catalog::ModelOption::price_label,
    );
    let context = option.and_then(|option| option.context_limit).map_or_else(
        || "context unknown".into(),
        |limit| format!("{limit} context"),
    );
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(62), Constraint::Percentage(38)])
        .split(area);
    let form = vec![
        form_line(
            "Type",
            if index == 0 {
                "Technical lead · required"
            } else {
                developer.role.label()
            },
            wizard.field == Field::Role,
            index != 0,
        ),
        Line::from(""),
        form_line(
            "Provider",
            developer.provider.label(),
            wizard.field == Field::Provider,
            true,
        ),
        Line::from(""),
        form_line(
            "Model",
            &model_label,
            wizard.field == Field::Model,
            !editable_model,
        ),
        Line::from(Span::styled(
            format!("  {pricing} · {context}"),
            Style::default().fg(Color::DarkGray),
        )),
        Line::from(Span::styled(
            format!(
                "  {} · {} output tokens",
                developer.name, developer.max_output_tokens
            ),
            Style::default().fg(Color::DarkGray),
        )),
    ];
    frame.render_widget(
        Paragraph::new(form)
            .block(
                Block::default()
                    .title(format!(
                        " Developer {} of {} ",
                        index + 1,
                        wizard.developers.len()
                    ))
                    .borders(Borders::ALL)
                    .border_type(BorderType::Rounded),
            )
            .wrap(Wrap { trim: false }),
        cols[0],
    );
    render_preview_card(frame, developer, cols[1], index == 0);
}

fn form_line<'a>(label: &'a str, value: &'a str, selected: bool, choice: bool) -> Line<'a> {
    let marker = if selected { "›" } else { " " };
    let value = if choice {
        format!("‹ {value} ›")
    } else {
        format!("{value}{}", if selected { "▏" } else { "" })
    };
    Line::from(vec![
        Span::styled(
            format!("{marker} {label:<13}"),
            if selected {
                Style::default()
                    .fg(Color::LightCyan)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::DarkGray)
            },
        ),
        Span::raw(value),
    ])
}

fn render_review(frame: &mut ratatui::Frame<'_>, wizard: &Wizard, area: Rect) {
    let title = Paragraph::new(format!(
        "Your {}-developer team is ready in {}. The required Team Lead is the sole decision and delegation route; specialists return isolated results for integration.",
        wizard.developers.len(),
        wizard.working_directory.display()
    ))
    .alignment(Alignment::Center)
    .wrap(Wrap { trim: true });
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(3), Constraint::Min(7)])
        .split(area);
    frame.render_widget(title, rows[0]);
    let card_width = 26_u16.min(rows[1].width.max(1));
    let columns = usize::from((rows[1].width / card_width.max(1)).max(1));
    let card_height = 7;
    for (index, developer) in wizard.developers.iter().enumerate() {
        let row = index / columns;
        let column = index % columns;
        let x = rows[1].x + column as u16 * card_width;
        let y = rows[1].y + row as u16 * card_height;
        if y + card_height > rows[1].bottom() {
            break;
        }
        render_preview_card(
            frame,
            developer,
            Rect::new(x, y, card_width, card_height),
            index == 0,
        );
    }
}

fn render_preview_card(
    frame: &mut ratatui::Frame<'_>,
    developer: &DeveloperDraft,
    area: Rect,
    entry: bool,
) {
    let card = centered(
        area,
        area.width.saturating_sub(2).min(28),
        7.min(area.height),
    );
    if card.width < 4 || card.height < 3 {
        return;
    }
    let title = format!(" {} {} ", developer.role.icon(), developer.name);
    let body = Text::from(vec![
        Line::from(developer.role.label()),
        Line::from(Span::styled(
            developer.provider.id(),
            Style::default().fg(Color::DarkGray),
        )),
        Line::from(Span::styled(
            developer.model.as_str(),
            Style::default().fg(Color::LightCyan),
        )),
        Line::from(Span::styled(
            format!("{} output tokens", developer.max_output_tokens),
            Style::default().fg(Color::DarkGray),
        )),
        Line::from(if entry {
            "◆ required Team Lead"
        } else {
            "○ specialist · via lead"
        }),
    ]);
    frame.render_widget(
        Paragraph::new(body).block(
            Block::default()
                .title(title)
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(developer.role.color())),
        ),
        card,
    );
}

fn footer_text(wizard: &Wizard) -> String {
    match wizard.step {
        Step::Basics => {
            "Enter accepts · Workspace opens picker · Tab/↑/↓ fields · Esc cancel".into()
        }
        Step::WorkspacePicker => {
            if wizard.workspace_filtering {
                format!(
                    "Filter: {}█ · type to refine · ↑/↓ move · Enter highlighted · Esc finish",
                    wizard.workspace_filter
                )
            } else if wizard.workspace_filter.is_empty() {
                "/ filter · ↑↓/jk move · ←→/hl browse · Enter highlighted · Space current".into()
            } else {
                format!(
                    "Filter: {} · ↑↓/jk move · Enter highlighted · Space current · Esc clear",
                    wizard.workspace_filter
                )
            }
        }
        Step::Developer(0) => {
            "Team Lead is required · Enter accepts · Tab fields · ←/→ provider/model · Esc back"
                .into()
        }
        Step::Developer(_) => {
            "Enter accepts this specialist · Tab fields · ←/→ choices · Esc back".into()
        }
        Step::Credential => "paste token (optional) · Enter continue · Esc back".into(),
        Step::Review => "Enter save & launch · Esc edit · q cancel".into(),
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

struct TerminalGuard;

impl TerminalGuard {
    fn enter() -> io::Result<Self> {
        enable_raw_mode()?;
        execute!(io::stdout(), EnterAlternateScreen, EnableBracketedPaste)?;
        let old = panic::take_hook();
        panic::set_hook(Box::new(move |info| {
            let _ = disable_raw_mode();
            let _ = execute!(io::stdout(), DisableBracketedPaste, LeaveAlternateScreen);
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
    fn count_creates_friendly_role_defaults() {
        let mut wizard = Wizard {
            count_input: "4".into(),
            ..Wizard::default()
        };
        wizard.handle_key(key(KeyCode::Enter));
        assert_eq!(wizard.step, Step::Developer(0));
        assert_eq!(wizard.developers.len(), 4);
        assert_eq!(wizard.developers[0].role, DeveloperRole::Lead);
        assert_eq!(wizard.developers[1].role, DeveloperRole::Backend);
        assert_eq!(wizard.developers[2].role, DeveloperRole::Frontend);
        assert_eq!(wizard.developers[3].role, DeveloperRole::Reviewer);
    }

    #[test]
    fn setup_keeps_exactly_one_required_team_lead() {
        let mut wizard = Wizard::default();
        wizard.handle_key(key(KeyCode::Enter));

        wizard.field = Field::Role;
        wizard.handle_key(key(KeyCode::Right));
        assert_eq!(wizard.developers[0].role, DeveloperRole::Lead);

        wizard.handle_key(key(KeyCode::Enter));
        wizard.field = Field::Role;
        for _ in 0..12 {
            wizard.handle_key(key(KeyCode::Right));
            assert_ne!(wizard.developers[1].role, DeveloperRole::Lead);
        }
    }

    #[test]
    fn solo_setup_still_requires_a_team_lead() {
        let mut wizard = Wizard {
            count_input: "1".into(),
            ..Wizard::default()
        };

        wizard.handle_key(key(KeyCode::Enter));

        assert_eq!(wizard.developers.len(), 1);
        assert_eq!(wizard.developers[0].role, DeveloperRole::Lead);
    }

    #[test]
    fn each_developer_keeps_an_independent_model() {
        let mut catalog = ModelCatalog::default();
        catalog.set_ollama_models(vec![
            voff_provider::ModelDescriptor {
                id: "first".into(),
                display_name: "First".into(),
                context_limit: None,
            },
            voff_provider::ModelDescriptor {
                id: "second".into(),
                display_name: "Second".into(),
                context_limit: None,
            },
        ]);
        let mut wizard = Wizard::new(catalog, false);
        wizard.handle_key(key(KeyCode::Enter));
        wizard.field = Field::Model;
        wizard.handle_key(key(KeyCode::Right));
        wizard.handle_key(key(KeyCode::Enter));
        assert_eq!(wizard.step, Step::Developer(1));
        assert_eq!(wizard.developers[0].model, "second");
        assert_eq!(wizard.developers[1].model, "first");
    }

    #[test]
    fn changing_provider_updates_an_untouched_default_model() {
        let mut wizard = Wizard::default();
        wizard.handle_key(key(KeyCode::Enter));
        wizard.field = Field::Provider;
        wizard.handle_key(key(KeyCode::Right));
        assert_eq!(wizard.developers[0].provider, ProviderChoice::OpenAi);
        assert_eq!(wizard.developers[0].model, "gpt-5-mini");
    }

    #[test]
    fn unavailable_ollama_keeps_a_custom_model_fallback() {
        let mut wizard = Wizard::default();
        wizard.handle_key(key(KeyCode::Enter));
        wizard.field = Field::Model;
        for _ in 0..8 {
            wizard.handle_key(key(KeyCode::Backspace));
        }
        for value in "local-code".chars() {
            wizard.handle_key(key(KeyCode::Char(value)));
        }

        assert_eq!(wizard.developers[0].model, "local-code");
    }

    #[test]
    fn one_default_output_token_limit_applies_to_every_developer() {
        let mut wizard = Wizard {
            basics_field: BasicsField::Tokens,
            ..Wizard::default()
        };
        for value in "8192".chars() {
            wizard.handle_key(key(KeyCode::Char(value)));
        }
        wizard.handle_key(key(KeyCode::Enter));

        assert_eq!(wizard.developers[0].max_output_tokens, 8192);
        assert_eq!(wizard.developers[1].max_output_tokens, 8192);
        assert_eq!(wizard.step, Step::Developer(0));
    }

    #[test]
    fn zero_output_tokens_do_not_advance() {
        let mut wizard = Wizard {
            basics_field: BasicsField::Tokens,
            ..Wizard::default()
        };
        for _ in 0..4 {
            wizard.handle_key(key(KeyCode::Backspace));
        }
        wizard.handle_key(key(KeyCode::Enter));

        assert_eq!(wizard.step, Step::Basics);
        assert!(wizard.error.is_some());
    }

    #[test]
    fn enter_accepts_all_suggested_developers_without_extra_fields() {
        let mut wizard = Wizard::default();
        wizard.handle_key(key(KeyCode::Enter));
        for _ in 0..wizard.developers.len() {
            wizard.handle_key(key(KeyCode::Enter));
        }
        assert_eq!(wizard.step, Step::Review);
    }

    #[test]
    fn openai_team_collects_and_masks_the_actual_token_once() {
        let mut wizard = Wizard::default();
        wizard.handle_key(key(KeyCode::Enter));
        for _ in 0..wizard.developers.len() {
            wizard.field = Field::Provider;
            wizard.handle_key(key(KeyCode::Right));
            wizard.handle_key(key(KeyCode::Enter));
        }
        assert_eq!(wizard.step, Step::Credential);

        wizard.handle_paste("sk-test-secret");
        let backend = ratatui::backend::TestBackend::new(100, 32);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| render(frame, &wizard)).unwrap();
        let buffer = terminal.backend().buffer();
        let rendered = (0..buffer.area.height)
            .flat_map(|y| (0..buffer.area.width).map(move |x| buffer[(x, y)].symbol()))
            .collect::<String>();
        assert!(!rendered.contains("sk-test-secret"));
        assert!(rendered.contains('•'));

        wizard.handle_key(key(KeyCode::Enter));
        assert_eq!(wizard.step, Step::Review);
    }

    #[test]
    fn existing_openai_credential_skips_the_token_step() {
        let mut wizard = Wizard::new(ModelCatalog::default(), true);
        wizard.handle_key(key(KeyCode::Enter));
        for _ in 0..wizard.developers.len() {
            wizard.field = Field::Provider;
            wizard.handle_key(key(KeyCode::Right));
            wizard.handle_key(key(KeyCode::Enter));
        }
        assert_eq!(wizard.step, Step::Review);
        assert!(wizard.openai_token.is_empty());
    }

    #[test]
    fn openai_token_can_be_skipped_without_blocking_launch() {
        let mut wizard = Wizard {
            step: Step::Credential,
            ..Wizard::default()
        };

        wizard.handle_key(key(KeyCode::Enter));

        assert_eq!(wizard.step, Step::Review);
        assert!(wizard.openai_token.is_empty());
        assert!(wizard.error.is_none());
    }

    #[test]
    fn quick_defaults_select_count_working_directory_and_token_limit() {
        let mut wizard = Wizard::default();
        assert_eq!(wizard.basics_field, BasicsField::Count);

        wizard.handle_key(key(KeyCode::Tab));
        assert_eq!(wizard.basics_field, BasicsField::WorkingDirectory);
        wizard.handle_key(key(KeyCode::Tab));
        assert_eq!(wizard.basics_field, BasicsField::Tokens);
        wizard.handle_key(key(KeyCode::Tab));
        assert_eq!(wizard.basics_field, BasicsField::Count);
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

        wizard.handle_key(key(KeyCode::Enter));
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

        wizard.handle_key(key(KeyCode::Enter));
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

        wizard.handle_key(key(KeyCode::Enter));
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

        wizard.handle_key(key(KeyCode::Enter));
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
                .is_some_and(|error| error.starts_with("Cannot open that workspace:"))
        );
    }

    #[test]
    fn leaving_the_credential_step_discards_the_token() {
        let mut wizard = Wizard {
            step: Step::Credential,
            openai_token: "sk-discard-me".into(),
            ..Wizard::default()
        };
        wizard.handle_key(key(KeyCode::Esc));
        assert!(wizard.openai_token.is_empty());
        assert_eq!(wizard.step, Step::Developer(0));
    }
}
