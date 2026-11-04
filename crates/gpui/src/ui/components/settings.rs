use crate::i18n::Localization;
use crate::ui::{tokens, UiTheme};
use averroes_core::work::EmbeddingIndexStatus;
use gpui::prelude::FluentBuilder as _;
use gpui::{div, px, Div, IntoElement, ParentElement, SharedString, Styled};
use gpui_component::input::{Input, InputState};

pub fn settings_group(theme: UiTheme) -> Div {
    div()
        .w_full()
        .flex()
        .flex_col()
        .bg(theme.surface)
        .border_1()
        .border_color(theme.hairline)
        .rounded(px(tokens::RADIUS_SHEET))
        .overflow_hidden()
}

pub fn settings_row(
    theme: UiTheme,
    label: impl Into<SharedString>,
    hint: impl Into<SharedString>,
    control: impl IntoElement,
) -> Div {
    let hint = hint.into();
    div()
        .w_full()
        .min_h(px(72.0))
        .p(px(tokens::SPACE_16))
        .flex()
        .items_center()
        .gap(px(tokens::SPACE_12))
        .child(
            div()
                .flex_1()
                .min_w(px(0.0))
                .child(
                    div()
                        .text_size(px(tokens::TEXT_BODY))
                        .font_weight(tokens::WEIGHT_MEDIUM)
                        .child(label.into()),
                )
                .when(!hint.is_empty(), |description| {
                    description.child(
                        div()
                            .mt(px(tokens::SPACE_2))
                            .text_size(px(tokens::TEXT_SMALL))
                            .text_color(theme.muted)
                            .child(hint),
                    )
                }),
        )
        .child(control)
}

pub fn settings_card(theme: UiTheme) -> Div {
    div()
        .w_full()
        .p(px(tokens::SPACE_20))
        .bg(theme.surface)
        .border_1()
        .border_color(theme.hairline)
        .rounded(px(tokens::RADIUS_SHEET))
}

pub fn settings_input(state: &gpui::Entity<InputState>) -> Input {
    Input::new(state)
        .w_full()
        .h(px(tokens::SETTINGS_CONTROL_HEIGHT))
        .rounded(px(tokens::SPACE_8))
        .text_size(px(tokens::TEXT_BODY))
}

pub fn embedding_status_text(
    localization: &Localization,
    status: Option<&EmbeddingIndexStatus>,
) -> String {
    match status.filter(|status| status.config.is_some()) {
        Some(status) => localization.format(
            "settings.index_status",
            &[
                ("indexed", status.indexed_conversations.to_string()),
                ("total", status.total_conversations.to_string()),
                ("fragments", status.indexed_fragments.to_string()),
            ],
        ),
        None => localization.text("settings.not_configured").to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::i18n::{Locale, Localization};
    use averroes_core::{
        connection::ConnectionId,
        work::{EmbeddingConfig, EmbeddingIndexStatus},
    };

    #[test]
    fn index_status_uses_the_selected_language_and_real_counts() {
        let status = EmbeddingIndexStatus {
            config: Some(EmbeddingConfig {
                connection_id: ConnectionId("test".into()),
                model_id: "embedding".into(),
            }),
            indexed_conversations: 117,
            total_conversations: 118,
            indexed_fragments: 859,
        };
        assert_eq!(
            embedding_status_text(&Localization::new(Locale::Spanish), Some(&status)),
            "Indexadas 117 de 118 conversaciones · 859 fragmentos"
        );
        assert_eq!(
            embedding_status_text(&Localization::new(Locale::English), Some(&status)),
            "Indexed 117 of 118 conversations · 859 fragments"
        );
    }

    #[test]
    fn missing_and_unconfigured_indexes_use_the_localized_fallback() {
        for locale in [Locale::English, Locale::Spanish] {
            let localization = Localization::new(locale);
            let expected = localization.text("settings.not_configured").to_string();
            assert_eq!(embedding_status_text(&localization, None), expected);
            let unconfigured = EmbeddingIndexStatus {
                config: None,
                indexed_conversations: 0,
                total_conversations: 42,
                indexed_fragments: 0,
            };
            assert_eq!(
                embedding_status_text(&localization, Some(&unconfigured)),
                expected
            );
        }
    }
}
