use std::sync::Arc;

use sqlx::SqlitePool;

use crate::config::Config;
use crate::health::probe::HealthRegistry;
use crate::proxy::backend::BackendClient;

#[derive(Clone)]
pub struct AppState {
    pub inner: Arc<AppStateInner>,
}

pub struct AppStateInner {
    pub config: Config,
    pub backends: Vec<BackendClient>,
    pub db: SqlitePool,
    pub health: HealthRegistry,
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
}
