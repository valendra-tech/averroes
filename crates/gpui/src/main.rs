mod app;
mod shortcuts;
mod theme;
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

fn create_agent() -> Option<Arc<Agent>> {
    let config = AppConfig::load().ok()?;
    if config.needs_setup() {
        return None;
    }
    let provider = create_provider(&config).ok()?;

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
            "bash".into(),
            "file_read".into(),
            "file_write".into(),
            "glob".into(),
            "grep".into(),
            "web_fetch".into(),
        ],
        max_iterations: 30,
        compaction: compaction_config,
        ..Default::default()
    };

    Some(Arc::new(Agent::new(
        agent_config,
        provider,
        tool_registry,
        governor,
        "gpui-session".into(),
        std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
    )))
}

struct RootView {
    setup: Option<Entity<views::setup_wizard::SetupWizardView>>,
    app: Option<Entity<AverroesApp>>,
}

impl RootView {
    fn new(cx: &mut Context<Self>) -> Self {
        let agent = create_agent();

        if agent.is_some() {
            let app = cx.new(|cx| AverroesApp::new(cx, agent));
            Self {
                setup: None,
                app: Some(app),
            }
        } else {
            let setup = cx.new(|cx| views::setup_wizard::SetupWizardView::new(cx));
            Self {
                setup: Some(setup),
                app: None,
            }
        }
    }
}

impl Render for RootView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if let Some(ref app) = self.app {
            return app.clone().into_any_element();
        }

        if let Some(ref setup) = self.setup {
            let is_done = setup.read(cx).is_done();
            if is_done {
                let agent = create_agent();
                let app_entity = cx.new(|cx| AverroesApp::new(cx, agent));
                self.app = Some(app_entity);
                self.setup = None;
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
