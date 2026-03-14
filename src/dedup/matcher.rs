use std::collections::HashMap;

use sqlx::SqlitePool;
use tracing::debug;

use crate::error::FugueError;
use crate::id::encode_dedup_id;

/// A member of a dedup group.
pub struct DedupMember {
    pub namespaced_id: String,
    pub backend_idx: usize,
    pub bitrate: Option<i64>,
    pub format: Option<String>,
}

/// Normalize a string for fingerprinting: lowercase, trim, collapse whitespace,
/// strip common noise like "(Remastered)", "[Deluxe Edition]", etc.
fn normalize(s: &str) -> String {
    let s = s.to_lowercase();
    // Strip common suffixes in parentheses/brackets
    let s = regex_lite_strip(&s);
    // Collapse whitespace
    s.split_whitespace().collect::<Vec<_>>().join(" ").trim().to_string()
}

/// Simple bracket/paren stripping without pulling in regex crate.
fn regex_lite_strip(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let mut depth_paren = 0i32;
    let mut depth_bracket = 0i32;
    for c in s.chars() {
        match c {
            '(' => depth_paren += 1,
            ')' => {
                depth_paren = (depth_paren - 1).max(0);
                continue;
            }
            '[' => depth_bracket += 1,
            ']' => {
                depth_bracket = (depth_bracket - 1).max(0);
                continue;
            }
            _ if depth_paren > 0 || depth_bracket > 0 => continue,
            _ => result.push(c),
        }
    }
    result
}

/// Generate a track fingerprint: "artist::album::title::track_number"
pub fn track_fingerprint(artist: &str, album: &str, title: &str, track_number: Option<i64>) -> String {
    let track_str = track_number.map(|n| n.to_string()).unwrap_or_default();
    format!(
        "t::{}::{}::{}::{}",
        normalize(artist),
        normalize(album),
        normalize(title),
        track_str
    )
}

/// Generate an album fingerprint: "artist::album"
pub fn album_fingerprint(artist: &str, album: &str) -> String {
    format!("a::{}::{}", normalize(artist), normalize(album))
}

/// Find duplicate tracks by grouping on normalized fingerprint.
pub async fn find_duplicate_tracks(
    db: &SqlitePool,
) -> Result<HashMap<String, Vec<DedupMember>>, FugueError> {
    let rows: Vec<(String, String, i64, String, Option<String>, Option<i64>, Option<i64>, Option<String>)> =
        sqlx::query_as(
            "SELECT id, COALESCE(artist, ''), backend_idx, COALESCE(album, ''), title, track_number, bitrate, suffix FROM tracks",
        )
        .fetch_all(db)
        .await?;

    let mut groups: HashMap<String, Vec<DedupMember>> = HashMap::new();

    for (namespaced_id, artist, backend_idx, album, title, track_number, bitrate, suffix) in rows {
        let title = title.unwrap_or_default();
        let fp = track_fingerprint(&artist, &album, &title, track_number);
        groups.entry(fp).or_default().push(DedupMember {
            namespaced_id,
            backend_idx: backend_idx as usize,
            bitrate,
            format: suffix,
        });
    }

    // Only keep groups with 2+ members (actual duplicates)
    groups.retain(|_, members| members.len() >= 2);

    debug!("dedup matcher: found {} duplicate track groups", groups.len());
    Ok(groups)
}

/// Find duplicate albums by grouping on normalized fingerprint.
pub async fn find_duplicate_albums(
    db: &SqlitePool,
) -> Result<HashMap<String, Vec<DedupMember>>, FugueError> {
    let rows: Vec<(String, String, i64, String)> =
        sqlx::query_as(
            "SELECT id, COALESCE(artist, ''), backend_idx, name FROM albums",
        )
        .fetch_all(db)
        .await?;

    let mut groups: HashMap<String, Vec<DedupMember>> = HashMap::new();

    for (namespaced_id, artist, backend_idx, name) in rows {
        let fp = album_fingerprint(&artist, &name);
        groups.entry(fp).or_default().push(DedupMember {
            namespaced_id,
            backend_idx: backend_idx as usize,
            bitrate: None,
            format: None,
        });
    }

    groups.retain(|_, members| members.len() >= 2);

    debug!("dedup matcher: found {} duplicate album groups", groups.len());
    Ok(groups)
}

/// Insert or update a dedup group.
pub async fn upsert_dedup_group(
    db: &SqlitePool,
    fingerprint: &str,
    entity_type: &str,
) -> Result<(), FugueError> {
    let canonical_id = encode_dedup_id(fingerprint);
    sqlx::query(
        "INSERT INTO dedup_groups (fingerprint, canonical_id, entity_type)
         VALUES (?, ?, ?)
         ON CONFLICT(fingerprint) DO UPDATE SET
           canonical_id = excluded.canonical_id,
           entity_type = excluded.entity_type",
    )
    .bind(fingerprint)
    .bind(canonical_id)
    .bind(entity_type)
    .execute(db)
    .await?;
    Ok(())
}

/// Insert or update a dedup member.
pub async fn upsert_dedup_member(
    db: &SqlitePool,
    fingerprint: &str,
    namespaced_id: &str,
    backend_idx: usize,
    bitrate: Option<i64>,
    format: Option<&str>,
) -> Result<(), FugueError> {
    sqlx::query(
        "INSERT INTO dedup_members (fingerprint, namespaced_id, backend_idx, bitrate, format)
         VALUES (?, ?, ?, ?, ?)
         ON CONFLICT(fingerprint, namespaced_id) DO UPDATE SET
           bitrate = excluded.bitrate,
           format = excluded.format",
    )
    .bind(fingerprint)
    .bind(namespaced_id)
    .bind(backend_idx as i64)
    .bind(bitrate)
    .bind(format)
    .execute(db)
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_track_fingerprint() {
        let fp1 = track_fingerprint("Red Hot Chili Peppers", "The Getaway", "Dark Necessities", Some(1));
        let fp2 = track_fingerprint("red hot chili peppers", "the getaway", "dark necessities", Some(1));
        assert_eq!(fp1, fp2);
    }

    #[test]
    fn test_fingerprint_strips_remaster() {
        let fp1 = album_fingerprint("Pink Floyd", "The Wall");
        let fp2 = album_fingerprint("Pink Floyd", "The Wall (Remastered 2011)");
        assert_eq!(fp1, fp2);
    }

    #[test]
    fn test_fingerprint_strips_brackets() {
        let fp1 = album_fingerprint("Radiohead", "OK Computer");
        let fp2 = album_fingerprint("Radiohead", "OK Computer [Deluxe Edition]");
        assert_eq!(fp1, fp2);
    }

    #[test]
    fn test_different_albums_different_fingerprints() {
        let fp1 = album_fingerprint("Pink Floyd", "The Wall");
        let fp2 = album_fingerprint("Pink Floyd", "Wish You Were Here");
        assert_ne!(fp1, fp2);
    }
}
