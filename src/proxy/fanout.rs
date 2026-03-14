use futures::future::join_all;
use serde_json::Value;
use tracing::{debug, error, warn};

use crate::error::FugueError;
use crate::proxy::backend::BackendClient;

/// Fan out a request to all backends in parallel.
/// Returns results from all successful backends (tolerates individual failures).
pub async fn fan_out(
    backends: &[BackendClient],
    endpoint: &str,
    extra_params: &[(&str, &str)],
) -> Result<Vec<(usize, Value)>, FugueError> {
    debug!("fan_out endpoint={} backends={}", endpoint, backends.len());
    let futures: Vec<_> = backends
        .iter()
        .map(|backend| {
            let endpoint = endpoint.to_string();
            let params: Vec<(String, String)> = extra_params
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect();
            let backend = backend.clone();
            async move {
                let param_refs: Vec<(&str, &str)> =
                    params.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
                let result = backend.request_json(&endpoint, &param_refs).await;
                (backend.index, result)
            }
        })
        .collect();

    let results = join_all(futures).await;

    let mut successes = Vec::new();
    let mut last_error = None;

    for (idx, result) in results {
        match result {
            Ok(value) => successes.push((idx, value)),
            Err(e) => {
                warn!("Backend {} failed: {}", idx, e);
                last_error = Some(e);
            }
        }
    }

    if successes.is_empty() {
        error!("fan_out endpoint={} all backends failed", endpoint);
        return Err(last_error.unwrap_or_else(|| FugueError::Backend("No backends configured".into())));
    }

    debug!("fan_out endpoint={} succeeded={}/{}", endpoint, successes.len(), backends.len());
    Ok(successes)
}
