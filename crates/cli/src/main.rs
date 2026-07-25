mod config;
mod session;
mod tui;

use anyhow::Result;
use averroes_core::agent::{Agent, AgentConfig};
use averroes_core::compaction::{CompactionConfig, CompactionStrategyType};
use averroes_core::provider::anthropic::AnthropicProvider;
use averroes_core::provider::openai::OpenAiProvider;
use averroes_core::provider::Provider;
use averroes_core::runtime::ResourceGovernor;
use averroes_core::skill::{SkillIndex};
use averroes_core::skill::loader::SkillLoader;
use averroes_core::tool::ToolRegistry;
use averroes_core::tool::builtin;
use clap::Parser;
use std::io::{self, Write};
use std::path::PathBuf;
use std::sync::Arc;

#[derive(Parser)]
#[command(name = "averroes", about = "High-performance AI harness CLI")]
struct Cli {
    #[arg(short, long)]
    message: Option<String>,

    #[arg(short, long)]
    session: Option<String>,

    #[arg(long)]
    interactive: bool,
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let cli = Cli::parse();
    let config = config::AppConfig::load()?;

    let provider: Arc<dyn Provider> = if let Some(ref anthropic) = config.provider.anthropic {
        let key_env = anthropic
            .api_key_env
            .as_deref()
            .unwrap_or("ANTHROPIC_API_KEY");
        let api_key = std::env::var(key_env).unwrap_or_else(|_| {
            eprintln!("Error: {} not set", key_env);
            std::process::exit(1);
        });
        let mut provider = AnthropicProvider::new(api_key);
        if let Some(ref model) = anthropic.default_model {
            provider = provider.with_default_model(model);
        }
        Arc::new(provider)
    } else if let Some(ref openai) = config.provider.openai {
        let key_env = openai
            .api_key_env
            .as_deref()
            .unwrap_or("OPENAI_API_KEY");
        let api_key = std::env::var(key_env).unwrap_or_else(|_| {
            eprintln!("Error: {} not set", key_env);
            std::process::exit(1);
        });
        let mut provider = OpenAiProvider::new(api_key);
        if let Some(ref model) = openai.default_model {
            provider = provider.with_default_model(model);
        }
        Arc::new(provider)
    } else if config.provider.default.as_deref() == Some("anthropic") {
        let api_key = std::env::var("ANTHROPIC_API_KEY").unwrap_or_else(|_| {
            eprintln!("Error: ANTHROPIC_API_KEY not set");
            std::process::exit(1);
        });
        Arc::new(AnthropicProvider::new(api_key))
    } else if config.provider.default.as_deref() == Some("openai") {
        let api_key = std::env::var("OPENAI_API_KEY").unwrap_or_else(|_| {
            eprintln!("Error: OPENAI_API_KEY not set");
            std::process::exit(1);
        });
        Arc::new(OpenAiProvider::new(api_key))
    } else {
        eprintln!("Error: No provider configured. Set provider.default or add an anthropic/openai section in config.");
        std::process::exit(1);
    };

    let tool_registry = Arc::new(ToolRegistry::new());
    builtin::register_all(&tool_registry);

    let skill_paths: Vec<PathBuf> = config
        .skills
        .paths
        .unwrap_or_else(|| vec![".".to_string()])
        .iter()
        .map(PathBuf::from)
        .collect();
    let skill_loader = SkillLoader::new(skill_paths);
    let _skill_index = match SkillIndex::build(skill_loader) {
        Ok(idx) => Some(Arc::new(idx)),
        Err(e) => {
            tracing::warn!("Failed to load skills: {}", e);
            None
        }
    };

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
        name: "cli".into(),
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

    let agent = Arc::new(Agent::new(
        agent_config,
        provider,
        tool_registry.clone(),
        governor,
        cli.session.unwrap_or_else(|| "default".into()),
        std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
    ));

    let store = session::SessionStore::open()?;
    let session_id = store.create_session("interactive")?;

    let rt = tokio::runtime::Runtime::new()?;

    if let Some(message) = cli.message {
        let result = rt.block_on(agent.run(&message))?;
        println!("{}", result);
        store.save_message(&session_id, "user", &message)?;
        store.save_message(&session_id, "assistant", &result)?;
    } else {
        tracing::info!("Averroes CLI starting");
        loop {
            let input = read_line()?;
            if input.is_empty() || input == "exit" || input == "quit" {
                break;
            }

            store.save_message(&session_id, "user", &input)?;

            match rt.block_on(agent.run(&input)) {
                Ok(response) => {
                    println!("\n---\n{}\n---", response);
                    store.save_message(&session_id, "assistant", &response)?;
                }
                Err(e) => {
                    eprintln!("Error: {}", e);
                }
            }
        }
    }

    Ok(())
}

fn read_line() -> io::Result<String> {
    print!("> ");
    io::stdout().flush()?;
    let mut input = String::new();
    io::stdin().read_line(&mut input)?;
    Ok(input.trim().to_string())
}
