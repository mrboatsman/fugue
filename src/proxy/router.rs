use tracing::debug;

use crate::error::FugueError;
use crate::id::decode_id;
use crate::proxy::backend::BackendClient;
use crate::state::AppState;

/// Decode a namespaced ID and return the corresponding backend client and original ID.
pub fn route_to_backend<'a>(
    state: &'a AppState,
    namespaced_id: &str,
) -> Result<(&'a BackendClient, String), FugueError> {
    let (idx, original_id) = decode_id(namespaced_id)?;

    let backend = state
        .backend(idx)
        .ok_or_else(|| FugueError::Internal(format!("Backend index {idx} out of range")))?;

    debug!("route id={} -> backend={} original_id={}", namespaced_id, backend.name, original_id);
    Ok((backend, original_id))
}
