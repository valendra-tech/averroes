mod config;
mod session;
mod tui;

use anyhow::{Context, Result};
use averroes_core::agent::{Agent, AgentConfig};
use averroes_core::compaction::{CompactionConfig, CompactionStrategyType};
use averroes_core::config::SetupWizard;
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
    let mut config = config::AppConfig::load()
        .map_err(|e| anyhow::anyhow!("{}", e))?;

    if config.needs_setup() {
        eprintln!("\n  Welcome to Averroes! Let's configure your setup.\n");
        let wizard = run_setup_wizard()?;
        wizard.save_config()
            .map_err(|e| anyhow::anyhow!("{}", e))?;
        config = wizard.to_config();
        eprintln!("\n  Config saved. Starting...\n");
    }

    let rt = tokio::runtime::Runtime::new()?;
    let _guard = rt.enter();

    let provider = config::create_provider(&config)
        .map_err(|e| anyhow::anyhow!("{}", e))
        .with_context(|| "Failed to create provider. Check your API key.")?;

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

fn prompt(label: &str) -> io::Result<String> {
    print!("  {} ", label);
    io::stdout().flush()?;
    let mut input = String::new();
    io::stdin().read_line(&mut input)?;
    Ok(input.trim().to_string())
}

fn run_setup_wizard() -> Result<SetupWizard> {
    let mut wizard = SetupWizard::new();

    let choice = prompt("Provider? [1] Anthropic  [2] OpenAI (default: 1)")?;
    wizard.provider = match choice.as_str() {
        "2" => "openai".into(),
        _ => "anthropic".into(),
    };

    let env_name = match wizard.provider.as_str() {
        "openai" => "OPENAI_API_KEY",
        _ => "ANTHROPIC_API_KEY",
    };

    eprintln!("  API key: set the `{}` environment variable.", env_name);
    let key_input = prompt(&format!("API key env var name? [default: {}]", env_name))?;
    wizard.api_key_env = if key_input.is_empty() {
        env_name.into()
    } else {
        key_input
    };

    let default = wizard.default_model().to_string();
    let model = prompt(&format!("Model? [default: {}]", default))?;
    wizard.model = if model.is_empty() { default } else { model };

    Ok(wizard)
}
