use super::sanitize;
use crate::theme;
use crossterm::event::{MouseEvent, MouseEventKind};
use rataflow::{
    ControlsAction, Edge, EdgeMarker, EdgeStyle, FitViewOptions, Flow, FlowEvent, Handle,
    HandlePosition, Node, NodeContent, NodeRenderContext, Position, Reconnectable, StepEdge,
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
    max_width: f64,
    identity: String,
    provider: String,
    model: String,
    detail: String,
    response: Option<String>,
    status: &'static str,
    status_color: Color,
    role_color: Color,
    display_level: u8,
}

impl AgentCard {
    fn desired_width(&self, zoom: f64) -> u16 {
        let content_width = self
            .lines(0)
            .iter()
            .map(Line::width)
            .max()
            .unwrap_or(1)
            .saturating_add(2);
        let available = (self.max_width * zoom).round().max(3.0) as u16;
        u16::try_from(content_width)
            .unwrap_or(u16::MAX)
            .min(available)
            .max(3)
    }

    fn lines(&self, phase: usize) -> Vec<Line<'_>> {
        let status_symbol = if self.status == "working" {
            WORKING_FRAMES[phase % WORKING_FRAMES.len()]
        } else {
            status_symbol(self.status)
        };
        let status_label = match self.status {
            "working" => "LIVE · working",
            "reviewing" => "REVIEW · reporting agent",
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
                Style::default().fg(theme::MUTED),
            ),
        ]);
        let identity = Line::from(Span::styled(
            self.identity.as_str(),
            Style::default()
                .fg(self.role_color)
                .add_modifier(Modifier::BOLD),
        ));
        let mut lines = vec![identity];
        if self.display_level >= 1 {
            lines.push(status);
        }
        if self.display_level >= 2
            && let Some(response) = &self.response
        {
            lines.push(Line::from(vec![
                Span::styled("↳ ", Style::default().fg(theme::ORANGE)),
                Span::styled(response.as_str(), Style::default().fg(theme::TEXT)),
            ]));
        }
        if self.display_level >= 2 {
            lines.push(Line::from(Span::styled(
                self.model.as_str(),
                Style::default().fg(theme::MUTED),
            )));
            lines.push(Line::from(Span::styled(
                self.detail.as_str(),
                Style::default().fg(if self.status == "idle" {
                    theme::MUTED
                } else {
                    self.status_color
                }),
            )));
        }
        lines
    }

    fn desired_height(&self, width: u16) -> u16 {
        let inner = width.saturating_sub(2).max(1);
        let lines = self.lines(0);
        if lines.iter().all(|line| line.width() <= usize::from(inner)) {
            return u16::try_from(lines.len().saturating_add(2)).unwrap_or(u16::MAX);
        }
        let max_height = lines
            .iter()
            .map(Line::width)
            .sum::<usize>()
            .saturating_add(lines.len())
            .clamp(1, 128) as u16;
        let area = Rect::new(0, 0, inner, max_height);
        let mut buffer = Buffer::empty(area);
        Paragraph::new(Text::from(lines))
            .wrap(Wrap { trim: true })
            .render(area, &mut buffer);
        let used = (0..max_height)
            .rfind(|&row| (0..inner).any(|column| buffer[(column, row)].symbol() != " "))
            .map_or(1, |row| row.saturating_add(1));
        used.saturating_add(2)
    }
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
            Style::default().fg(theme::BORDER)
        };
        let lines = self.lines(ctx.animation_phase);
        Paragraph::new(Text::from(lines))
            .wrap(Wrap { trim: true })
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_type(BorderType::Rounded)
                    .style(if ctx.selected {
                        theme::elevated()
                    } else {
                        theme::surface()
                    })
                    .border_style(border_style),
            )
            .render(ctx.area, buf);
    }
}

pub(super) struct TeamFlow {
    flow: Flow<AgentCard, StepEdge>,
    agent_ids: Vec<AgentId>,
    mouse_highlight: Option<AgentId>,
    layout_pending: bool,
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
            layout_pending: false,
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
                    let max_width = content.max_width;
                    *content = agent_card(
                        agent,
                        snapshot.tasks,
                        snapshot.approvals,
                        snapshot.responses,
                        snapshot.provider_kinds,
                    );
                    content.max_width = max_width;
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
            let reviewer = task
                .parent
                .and_then(|id| snapshot.tasks.iter().find(|parent| parent.id == id))
                .map(|parent| parent.owner)
                .or(snapshot.team_lead);
            for owner in [Some(task.owner), reviewer].into_iter().flatten() {
                if let Some(card) = self.flow.node_content_mut(&node_id(owner)) {
                    card.status = "reviewing";
                    card.status_color = theme::AQUA;
                    card.detail = caption.clone();
                    card.response = Some(if Some(owner) == reviewer {
                        format!("Reviewing {}", super::short_task_id(*task_id))
                    } else {
                        "→ Reporting agent for decision".into()
                    });
                }
            }
        }
        self.resize_cards();
        let mut edges = build_edges(
            snapshot.agents,
            snapshot.tasks,
            snapshot.approvals,
            snapshot.team_lead,
            snapshot.agents.get(selected).map(|agent| agent.id),
            snapshot.reviews,
        );
        for edge in &mut edges {
            let (Some(source), Some(target)) =
                (self.flow.node(&edge.source), self.flow.node(&edge.target))
            else {
                continue;
            };
            let source_rect = rataflow::Rect::from_coords(
                source.position.x,
                source.position.y,
                source.width,
                source.height,
            );
            let target_rect = rataflow::Rect::from_coords(
                target.position.x,
                target.position.y,
                target.width,
                target.height,
            );
            let (source_side, target_side) = nearest_sides(&source_rect, &target_rect);
            edge.source_handle = Some(source_side.side_name().into());
            edge.target_handle = Some(target_side.side_name().into());
        }
        if self.flow.set_edges(edges).is_err() {
            self.flow.retain_edges(|_| false);
        }
    }

    fn resize_cards(&mut self) {
        let zoom = self.flow.viewport.zoom.max(0.25);
        for id in &self.agent_ids {
            let id = node_id(*id);
            let Some(node) = self.flow.node(&id) else {
                continue;
            };
            let width = node.width;
            let position = node.position;
            let previous_height = node.height;
            let level = if zoom < 0.5 {
                0
            } else if zoom < 0.8 {
                1
            } else {
                2
            };
            let Some(card) = self.flow.node_content_mut(&id) else {
                continue;
            };
            card.display_level = level;
            let terminal_width = card.desired_width(zoom);
            let desired = card.desired_height(terminal_width);
            let new_width = (f64::from(terminal_width) / zoom).min(card.max_width);
            let actual = self.flow.node_terminal_rect(&id).map_or(
                (previous_height * zoom).round() as i32,
                |(_, top, _, bottom)| bottom - top,
            );
            let height = if actual != i32::from(desired) {
                previous_height + f64::from(i32::from(desired) - actual) / zoom
            } else {
                previous_height
            };
            if (new_width - width).abs() > f64::EPSILON
                || (height - previous_height).abs() > f64::EPSILON
            {
                self.flow
                    .set_node_dimensions(&id, new_width, height.max(1.0));
                if (new_width - width).abs() > f64::EPSILON {
                    self.flow.set_node_position(
                        &id,
                        Position::new(position.x + (width - new_width) / 2.0, position.y),
                    );
                }
            }
        }
    }

    fn rebuild_nodes(&mut self, snapshot: &TeamSnapshot<'_>, selected: usize, area: Rect) {
        let existing_positions = self
            .flow
            .nodes()
            .map(|node| (node.id.clone(), node.position))
            .collect::<HashMap<_, _>>();
        let layout = node_layout(area, snapshot.agents);
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
                    AgentCard {
                        max_width: width,
                        ..agent_card(
                            agent,
                            snapshot.tasks,
                            snapshot.approvals,
                            snapshot.responses,
                            snapshot.provider_kinds,
                        )
                    },
                )
                .with_selected(index == selected)
                .with_deletable(false)
                .with_connectable(false)
                .with_handles(
                    HandlePosition::ALL
                        .into_iter()
                        .flat_map(|side| {
                            [Handle::source(side), Handle::target(side)]
                                .map(|handle| handle.with_id(side.side_name()).with_hidden(true))
                        })
                        .collect(),
                )
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
            if snapshot.agents.len() > 1 {
                self.flow
                    .request_fit_view_with_options(FitViewOptions::default().with_max_zoom(1.0));
            } else {
                self.flow.request_fit_view();
            }
            self.layout_pending = true;
        }
    }

    pub fn render(&mut self, frame: &mut ratatui::Frame<'_>, area: Rect) {
        if self.layout_pending {
            let mut scratch = Buffer::empty(area);
            (&mut self.flow).render(area, &mut scratch);
            self.resize_cards();
            self.layout_pending = false;
        }
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

    pub fn node_at(&self, x: u16, y: u16) -> Option<usize> {
        self.agent_ids
            .iter()
            .enumerate()
            .rev()
            .find_map(|(index, id)| {
                let (left, top, right, bottom) = self.flow.node_terminal_rect(&node_id(*id))?;
                let (x, y) = (i32::from(x), i32::from(y));
                (x >= left && x < right && y >= top && y < bottom).then_some(index)
            })
    }

    #[cfg(test)]
    pub fn node_position(&self, index: usize) -> Option<Position> {
        let agent_id = self.agent_ids.get(index)?;
        self.flow
            .node(&node_id(*agent_id))
            .map(|node| node.position)
    }

    #[cfg(test)]
    pub fn zoom_level(&self) -> f64 {
        self.flow.viewport.zoom
    }

    #[cfg(test)]
    pub fn set_node_position_for_test(&mut self, index: usize, position: Position) {
        if let Some(agent_id) = self.agent_ids.get(index) {
            self.flow.set_node_position(&node_id(*agent_id), position);
        }
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

fn nearest_sides(
    source: &rataflow::Rect,
    target: &rataflow::Rect,
) -> (HandlePosition, HandlePosition) {
    (
        HandlePosition::facing(source, target.center()),
        HandlePosition::facing(target, source.center()),
    )
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
    let name = sanitize(&agent.name);
    let identity = format!("{} {name}", role_icon(&agent.role));
    AgentCard {
        max_width: 28.0,
        identity,
        provider: match provider_kind {
            "codex_cli" => "Codex · CLI",
            "claude_cli" => "Claude · CLI",
            "ollama" => "Ollama · local",
            "openai" => "OpenAI · cloud",
            _ => provider_kind,
        }
        .into(),
        model,
        detail,
        response,
        status,
        status_color,
        role_color: role_color(&agent.role),
        display_level: 2,
    }
}

fn node_id(agent_id: AgentId) -> String {
    agent_id.to_string()
}

fn build_edges(
    agents: &[AgentProfile],
    tasks: &[Task],
    approvals: &[Approval],
    _team_lead: Option<AgentId>,
    highlighted_agent: Option<AgentId>,
    reviews: &HashMap<TaskId, String>,
) -> Vec<Edge<StepEdge>> {
    let mut edges = Vec::new();
    let mut returning_edges = Vec::new();
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
            let returning = tasks.iter().any(|task| {
                task.owner == target.id
                    && reviews.contains_key(&task.id)
                    && task
                        .parent
                        .and_then(|parent| tasks.iter().find(|candidate| candidate.id == parent))
                        .is_some_and(|parent| parent.owner == source.id)
            });
            let style = edge_presentation(
                task,
                pending.as_ref(),
                !target.enabled,
                highlighted_agent.is_some_and(|id| id == source.id || id == target.id),
                returning,
            );
            let edge = routing_edge(
                format!("handoff:{}:{}", source.id, target.id),
                if returning { target.id } else { source.id },
                if returning { source.id } else { target.id },
                style,
            );
            if returning {
                returning_edges.push(edge);
            } else {
                edges.push(edge);
            }
        }
    }
    edges.extend(returning_edges);
    edges
}

fn routing_edge(id: String, source: AgentId, target: AgentId, style: EdgeStyle) -> Edge<StepEdge> {
    Edge::new(id, node_id(source), node_id(target))
        .with_content(StepEdge::default().with_stem_length(1.0).with_style(style))
        .with_deletable(false)
        .with_selectable(false)
        .with_reconnectable(Reconnectable::None)
}

fn edge_presentation(
    task: Option<&Task>,
    pending: Option<&DelegationProposal>,
    disabled: bool,
    highlighted: bool,
    returning: bool,
) -> EdgeStyle {
    if highlighted {
        return solid_edge_style(
            if returning {
                theme::AQUA
            } else {
                theme::YELLOW
            },
            task.is_some_and(|task| task.status == TaskStatus::Running) || returning,
        );
    }
    let color = if disabled {
        theme::BORDER
    } else if returning || pending.is_some() || task.is_some() {
        theme::MUTED
    } else {
        theme::BORDER
    };
    EdgeStyle::dotted()
        .with_stroke_style(Style::default().fg(color))
        .with_label_style(Style::default().fg(color))
}

fn solid_edge_style(color: Color, live: bool) -> EdgeStyle {
    EdgeStyle::default()
        .with_stroke_style(Style::default().fg(color).add_modifier(Modifier::BOLD))
        .with_label_style(Style::default().fg(color).add_modifier(Modifier::BOLD))
        .with_marker_start(if live {
            EdgeMarker::Circle
        } else {
            EdgeMarker::Diamond
        })
        .with_marker_end(EdgeMarker::ArrowClosed)
        .with_line_chars('─', '│')
        .with_corner_chars(['┌', '┐', '└', '┘'])
}

fn node_layout(area: Rect, agents: &[AgentProfile]) -> Vec<(Position, f64, f64)> {
    let count = agents.len();
    if count == 0 {
        return Vec::new();
    }
    let depths = (0..count)
        .map(|index| {
            let mut current = index;
            let mut depth = 0_usize;
            for _ in 0..count {
                let parent = agents
                    .iter()
                    .position(|agent| agent.allowed_targets.contains(&agents[current].name));
                let Some(parent) = parent else {
                    break;
                };
                if parent == current {
                    break;
                }
                depth += 1;
                current = parent;
            }
            depth.min(count.saturating_sub(1))
        })
        .collect::<Vec<_>>();
    let card_width = area.width.clamp(1, 28);
    let peer_gap = area
        .width
        .saturating_sub(card_width.saturating_mul(2))
        .min(20);
    let columns = usize::from(
        (area.width.saturating_add(peer_gap) / card_width.saturating_add(peer_gap).max(1)).max(1),
    )
    .min(count);
    let max_depth = depths.iter().copied().max().unwrap_or(0);
    let layer_counts = (0..=max_depth)
        .map(|depth| {
            depths
                .iter()
                .filter(|candidate| **candidate == depth)
                .count()
        })
        .collect::<Vec<_>>();
    let total_rows = layer_counts
        .iter()
        .map(|count| count.div_ceil(columns))
        .sum::<usize>()
        .max(1);
    let card_height = (area.height / u16::try_from(total_rows).unwrap_or(u16::MAX).max(1))
        .clamp(3, 7)
        .min(area.height);
    let row_gaps = total_rows.saturating_sub(1);
    let row_gap = if row_gaps == 0 {
        0
    } else {
        usize::from(area.height)
            .saturating_sub(total_rows.saturating_mul(usize::from(card_height)))
            .checked_div(row_gaps)
            .unwrap_or(0)
            .min(19)
    };
    (0..count)
        .map(|index| {
            let depth = depths[index];
            let rank = depths[..index]
                .iter()
                .filter(|candidate| **candidate == depth)
                .count();
            let row = layer_counts[..depth]
                .iter()
                .map(|count| count.div_ceil(columns))
                .sum::<usize>()
                + rank / columns;
            let column = rank % columns;
            let peers = layer_counts[depth].min(columns);
            let group_width = usize::from(card_width)
                .saturating_mul(peers)
                .saturating_add(usize::from(peer_gap).saturating_mul(peers.saturating_sub(1)));
            let group_left = usize::from(area.width).saturating_sub(group_width) / 2;
            let x = u16::try_from(group_left.saturating_add(
                column.saturating_mul(usize::from(card_width.saturating_add(peer_gap))),
            ))
            .unwrap_or(u16::MAX);
            let y = u16::try_from(row).unwrap_or(u16::MAX).saturating_mul(
                card_height.saturating_add(u16::try_from(row_gap).unwrap_or(u16::MAX)),
            );
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
        return ("disabled", theme::TEXT);
    }
    let statuses = tasks
        .iter()
        .filter(|task| task.owner == agent.id)
        .map(|task| task.status)
        .collect::<Vec<_>>();
    for (status, label, color) in [
        (TaskStatus::WaitingApproval, "approval", theme::YELLOW),
        (TaskStatus::Running, "working", theme::GREEN),
        (TaskStatus::Ready, "ready", theme::AQUA),
        (TaskStatus::Queued, "queued", theme::BLUE),
        (TaskStatus::Blocked, "blocked", theme::RED),
        (TaskStatus::Failed, "failed", theme::RED),
        (TaskStatus::Completed, "complete", theme::GREEN),
        (TaskStatus::Cancelled, "cancelled", theme::RED),
    ] {
        if statuses.contains(&status) {
            return (label, color);
        }
    }
    if pending {
        return ("approval", theme::YELLOW);
    }
    ("idle", theme::TEXT)
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
        "team_lead" | "technical_lead" => theme::ORANGE,
        "frontend_agent" | "frontend_developer" => theme::AQUA,
        "backend_agent" | "backend_developer" => theme::YELLOW,
        "reviewer_agent" | "code_reviewer" => theme::PURPLE,
        _ => theme::TEXT,
    }
}

pub(super) fn role_icon(role: &str) -> &'static str {
    match role {
        "team_lead" | "technical_lead" => "◆",
        "frontend_agent" | "frontend_developer" => "▣",
        "backend_agent" | "backend_developer" => "⬢",
        "reviewer_agent" | "code_reviewer" => "◇",
        _ => "●",
    }
}

const fn plural_suffix(count: usize) -> &'static str {
    if count == 1 { "" } else { "s" }
}
#[cfg(test)]
mod geometry_tests {
    use super::*;

    fn agent(name: &str, targets: Vec<String>) -> AgentProfile {
        AgentProfile {
            id: AgentId::new(),
            name: name.into(),
            role: "agent".into(),
            provider: "codex_cli".into(),
            model: "default".into(),
            system_prompt: String::new(),
            tools: Vec::new(),
            max_output_tokens: 16_384,
            enabled: true,
            allowed_targets: targets,
            delegation_limits: None,
        }
    }

    #[test]
    fn three_agent_startup_layout_centers_a_compact_reporting_tree() {
        let agents = [
            agent(
                "Team Lead",
                vec!["Backend Agent".into(), "Frontend Agent".into()],
            ),
            agent("Backend Agent", vec![]),
            agent("Frontend Agent", vec![]),
        ];
        let layout = node_layout(Rect::new(0, 0, 160, 40), &agents);
        assert_eq!(layout[0].0, Position::new(66.0, 0.0));
        assert_eq!(layout[1].0, Position::new(42.0, 26.0));
        assert_eq!(layout[2].0, Position::new(90.0, 26.0));
        assert_eq!(layout[1].2, layout[2].2);
        let lead = rataflow::Rect::new(
            layout[0].0,
            rataflow::Dimensions::new(layout[0].1, layout[0].2),
        );
        for child in &layout[1..] {
            let child = rataflow::Rect::new(child.0, rataflow::Dimensions::new(child.1, child.2));
            assert_eq!(
                nearest_sides(&lead, &child),
                (HandlePosition::Bottom, HandlePosition::Top)
            );
        }

        let narrow = node_layout(Rect::new(0, 0, 80, 40), &agents);
        assert_eq!(narrow[0].0, Position::new(26.0, 0.0));
        assert_eq!(narrow[1].0, Position::new(2.0, 26.0));
        assert_eq!(narrow[2].0, Position::new(50.0, 26.0));
    }

    #[test]
    fn ports_follow_relative_card_position() {
        let lead = rataflow::Rect::from_coords(20.0, 20.0, 20.0, 8.0);
        let beside = rataflow::Rect::from_coords(43.0, 20.0, 20.0, 8.0);
        let above = rataflow::Rect::from_coords(20.0, 8.0, 20.0, 8.0);
        assert_eq!(
            nearest_sides(&lead, &beside),
            (HandlePosition::Right, HandlePosition::Left)
        );
        assert_eq!(
            nearest_sides(&lead, &above),
            (HandlePosition::Top, HandlePosition::Bottom)
        );
        assert_eq!(
            nearest_sides(&above, &lead),
            (HandlePosition::Bottom, HandlePosition::Top)
        );
    }

    #[test]
    fn selected_connections_are_solid_and_unselected_connections_are_dotted() {
        let selected = edge_presentation(None, None, false, true, false);
        let unselected = edge_presentation(None, None, false, false, false);
        assert_eq!(selected, solid_edge_style(theme::YELLOW, false));
        assert_eq!(
            unselected,
            EdgeStyle::dotted()
                .with_stroke_style(Style::default().fg(theme::BORDER))
                .with_label_style(Style::default().fg(theme::BORDER))
        );
    }

    #[test]
    fn card_height_tracks_wrapping_and_zoom_detail_level() {
        let mut card = AgentCard {
            max_width: 28.0,
            identity: "⬢ Backend Agent".into(),
            provider: "Codex · CLI".into(),
            model: "Model · CLI automatic".into(),
            detail: "✓ Implement calculator UI".into(),
            response: None,
            status: "complete",
            status_color: theme::GREEN,
            role_color: theme::YELLOW,
            display_level: 2,
        };
        assert_eq!(card.desired_height(28), 6);
        let full_width = card.desired_width(2.0);
        assert!(full_width <= 28);
        card.detail = "idle".into();
        card.model = "Model · default".into();
        assert!(card.desired_width(2.0) <= full_width);
        card.detail = "✓ Implement calculator UI".into();
        card.response = Some("Reviewing generated code".into());
        assert!(card.desired_height(28) > 6);
        assert!(card.desired_height(12) > card.desired_height(28));
        card.display_level = 0;
        assert!(card.desired_height(12) < 6);
    }
}
