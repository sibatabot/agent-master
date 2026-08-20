use std::sync::Arc;

use anyhow::{Context, Result};
use clap::Parser;
use tokio::net::TcpListener;

use agent_master::{serve, Session};

#[derive(Parser)]
#[command(
    about = "Runs one process under a terminal of its own and gives control of it over a port"
)]
struct Cli {
    /// Address to accept control connections on, e.g. 127.0.0.1:7620 or 0.0.0.0:7620.
    #[arg(long, env = "AGENT_MASTER_LISTEN")]
    listen: String,
}

#[tokio::main]
async fn main() -> Result<()> {
    env_logger::init();
    let cli = Cli::parse();
    let listener = TcpListener::bind(&cli.listen)
        .await
        .with_context(|| format!("listening on {}", cli.listen))?;
    log::info!("listening on {}", listener.local_addr().context("reading the bound address")?);
    serve(listener, Arc::new(Session::new())).await
}
