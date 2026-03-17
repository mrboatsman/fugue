//! Bandwidth measurement and adaptive quality selection.
//!
//! Measurements come from two sources:
//! - Passive: actual streaming throughput (updated after each stream completes)
//! - Active: background probe triggered when data is stale and a stream is requested
//!
//! No periodic probing — bandwidth is only measured when needed.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use tracing::debug;

/// How old a measurement can be before it's considered stale.
const STALE_THRESHOLD: Duration = Duration::from_secs(3600); // 1 hour

/// Default conservative bitrate (kbps) when no measurement exists.
const DEFAULT_BITRATE: u32 = 192;

/// Bandwidth measurement for a single peer.
#[derive(Debug, Clone)]
pub struct PeerBandwidth {
    /// Measured throughput in kbps.
    pub kbps: u32,
    /// When this measurement was taken.
    pub measured_at: Instant,
    /// How many samples contributed to this value (exponential moving average).
    pub sample_count: u32,
}

/// Shared bandwidth registry for all peers.
#[derive(Clone)]
pub struct BandwidthTracker {
    inner: Arc<RwLock<HashMap<String, PeerBandwidth>>>,
}

impl BandwidthTracker {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Get the measured bandwidth for a peer. Returns None if never measured.
    pub fn get(&self, node_id: &str) -> Option<PeerBandwidth> {
        self.inner.read().unwrap().get(node_id).cloned()
    }

    /// Get the effective bandwidth for quality selection.
    /// Returns measured value if fresh, or default if stale/missing.
    pub fn effective_kbps(&self, node_id: &str) -> u32 {
        match self.get(node_id) {
            Some(bw) if bw.measured_at.elapsed() < STALE_THRESHOLD => bw.kbps,
            Some(bw) => {
                debug!(
                    "bandwidth: stale measurement for {} ({}s old), using as-is",
                    node_id,
                    bw.measured_at.elapsed().as_secs()
                );
                bw.kbps
            }
            None => {
                debug!("bandwidth: no measurement for {}, using default {}kbps", node_id, DEFAULT_BITRATE);
                DEFAULT_BITRATE
            }
        }
    }

    /// Check if the measurement for a peer is stale or missing.
    pub fn is_stale(&self, node_id: &str) -> bool {
        match self.get(node_id) {
            Some(bw) => bw.measured_at.elapsed() >= STALE_THRESHOLD,
            None => true,
        }
    }

    /// Update bandwidth from a completed stream (passive measurement).
    /// Uses exponential moving average to smooth fluctuations.
    pub fn update_from_stream(&self, node_id: &str, bytes: usize, duration: Duration) {
        if duration.as_millis() == 0 {
            return;
        }

        let kbps = ((bytes as f64 * 8.0) / duration.as_secs_f64() / 1000.0) as u32;

        let mut inner = self.inner.write().unwrap();
        let entry = inner.entry(node_id.to_string()).or_insert(PeerBandwidth {
            kbps,
            measured_at: Instant::now(),
            sample_count: 0,
        });

        // Exponential moving average: new = 0.3 * sample + 0.7 * old
        if entry.sample_count > 0 {
            entry.kbps = ((0.3 * kbps as f64) + (0.7 * entry.kbps as f64)) as u32;
        } else {
            entry.kbps = kbps;
        }
        entry.measured_at = Instant::now();
        entry.sample_count += 1;

        debug!(
            "bandwidth: updated {} -> {}kbps (sample #{}, raw={}kbps)",
            node_id, entry.kbps, entry.sample_count, kbps
        );
    }

    /// Set bandwidth from an active probe.
    pub fn update_from_probe(&self, node_id: &str, kbps: u32) {
        let mut inner = self.inner.write().unwrap();
        let entry = inner.entry(node_id.to_string()).or_insert(PeerBandwidth {
            kbps,
            measured_at: Instant::now(),
            sample_count: 0,
        });
        entry.kbps = kbps;
        entry.measured_at = Instant::now();
        entry.sample_count += 1;

        debug!("bandwidth: probe {} -> {}kbps", node_id, kbps);
    }
}

/// Select the appropriate streaming quality based on measured bandwidth
/// and configuration limits.
pub fn select_quality(
    measured_kbps: u32,
    sender_max_bitrate: u32,      // 0 = no limit
    sender_format: &str,          // "raw" = original
    receiver_preferred: u32,       // 0 = auto
    receiver_format: &str,         // "auto" = accept whatever
) -> (Option<u32>, Option<String>) {
    // If receiver has a fixed preference, use it (capped by sender max)
    if receiver_preferred > 0 {
        let bitrate = if sender_max_bitrate > 0 {
            receiver_preferred.min(sender_max_bitrate)
        } else {
            receiver_preferred
        };
        let format = if receiver_format != "auto" {
            Some(receiver_format.to_string())
        } else if sender_format != "raw" {
            Some(sender_format.to_string())
        } else {
            Some("mp3".to_string())
        };
        return (Some(bitrate), format);
    }

    // Auto mode: select based on measured bandwidth
    let (bitrate, format) = match measured_kbps {
        0..=80 => (64, "opus"),
        81..=150 => (128, "mp3"),
        151..=300 => (192, "mp3"),
        301..=600 => (320, "mp3"),
        _ => (0, "raw"), // no transcoding needed
    };

    // Apply sender cap
    let bitrate = if sender_max_bitrate > 0 && bitrate > 0 {
        bitrate.min(sender_max_bitrate)
    } else if sender_max_bitrate > 0 {
        sender_max_bitrate
    } else {
        bitrate
    };

    let format = if sender_format != "raw" && format == "raw" {
        sender_format.to_string()
    } else {
        format.to_string()
    };

    if bitrate == 0 && format == "raw" {
        (None, None) // no transcoding
    } else {
        (Some(bitrate), Some(format))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_auto_quality_slow() {
        let (br, fmt) = select_quality(100, 0, "raw", 0, "auto");
        assert_eq!(br, Some(128));
        assert_eq!(fmt, Some("mp3".into()));
    }

    #[test]
    fn test_auto_quality_fast() {
        let (br, fmt) = select_quality(2000, 0, "raw", 0, "auto");
        assert_eq!(br, None);
        assert_eq!(fmt, None);
    }

    #[test]
    fn test_sender_cap() {
        let (br, fmt) = select_quality(2000, 320, "mp3", 0, "auto");
        assert_eq!(br, Some(320));
        assert_eq!(fmt, Some("mp3".into()));
    }

    #[test]
    fn test_receiver_fixed() {
        let (br, fmt) = select_quality(2000, 0, "raw", 192, "mp3");
        assert_eq!(br, Some(192));
        assert_eq!(fmt, Some("mp3".into()));
    }

    #[test]
    fn test_sender_cap_limits_receiver() {
        let (br, fmt) = select_quality(2000, 128, "mp3", 320, "mp3");
        assert_eq!(br, Some(128));
        assert_eq!(fmt, Some("mp3".into()));
    }
}
