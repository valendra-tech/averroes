mod app;
mod shortcuts;
mod theme;
mod ui;
mod views;

use app::AverroesApp;
use averroes_core::agent::{Agent, AgentConfig};
use averroes_core::compaction::{CompactionConfig, CompactionStrategyType};
use averroes_core::config::{AppConfig, create_provider};
use averroes_core::runtime::ResourceGovernor;
use averroes_core::tool::ToolRegistry;
use averroes_core::tool::builtin;
use gpui::*;
use shortcuts::Quit;
use std::path::PathBuf;
use std::sync::Arc;

fn create_agent() -> (Option<Arc<Agent>>, Option<String>) {
    let config = match AppConfig::load() {
        Ok(c) => c,
        Err(_) => return (None, None),
    };
    if config.needs_setup() {
        return (None, None);
    }
    match create_provider(&config) {
        Ok(provider) => {
            let tool_registry = Arc::new(ToolRegistry::new());
            builtin::register_all(&tool_registry);

            let governor = Arc::new(ResourceGovernor::new(
                config.runtime.max_concurrent_calls.unwrap_or(10),
                config.runtime.token_budget_per_minute.unwrap_or(200_000),
            ));

            let compaction_config = CompactionConfig {
                strategy: match config.compaction.strategy.as_deref() {
                    Some("trim") => CompactionStrategyType::Trim,
                    Some("summary") => CompactionStrategyType::Summary,
                    _ => CompactionStrategyType::Hybrid,
                },
                threshold: config.compaction.threshold.unwrap_or(0.8),
                ..Default::default()
            };

            let agent_config = AgentConfig {
                name: "gpui".into(),
                model: provider.default_model().to_string(),
                tools: vec![
                    "bash".into(), "file_read".into(), "file_write".into(),
                    "glob".into(), "grep".into(), "web_fetch".into(),
                ],
                max_iterations: 30,
                compaction: compaction_config,
                ..Default::default()
            };

            (Some(Arc::new(Agent::new(
                agent_config, provider, tool_registry, governor,
                "gpui-session".into(),
                std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            ))), None)
        }
        Err(e) => {
            let key_hint = match config.provider.default.as_deref() {
                Some("openai") => "OPENAI_API_KEY",
                _ => "ANTHROPIC_API_KEY",
            };
            (None, Some(format!("{}: set {} and restart", e, key_hint)))
        }
    }
}

struct RootView {
    setup: Option<Entity<views::setup_wizard::SetupWizardView>>,
    app: Option<Entity<AverroesApp>>,
    error: Option<String>,
}

impl RootView {
    fn new(cx: &mut Context<Self>) -> Self {
        let (agent, error) = create_agent();

        if let Some(agent) = agent {
            let app = cx.new(|cx| AverroesApp::new(cx, Some(agent)));
            Self { setup: None, app: Some(app), error: None }
        } else if error.is_some() {
            Self { setup: None, app: None, error }
        } else {
            let setup = cx.new(|cx| views::setup_wizard::SetupWizardView::new(cx));
            Self { setup: Some(setup), app: None, error: None }
        }
    }
}

impl Render for RootView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if let Some(ref app) = self.app {
            return app.clone().into_any_element();
        }

        if let Some(ref error) = self.error {
            return div()
                .flex()
                .flex_col()
                .size_full()
                .bg(rgb(0x1e1e2e))
                .text_color(rgb(0xcdd6f4))
                .font_family("SF Mono")
                .justify_center()
                .items_center()
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .bg(rgb(0x313244))
                        .rounded_lg()
                        .border_1()
                        .border_color(rgb(0xf38ba8))
                        .p_8()
                        .gap_4()
                        .child(
                            div()
                                .text_lg()
                                .font_weight(FontWeight::BOLD)
                                .text_color(rgb(0xf38ba8))
                                .child("Connection Error"),
                        )
                        .child(
                            div().text_sm().text_color(rgb(0x6c7086))
                                .child(error.clone()),
                        )
                        .child(
                            div()
                                .px_4()
                                .py_2()
                                .bg(rgb(0xf38ba8))
                                .text_color(rgb(0x1e1e2e))
                                .rounded_md()
                                .text_sm()
                                .font_weight(FontWeight::SEMIBOLD)
                                .cursor_pointer()
                                .id(ElementId::Name("reset-config".into()))
                                .on_click(cx.listener(|this, _event: &ClickEvent, _window, cx| {
                                    let config_dir = std::path::PathBuf::from(
                                        std::env::var("HOME").unwrap_or_else(|_| ".".into()),
                                    )
                                    .join(".config")
                                    .join("averroes");
                                    let _ = std::fs::remove_file(config_dir.join("config.toml"));
                                    let setup = cx.new(|cx| views::setup_wizard::SetupWizardView::new(cx));
                                    this.setup = Some(setup);
                                    this.error = None;
                                    cx.notify();
                                }))
                                .child("Reset Config"),
                        ),
                )
                .into_any_element();
        }

        if let Some(ref setup) = self.setup {
            let is_done = setup.read(cx).is_done();
            if is_done {
                let (agent, error) = create_agent();
                if let Some(agent) = agent {
                    let app_entity = cx.new(|cx| AverroesApp::new(cx, Some(agent)));
                    self.app = Some(app_entity);
                    self.setup = None;
                } else if let Some(err) = error {
                    self.error = Some(err);
                    self.setup = None;
                }
                cx.notify();
                return div().into_any_element();
            }
            return setup.clone().into_any_element();
        }

        div().into_any_element()
    }
}

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    Application::new().run(|cx: &mut App| {
        cx.activate(true);
        cx.bind_keys([
            KeyBinding::new("cmd-q", Quit, None),
        ]);

        cx.on_action(|_: &Quit, cx| cx.quit());

        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
                    None,
                    size(px(1200.0), px(800.0)),
                    cx,
                ))),
                titlebar: Some(TitlebarOptions {
                    title: Some("Averroes".into()),
                    ..Default::default()
                }),
                ..Default::default()
            },
            |_window, cx| cx.new(|cx| RootView::new(cx)),
        )
        .unwrap();
    });
}

#[cfg(test)]
mod ui_api_tests {
    use super::ui::{
        button, field_label, field_surface, panel, panel_with_padding, provider_card,
        status_badge, ButtonVariant, UiTheme,
    };
    use gpui::{px, rgb};

    #[test]
    fn tokenfactory_primitives_are_composable() {
        let theme = UiTheme::light();

        assert_eq!(theme.background, rgb(0xfff9f4));
        assert_eq!(theme.foreground, rgb(0x20131a));
        assert_eq!(theme.card, rgb(0xffffff));
        assert_eq!(theme.primary, rgb(0xb83a27));
        assert_eq!(theme.brand_orange, rgb(0xf15a2a));
        assert_eq!(theme.brand_coral, rgb(0xe94b2f));
        assert_eq!(theme.brand_magenta, rgb(0xd94b83));
        assert_eq!(theme.muted_foreground, rgb(0x725f5b));
        assert_eq!(theme.border, rgb(0xead8ce));
        assert_eq!(theme.accent, rgb(0xffe4d5));
        assert_eq!(theme.destructive, rgb(0xb42318));
        assert_eq!(UiTheme::RADIUS, 6.0);

        let _ = panel(theme);
        let _ = panel_with_padding(theme, px(12.0));
        let _ = button(theme, ButtonVariant::Primary, "Run");
        let _ = field_label(theme, "Provider");
        let _ = field_surface(theme, true, false);
        let _ = provider_card(theme, true);
        let _ = status_badge(theme, "Ready");
    }
}
