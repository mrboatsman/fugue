use sqlx::SqlitePool;
use tracing::debug;

use crate::error::FugueError;
use crate::id::decode_dedup_id;

/// A resolved source for a deduplicated item.
pub struct ResolvedSource {
    pub namespaced_id: String,
    pub backend_idx: usize,
    pub score: f64,
}

/// Resolve the best source for a dedup canonical ID.
/// Returns members sorted by score (highest first).
pub async fn resolve_best_sources(
    db: &SqlitePool,
    dedup_id: &str,
) -> Result<Vec<ResolvedSource>, FugueError> {
    let fingerprint = decode_dedup_id(dedup_id)?;

    let rows: Vec<(String, i64, Option<i64>, Option<String>, f64)> = sqlx::query_as(
        "SELECT dm.namespaced_id, dm.backend_idx, dm.bitrate, dm.format, dm.score
         FROM dedup_members dm
         WHERE dm.fingerprint = ?
         ORDER BY dm.score DESC, dm.bitrate DESC NULLS LAST",
    )
    .bind(&fingerprint)
    .fetch_all(db)
    .await?;

    let sources: Vec<ResolvedSource> = rows
        .into_iter()
        .map(|(namespaced_id, backend_idx, bitrate, format, score)| {
            // Compute a runtime score if stored score is 0
            let computed_score = if score > 0.0 {
                score
            } else {
                compute_score(bitrate, format.as_deref(), backend_idx as usize)
            };
            ResolvedSource {
                namespaced_id,
                backend_idx: backend_idx as usize,
                score: computed_score,
            }
        })
        .collect();

    debug!(
        "dedup resolve fingerprint={} sources={}",
        fingerprint,
        sources.len()
    );

    Ok(sources)
}

/// Compute a quality score for source selection.
/// Higher is better.
fn compute_score(bitrate: Option<i64>, format: Option<&str>, _backend_idx: usize) -> f64 {
    let bitrate_score = bitrate.unwrap_or(128) as f64;

    let format_weight = match format {
        Some("flac") => 2.0,
        Some("opus") => 1.5,
        Some("ogg") => 1.3,
        Some("mp3") => 1.0,
        Some("aac") | Some("m4a") => 1.1,
        Some("wav") => 1.8,
        _ => 1.0,
    };

    bitrate_score * format_weight
}

/// Look up which dedup group (if any) a namespaced ID belongs to.
/// Returns the canonical dedup ID if the item is part of a duplicate group.
pub async fn find_dedup_canonical(
    db: &SqlitePool,
    namespaced_id: &str,
) -> Result<Option<String>, FugueError> {
    let row: Option<(String,)> = sqlx::query_as(
        "SELECT dg.canonical_id FROM dedup_groups dg
         JOIN dedup_members dm ON dm.fingerprint = dg.fingerprint
         WHERE dm.namespaced_id = ?",
    )
    .bind(namespaced_id)
    .fetch_optional(db)
    .await?;

    Ok(row.map(|(id,)| id))
}

/// Get the best single source for a dedup fingerprint (convenience wrapper).
pub async fn resolve_best_source(
    db: &SqlitePool,
    dedup_id: &str,
) -> Result<Option<ResolvedSource>, FugueError> {
    let mut sources = resolve_best_sources(db, dedup_id).await?;
    Ok(if sources.is_empty() {
        None
    } else {
        Some(sources.remove(0))
    })
}

/// Update stored scores for dedup members based on backend config weights.
pub async fn update_scores(
    db: &SqlitePool,
    backend_weights: &[(usize, i32)],
) -> Result<(), FugueError> {
    for (backend_idx, config_weight) in backend_weights {
        // Recompute scores for all members of this backend
        let rows: Vec<(i64, Option<i64>, Option<String>)> = sqlx::query_as(
            "SELECT id, bitrate, format FROM dedup_members WHERE backend_idx = ?",
        )
        .bind(*backend_idx as i64)
        .fetch_all(db)
        .await?;

        for (id, bitrate, format) in rows {
            let base = compute_score(bitrate, format.as_deref(), *backend_idx);
            let score = base + *config_weight as f64;
            sqlx::query("UPDATE dedup_members SET score = ? WHERE id = ?")
                .bind(score)
                .bind(id)
                .execute(db)
                .await?;
        }
    }

    debug!("dedup resolver: scores updated for {} backends", backend_weights.len());
    Ok(())
}
