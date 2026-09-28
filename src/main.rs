use std::net::SocketAddr;
use std::path::PathBuf;

use clap::Parser;
use openllm::{Config, run};
use tracing_subscriber::EnvFilter;

#[derive(Debug, Parser)]
#[command(name = "openllm", version, about)]
struct Args {
    /// Address used by the web console and gateway API.
    #[arg(long, env = "OPENLLM_BIND", default_value = "127.0.0.1:8080")]
    bind: SocketAddr,

    /// Directory used for the SQLite database by default.
    #[arg(long, env = "OPENLLM_DATA_DIR", default_value = "data")]
    data_dir: PathBuf,

    /// Explicit SQLite URL. Overrides --data-dir.
    #[arg(long, env = "OPENLLM_DATABASE_URL")]
    database_url: Option<String>,

    /// Protect management APIs. If unset, the console is open and intended for local use.
    #[arg(long, env = "OPENLLM_ADMIN_TOKEN")]
    admin_token: Option<String>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("openllm=info,tower_http=info")),
        )
        .init();

    let args = Args::parse();
    run(Config {
        bind: args.bind,
        data_dir: args.data_dir,
        database_url: args.database_url,
        admin_token: args.admin_token,
    })
    .await
}
