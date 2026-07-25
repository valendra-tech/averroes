mod app;
mod shortcuts;
mod theme;
mod views;

use app::AverroesApp;
use averroes_core::agent::{Agent, AgentConfig};
use averroes_core::compaction::{CompactionConfig, CompactionStrategyType};
use averroes_core::provider::anthropic::AnthropicProvider;
use averroes_core::provider::openai::OpenAiProvider;
use averroes_core::provider::Provider;
use averroes_core::runtime::ResourceGovernor;
use averroes_core::tool::ToolRegistry;
use averroes_core::tool::builtin;
use gpui::*;
use shortcuts::Quit;
use std::path::PathBuf;
use std::sync::Arc;

fn create_agent() -> Option<Arc<Agent>> {
    let provider: Arc<dyn Provider> = if let Ok(key) = std::env::var("ANTHROPIC_API_KEY") {
        Arc::new(AnthropicProvider::new(key))
    } else if let Ok(key) = std::env::var("OPENAI_API_KEY") {
        Arc::new(OpenAiProvider::new(key))
    } else {
        return None;
    };

    let tool_registry = Arc::new(ToolRegistry::new());
    builtin::register_all(&tool_registry);

    let governor = Arc::new(ResourceGovernor::new(10, 200_000));

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
        compaction: CompactionConfig {
            strategy: CompactionStrategyType::Hybrid,
            threshold: 0.8,
            ..Default::default()
        },
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

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let agent = create_agent();

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
            |_window, cx| cx.new(|cx| AverroesApp::new(cx, agent)),
        )
        .unwrap();
    });
}
