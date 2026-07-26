mod app;
mod runtime;
mod session;
mod shortcuts;
mod theme;
mod ui;
mod views;

use crate::ui::{button, panel, ButtonVariant, UiTheme};
use app::AverroesApp;
use averroes_core::config::AppConfig;
use gpui::*;
use runtime::{AgentFactory, RuntimeError};
use shortcuts::{CloseSession, FocusInput, NewSession, Quit, SendMessage, ToggleSettings};
use std::sync::Arc;

struct RootView {
    setup: Option<Entity<views::setup_wizard::SetupWizardView>>,
    setup_subscription: Option<Subscription>,
    app: Option<Entity<AverroesApp>>,
    error: Option<String>,
    factory: Option<Arc<AgentFactory>>,
}

impl RootView {
    fn new(cx: &mut Context<Self>) -> Self {
        let mut root = Self {
            setup: None,
            setup_subscription: None,
            app: None,
            error: None,
            factory: None,
        };
        root.load_factory(cx);
        root
    }

    fn load_factory(&mut self, cx: &mut Context<Self>) {
        self.app = None;
        self.factory = None;
        self.error = None;
        self.setup_subscription = None;

        match AgentFactory::load() {
            Ok(factory) => self.install_factory(factory, cx),
            Err(RuntimeError::NeedsSetup) => {
                let config = AppConfig::load().unwrap_or_default();
                self.install_setup(config, cx);
            }
            Err(RuntimeError::Configuration(error)) => self.show_error(error),
            Err(RuntimeError::Provider {
                message,
                api_key_env,
            }) => {
                self.show_error(provider_error_message(&message, api_key_env.as_deref()));
            }
        }
    }

    fn install_factory(&mut self, factory: AgentFactory, cx: &mut Context<Self>) {
        let factory = Arc::new(factory);
        let app_factory = Arc::clone(&factory);
        let app = cx.new(|cx| AverroesApp::new(cx, app_factory));

        self.setup = None;
        self.setup_subscription = None;
        self.error = None;
        self.factory = Some(factory);
        self.app = Some(app);
    }

    fn install_setup(&mut self, config: AppConfig, cx: &mut Context<Self>) {
        let setup = cx.new(|cx| views::setup_wizard::SetupWizardView::new(cx, config));
        let subscription = cx.subscribe(
            &setup,
            |this, _setup, _event: &views::setup_wizard::SetupWizardSaved, cx| {
                this.load_factory(cx);
                cx.notify();
            },
        );
        self.setup = Some(setup);
        self.setup_subscription = Some(subscription);
    }

    fn show_error(&mut self, message: String) {
        self.setup = None;
        self.setup_subscription = None;
        self.app = None;
        self.factory = None;
        self.error = Some(message);
    }
}

fn provider_error_message(error: &str, api_key_env: Option<&str>) -> String {
    match api_key_env {
        Some(api_key_env) => format!("{}: set {} and restart", error, api_key_env),
        None => error.to_string(),
    }
}

impl Render for RootView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if let Some(ref app) = self.app {
            return app.clone().into_any_element();
        }

        if let Some(ref error) = self.error {
            let theme = UiTheme::light();
            return div()
                .flex()
                .flex_col()
                .size_full()
                .bg(theme.background)
                .text_color(theme.foreground)
                .font(UiTheme::ui_font())
                .justify_center()
                .items_center()
                .child(
                    panel(theme)
                        .flex()
                        .flex_col()
                        .w(px(520.0))
                        .p(px(28.0))
                        .gap(px(14.0))
                        .child(
                            div()
                                .font(UiTheme::display_font())
                                .text_lg()
                                .font_weight(FontWeight::BOLD)
                                .text_color(theme.destructive)
                                .child("Connection Error"),
                        )
                        .child(
                            div()
                                .text_sm()
                                .text_color(theme.muted_foreground)
                                .child(error.clone()),
                        )
                        .child(
                            button(theme, ButtonVariant::Danger, "Reset configuration")
                                .id(ElementId::Name("reset-config".into()))
                                .on_click(cx.listener(|this, _event: &ClickEvent, _window, cx| {
                                    if let Err(error) = AppConfig::reset() {
                                        this.show_error(format!("Could not reset config: {error}"));
                                        cx.notify();
                                        return;
                                    }
                                    this.install_setup(AppConfig::default(), cx);
                                    this.app = None;
                                    this.factory = None;
                                    this.error = None;
                                    cx.notify();
                                })),
                        ),
                )
                .into_any_element();
        }

        if let Some(ref setup) = self.setup {
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
            KeyBinding::new("cmd-n", NewSession, None),
            KeyBinding::new("cmd-w", CloseSession, None),
            KeyBinding::new("cmd-l", FocusInput, None),
            KeyBinding::new("cmd-enter", SendMessage, None),
            KeyBinding::new("cmd-,", ToggleSettings, None),
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
                    appears_transparent: true,
                    traffic_light_position: Some(point(px(12.0), px(14.0))),
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
        button, field_label, field_surface, panel, panel_with_padding, provider_card, status_badge,
        ButtonVariant, UiTheme,
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
