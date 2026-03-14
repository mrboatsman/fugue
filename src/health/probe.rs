use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use tracing::{debug, warn};

use crate::proxy::backend::BackendClient;

/// Health status for a single backend.
#[derive(Debug, Clone)]
pub struct BackendHealth {
    pub available: bool,
    pub latency_ms: u64,
    pub last_checked: Instant,
    pub consecutive_failures: u32,
}

impl Default for BackendHealth {
    fn default() -> Self {
        Self {
            available: true,
            latency_ms: 0,
            last_checked: Instant::now(),
            consecutive_failures: 0,
        }
    }
}

/// Shared health registry for all backends.
#[derive(Clone)]
pub struct HealthRegistry {
    inner: Arc<RwLock<HashMap<usize, BackendHealth>>>,
}

impl HealthRegistry {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Get health status for a backend. Returns default (available) if unknown.
    pub fn get(&self, backend_idx: usize) -> BackendHealth {
        self.inner
            .read()
            .unwrap()
            .get(&backend_idx)
            .cloned()
            .unwrap_or_default()
    }

    /// Check if a backend is currently considered available.
    pub fn is_available(&self, backend_idx: usize) -> bool {
        self.get(backend_idx).available
    }

    /// Get latency in ms for a backend.
    pub fn latency_ms(&self, backend_idx: usize) -> u64 {
        self.get(backend_idx).latency_ms
    }

    fn update(&self, backend_idx: usize, health: BackendHealth) {
        self.inner.write().unwrap().insert(backend_idx, health);
    }
}

/// Spawn the background health probe task.
pub fn spawn_health_probe(
    registry: HealthRegistry,
    backends: Vec<BackendClient>,
    interval_secs: u64,
) {
    tokio::spawn(async move {
        let interval = Duration::from_secs(interval_secs);
        loop {
            for backend in &backends {
                let start = Instant::now();
                let result = backend.request_json("ping", &[]).await;
                let elapsed = start.elapsed();

                let mut current = registry.get(backend.index);
                current.last_checked = Instant::now();

                match result {
                    Ok(_) => {
                        current.available = true;
                        current.latency_ms = elapsed.as_millis() as u64;
                        current.consecutive_failures = 0;
                        debug!(
                            "health probe: backend={} ok latency={}ms",
                            backend.name, current.latency_ms
                        );
                    }
                    Err(e) => {
                        current.consecutive_failures += 1;
                        // Mark unavailable after 3 consecutive failures
                        if current.consecutive_failures >= 3 {
                            if current.available {
                                warn!(
                                    "health probe: backend={} marked unavailable after {} failures: {}",
                                    backend.name, current.consecutive_failures, e
                                );
                            }
                            current.available = false;
                        } else {
                            debug!(
                                "health probe: backend={} failure {}/3: {}",
                                backend.name, current.consecutive_failures, e
                            );
                        }
                    }
                }

                registry.update(backend.index, current);
            }

            tokio::time::sleep(interval).await;
        }
    });
}
