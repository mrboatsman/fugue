// Many fields, variants, and functions are defined now but only used in later phases
// (caching, dedup, smart routing). Suppress until those phases are implemented.
#![allow(dead_code)]

use clap::{Parser, Subcommand};
use sqlx::sqlite::SqlitePoolOptions;
use tracing::{info, error};

mod config;
mod error;
mod id;
mod proxy;
mod state;
mod subsonic;

mod cache;
mod dedup;
mod health;

use config::Config;
use proxy::backend::BackendClient;
use state::AppState;

#[derive(Parser)]
#[command(name = "fugue", version, about = "Smart Subsonic API proxy")]
struct Cli {
    /// Path to config file
    #[arg(short, long, default_value = "fugue.toml")]
    config: String,

    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// Start the proxy server (default)
    Serve,
    /// Check backend connectivity
    Check,
    /// Force a cache refresh and exit
    Sync,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    // Load config before initializing tracing so we can use the configured log level
    let config_path = &cli.config;
    if !std::path::Path::new(config_path).exists() {
        eprintln!("Config file '{}' not found.", config_path);
        eprintln!("Create one from the example:  cp fugue.toml.example {}", config_path);
        std::process::exit(1);
    }

    let config = Config::load(Some(config_path)).map_err(|e| {
        eprintln!("Invalid config in '{}': {e}", config_path);
        anyhow::anyhow!("Config error: {e}")
    })?;

    let log_level = &config.server.log_level;
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| format!("fugue={log_level},tower_http={log_level}").into());

    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .init();

    match cli.command.unwrap_or(Commands::Serve) {
        Commands::Serve => serve(config).await?,
        Commands::Check => check(config).await?,
        Commands::Sync => sync(config).await?,
    }

    Ok(())
}

async fn init_db(config: &Config) -> anyhow::Result<sqlx::SqlitePool> {
    let db_url = format!("sqlite:{}?mode=rwc", config.cache.db_path.display());
    let pool = SqlitePoolOptions::new()
        .max_connections(5)
        .connect(&db_url)
        .await?;

    sqlx::migrate!("./migrations").run(&pool).await?;

    info!("Database initialized at {}", config.cache.db_path.display());
    Ok(pool)
}

async fn serve(config: Config) -> anyhow::Result<()> {
    let backends: Vec<BackendClient> = config
        .backends
        .iter()
        .enumerate()
        .map(|(i, bc)| {
            BackendClient::new(
                i,
                bc.name.clone(),
                bc.url.clone(),
                bc.username.clone(),
                bc.password.clone(),
                bc.weight,
            )
        })
        .collect();

    info!(
        "Configured {} backend(s): {}",
        backends.len(),
        backends
            .iter()
            .map(|b| b.name.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    );

    let db = init_db(&config).await?;
    let health_registry = health::probe::HealthRegistry::new();
    let state = AppState::new(config.clone(), backends.clone(), db.clone(), health_registry.clone());

    // Spawn background health probes (every 30s)
    health::probe::spawn_health_probe(health_registry, backends.clone(), 30);

    // Spawn background cache refresh
    cache::refresh::spawn_refresh_task(
        db,
        backends,
        config.cache.refresh_interval_secs,
    );

    let app = subsonic::router()
        .with_state(state);

    let addr = format!("{}:{}", config.server.host, config.server.port);
    info!("Fugue listening on {addr}");

    let listener = tokio::net::TcpListener::bind(&addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}

async fn sync(config: Config) -> anyhow::Result<()> {
    let url = format!(
        "http://{}:{}/admin/sync",
        config.server.host, config.server.port
    );
    // If host is 0.0.0.0, connect to localhost instead
    let url = url.replace("://0.0.0.0:", "://127.0.0.1:");

    info!("Triggering sync on running Fugue server at {}...", url);

    let client = reqwest::Client::new();
    match client.post(&url).send().await {
        Ok(resp) if resp.status().is_success() => {
            info!("Sync triggered successfully. Refresh is running in the background.");
        }
        Ok(resp) => {
            error!("Server returned HTTP {}", resp.status());
        }
        Err(e) => {
            error!("Could not connect to Fugue server: {e}");
            error!("Is Fugue running? Start it with: fugue serve");
        }
    }

    Ok(())
}

async fn check(config: Config) -> anyhow::Result<()> {
    info!("Checking {} backend(s)...", config.backends.len());

    for (i, bc) in config.backends.iter().enumerate() {
        let client = BackendClient::new(
            i,
            bc.name.clone(),
            bc.url.clone(),
            bc.username.clone(),
            bc.password.clone(),
            bc.weight,
        );

        match client.request_json("ping", &[]).await {
            Ok(_) => info!("  [OK] {} ({})", bc.name, bc.url),
            Err(e) => error!("  [FAIL] {} ({}): {}", bc.name, bc.url, e),
        }
    }

    Ok(())
}
