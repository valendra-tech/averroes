mod app;
mod runtime;
mod session;
mod shortcuts;
mod theme;
mod ui;
mod views;

use app::AverroesApp;
use gpui::*;
use runtime::{AgentFactory, RuntimeError};
use session::SessionManager;
use shortcuts::Quit;
use std::sync::Arc;

struct RootView {
    setup: Option<Entity<views::setup_wizard::SetupWizardView>>,
    app: Option<Entity<AverroesApp>>,
    error: Option<String>,
    factory: Option<Arc<AgentFactory>>,
    sessions: SessionManager,
}

impl RootView {
    fn new(cx: &mut Context<Self>) -> Self {
        let mut root = Self {
            setup: None,
            app: None,
            error: None,
            factory: None,
            sessions: SessionManager::new(),
        };
        root.load_factory(cx);
        root
    }

    fn load_factory(&mut self, cx: &mut Context<Self>) {
        self.app = None;
        self.factory = None;
        self.error = None;

        match AgentFactory::load() {
            Ok(factory) => self.install_factory(factory, cx),
            Err(RuntimeError::NeedsSetup) => {
                if self.setup.is_none() {
                    self.setup = Some(cx.new(|cx| {
                        views::setup_wizard::SetupWizardView::new(cx)
                    }));
                }
            }
            Err(RuntimeError::Configuration(error)) => self.show_error(error),
            Err(RuntimeError::Provider(error)) => {
                self.show_error(provider_error_message(&error));
            }
        }
    }

    fn install_factory(&mut self, factory: AgentFactory, cx: &mut Context<Self>) {
        let factory = Arc::new(factory);
        let session_id = self.sessions.active().id.clone();
        let agent = factory.new_agent(&session_id);
        let app_factory = Arc::clone(&factory);
        let app = cx.new(|cx| AverroesApp::new(cx, Some(agent), app_factory));

        self.setup = None;
        self.error = None;
        self.factory = Some(factory);
        self.app = Some(app);
    }

    fn show_error(&mut self, message: String) {
        self.setup = None;
        self.app = None;
        self.factory = None;
        self.error = Some(message);
    }
}

fn provider_error_message(error: &str) -> String {
    let key_hint = if error.to_ascii_lowercase().contains("openai") {
        "OPENAI_API_KEY"
    } else {
        "ANTHROPIC_API_KEY"
    };
    format!("{}: set {} and restart", error, key_hint)
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
                                    let setup =
                                        cx.new(|cx| views::setup_wizard::SetupWizardView::new(cx));
                                    this.setup = Some(setup);
                                    this.app = None;
                                    this.factory = None;
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
                self.load_factory(cx);
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
