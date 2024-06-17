use dotenv::dotenv;
use eyre::Result;
use tracing_subscriber::EnvFilter;

pub mod actor;
pub mod builder;
pub mod cli;
pub mod commands;
pub mod gas_oracle;
pub mod rate;

fn setup_tracing() -> Result<()> {
    // construct a subscriber that prints formatted traces to info logs to stdout
    // unless overridden by the RUST_LOG environment variable
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::builder()
                .with_default_directive("inu=info".parse()?)
                .from_env_lossy(),
        )
        // .with_max_level(Level::DEBUG)
        .with_target(false)
        .with_ansi(false)
        .init();

    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    use crate::cli::InuConfig;

    setup_tracing()?;
    dotenv()?;

    let (config, command) = InuConfig::load()?;
    command.execute(&config).await?;

    Ok(())
}
