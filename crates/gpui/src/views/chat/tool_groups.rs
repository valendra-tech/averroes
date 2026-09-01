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
    use super::{group_ids_for_events, summarize_tool_names, ToolGroupEvent};

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
}
