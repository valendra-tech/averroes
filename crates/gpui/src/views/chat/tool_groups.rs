//! Pure state and presentation helpers for grouping tool activity in a
//! conversation.  Keeping this independent from GPUI makes the stream rules
//! easy to test and prevents rendering concerns from leaking into event
//! handling.

use std::collections::HashMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ToolGroupEvent {
    Tool { inside_reasoning: bool },
    AssistantText,
    Reasoning,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct ToolGroupTracker {
    next_group_id: usize,
    active_group_id: Option<usize>,
}

impl ToolGroupTracker {
    pub(crate) fn from_persisted_group_ids(group_ids: impl IntoIterator<Item = usize>) -> Self {
        let next_group_id = group_ids
            .into_iter()
            .max()
            .map(|group_id| group_id.saturating_add(1))
            .unwrap_or(0);
        Self {
            next_group_id,
            active_group_id: None,
        }
    }

    pub(crate) fn active_group_id(&self) -> Option<usize> {
        self.active_group_id
    }

    pub(crate) fn apply(&mut self, event: ToolGroupEvent) -> Option<usize> {
        match event {
            ToolGroupEvent::Tool {
                inside_reasoning: true,
            }
            | ToolGroupEvent::Reasoning => None,
            ToolGroupEvent::Tool {
                inside_reasoning: false,
            } => Some(self.active_group_id.unwrap_or_else(|| {
                let group_id = self.next_group_id;
                self.next_group_id = self.next_group_id.saturating_add(1);
                self.active_group_id = Some(group_id);
                group_id
            })),
            ToolGroupEvent::AssistantText => {
                self.active_group_id = None;
                None
            }
        }
    }

    pub(crate) fn close_on_assistant_text(&mut self) {
        self.apply(ToolGroupEvent::AssistantText);
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ToolNameCount {
    pub(crate) name: String,
    pub(crate) count: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ToolGroupRenderMode {
    /// The live group stays compact, but `hidden_count` tells the UI to keep
    /// an explicit affordance for revealing the calls that preceded `index`.
    Latest {
        index: usize,
        hidden_count: usize,
    },
    Expanded,
    Collapsed,
}

impl ToolGroupRenderMode {
    pub(crate) fn for_group(
        expanded: bool,
        active_group_id: Option<usize>,
        group_id: usize,
        activity_indices: &[usize],
    ) -> Self {
        let Some(index) = activity_indices.last().copied() else {
            return Self::Collapsed;
        };
        if expanded {
            Self::Expanded
        } else if active_group_id == Some(group_id) {
            Self::Latest {
                index,
                hidden_count: activity_indices.len().saturating_sub(1),
            }
        } else {
            Self::Collapsed
        }
    }
}

/// Assigns group IDs to tool events. Non-tool events return `None`; reasoning
/// tools also return `None` because they belong to the reasoning spoiler and
/// must never create an independent visible group.
#[cfg(test)]
pub(crate) fn group_ids_for_events(events: &[ToolGroupEvent]) -> Vec<Option<usize>> {
    let mut tracker = ToolGroupTracker::default();
    events.iter().map(|event| tracker.apply(*event)).collect()
}

/// Counts names in first-seen order so the UI can render a stable, compact
/// summary without retaining or displaying tool arguments and outputs.
pub(crate) fn summarize_tool_names(names: &[&str]) -> Vec<ToolNameCount> {
    let mut indexes = HashMap::<&str, usize>::new();
    let mut summary: Vec<ToolNameCount> = Vec::new();
    for name in names {
        if let Some(index) = indexes.get(name).copied() {
            summary[index].count = summary[index].count.saturating_add(1);
        } else {
            indexes.insert(name, summary.len());
            summary.push(ToolNameCount {
                name: (*name).to_owned(),
                count: 1,
            });
        }
    }
    summary
}

#[cfg(test)]
mod tests {
    use super::{group_ids_for_events, summarize_tool_names, ToolGroupEvent, ToolGroupRenderMode};

    #[test]
    fn assistant_text_closes_the_current_group() {
        assert_eq!(
            group_ids_for_events(&[
                ToolGroupEvent::Tool {
                    inside_reasoning: false,
                },
                ToolGroupEvent::Tool {
                    inside_reasoning: false,
                },
                ToolGroupEvent::AssistantText,
                ToolGroupEvent::Tool {
                    inside_reasoning: false,
                },
            ]),
            vec![Some(0), Some(0), None, Some(1)]
        );
    }

    #[test]
    fn reasoning_does_not_close_or_create_a_visible_group() {
        assert_eq!(
            group_ids_for_events(&[
                ToolGroupEvent::Tool {
                    inside_reasoning: false,
                },
                ToolGroupEvent::Reasoning,
                ToolGroupEvent::Tool {
                    inside_reasoning: true,
                },
                ToolGroupEvent::Reasoning,
                ToolGroupEvent::Tool {
                    inside_reasoning: false,
                },
            ]),
            vec![Some(0), None, None, None, Some(0)]
        );
    }

    #[test]
    fn summary_counts_names_in_first_seen_order() {
        assert_eq!(
            summarize_tool_names(&["file_read", "file_read", "web_search", "file_read"]),
            vec![
                super::ToolNameCount {
                    name: "file_read".into(),
                    count: 3,
                },
                super::ToolNameCount {
                    name: "web_search".into(),
                    count: 1,
                },
            ]
        );
    }

    #[test]
    fn active_group_keeps_an_expandable_path_to_older_calls() {
        assert_eq!(
            ToolGroupRenderMode::for_group(false, Some(4), 4, &[0, 1, 2]),
            ToolGroupRenderMode::Latest {
                index: 2,
                hidden_count: 2,
            }
        );
    }

    #[test]
    fn closed_group_is_collapsed_until_the_user_expands_it() {
        assert_eq!(
            ToolGroupRenderMode::for_group(false, None, 4, &[0, 1, 2]),
            ToolGroupRenderMode::Collapsed
        );
        assert_eq!(
            ToolGroupRenderMode::for_group(true, Some(4), 4, &[0, 1, 2]),
            ToolGroupRenderMode::Expanded
        );
    }
}
