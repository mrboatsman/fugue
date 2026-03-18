//! Application configuration.
//!
//! Configuration is loaded from a TOML file (default: `fugue.toml`) using
//! [figment](https://docs.rs/figment). Every setting can be overridden via
//! environment variables with the `FUGUE_` prefix, where nested keys use
//! underscores as separators:
//!
//! ```text
//! server.port       → FUGUE_SERVER_PORT
//! cache.db_path     → FUGUE_CACHE_DB_PATH
//! social.enabled    → FUGUE_SOCIAL_ENABLED
//! ```
//!
//! Environment variables take precedence over the TOML file. See
//! [`Config::load`] for the merge order.

use figment::{
    providers::{Env, Format, Toml},
    Figment,
};
use serde::Deserialize;
use std::path::PathBuf;

#[derive(Debug, Deserialize, Clone)]
pub struct Config {
    pub server: ServerConfig,
    pub backends: Vec<BackendConfig>,
    pub auth: AuthConfig,
    #[serde(default)]
    pub cache: CacheConfig,
    #[serde(default)]
    pub social: SocialConfig,
}

#[derive(Debug, Deserialize, Clone)]
pub struct ServerConfig {
    #[serde(default = "default_host")]
    pub host: String,
    #[serde(default = "default_port")]
    pub port: u16,
    #[serde(default = "default_log_level")]
    pub log_level: String,
}

fn default_log_level() -> String {
    "info".into()
}

fn default_host() -> String {
    "0.0.0.0".into()
}

fn default_port() -> u16 {
    4533
}

#[derive(Debug, Deserialize, Clone)]
pub struct BackendConfig {
    pub name: String,
    pub url: String,
    pub username: String,
    pub password: String,
    #[serde(default)]
    pub weight: i32,
}

#[derive(Debug, Deserialize, Clone)]
pub struct AuthConfig {
    pub users: Vec<UserCredential>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct UserCredential {
    pub username: String,
    pub password: String,
}

#[derive(Debug, Deserialize, Clone)]
pub struct CacheConfig {
    #[serde(default = "default_db_path")]
    pub db_path: PathBuf,
    #[serde(default = "default_refresh_interval")]
    pub refresh_interval_secs: u64,
}

fn default_db_path() -> PathBuf {
    PathBuf::from("fugue.db")
}

fn default_refresh_interval() -> u64 {
    300
}

impl Default for CacheConfig {
    fn default() -> Self {
        Self {
            db_path: default_db_path(),
            refresh_interval_secs: default_refresh_interval(),
        }
    }
}

#[derive(Debug, Deserialize, Clone)]
pub struct SocialConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_display_name")]
    pub display_name: String,
    #[serde(default)]
    pub streaming: StreamingConfig,
}

fn default_display_name() -> String {
    "Fugue User".into()
}

impl Default for SocialConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            display_name: default_display_name(),
            streaming: StreamingConfig::default(),
        }
    }
}

#[derive(Debug, Deserialize, Clone)]
pub struct StreamingConfig {
    /// Max bitrate (kbps) to serve to friends. "0" = no limit (raw/original).
    #[serde(default)]
    pub max_serve_bitrate: u32,
    /// Format to transcode to when serving to friends. "raw" = original format.
    #[serde(default = "default_serve_format")]
    pub serve_format: String,
    /// Preferred bitrate when receiving from friends.
    /// "0" = auto (adaptive based on measured bandwidth).
    /// Any other value = fixed cap in kbps.
    #[serde(default)]
    pub preferred_bitrate: u32,
    /// Preferred format when receiving from friends. "auto" = accept whatever.
    #[serde(default = "default_preferred_format")]
    pub preferred_format: String,
}

fn default_serve_format() -> String {
    "raw".into()
}

fn default_preferred_format() -> String {
    "auto".into()
}

impl Default for StreamingConfig {
    fn default() -> Self {
        Self {
            max_serve_bitrate: 0,        // no limit
            serve_format: default_serve_format(),
            preferred_bitrate: 0,         // auto
            preferred_format: default_preferred_format(),
        }
    }
}

impl Config {
    pub fn load(path: Option<&str>) -> Result<Self, figment::Error> {
        let mut figment = Figment::new();

        if let Some(path) = path {
            figment = figment.merge(Toml::file(path));
        } else {
            figment = figment.merge(Toml::file("fugue.toml"));
        }

        figment = figment.merge(Env::prefixed("FUGUE_").split("_"));

        figment.extract()
    }
}
