#![recursion_limit = "512"]

mod app;
mod i18n;
mod keychain;
mod runtime;
mod session;
mod shortcuts;
#[path = "views/chat/tool_groups.rs"]
mod tool_groups;
mod ui;
mod update;
mod version;

use app::AverroesApp;
use gpui::{
    div, img, point, px, size, App, AppContext, AssetSource, Bounds, Context, FontWeight,
    IntoElement, ParentElement, Render, SharedString, Styled, TitlebarOptions, Window,
    WindowBounds, WindowOptions,
};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::Root as ComponentRoot;
use i18n::{Locale, Localization};
use keychain::MacKeychainKeyProvider;
use reqwest_client::ReqwestClient;
use runtime::AppRuntime;
use shortcuts::{CloseSession, FocusInput, NewSession, Quit, SendMessage, ToggleSettings};
use std::borrow::Cow;
use std::sync::Arc;
use ui::UiTheme;

fn application_quit_mode() -> gpui::QuitMode {
    gpui::QuitMode::LastWindowClosed
}

struct UiAssets;

impl AssetSource for UiAssets {
    fn load(&self, path: &str) -> anyhow::Result<Option<Cow<'static, [u8]>>> {
        let custom: Option<&'static [u8]> = match path {
            "providers/openai.svg" => Some(&include_bytes!("../assets/providers/openai.svg")[..]),
            "providers/anthropic.svg" => {
                Some(&include_bytes!("../assets/providers/anthropic.svg")[..])
            }
            "providers/deepseek.svg" => {
                Some(&include_bytes!("../assets/providers/deepseek.svg")[..])
            }
            "providers/ollama.svg" => Some(&include_bytes!("../assets/providers/ollama.svg")[..]),
            "providers/github-copilot.svg" => {
                Some(&include_bytes!("../assets/providers/github-copilot.svg")[..])
            }
            "providers/qdivzero.svg" => {
                Some(&include_bytes!("../assets/providers/qdivzero.svg")[..])
            }
            "providers/generic.svg" => Some(&include_bytes!("../assets/providers/generic.svg")[..]),
            "icons/pin.svg" => Some(&include_bytes!("../assets/pin.svg")[..]),
            "icons/pencil.svg" => Some(&include_bytes!("../assets/pencil.svg")[..]),
            "icons/trash.svg" => Some(&include_bytes!("../assets/trash.svg")[..]),
            "tools/terminal.svg" => Some(&include_bytes!("../assets/tools/terminal.svg")[..]),
            "tools/file-read.svg" => Some(&include_bytes!("../assets/tools/file-read.svg")[..]),
            "tools/file-write.svg" => Some(&include_bytes!("../assets/tools/file-write.svg")[..]),
            "tools/folder-search.svg" => {
                Some(&include_bytes!("../assets/tools/folder-search.svg")[..])
            }
            "tools/search.svg" => Some(&include_bytes!("../assets/tools/search.svg")[..]),
            "tools/globe.svg" => Some(&include_bytes!("../assets/tools/globe.svg")[..]),
            "tools/checkpoint.svg" => Some(&include_bytes!("../assets/tools/checkpoint.svg")[..]),
            "tools/task.svg" => Some(&include_bytes!("../assets/tools/task.svg")[..]),
            "tools/ask-user.svg" => Some(&include_bytes!("../assets/tools/ask-user.svg")[..]),
            "tools/skills.svg" => Some(&include_bytes!("../assets/tools/skills.svg")[..]),
            "tools/skill.svg" => Some(&include_bytes!("../assets/tools/skill.svg")[..]),
            "tools/tool.svg" => Some(&include_bytes!("../assets/tools/tool.svg")[..]),
            "brand/averroes.png" => Some(&include_bytes!("../../../assets/logo.png")[..]),
            "brand/valendra.svg" => Some(&include_bytes!("../assets/brand/valendra.svg")[..]),
            _ => None,
        };

        match custom {
            Some(bytes) => Ok(Some(Cow::Borrowed(bytes))),
            None => gpui_component_assets::Assets.load(path),
        }
    }

    fn list(&self, path: &str) -> anyhow::Result<Vec<SharedString>> {
        gpui_component_assets::Assets.list(path)
    }
}

struct RootView {
    app: Option<gpui::Entity<AverroesApp>>,
    error: Option<String>,
}

impl RootView {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let mut root = Self {
            app: None,
            error: None,
        };
        root.load(window, cx);
        root
    }

    fn load(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.app = None;
        self.error = None;
        match AppRuntime::load(Arc::new(MacKeychainKeyProvider)) {
            Ok(runtime) => {
                let runtime = Arc::new(runtime);
                self.app = Some(cx.new(|cx| AverroesApp::new(window, cx, runtime)));
            }
            Err(error) => self.error = Some(error.to_string()),
        }
        cx.notify();
    }
}

impl Render for RootView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if let Some(app) = self.app.as_ref() {
            return app.clone().into_any_element();
        }

        let theme = UiTheme::current(cx);
        let brand_asset = "brand/averroes.png";
        let message = self
            .error
            .clone()
            .unwrap_or_else(|| i18n::text(cx, "app.initialize_error").to_string());
        div()
            .flex()
            .size_full()
            .items_center()
            .justify_center()
            .bg(theme.background)
            .text_color(theme.foreground)
            .font(UiTheme::ui_font())
            .child(
                div()
                    .w(px(560.0))
                    .p(px(28.0))
                    .rounded(px(16.0))
                    .bg(theme.surface)
                    .border_1()
                    .border_color(theme.border)
                    .shadow_lg()
                    .child(img(brand_asset).size(px(56.0)))
                    .child(
                        div()
                            .mt(px(12.0))
                            .font(UiTheme::display_font())
                            .text_size(px(22.0))
                            .font_weight(FontWeight::BOLD)
                            .child(i18n::text(cx, "app.needs_attention")),
                    )
                    .child(
                        div()
                            .mt(px(8.0))
                            .mb(px(18.0))
                            .text_color(theme.muted)
                            .child(message),
                    )
                    .child(
                        Button::new("retry-startup")
                            .primary()
                            .label(i18n::text(cx, "app.try_again"))
                            .on_click(cx.listener(|this, _, window, cx| this.load(window, cx))),
                    ),
            )
            .into_any_element()
    }
}

fn main() {
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .init();

    // GPUI uses its application HttpClient for remote image assets. Without
    // this, source favicons are sent to the default NullHttpClient and every
    // render produces a noisy "No HttpClient available" error.
    let http_client = Arc::new(
        ReqwestClient::user_agent("averroes-gpui").unwrap_or_else(|_| ReqwestClient::new()),
    );

    gpui_platform::application()
        .with_assets(UiAssets)
        .with_http_client(http_client)
        .with_quit_mode(application_quit_mode())
        .run(|cx: &mut App| {
            gpui_component::init(cx);
            // Keep the UI copy in embedded catalogs. English is the default;
            // the catalog can be switched without any filesystem or network
            // work during rendering.
            cx.set_global(Localization::new(Locale::English));
            UiTheme::install_component_theme(cx);
            cx.activate(true);
            cx.bind_keys([
                gpui::KeyBinding::new("cmd-q", Quit, None),
                gpui::KeyBinding::new("cmd-n", NewSession, None),
                gpui::KeyBinding::new("cmd-w", CloseSession, None),
                gpui::KeyBinding::new("cmd-l", FocusInput, None),
                gpui::KeyBinding::new("cmd-enter", SendMessage, None),
                gpui::KeyBinding::new("cmd-,", ToggleSettings, None),
            ]);
            cx.on_action(|_: &Quit, cx| cx.quit());

            cx.open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
                        None,
                        size(px(1440.0), px(900.0)),
                        cx,
                    ))),
                    window_min_size: Some(size(px(980.0), px(680.0))),
                    titlebar: Some(TitlebarOptions {
                        title: Some("Averroes".into()),
                        appears_transparent: true,
                        traffic_light_position: Some(point(px(14.0), px(15.0))),
                    }),
                    ..Default::default()
                },
                |window, cx| {
                    let view = cx.new(|cx| RootView::new(window, cx));
                    cx.new(|cx| ComponentRoot::new(view, window, cx).bordered(false))
                },
            )
            .expect("failed to open Averroes window");
        });
}

#[cfg(test)]
mod tests {
    use super::application_quit_mode;
    use gpui::QuitMode;

    #[test]
    fn closing_the_last_window_quits_the_desktop_process() {
        assert_eq!(application_quit_mode(), QuitMode::LastWindowClosed);
    }
}
