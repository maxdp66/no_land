//! Per-session stream quality history.
//!
//! While a stream runs, samples of the Moonlight control-channel RTT, RTT
//! variation, frame rate and missing frames are accumulated. When the stream
//! ends, a summary record is kept (newest 200). Offer ranking uses these
//! records to surface hosts and regions that have streamed well for this
//! user and to push down ones that have not.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

pub const MAX_QUALITY_RECORDS: usize = 200;
/// Sessions shorter than this many samples (5 s apart) are not recorded.
pub const MIN_SAMPLES: u32 = 6;
/// Scores below this mark a host/region as a poor experience (about
/// 55 ms average RTT with no jitter or frame loss).
pub const POOR_SCORE: f64 = 60.0;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SessionQualityRecord {
    pub instance_id: u64,
    #[serde(default)]
    pub provider: String,
    #[serde(default)]
    pub host_id: Option<u64>,
    #[serde(default)]
    pub gpu_name: String,
    #[serde(default)]
    pub city: String,
    #[serde(default)]
    pub region: String,
    #[serde(default)]
    pub country: String,
    pub started_at: DateTime<Utc>,
    pub ended_at: DateTime<Utc>,
    pub samples: u32,
    pub avg_rtt_ms: Option<f64>,
    pub max_rtt_ms: Option<f64>,
    pub avg_rtt_variance_ms: Option<f64>,
    pub avg_fps: f64,
    pub avg_missing_frames_percent: f64,
    pub avg_video_mbps: f64,
    pub score: f64,
}

/// Where an offer's quality estimate comes from.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ObservedQuality {
    pub score: f64,
    pub sessions: usize,
    pub avg_rtt_ms: Option<f64>,
    /// `host` (same machine) or `region` (same country/region).
    pub basis: String,
}

/// Placement of the session, captured when it started.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SessionPlacement {
    pub provider: String,
    pub host_id: Option<u64>,
    pub gpu_name: String,
    pub city: String,
    pub region: String,
    pub country: String,
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct QualitySample {
    pub rtt_ms: Option<f64>,
    pub rtt_variance_ms: Option<f64>,
    pub fps: f64,
    pub missing_frames_percent: f64,
    pub video_mbps: f64,
}

#[derive(Debug, Clone)]
pub struct QualityAccumulator {
    pub instance_id: u64,
    pub placement: SessionPlacement,
    pub started_at: DateTime<Utc>,
    samples: u32,
    rtt_samples: u32,
    rtt_sum: f64,
    rtt_max: f64,
    variance_samples: u32,
    variance_sum: f64,
    fps_sum: f64,
    missing_sum: f64,
    mbps_sum: f64,
}

impl QualityAccumulator {
    pub fn new(instance_id: u64, placement: SessionPlacement, started_at: DateTime<Utc>) -> Self {
        Self {
            instance_id,
            placement,
            started_at,
            samples: 0,
            rtt_samples: 0,
            rtt_sum: 0.0,
            rtt_max: 0.0,
            variance_samples: 0,
            variance_sum: 0.0,
            fps_sum: 0.0,
            missing_sum: 0.0,
            mbps_sum: 0.0,
        }
    }

    pub fn add(&mut self, sample: QualitySample) {
        let finite = |value: f64| value.is_finite() && value >= 0.0;
        self.samples += 1;
        if let Some(rtt) = sample.rtt_ms.filter(|value| finite(*value)) {
            self.rtt_samples += 1;
            self.rtt_sum += rtt;
            self.rtt_max = self.rtt_max.max(rtt);
        }
        if let Some(variance) = sample.rtt_variance_ms.filter(|value| finite(*value)) {
            self.variance_samples += 1;
            self.variance_sum += variance;
        }
        if finite(sample.fps) {
            self.fps_sum += sample.fps;
        }
        if finite(sample.missing_frames_percent) {
            self.missing_sum += sample.missing_frames_percent;
        }
        if finite(sample.video_mbps) {
            self.mbps_sum += sample.video_mbps;
        }
    }

    pub fn finish(self, ended_at: DateTime<Utc>) -> Option<SessionQualityRecord> {
        if self.samples < MIN_SAMPLES {
            return None;
        }
        let count = f64::from(self.samples);
        let avg_rtt_ms = (self.rtt_samples > 0).then(|| self.rtt_sum / f64::from(self.rtt_samples));
        let avg_rtt_variance_ms = (self.variance_samples > 0)
            .then(|| self.variance_sum / f64::from(self.variance_samples));
        let avg_missing_frames_percent = self.missing_sum / count;
        Some(SessionQualityRecord {
            instance_id: self.instance_id,
            provider: self.placement.provider,
            host_id: self.placement.host_id,
            gpu_name: self.placement.gpu_name,
            city: self.placement.city,
            region: self.placement.region,
            country: self.placement.country,
            started_at: self.started_at,
            ended_at,
            samples: self.samples,
            avg_rtt_ms,
            max_rtt_ms: (self.rtt_samples > 0).then_some(self.rtt_max),
            avg_rtt_variance_ms,
            avg_fps: self.fps_sum / count,
            avg_missing_frames_percent,
            avg_video_mbps: self.mbps_sum / count,
            score: quality_score(avg_rtt_ms, avg_rtt_variance_ms, avg_missing_frames_percent),
        })
    }
}

/// 0–100: full marks up to 15 ms RTT, then 1 point per ms (max −50),
/// 2 points per ms of RTT variation (max −25), and 5 points per percent of
/// missing frames (max −25).
pub fn quality_score(
    avg_rtt_ms: Option<f64>,
    avg_rtt_variance_ms: Option<f64>,
    missing_frames_percent: f64,
) -> f64 {
    let rtt_penalty = avg_rtt_ms.map_or(0.0, |rtt| (rtt - 15.0).clamp(0.0, 50.0));
    let jitter_penalty = avg_rtt_variance_ms.map_or(0.0, |variance| (variance * 2.0).min(25.0));
    let loss_penalty = (missing_frames_percent.max(0.0) * 5.0).min(25.0);
    (100.0 - rtt_penalty - jitter_penalty - loss_penalty).clamp(0.0, 100.0)
}

pub fn push_record(history: &mut Vec<SessionQualityRecord>, record: SessionQualityRecord) {
    history.push(record);
    if history.len() > MAX_QUALITY_RECORDS {
        let excess = history.len() - MAX_QUALITY_RECORDS;
        history.drain(..excess);
    }
}

fn normalize(value: &str) -> String {
    value
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .collect::<String>()
        .to_ascii_lowercase()
}

fn summarize<'a>(
    records: impl Iterator<Item = &'a SessionQualityRecord>,
    basis: &str,
) -> Option<ObservedQuality> {
    let records = records.collect::<Vec<_>>();
    if records.is_empty() {
        return None;
    }
    let sessions = records.len();
    let score = records.iter().map(|record| record.score).sum::<f64>() / sessions as f64;
    let rtts = records
        .iter()
        .filter_map(|record| record.avg_rtt_ms)
        .collect::<Vec<_>>();
    Some(ObservedQuality {
        score,
        sessions,
        avg_rtt_ms: (!rtts.is_empty()).then(|| rtts.iter().sum::<f64>() / rtts.len() as f64),
        basis: basis.to_string(),
    })
}

/// The user's own history for this host, else for its country/region.
pub fn observed_quality(
    history: &[SessionQualityRecord],
    provider: &str,
    host_id: Option<u64>,
    country: &str,
    region: &str,
) -> Option<ObservedQuality> {
    if let Some(host_id) = host_id {
        let host = summarize(
            history
                .iter()
                .filter(|record| record.host_id == Some(host_id) && record.provider == provider),
            "host",
        );
        if host.is_some() {
            return host;
        }
    }
    let country = normalize(country);
    if country.is_empty() {
        return None;
    }
    let region = normalize(region);
    summarize(
        history.iter().filter(|record| {
            normalize(&record.country) == country && normalize(&record.region) == region
        }),
        "region",
    )
}

/// Rough round-trip estimate from distance: ~1 ms per 100 km of fiber plus
/// a fixed allowance for access networks and encode/decode hops.
pub fn estimated_rtt_from_distance(distance_km: f64) -> Option<f64> {
    (0.0..20_000.0)
        .contains(&distance_km)
        .then(|| (distance_km / 100.0 + 8.0).round())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(minute: u32) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(&format!("2026-10-05T10:{minute:02}:00Z"))
            .unwrap()
            .with_timezone(&Utc)
    }

    fn placement(host: Option<u64>, country: &str, region: &str) -> SessionPlacement {
        SessionPlacement {
            provider: "vast".into(),
            host_id: host,
            gpu_name: "RTX 4090".into(),
            city: "City".into(),
            region: region.into(),
            country: country.into(),
        }
    }

    fn record(host: Option<u64>, country: &str, region: &str, rtt: f64) -> SessionQualityRecord {
        let mut accumulator = QualityAccumulator::new(1, placement(host, country, region), at(0));
        for _ in 0..MIN_SAMPLES {
            accumulator.add(QualitySample {
                rtt_ms: Some(rtt),
                rtt_variance_ms: Some(1.0),
                fps: 60.0,
                missing_frames_percent: 0.0,
                video_mbps: 40.0,
            });
        }
        accumulator.finish(at(5)).unwrap()
    }

    #[test]
    fn short_sessions_are_not_recorded() {
        let mut accumulator = QualityAccumulator::new(1, SessionPlacement::default(), at(0));
        accumulator.add(QualitySample::default());
        assert!(accumulator.finish(at(1)).is_none());
    }

    #[test]
    fn accumulator_averages_and_ignores_invalid_values() {
        let mut accumulator = QualityAccumulator::new(9, placement(Some(3), "US", "CA"), at(0));
        for index in 0..MIN_SAMPLES {
            accumulator.add(QualitySample {
                rtt_ms: if index == 0 {
                    None
                } else {
                    Some(20.0 + f64::from(index))
                },
                rtt_variance_ms: Some(f64::NAN),
                fps: 120.0,
                missing_frames_percent: 1.0,
                video_mbps: 50.0,
            });
        }
        let record = accumulator.finish(at(3)).unwrap();
        assert_eq!(record.samples, MIN_SAMPLES);
        assert!((record.avg_rtt_ms.unwrap() - 23.0).abs() < 1e-9);
        assert_eq!(record.max_rtt_ms, Some(25.0));
        assert_eq!(record.avg_rtt_variance_ms, None);
        assert!((record.avg_fps - 120.0).abs() < 1e-9);
        // 8 ms over the free threshold and 1% missing frames.
        assert!((record.score - (100.0 - 8.0 - 5.0)).abs() < 1e-9);
    }

    #[test]
    fn score_is_bounded() {
        assert_eq!(quality_score(Some(10.0), Some(0.0), 0.0), 100.0);
        assert_eq!(quality_score(Some(500.0), Some(100.0), 50.0), 0.0);
        assert_eq!(quality_score(None, None, 0.0), 100.0);
    }

    #[test]
    fn host_history_wins_over_region_history() {
        let history = vec![
            record(Some(7), "US", "California", 80.0),
            record(Some(8), "US", "California", 12.0),
            record(Some(8), "US", "California", 14.0),
        ];
        let host = observed_quality(&history, "vast", Some(8), "US", "California").unwrap();
        assert_eq!(host.basis, "host");
        assert_eq!(host.sessions, 2);
        assert!((host.avg_rtt_ms.unwrap() - 13.0).abs() < 1e-9);

        let region = observed_quality(&history, "vast", Some(99), "us", "california").unwrap();
        assert_eq!(region.basis, "region");
        assert_eq!(region.sessions, 3);

        assert!(observed_quality(&history, "vast", None, "DE", "").is_none());
        assert!(
            observed_quality(&history, "tensordock", Some(8), "", "").is_none(),
            "host ids are provider-scoped"
        );
    }

    #[test]
    fn history_is_capped() {
        let mut history = Vec::new();
        for _ in 0..(MAX_QUALITY_RECORDS + 5) {
            push_record(&mut history, record(None, "US", "", 20.0));
        }
        assert_eq!(history.len(), MAX_QUALITY_RECORDS);
    }

    #[test]
    fn distance_estimate() {
        assert_eq!(estimated_rtt_from_distance(0.0), Some(8.0));
        assert_eq!(estimated_rtt_from_distance(1500.0), Some(23.0));
        assert_eq!(estimated_rtt_from_distance(99999.0), None);
    }
}
