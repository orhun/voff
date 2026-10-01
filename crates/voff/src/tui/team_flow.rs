use super::sanitize;
use crossterm::event::{MouseEvent, MouseEventKind};
use rataflow::{
    ControlsAction, Edge, EdgeMarker, EdgeStyle, Flow, FlowEvent, HandlePosition, Node,
    NodeContent, NodeRenderContext, Position, Reconnectable, StepEdge,
};
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, BorderType, Borders, Paragraph, Widget, Wrap},
};
use std::collections::HashMap;
use voff_core::{AgentId, AgentProfile, Approval, ApprovalDecision, Task, TaskId, TaskStatus};
use voff_engine::DelegationProposal;

const WORKING_FRAMES: [&str; 4] = ["◒", "◐", "◓", "◑"];

#[derive(Clone, Debug)]
struct AgentCard {
    identity: String,
    provider: String,
    model: String,
    detail: String,
    response: Option<String>,
    status: &'static str,
    status_color: Color,
    role_color: Color,
}

impl NodeContent for AgentCard {
    fn render(&self, ctx: &NodeRenderContext<'_>, buf: &mut Buffer) {
        let live_pulse =
            matches!(self.status, "working" | "reviewing") && ctx.animation_phase % 8 < 4;
        let border_style = if ctx.selected || live_pulse {
            Style::default()
                .fg(if live_pulse {
                    self.status_color
                } else {
                    self.role_color
                })
                .add_modifier(Modifier::BOLD)
        } else if matches!(self.status, "approval" | "ready" | "queued") {
            Style::default().fg(self.status_color)
        } else {
            Style::default().fg(Color::DarkGray)
        };
        let status_symbol = if self.status == "working" {
            WORKING_FRAMES[ctx.animation_phase % WORKING_FRAMES.len()]
        } else {
            status_symbol(self.status)
        };
        let status_label = match self.status {
            "working" => "LIVE · working",
            "reviewing" => "REVIEW · Team Lead",
            "approval" => "GATE · approval",
            "ready" | "queued" => "ASSIGNED",
            value => value,
        };
        let status = Line::from(vec![
            Span::styled(
                format!("{status_symbol} {status_label}"),
                Style::default()
                    .fg(self.status_color)
                    .add_modifier(if self.status == "working" {
                        Modifier::BOLD
                    } else {
                        Modifier::empty()
                    }),
            ),
            Span::styled(
                format!("  {}", self.provider),
                Style::default().fg(Color::DarkGray),
            ),
        ]);
        let identity = Line::from(Span::styled(
            self.identity.as_str(),
            Style::default()
                .fg(self.role_color)
                .add_modifier(Modifier::BOLD),
        ));
        let mut lines = if ctx.area.height <= 3 {
            vec![identity]
        } else {
            vec![identity, status]
        };
        if ctx.area.height >= 5
            && let Some(response) = &self.response
        {
            lines.push(Line::from(vec![
                Span::styled("↳ ", Style::default().fg(Color::DarkGray)),
                Span::styled(response.as_str(), Style::default().fg(Color::Gray)),
            ]));
        }
        if ctx.area.height >= 6 {
            lines.push(Line::from(Span::styled(
                self.model.as_str(),
                Style::default().fg(Color::LightCyan),
            )));
        }
        if ctx.area.height >= 7 {
            lines.push(Line::from(Span::styled(
                self.detail.as_str(),
                Style::default().fg(if self.status == "idle" {
                    Color::DarkGray
                } else {
                    self.status_color
                }),
            )));
        }
        Paragraph::new(Text::from(lines))
            .wrap(Wrap { trim: true })
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_type(BorderType::Rounded)
                    .border_style(border_style),
            )
            .render(ctx.area, buf);
    }
}

pub(super) struct TeamFlow {
    flow: Flow<AgentCard, StepEdge>,
    agent_ids: Vec<AgentId>,
    mouse_highlight: Option<AgentId>,
}

pub(super) struct TeamSnapshot<'a> {
    pub agents: &'a [AgentProfile],
    pub tasks: &'a [Task],
    pub approvals: &'a [Approval],
    pub reviews: &'a HashMap<TaskId, String>,
    pub responses: &'a HashMap<AgentId, String>,
    pub provider_kinds: &'a HashMap<AgentId, String>,
    pub team_lead: Option<AgentId>,
}

pub(super) struct MouseInteraction {
    pub handled: bool,
    pub selected: Option<usize>,
}

impl Default for TeamFlow {
    fn default() -> Self {
        Self {
            flow: Flow::new()
                .with_min_zoom(0.25)
                .with_max_zoom(2.0)
                .with_animation_speed(100)
                .with_deselect_on_pane_click(false),
            agent_ids: Vec::new(),
            mouse_highlight: None,
        }
    }
}

impl TeamFlow {
    pub fn sync(&mut self, snapshot: TeamSnapshot<'_>, selected: usize, area: Rect) {
        let expected_ids = snapshot
            .agents
            .iter()
            .map(|agent| agent.id)
            .collect::<Vec<_>>();
        if expected_ids != self.agent_ids {
            self.rebuild_nodes(&snapshot, selected, area);
        } else {
            for agent in snapshot.agents {
                if let Some(content) = self.flow.node_content_mut(&node_id(agent.id)) {
                    *content = agent_card(
                        agent,
                        snapshot.tasks,
                        snapshot.approvals,
                        snapshot.responses,
                        snapshot.provider_kinds,
                    );
                }
            }
            self.select(selected);
        }
        if self
            .mouse_highlight
            .is_some_and(|id| !expected_ids.contains(&id))
        {
            self.mouse_highlight = None;
        }
        for (task_id, caption) in snapshot.reviews {
            let Some(task) = snapshot.tasks.iter().find(|task| task.id == *task_id) else {
                continue;
            };
            for owner in [Some(task.owner), snapshot.team_lead].into_iter().flatten() {
                if let Some(card) = self.flow.node_content_mut(&node_id(owner)) {
                    card.status = "reviewing";
                    card.status_color = Color::LightCyan;
                    card.detail = caption.clone();
                    card.response = Some(if Some(owner) == snapshot.team_lead {
                        format!("Reviewing {}", super::short_task_id(*task_id))
                    } else {
                        "→ Team Lead for decision".into()
                    });
                }
            }
        }
        let edges = build_edges(
            snapshot.agents,
            snapshot.tasks,
            snapshot.approvals,
            snapshot.team_lead,
            self.mouse_highlight,
            snapshot.reviews,
        );
        if self.flow.set_edges(edges).is_err() {
            self.flow.retain_edges(|_| false);
        }
    }

    fn rebuild_nodes(&mut self, snapshot: &TeamSnapshot<'_>, selected: usize, area: Rect) {
        let existing_positions = self
            .flow
            .nodes()
            .map(|node| (node.id.clone(), node.position))
            .collect::<HashMap<_, _>>();
        let layout = node_layout(area, snapshot.agents.len());
        let nodes = snapshot
            .agents
            .iter()
            .enumerate()
            .map(|(index, agent)| {
                let id = node_id(agent.id);
                let (default_position, width, height) = layout[index];
                Node::new(
                    id.clone(),
                    existing_positions
                        .get(&id)
                        .copied()
                        .unwrap_or(default_position),
                    (width, height),
                    agent_card(
                        agent,
                        snapshot.tasks,
                        snapshot.approvals,
                        snapshot.responses,
                        snapshot.provider_kinds,
                    ),
                )
                .with_selected(index == selected)
                .with_deletable(false)
                .with_connectable(false)
                .with_source_position(if snapshot.team_lead.is_some_and(|lead| lead != agent.id) {
                    HandlePosition::Top
                } else {
                    HandlePosition::Bottom
                })
                .with_target_position(if snapshot.team_lead == Some(agent.id) {
                    HandlePosition::Bottom
                } else {
                    HandlePosition::Top
                })
            })
            .collect();
        if self.flow.set_nodes(nodes).is_ok() {
            self.agent_ids = snapshot.agents.iter().map(|agent| agent.id).collect();
            self.flow.request_fit_view();
        }
    }

    pub fn render(&mut self, frame: &mut ratatui::Frame<'_>, area: Rect) {
        frame.render_widget(&mut self.flow, area);
    }

    pub fn tick(&mut self, elapsed: std::time::Duration) {
        self.flow.tick_animation(elapsed);
        let _ = self.flow.tick_auto_pan(elapsed);
    }

    pub fn select(&mut self, index: usize) {
        if let Some(agent_id) = self.agent_ids.get(index) {
            self.flow.select_node(&node_id(*agent_id));
        }
    }

    pub fn fit_view(&mut self) {
        let _ = self.flow.apply_controls_action(ControlsAction::FitView);
    }

    pub fn zoom_in(&mut self) {
        let _ = self.flow.apply_controls_action(ControlsAction::ZoomIn);
    }

    pub fn zoom_out(&mut self) {
        let _ = self.flow.apply_controls_action(ControlsAction::ZoomOut);
    }

    pub fn reset_zoom(&mut self) {
        let _ = self.flow.apply_controls_action(ControlsAction::ResetZoom);
    }

    pub fn handle_mouse(&mut self, mouse: MouseEvent) -> MouseInteraction {
        let area = self.flow.canvas_area();
        let inside = area.contains((mouse.column, mouse.row).into());
        let continue_drag = self.flow.is_dragging()
            && matches!(mouse.kind, MouseEventKind::Drag(_) | MouseEventKind::Up(_));
        if !inside && !continue_drag {
            return MouseInteraction {
                handled: false,
                selected: None,
            };
        }

        let mut selected = None;
        for event in self.flow.handle_mouse_event(mouse).into_events() {
            match event {
                FlowEvent::NodeClicked { node_id } => {
                    selected = self.index_for_node(&node_id);
                }
                FlowEvent::SelectionChanged { node_ids, .. } => {
                    selected = node_ids.first().and_then(|id| self.index_for_node(id));
                }
                _ => {}
            }
        }
        if let Some(index) = selected {
            self.mouse_highlight = self.agent_ids.get(index).copied();
        }
        MouseInteraction {
            handled: true,
            selected,
        }
    }

    fn index_for_node(&self, id: &str) -> Option<usize> {
        self.agent_ids
            .iter()
            .position(|agent_id| node_id(*agent_id) == id)
    }

    #[cfg(test)]
    pub fn node_position(&self, index: usize) -> Option<Position> {
        let agent_id = self.agent_ids.get(index)?;
        self.flow
            .node(&node_id(*agent_id))
            .map(|node| node.position)
    }

    #[cfg(test)]
    pub fn connection_positions(&self, index: usize) -> Option<(HandlePosition, HandlePosition)> {
        let node = self.flow.node(&node_id(*self.agent_ids.get(index)?))?;
        Some((node.source_position, node.target_position))
    }

    #[cfg(test)]
    pub fn edges(&self) -> &[Edge<StepEdge>] {
        self.flow.edges()
    }

    #[cfg(test)]
    pub fn node_terminal_rect(&self, index: usize) -> Option<(i32, i32, i32, i32)> {
        let agent_id = self.agent_ids.get(index)?;
        self.flow.node_terminal_rect(&node_id(*agent_id))
    }

    #[cfg(test)]
    pub fn mouse_highlight(&self) -> Option<AgentId> {
        self.mouse_highlight
    }
}

fn agent_card(
    agent: &AgentProfile,
    tasks: &[Task],
    approvals: &[Approval],
    responses: &HashMap<AgentId, String>,
    provider_kinds: &HashMap<AgentId, String>,
) -> AgentCard {
    let pending = pending_delegation_for_target(approvals, &agent.name);
    let (status, status_color) = agent_status(tasks, agent, pending.is_some());
    let display_task = agent_display_task(tasks, agent.id);
    let detail = if let Some(task) = display_task {
        format!(
            "{} {}",
            task_status_marker(task.status),
            truncate_label(&sanitize(&task.objective), 24)
        )
    } else if let Some(proposal) = pending {
        format!("? {}", truncate_label(&sanitize(&proposal.objective), 24))
    } else if !agent.enabled {
        "Unavailable · no new assignments".into()
    } else {
        format!("{} token limit", agent.max_output_tokens)
    };
    let provider_kind = provider_kinds
        .get(&agent.id)
        .map_or(agent.provider.as_str(), String::as_str);
    let model = if agent.model == "default" && matches!(provider_kind, "codex_cli" | "claude_cli") {
        "Model · CLI automatic".into()
    } else {
        format!("Model · {}", sanitize(&agent.model))
    };
    let response = responses
        .get(&agent.id)
        .filter(|response| !response.trim().is_empty())
        .map(|response| {
            let excerpt = sanitize(response)
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ");
            truncate_label(&excerpt, 30)
        });
    let role = human_role(&agent.role);
    let name = sanitize(&agent.name);
    let identity = if name.eq_ignore_ascii_case(&role) {
        format!("{} {role}", role_icon(&agent.role))
    } else {
        format!("{} {role} · {name}", role_icon(&agent.role))
    };
    AgentCard {
        identity,
        provider: sanitize(&agent.provider),
        model,
        detail,
        response,
        status,
        status_color,
        role_color: role_color(&agent.role),
    }
}

fn node_id(agent_id: AgentId) -> String {
    agent_id.to_string()
}

fn build_edges(
    agents: &[AgentProfile],
    tasks: &[Task],
    approvals: &[Approval],
    team_lead: Option<AgentId>,
    highlighted_agent: Option<AgentId>,
    reviews: &HashMap<TaskId, String>,
) -> Vec<Edge<StepEdge>> {
    let mut edges = Vec::new();
    if let Some(lead_id) = team_lead {
        let Some(lead) = agents.iter().find(|agent| agent.id == lead_id) else {
            return edges;
        };
        for specialist in agents.iter().filter(|agent| agent.id != lead_id) {
            let task = delegated_task(tasks, lead.id, specialist.id);
            let pending = pending_delegation(approvals, tasks, lead.id, &specialist.name);
            if !lead.allowed_targets.contains(&specialist.name)
                && task.is_none()
                && pending.is_none()
            {
                continue;
            }
            let (assignment_style, assignment_animated) = edge_presentation(
                task,
                pending.as_ref(),
                !specialist.enabled,
                highlighted_agent.is_some_and(|id| id == lead.id || id == specialist.id),
            );
            let returning = tasks
                .iter()
                .any(|task| task.owner == specialist.id && reviews.contains_key(&task.id));
            edges.push(routing_edge(
                format!("assignment:{}:{}", lead.id, specialist.id),
                if returning { specialist.id } else { lead.id },
                if returning { lead.id } else { specialist.id },
                if returning {
                    active_edge_style(Color::LightCyan, true)
                } else {
                    assignment_style
                },
                returning || assignment_animated,
            ));
        }
        // All routes share the Lead's bottom port. Paint returning review
        // arrows last so an outgoing route's diamond cannot cover them.
        let lead_node = node_id(lead_id);
        edges.sort_by_key(|edge| edge.target == lead_node);
        return edges;
    }

    for source in agents {
        for target in agents {
            if source.id == target.id {
                continue;
            }
            let task = delegated_task(tasks, source.id, target.id);
            let pending = pending_delegation(approvals, tasks, source.id, &target.name);
            if !source.allowed_targets.contains(&target.name) && task.is_none() && pending.is_none()
            {
                continue;
            }
            let (style, animated) = edge_presentation(
                task,
                pending.as_ref(),
                !target.enabled,
                highlighted_agent.is_some_and(|id| id == source.id || id == target.id),
            );
            let edge = routing_edge(
                format!("handoff:{}:{}", source.id, target.id),
                source.id,
                target.id,
                style,
                animated,
            );
            edges.push(edge);
        }
    }
    edges
}

fn routing_edge(
    id: String,
    source: AgentId,
    target: AgentId,
    style: EdgeStyle,
    animated: bool,
) -> Edge<StepEdge> {
    Edge::new(id, node_id(source), node_id(target))
        .with_content(StepEdge::default().with_stem_length(1.0).with_style(style))
        .with_deletable(false)
        .with_selectable(false)
        .with_reconnectable(Reconnectable::None)
        .with_animated(animated)
}

fn edge_presentation(
    task: Option<&Task>,
    pending: Option<&DelegationProposal>,
    disabled: bool,
    highlighted: bool,
) -> (EdgeStyle, bool) {
    if let Some(task) = task {
        let color = if highlighted {
            Color::Yellow
        } else {
            task_status_color(task.status)
        };
        return (
            active_edge_style(color, task.status == TaskStatus::Running),
            task.status == TaskStatus::Running,
        );
    }
    if pending.is_some() {
        return (active_edge_style(Color::Yellow, false), false);
    }
    if highlighted {
        return (
            EdgeStyle::dotted()
                .with_stroke_style(
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                )
                .with_label_style(Style::default().fg(Color::Yellow))
                .with_marker_start(EdgeMarker::Diamond)
                .with_marker_end(EdgeMarker::ArrowClosed),
            false,
        );
    }
    if disabled {
        return (
            EdgeStyle::dotted()
                .with_stroke_style(Style::default().fg(Color::DarkGray))
                .with_label_style(Style::default().fg(Color::DarkGray)),
            false,
        );
    }
    (
        EdgeStyle::dotted()
            .with_stroke_style(Style::default().fg(Color::DarkGray))
            .with_label_style(Style::default().fg(Color::DarkGray)),
        false,
    )
}

fn active_edge_style(color: Color, live: bool) -> EdgeStyle {
    let style = EdgeStyle::default()
        .with_stroke_style(Style::default().fg(color).add_modifier(Modifier::BOLD))
        .with_label_style(Style::default().fg(color).add_modifier(Modifier::BOLD))
        .with_marker_start(if live {
            EdgeMarker::Circle
        } else {
            EdgeMarker::Diamond
        })
        .with_marker_end(EdgeMarker::ArrowClosed);
    if live {
        style
            .with_line_chars('═', '║')
            .with_corner_chars(['╔', '╗', '╚', '╝'])
    } else {
        style
    }
}

fn node_layout(area: Rect, count: usize) -> Vec<(Position, f64, f64)> {
    if count == 0 {
        return Vec::new();
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
            let (x, y) = if index == 0 {
                (area.width.saturating_sub(card_width) / 2, 0)
            } else {
                let worker = index - 1;
                let column = worker % columns;
                let row = worker / columns + 1;
                (
                    u16::try_from(column).unwrap_or(0) * column_width
                        + column_width.saturating_sub(card_width) / 2,
                    u16::try_from(row).unwrap_or(u16::MAX).saturating_mul(
                        card_height.saturating_add(u16::try_from(row_gap).unwrap_or(u16::MAX)),
                    ),
                )
            };
            (
                Position::new(f64::from(x), f64::from(y)),
                f64::from(card_width),
                f64::from(card_height),
            )
        })
        .collect()
}

pub(super) fn handoff_summary(tasks: &[Task], approvals: &[Approval]) -> String {
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

fn agent_status(tasks: &[Task], agent: &AgentProfile, pending: bool) -> (&'static str, Color) {
    if !agent.enabled {
        return ("disabled", Color::DarkGray);
    }
    let statuses = tasks
        .iter()
        .filter(|task| task.owner == agent.id)
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
    if pending {
        return ("approval", Color::Yellow);
    }
    ("idle", Color::DarkGray)
}

fn status_symbol(status: &str) -> &'static str {
    match status {
        "complete" => "✓",
        "failed" | "blocked" => "!",
        "approval" => "?",
        _ => "○",
    }
}

pub(super) fn role_color(role: &str) -> Color {
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

pub(super) fn role_icon(role: &str) -> &'static str {
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

const fn plural_suffix(count: usize) -> &'static str {
    if count == 1 { "" } else { "s" }
}
