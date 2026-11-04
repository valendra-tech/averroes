use super::{provider_logo, tokens, UiTheme};
use averroes_core::{
    connection::{ConnectionId, ConnectionKind},
    provider::ModelInfo,
};
use gpui::{div, px, AnyElement, App, IntoElement, ParentElement, SharedString, Styled, Window};
use gpui_component::select::{SearchableVec, SelectGroup, SelectItem};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ModelChoice {
    pub connection_id: ConnectionId,
    pub connection_name: SharedString,
    pub connection_kind: ConnectionKind,
    pub info: ModelInfo,
}

impl ModelChoice {
    pub(crate) fn display_label(&self) -> SharedString {
        let name = self.info.display_name.trim();
        let name = if self.connection_kind == ConnectionKind::QDivZero {
            name.strip_prefix("unlimited:").unwrap_or(name)
        } else {
            name
        };
        if name.is_empty() {
            self.info.id.clone().into()
        } else {
            name.to_owned().into()
        }
    }
}

impl SelectItem for ModelChoice {
    type Value = Self;

    fn title(&self) -> SharedString {
        self.display_label()
    }

    fn display_title(&self) -> Option<AnyElement> {
        Some(
            div()
                .flex()
                .w_full()
                .min_w(px(0.0))
                .overflow_hidden()
                .items_center()
                .gap(px(tokens::SPACE_6))
                .text_size(px(tokens::TEXT_SMALL))
                .child(provider_logo(self.connection_kind, 14.0))
                .child(div().flex_1().min_w(px(0.0)).truncate().child(self.title()))
                .into_any_element(),
        )
    }

    fn render(&self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = UiTheme::current(cx);
        div()
            .flex()
            .items_center()
            .gap(px(tokens::SPACE_8))
            .min_w(px(0.0))
            .min_h(px(40.0))
            .child(provider_logo(self.connection_kind, 24.0))
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.0))
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .text_size(px(tokens::TEXT_BODY))
                            .truncate()
                            .child(self.display_label()),
                    )
                    .child(
                        div()
                            .text_size(px(tokens::TEXT_CAPTION))
                            .font(UiTheme::mono_font())
                            .text_color(theme.muted)
                            .truncate()
                            .child(self.info.id.clone()),
                    ),
            )
    }

    fn value(&self) -> &Self::Value {
        self
    }

    fn matches(&self, query: &str) -> bool {
        let haystack = format!(
            "{} {} {} {}",
            self.info.display_name, self.info.id, self.info.provider, self.connection_name
        )
        .to_lowercase();
        query
            .split_whitespace()
            .all(|part| haystack.contains(&part.to_lowercase()))
    }
}

pub(crate) fn grouped_model_items(
    choices: &[ModelChoice],
) -> SearchableVec<SelectGroup<ModelChoice>> {
    let mut groups: Vec<(ConnectionId, SelectGroup<ModelChoice>)> = Vec::new();
    for choice in choices {
        if let Some((_, group)) = groups
            .iter_mut()
            .find(|(id, _)| id == &choice.connection_id)
        {
            group.items.push(choice.clone());
        } else {
            groups.push((
                choice.connection_id.clone(),
                SelectGroup::new(choice.connection_name.clone()).item(choice.clone()),
            ));
        }
    }
    SearchableVec::new(
        groups
            .into_iter()
            .map(|(_, group)| group)
            .collect::<Vec<_>>(),
    )
}
