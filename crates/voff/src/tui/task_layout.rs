//! Resizable task panes; tab order and the team graph remain independent.
use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Rect;

const DEFAULT_RATIO: f32 = 0.35;

pub(super) struct TaskLayout {
    preferred_ratio: f32,
    area: Rect,
    divider: Rect,
    dragging: bool,
}

impl Default for TaskLayout {
    fn default() -> Self {
        Self {
            preferred_ratio: DEFAULT_RATIO,
            area: Rect::default(),
            divider: Rect::default(),
            dragging: false,
        }
    }
}

impl TaskLayout {
    pub fn panes(&mut self, area: Rect) -> [Rect; 2] {
        if self.area != area {
            self.cancel_drag();
        }
        self.area = area;
        // Keep a complete task row and useful Inspector content when space permits.
        // Do not overwrite the user's preference when temporarily shrinking.
        let task_height = if area.height >= 10 {
            ((self.preferred_ratio * f32::from(area.height)).round() as u16)
                .clamp(4, area.height - 6)
        } else {
            area.height.div_ceil(2)
        };
        let tasks = Rect::new(area.x, area.y, area.width, task_height);
        let inspector = Rect::new(
            area.x,
            tasks.bottom(),
            area.width,
            area.height - task_height,
        );
        self.divider = Rect::new(
            tasks.x,
            tasks.bottom().saturating_sub(1),
            tasks.width,
            u16::from(tasks.height > 0),
        );
        [tasks, inspector]
    }

    pub fn adjust(&mut self, steps: i8) {
        self.preferred_ratio = (self.preferred_ratio + f32::from(steps) * 0.05).clamp(0.1, 0.9);
    }

    pub fn reset(&mut self) {
        self.preferred_ratio = DEFAULT_RATIO;
        self.cancel_drag();
    }

    pub fn cancel_drag(&mut self) {
        self.dragging = false;
    }

    pub fn dragging(&self) -> bool {
        self.dragging
    }

    pub fn handle_mouse(&mut self, event: MouseEvent) -> bool {
        match event.kind {
            MouseEventKind::Down(MouseButton::Left)
                if self.divider.contains((event.column, event.row).into()) =>
            {
                self.dragging = true;
                true
            }
            MouseEventKind::Drag(MouseButton::Left) if self.dragging => {
                if self.area.height > 0 {
                    let height = event.row.saturating_add(1).saturating_sub(self.area.y);
                    self.preferred_ratio =
                        (f32::from(height) / f32::from(self.area.height)).clamp(0.1, 0.9);
                }
                true
            }
            MouseEventKind::Up(MouseButton::Left) if self.dragging => {
                self.cancel_drag();
                true
            }
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyModifiers;

    #[test]
    fn resizing_preserves_preference_and_bounds_both_panes() {
        let mut layout = TaskLayout::default();
        let area = Rect::new(2, 3, 100, 40);
        let original = layout.panes(area);
        layout.adjust(4);
        let resized = layout.panes(area);
        assert!(resized[0].height > original[0].height);
        for height in 0..100 {
            let [tasks, inspector] = layout.panes(Rect::new(2, 3, 20, height));
            assert_eq!(tasks.height + inspector.height, height);
            assert_eq!(tasks.bottom(), inspector.y);
            if height >= 10 {
                assert!(tasks.height >= 4 && inspector.height >= 6);
            }
        }
        assert_eq!(layout.panes(area), resized);
        layout.reset();
        assert_eq!(layout.panes(area), original);
    }

    #[test]
    fn dragging_only_starts_on_the_divider_and_stops_on_release() {
        let mut layout = TaskLayout::default();
        let area = Rect::new(0, 2, 80, 40);
        let original = layout.panes(area);
        let mouse = |kind, row| MouseEvent {
            kind,
            column: 20,
            row,
            modifiers: KeyModifiers::NONE,
        };
        assert!(!layout.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 3)));
        assert!(layout.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            original[0].bottom() - 1
        )));
        assert!(layout.handle_mouse(mouse(MouseEventKind::Drag(MouseButton::Left), 25)));
        assert!(layout.panes(area)[0].height > original[0].height);
        assert!(layout.handle_mouse(mouse(MouseEventKind::Up(MouseButton::Left), 25)));
        assert!(!layout.handle_mouse(mouse(MouseEventKind::Drag(MouseButton::Left), 30)));
        assert!(!layout.dragging());
    }
}
