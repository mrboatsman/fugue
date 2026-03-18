//! Shared application state.
//!
//! [`AppState`] is the central struct passed to all axum handlers via
//! `State<AppState>`. It holds references to the config, backend clients,
//! SQLite pool, health registry, and optional social/Iroh components.
//!
//! The inner data is wrapped in `Arc` so cloning `AppState` is cheap
//! (just a reference count bump).

use std::sync::Arc;

use iroh::Endpoint;
use sqlx::SqlitePool;

use crate::config::Config;
use crate::health::probe::HealthRegistry;
use crate::proxy::backend::BackendClient;
use crate::social::bandwidth::BandwidthTracker;
use crate::social::service::SocialHandle;

#[derive(Clone)]
pub struct AppState {
    pub inner: Arc<AppStateInner>,
}

pub struct AppStateInner {
    pub config: Config,
    pub backends: Vec<BackendClient>,
    pub db: SqlitePool,
    pub health: HealthRegistry,
    pub iroh: Option<Endpoint>,
    pub social: Option<SocialHandle>,
    pub bandwidth: BandwidthTracker,
}

impl AppState {
    pub fn new(
        config: Config,
        backends: Vec<BackendClient>,
        db: SqlitePool,
        health: HealthRegistry,
    ) -> Self {
        Self {
            inner: Arc::new(AppStateInner {
                config,
                backends,
                db,
                health,
                iroh: None,
                social: None,
                bandwidth: BandwidthTracker::new(),
            }),
        }
    }

    pub fn with_social(
        config: Config,
        backends: Vec<BackendClient>,
        db: SqlitePool,
        health: HealthRegistry,
        endpoint: Endpoint,
        social_handle: SocialHandle,
    ) -> Self {
        Self {
            inner: Arc::new(AppStateInner {
                config,
                backends,
                db,
                health,
                iroh: Some(endpoint),
                social: Some(social_handle),
                bandwidth: BandwidthTracker::new(),
            }),
        }
    }

    pub fn backends(&self) -> &[BackendClient] {
        &self.inner.backends
    }

    pub fn config(&self) -> &Config {
        &self.inner.config
    }

    pub fn backend(&self, idx: usize) -> Option<&BackendClient> {
        self.inner.backends.get(idx)
    }

    pub fn db(&self) -> &SqlitePool {
        &self.inner.db
    }

    pub fn health(&self) -> &HealthRegistry {
        &self.inner.health
    }

    pub fn iroh(&self) -> Option<&Endpoint> {
        self.inner.iroh.as_ref()
    }

    pub fn social(&self) -> Option<&SocialHandle> {
        self.inner.social.as_ref()
    }

    pub fn bandwidth(&self) -> &BandwidthTracker {
        &self.inner.bandwidth
    }

    pub fn node_id(&self) -> Option<String> {
        self.inner.iroh.as_ref().map(|e| e.id().to_string())
    }
}
