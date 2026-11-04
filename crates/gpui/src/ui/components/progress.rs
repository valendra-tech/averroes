use crate::ui::{animation::animate_if, tokens, UiTheme};
use averroes_core::work::{TaskStatus, WorkTask};
use gpui::{div, px, Animation, AnyElement, Div, ElementId, Rgba, Styled, Transformation};
use gpui_component::{Icon, IconName};
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusTone {
    Neutral,
    Info,
    Success,
    Warning,
}

impl StatusTone {
    pub fn foreground(self, theme: UiTheme) -> Rgba {
        match self {
            Self::Neutral => theme.muted,
            Self::Info => theme.accent_text,
            Self::Success => theme.success_text,
            Self::Warning => theme.warning_text,
        }
    }

    pub fn background(self, theme: UiTheme) -> Rgba {
        match self {
            Self::Neutral => theme.surface_subtle,
            Self::Info => theme.accent_soft,
            Self::Success => theme.success_soft,
            Self::Warning => theme.warning_soft,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TaskProgressSummary {
    pub completed: usize,
    pub total: usize,
    pub running: usize,
    pub blocked: usize,
}

impl TaskProgressSummary {
    pub fn from_tasks(tasks: &[WorkTask]) -> Self {
        let mut summary = Self {
            total: tasks.len(),
            ..Self::default()
        };
        for task in tasks {
            match task.status {
                TaskStatus::Done => summary.completed += 1,
                TaskStatus::InProgress => summary.running += 1,
                TaskStatus::Blocked => summary.blocked += 1,
                TaskStatus::Cancelled | TaskStatus::Pending => {}
            }
        }
        summary
    }

    pub fn fraction(self) -> f32 {
        if self.total == 0 {
            0.0
        } else {
            self.completed as f32 / self.total as f32
        }
    }
}

pub struct TaskStatusPresentation {
    pub icon: IconName,
    pub label_key: &'static str,
    pub tone: StatusTone,
    pub spinning: bool,
}

impl TaskStatusPresentation {
    pub fn for_status(status: TaskStatus) -> Self {
        let (icon, label_key, tone) = match status {
            TaskStatus::Pending => (IconName::Minus, "task.state.pending", StatusTone::Neutral),
            TaskStatus::InProgress => (
                IconName::LoaderCircle,
                "task.state.running",
                StatusTone::Info,
            ),
            TaskStatus::Blocked => (IconName::Pause, "task.state.blocked", StatusTone::Warning),
            TaskStatus::Cancelled => (
                IconName::CircleX,
                "task.state.cancelled",
                StatusTone::Neutral,
            ),
            TaskStatus::Done => (IconName::Check, "task.state.completed", StatusTone::Success),
        };
        Self {
            icon,
            label_key,
            tone,
            spinning: status == TaskStatus::InProgress,
        }
    }
}

pub fn status_pill(theme: UiTheme, tone: StatusTone) -> Div {
    div()
        .flex_none()
        .h(px(22.0))
        .px(px(tokens::SPACE_8))
        .flex()
        .items_center()
        .gap(px(tokens::SPACE_4))
        .rounded_full()
        .bg(tone.background(theme))
        .text_color(tone.foreground(theme))
        .text_size(px(tokens::TEXT_CAPTION))
        .font_weight(tokens::WEIGHT_MEDIUM)
}

pub fn activity_ring(theme: UiTheme, id: impl Into<ElementId>, reduced: bool) -> AnyElement {
    animate_if(
        Icon::new(IconName::LoaderCircle)
            .size(px(tokens::ICON_SMALL))
            .text_color(theme.accent_text),
        id,
        Animation::new(Duration::from_millis(1000)).repeat(),
        !reduced,
        |icon, delta| icon.transform(Transformation::rotate(gpui::percentage(delta))),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use averroes_core::work::{TaskStatus, WorkTask};
    use gpui_component::IconNamed;

    fn task(status: TaskStatus) -> WorkTask {
        WorkTask {
            id: format!("{status:?}"),
            title: "Task".into(),
            description: None,
            parent_task_id: None,
            depends_on: Vec::new(),
            priority: Default::default(),
            status,
            created_at: 0,
            updated_at: 0,
        }
    }

    #[test]
    fn progress_counts_only_completed_tasks_and_retains_running_and_blocked_counts() {
        let tasks = [
            TaskStatus::Done,
            TaskStatus::InProgress,
            TaskStatus::Pending,
            TaskStatus::Blocked,
            TaskStatus::Cancelled,
        ]
        .map(task);
        let summary = TaskProgressSummary::from_tasks(&tasks);
        assert_eq!((summary.completed, summary.total), (1, 5));
        assert_eq!((summary.running, summary.blocked), (1, 1));
        assert_eq!(summary.fraction(), 0.2);
        assert_eq!(TaskProgressSummary::from_tasks(&[]).fraction(), 0.0);
    }

    #[test]
    fn blocked_and_cancelled_tasks_have_distinct_readable_presentations() {
        let blocked = TaskStatusPresentation::for_status(TaskStatus::Blocked);
        let cancelled = TaskStatusPresentation::for_status(TaskStatus::Cancelled);
        assert_ne!(blocked.icon.path(), cancelled.icon.path());
        assert_ne!(blocked.label_key, cancelled.label_key);
        assert_ne!(blocked.tone, cancelled.tone);
        assert!(!blocked.spinning && !cancelled.spinning);
        assert!(TaskStatusPresentation::for_status(TaskStatus::InProgress).spinning);
        for locale in [crate::i18n::Locale::English, crate::i18n::Locale::Spanish] {
            let localization = crate::i18n::Localization::new(locale);
            for status in [
                TaskStatus::Pending,
                TaskStatus::InProgress,
                TaskStatus::Blocked,
                TaskStatus::Cancelled,
                TaskStatus::Done,
            ] {
                let view = TaskStatusPresentation::for_status(status);
                assert_ne!(localization.text(view.label_key).as_ref(), view.label_key);
            }
        }
    }
}
