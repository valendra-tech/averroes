mod config;
mod session;
mod tui;

use clap::Parser;

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

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let _cli = Cli::parse();
    let _config = config::AppConfig::load()?;
    tracing::info!("Averroes CLI starting");
    Ok(())
}
