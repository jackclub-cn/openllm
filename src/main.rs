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

    /// Print what a restart would do to the database schema, then exit.
    /// Exit code 0 = up to date, 1 = a problem, 2 = migrations are pending.
    #[arg(long)]
    check_migrations: bool,

    /// Write a backup into <data-dir>/backups and exit. Safe while serving.
    #[arg(long)]
    backup: bool,

    /// List the backups available for a restore, then exit.
    #[arg(long)]
    list_backups: bool,

    /// Run the same read-only health checks as GET /api/diagnostics/self-check,
    /// then exit. Exit code 0 = pass, 1 = fail, 2 = warnings.
    #[arg(long)]
    self_check: bool,

    /// Restore a backup and exit. Takes a file name from the backups
    /// directory, or a path. The gateway must not be running.
    #[arg(long, value_name = "FILE")]
    restore_backup: Option<String>,

    /// Machine-readable JSON for the operator commands above.
    #[arg(long)]
    json: bool,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    init_tracing();

    let args = Args::parse();
    let config = Config {
        bind: args.bind,
        data_dir: args.data_dir.clone(),
        database_url: args.database_url.clone(),
        admin_token: args.admin_token.clone(),
    };
    if args.check_migrations {
        return finish(openllm::cli::check_migrations(&config, args.json).await);
    }
    if args.backup {
        return finish(openllm::cli::create_backup(&config, args.json).await);
    }
    if args.list_backups {
        return finish(openllm::cli::list_backups(&config, args.json).await);
    }
    if args.self_check {
        return finish(openllm::cli::self_check(&config, args.json).await);
    }
    if let Some(backup) = args.restore_backup.as_deref() {
        return finish(openllm::cli::restore_backup(&config, backup, args.json).await);
    }
    run(config).await
}

/// Turn an operator command's exit code into a process exit.
///
/// A non-zero code is part of the interface (a deploy script branches on it),
/// so it must not be swallowed into an `Err` that prints a stack trace.
fn finish(result: anyhow::Result<i32>) -> anyhow::Result<()> {
    match result {
        Ok(0) => Ok(()),
        Ok(code) => std::process::exit(code),
        Err(error) => Err(error),
    }
}

/// Installs the tracing subscriber.
///
/// `OPENLLM_LOG_FORMAT=json` switches to one JSON object per line for log
/// pipelines (Loki, ELK); anything else keeps the human-readable default. The
/// filter still comes from `RUST_LOG`.
fn init_tracing() {
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("openllm=info,tower_http=info"));
    let builder = tracing_subscriber::fmt().with_env_filter(filter);
    if std::env::var("OPENLLM_LOG_FORMAT")
        .is_ok_and(|value| value.trim().eq_ignore_ascii_case("json"))
    {
        builder
            .json()
            .flatten_event(true)
            .with_current_span(false)
            .with_span_list(false)
            .init();
    } else {
        builder.init();
    }
}
