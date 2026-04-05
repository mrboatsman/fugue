//! # Fugue
//!
//! A smart Subsonic API proxy that merges multiple Navidrome instances into
//! one unified music library.
//!
//! ## Architecture
//!
//! Fugue sits between Subsonic clients and one or more Navidrome backends:
//!
//! - **[`proxy`]** — fan-out requests to all backends, merge responses, proxy streams
//! - **[`id`]** — opaque ID namespacing so backend IDs never collide
//! - **[`cache`]** — background crawl + SQLite cache for instant browsing/search
//! - **[`dedup`]** — cross-backend deduplication via metadata fingerprinting
//! - **[`social`]** — optional P2P layer (Iroh) for sharing with friends
//! - **[`config`]** — TOML + env var configuration
//! - **[`subsonic`]** — Subsonic API endpoint handlers and auth
//!
//! Run `cargo doc --no-deps --open` to browse full module documentation.

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
mod social;

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
    /// Force a cache refresh on the running server
    Sync,
    /// Show your Fugue ticket for sharing with friends
    Ticket,
    /// Show status of backends, cache, and social network
    Status,
    /// Manage friends
    Friend {
        #[command(subcommand)]
        action: FriendAction,
    },
    /// Manage API keys
    ApiKey {
        #[command(subcommand)]
        action: ApiKeyAction,
    },
    /// Manage collaborative playlists
    Playlist {
        #[command(subcommand)]
        action: PlaylistAction,
    },
}

#[derive(Subcommand)]
enum FriendAction {
    /// Add a friend by ticket
    Add {
        /// Friend's display name
        #[arg(long)]
        name: String,
        /// Friend's Fugue ticket string
        ticket: String,
    },
    /// Remove a friend
    Remove {
        /// Friend's name
        name: String,
    },
    /// List all friends
    List,
}

#[derive(Subcommand)]
enum ApiKeyAction {
    /// Create a new API key
    Create {
        /// Username the key belongs to
        #[arg(long)]
        user: String,
        /// Label for this key (e.g. "phone", "desktop")
        #[arg(long, default_value = "")]
        label: String,
    },
    /// List API keys for a user
    List {
        /// Username
        #[arg(long)]
        user: String,
    },
    /// Revoke an API key by hash prefix
    Revoke {
        /// First characters of the key hash (from `api-key list`)
        hash_prefix: String,
    },
}

#[derive(Subcommand)]
enum PlaylistAction {
    /// Create a new collaborative playlist
    Create {
        /// Playlist name
        name: String,
    },
    /// Generate invite codes for a collaborative playlist
    Invite {
        /// Playlist ID (from `playlist list`)
        playlist_id: String,
        /// Role: collab or viewer
        #[arg(long, default_value = "viewer")]
        role: String,
    },
    /// Join a collaborative playlist by invite code
    Join {
        /// Invite code
        code: String,
    },
    /// List collaborative playlists
    List,
    /// Show members of a collaborative playlist
    Members {
        /// Playlist ID
        playlist_id: String,
    },
    /// Leave a collaborative playlist
    Leave {
        /// Playlist ID
        playlist_id: String,
    },
    /// Force sync a playlist to/from the running server
    Sync {
        /// Playlist ID
        playlist_id: String,
    },
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
        Commands::Ticket => ticket(config).await?,
        Commands::Status => status(config).await?,
        Commands::Friend { action } => friend(config, action).await?,
        Commands::ApiKey { action } => api_key(config, action).await?,
        Commands::Playlist { action } => playlist(config, action).await?,
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

fn build_backends(config: &Config) -> Vec<BackendClient> {
    config
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
        .collect()
}

async fn serve(config: Config) -> anyhow::Result<()> {
    let backends = build_backends(&config);

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

    // Build state without social handle first — handlers check social() for None
    let state = AppState::new(config.clone(), backends.clone(), db.clone(), health_registry.clone());

    // Build the axum router (with state applied) — shared by HTTP and Iroh transport
    let router = subsonic::router().with_state(state.clone());

    // Initialize Iroh social layer if enabled
    if config.social.enabled {
        let secret_key = social::node::load_or_create_secret_key(&db).await?;
        let endpoint = social::node::create_endpoint(secret_key).await?;
        info!(
            "Social enabled: node_id={}, display_name={}",
            endpoint.id(),
            config.social.display_name
        );

        // Store Iroh endpoint in state
        state.set_iroh(endpoint.clone());

        let _social_handle = social::service::start(
            endpoint.clone(),
            db.clone(),
            config.social.display_name.clone(),
            backends.clone(),
            Some(router.clone()),
        )
        .await
        .map_err(|e| anyhow::anyhow!("Failed to start social service: {e}"))?;

        state.set_social(_social_handle);

        // Log ticket after a delay so relay is discovered
        let ep_clone = endpoint.clone();
        let display_name = config.social.display_name.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
            let ticket = social::node::generate_ticket(&ep_clone, Some(&display_name));
            info!("Social ticket (share with friends):");
            info!("  {ticket}");
        });
    }

    // Spawn background health probes (every 30s)
    health::probe::spawn_health_probe(health_registry, backends.clone(), 30);

    // Spawn background cache refresh
    cache::refresh::spawn_refresh_task(
        db,
        backends,
        config.cache.refresh_interval_secs,
    );

    let addr = format!("{}:{}", config.server.host, config.server.port);
    info!("Fugue listening on {addr}");

    let listener = tokio::net::TcpListener::bind(&addr).await?;
    axum::serve(listener, router).await?;

    Ok(())
}

async fn ticket(config: Config) -> anyhow::Result<()> {
    let url = format!(
        "http://{}:{}/admin/ticket",
        config.server.host, config.server.port
    );
    let url = url.replace("://0.0.0.0:", "://127.0.0.1:");

    let client = reqwest::Client::new();
    match client.get(&url).send().await {
        Ok(resp) if resp.status().is_success() => {
            let body: serde_json::Value = resp.json().await?;
            if let Some(err) = body.get("error") {
                eprintln!("{}", err.as_str().unwrap_or("Unknown error"));
                return Ok(());
            }
            let ticket = body.get("ticket").and_then(|t| t.as_str()).unwrap_or("");
            let node_id = body.get("node_id").and_then(|n| n.as_str()).unwrap_or("");

            println!("Your Fugue ticket:");
            println!("  {ticket}");
            println!();
            println!("Node ID: {node_id}");
            println!();
            println!("Share the ticket with friends:");
            println!("  fugue friend add --name \"{}\" {ticket}", config.social.display_name);
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

async fn friend(config: Config, action: FriendAction) -> anyhow::Result<()> {
    let db = init_db(&config).await?;

    match action {
        FriendAction::Add { name, ticket } => {
            // Parse the ticket to extract the public key (handles "name:base64" format)
            let public_key = match social::node::parse_named_ticket(&ticket) {
                Ok((_, addr)) => addr.id.to_string(),
                Err(_) => match social::node::parse_ticket(&ticket) {
                    Ok(addr) => addr.id.to_string(),
                    Err(_) => ticket.clone(),
                }
            };
            social::friends::add_friend(&db, &name, &public_key, &ticket).await?;
            println!("Added friend: {name} ({public_key})");
            println!("The running server will pick up the new friend within 30 seconds.");
        }
        FriendAction::Remove { name } => {
            if social::friends::remove_friend(&db, &name).await? {
                println!("Removed friend: {name}");
            } else {
                println!("Friend not found: {name}");
            }
        }
        FriendAction::List => {
            let friends = social::friends::list_friends(&db).await?;
            if friends.is_empty() {
                println!("No friends added yet.");
                println!("Add a friend: fugue friend add --name \"Name\" <ticket>");
            } else {
                println!("{:<20} {:<50} {}", "Name", "Node ID", "Last seen");
                println!("{}", "-".repeat(85));
                for f in friends {
                    let last_seen = f.last_seen.as_deref().unwrap_or("never");
                    println!("{:<20} {:<50} {}", f.name, f.public_key, last_seen);
                }
            }
        }
    }

    Ok(())
}

async fn status(config: Config) -> anyhow::Result<()> {
    let url = format!(
        "http://{}:{}/admin/status",
        config.server.host, config.server.port
    );
    let url = url.replace("://0.0.0.0:", "://127.0.0.1:");

    let client = reqwest::Client::new();
    match client.get(&url).send().await {
        Ok(resp) if resp.status().is_success() => {
            let body: serde_json::Value = resp.json().await?;

            // Backends
            println!("Backends:");
            if let Some(backends) = body.get("backends").and_then(|b| b.as_array()) {
                for b in backends {
                    let name = b.get("name").and_then(|n| n.as_str()).unwrap_or("?");
                    let available = b.get("available").and_then(|a| a.as_bool()).unwrap_or(false);
                    let latency = b.get("latency_ms").and_then(|l| l.as_u64()).unwrap_or(0);
                    let status = if available { format!("ok  {}ms", latency) } else { "DOWN".into() };
                    println!("  {:<20} {}", name, status);
                }
            }

            // Cache
            println!();
            println!("Cache:");
            if let Some(cache) = body.get("cache") {
                let artists = cache.get("artists").and_then(|a| a.as_i64()).unwrap_or(0);
                let albums = cache.get("albums").and_then(|a| a.as_i64()).unwrap_or(0);
                let tracks = cache.get("tracks").and_then(|t| t.as_i64()).unwrap_or(0);
                println!("  {} artists, {} albums, {} tracks", artists, albums, tracks);
            }

            // Social
            println!();
            println!("Social:");
            if let Some(social) = body.get("social") {
                let enabled = social.get("enabled").and_then(|e| e.as_bool()).unwrap_or(false);
                if !enabled {
                    println!("  disabled");
                } else {
                    let node_id = social.get("node_id").and_then(|n| n.as_str()).unwrap_or("?");
                    let relay = social.get("relay").and_then(|r| r.as_str()).unwrap_or("none");
                    let addrs = social.get("direct_addresses").and_then(|a| a.as_array()).map(|a| a.len()).unwrap_or(0);
                    println!("  Node ID:    {}", node_id);
                    println!("  Relay:      {}", relay);
                    println!("  Direct IPs: {}", addrs);

                    if let Some(friends) = social.get("friends").and_then(|f| f.as_array()) {
                        println!();
                        println!("  Friends:");
                        if friends.is_empty() {
                            println!("    (none)");
                        }
                        for f in friends {
                            let name = f.get("name").and_then(|n| n.as_str()).unwrap_or("?");
                            let last_seen = f.get("last_seen").and_then(|l| l.as_str()).unwrap_or("never");
                            println!("    {:<20} last seen: {}", name, last_seen);
                        }
                    }
                }
            }
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

async fn api_key(config: Config, action: ApiKeyAction) -> anyhow::Result<()> {
    let db = init_db(&config).await?;

    match action {
        ApiKeyAction::Create { user, label } => {
            // Verify user exists in config
            if !config.auth.users.iter().any(|u| u.username == user) {
                eprintln!("User '{}' not found in config", user);
                std::process::exit(1);
            }
            let key = subsonic::auth::create_api_key(&db, &user, &label).await?;
            println!("API key created for user: {user}");
            println!("Key: {key}");
            println!();
            println!("Use it with: ?apiKey={key}");
            println!("This key is shown only once — store it securely.");
        }
        ApiKeyAction::List { user } => {
            let keys = subsonic::auth::list_api_keys(&db, &user).await?;
            if keys.is_empty() {
                println!("No API keys for user: {user}");
            } else {
                println!("{:<18} {:<20} {}", "Hash (prefix)", "Label", "Last used");
                println!("{}", "-".repeat(55));
                for (hash, _username, label, last_used) in &keys {
                    let prefix = &hash[..16.min(hash.len())];
                    let lu = last_used.as_deref().unwrap_or("never");
                    println!("{:<18} {:<20} {}", prefix, label, lu);
                }
            }
        }
        ApiKeyAction::Revoke { hash_prefix } => {
            if subsonic::auth::revoke_api_key(&db, &hash_prefix).await? {
                println!("API key revoked.");
            } else {
                println!("No matching API key found.");
            }
        }
    }

    Ok(())
}

async fn playlist(config: Config, action: PlaylistAction) -> anyhow::Result<()> {
    use social::collab_playlist::{self, Role};

    let db = init_db(&config).await?;
    let node_id = {
        let key = social::node::load_or_create_secret_key(&db).await?;
        key.public().to_string()
    };

    match action {
        PlaylistAction::Create { name } => {
            let playlist_id = format!("{:032x}", rand::random::<u128>());
            collab_playlist::create_playlist(&db, &playlist_id, &name, &node_id).await?;
            collab_playlist::add_member(&db, &playlist_id, &node_id, &config.social.display_name, Role::Owner).await?;

            // CLI-generated invites don't include sender ticket (no running endpoint).
            // Use /admin/playlist-invite from a running Fugue for self-contained invites.
            let collab_code = collab_playlist::generate_invite(&playlist_id, Role::Collab, &name, "", "");
            let viewer_code = collab_playlist::generate_invite(&playlist_id, Role::Viewer, &name, "", "");

            println!("Created collaborative playlist: {name}");
            println!("Playlist ID: {playlist_id}");
            println!();
            println!("Share these invite codes with friends:");
            println!("  Collaborate: fugue playlist join {collab_code}");
            println!("  View only:   fugue playlist join {viewer_code}");
        }
        PlaylistAction::Invite { playlist_id, role } => {
            let role = Role::from_str(&role).unwrap_or(Role::Viewer);
            // Get playlist name
            let name_row: Option<(String,)> = sqlx::query_as(
                "SELECT name FROM collab_playlists WHERE id = ?",
            )
            .bind(&playlist_id)
            .fetch_optional(&db)
            .await?;
            let name = name_row.map(|(n,)| n).unwrap_or_else(|| "Playlist".into());
            let code = collab_playlist::generate_invite(&playlist_id, role, &name, "", "");
            println!("Invite code ({:?}) for \"{}\":", role, name);
            println!("  fugue playlist join {code}");
        }
        PlaylistAction::Join { code } => {
            let invite = collab_playlist::parse_invite(&code)
                .ok_or_else(|| anyhow::anyhow!("Invalid invite code"))?;
            let (playlist_id, role, name) = (invite.playlist_id, invite.role, invite.name);

            // Create or update the playlist with the name from the invite
            let exists: Option<(i64,)> = sqlx::query_as(
                "SELECT 1 FROM collab_playlists WHERE id = ?",
            )
            .bind(&playlist_id)
            .fetch_optional(&db)
            .await?;

            if exists.is_none() {
                collab_playlist::create_playlist(&db, &playlist_id, &name, "friend").await?;
            } else {
                collab_playlist::rename_playlist(&db, &playlist_id, &name).await?;
            }

            collab_playlist::add_member(&db, &playlist_id, &node_id, &config.social.display_name, role).await?;
            println!("Joined \"{}\" as {:?}", name, role);
            println!("Tracks will sync when connected to the creator.");
        }
        PlaylistAction::List => {
            let playlists = collab_playlist::list_playlists(&db, &config.social.display_name, &node_id).await?;
            if playlists.is_empty() {
                println!("No collaborative playlists.");
                println!("Create one: fugue playlist create \"My Playlist\"");
            } else {
                println!("{:<36} {:<30} {}", "ID", "Name", "Tracks");
                println!("{}", "-".repeat(75));
                for p in &playlists {
                    let name = p.get("name").and_then(|n| n.as_str()).unwrap_or("?");
                    let tracks = p.get("songCount").and_then(|t| t.as_i64()).unwrap_or(0);
                    // Decode the collab ID to get the raw UUID
                    let encoded = p.get("id").and_then(|i| i.as_str()).unwrap_or("?");
                    let uuid = collab_playlist::decode_collab_id(encoded).unwrap_or_else(|| encoded.to_string());
                    println!("{:<36} {:<30} {}", uuid, name, tracks);
                }
            }
        }
        PlaylistAction::Members { playlist_id } => {
            let members = collab_playlist::list_members(&db, &playlist_id).await?;
            if members.is_empty() {
                println!("No members (or playlist not found).");
            } else {
                println!("{:<20} {:<10} {}", "Name", "Role", "Node ID (short)");
                println!("{}", "-".repeat(55));
                for (mid, name, role) in &members {
                    let short_id = &mid[..8.min(mid.len())];
                    let is_me = mid == &node_id;
                    let label = if is_me {
                        format!("{} (you)", name)
                    } else {
                        name.clone()
                    };
                    println!("{:<20} {:<10} {}...", label, role.as_str(), short_id);
                }
            }
        }
        PlaylistAction::Leave { playlist_id } => {
            collab_playlist::remove_member(&db, &playlist_id, &node_id).await?;
            println!("Left playlist {playlist_id}");
        }
        PlaylistAction::Sync { playlist_id } => {
            let url = format!(
                "http://{}:{}/admin/playlist-sync?id={}",
                config.server.host, config.server.port, playlist_id
            );
            let url = url.replace("://0.0.0.0:", "://127.0.0.1:");

            let client = reqwest::Client::new();
            match client.get(&url).send().await {
                Ok(resp) if resp.status().is_success() => {
                    println!("Playlist sync broadcast sent for {playlist_id}");
                }
                Ok(resp) => {
                    error!("Server returned HTTP {}", resp.status());
                }
                Err(e) => {
                    error!("Could not connect to Fugue server: {e}");
                }
            }
        }
    }

    Ok(())
}

async fn sync(config: Config) -> anyhow::Result<()> {
    let url = format!(
        "http://{}:{}/admin/sync",
        config.server.host, config.server.port
    );
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
