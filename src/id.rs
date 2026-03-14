use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;

use crate::error::FugueError;

/// Encode a backend index and original ID into a namespaced ID.
/// Format: base64url_nopad("{backend_idx}:{original_id}")
pub fn encode_id(backend_idx: usize, original_id: &str) -> String {
    let raw = format!("{backend_idx}:{original_id}");
    URL_SAFE_NO_PAD.encode(raw.as_bytes())
}

/// Decode a namespaced ID back to (backend_index, original_id).
pub fn decode_id(namespaced: &str) -> Result<(usize, String), FugueError> {
    let bytes = URL_SAFE_NO_PAD
        .decode(namespaced)
        .map_err(|e| FugueError::Internal(format!("Invalid ID encoding: {e}")))?;

    let raw = String::from_utf8(bytes)
        .map_err(|e| FugueError::Internal(format!("Invalid ID UTF-8: {e}")))?;

    let (idx_str, original) = raw
        .split_once(':')
        .ok_or_else(|| FugueError::Internal("Invalid ID format: missing colon".into()))?;

    let idx: usize = idx_str
        .parse()
        .map_err(|e| FugueError::Internal(format!("Invalid backend index: {e}")))?;

    Ok((idx, original.to_string()))
}

/// Encode a dedup canonical ID.
pub fn encode_dedup_id(fingerprint_hash: &str) -> String {
    let raw = format!("d:{fingerprint_hash}");
    URL_SAFE_NO_PAD.encode(raw.as_bytes())
}

/// Check if a namespaced ID is a dedup canonical ID.
pub fn is_dedup_id(namespaced: &str) -> bool {
    if let Ok(bytes) = URL_SAFE_NO_PAD.decode(namespaced) {
        if let Ok(raw) = String::from_utf8(bytes) {
            return raw.starts_with("d:");
        }
    }
    false
}

/// Decode a dedup ID to get the fingerprint hash.
pub fn decode_dedup_id(namespaced: &str) -> Result<String, FugueError> {
    let bytes = URL_SAFE_NO_PAD
        .decode(namespaced)
        .map_err(|e| FugueError::Internal(format!("Invalid ID encoding: {e}")))?;

    let raw = String::from_utf8(bytes)
        .map_err(|e| FugueError::Internal(format!("Invalid ID UTF-8: {e}")))?;

    raw.strip_prefix("d:")
        .map(|s| s.to_string())
        .ok_or_else(|| FugueError::Internal("Not a dedup ID".into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_roundtrip() {
        let encoded = encode_id(2, "abc123");
        let (idx, original) = decode_id(&encoded).unwrap();
        assert_eq!(idx, 2);
        assert_eq!(original, "abc123");
    }

    #[test]
    fn test_dedup_id() {
        let encoded = encode_dedup_id("sha256hash");
        assert!(is_dedup_id(&encoded));
        let hash = decode_dedup_id(&encoded).unwrap();
        assert_eq!(hash, "sha256hash");
    }

    #[test]
    fn test_regular_id_not_dedup() {
        let encoded = encode_id(0, "song1");
        assert!(!is_dedup_id(&encoded));
    }
}
