mod app;
mod shortcuts;
mod theme;
mod views;

use app::AverroesApp;
use gpui::*;
use shortcuts::Quit;

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    Application::new().run(|cx: &mut App| {
        cx.activate(true);

        cx.bind_keys([KeyBinding::new("cmd-q", Quit, None)]);

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
            |_window, cx| cx.new(|cx| AverroesApp::new(cx)),
        )
        .unwrap();
    });
}
