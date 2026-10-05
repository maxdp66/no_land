//! Records a stream-quality summary for each streaming session.
//!
//! Samples the Moonlight runtime statistics every five seconds while a
//! stream is live and, when the stream ends or switches instance, stores a
//! `SessionQualityRecord` in `state.quality_history`.

use std::time::Duration;

use chrono::Utc;
use tauri::{AppHandle, Emitter, Manager};
use tracing::warn;

use crate::{
    models::{
        app_state::PersistedAppState,
        provider::resolve_instance,
        quality::{push_record, QualityAccumulator, QualitySample, SessionPlacement},
    },
    moonlight::{composition::MoonlightManager, runtime::RuntimeStatistics},
    services::app_context::AppContext,
};

const SAMPLE_INTERVAL: Duration = Duration::from_secs(5);
const STALE_AFTER: Duration = Duration::from_secs(3);
pub const QUALITY_RECORDED_EVENT: &str = "quality:recorded";

/// Where an instance runs, from the offer it was rented from when known.
pub fn placement_for(state: &PersistedAppState, instance_id: u64) -> SessionPlacement {
    let provider = resolve_instance(&state.provider_instance_refs, instance_id)
        .map(|resolved| resolved.provider.as_str().to_string())
        .unwrap_or_else(|| "vast".to_string());
    let offer_id = state
        .provisioned_servers
        .iter()
        .find(|record| record.instance_id == instance_id)
        .and_then(|record| record.offer_id)
        .or_else(|| {
            (state.instance.instance_id == Some(instance_id))
                .then_some(state.instance.offer_id)
                .flatten()
        });
    let offer = state
        .selected_offer
        .as_ref()
        .filter(|offer| Some(offer.id) == offer_id);
    match offer {
        Some(offer) => SessionPlacement {
            provider,
            host_id: offer.host_id,
            gpu_name: offer.gpu_name.clone(),
            city: offer.city.clone(),
            region: offer.region.clone(),
            country: offer.country.clone(),
        },
        None => SessionPlacement {
            provider,
            ..SessionPlacement::default()
        },
    }
}

pub fn sample_from(stats: &RuntimeStatistics) -> QualitySample {
    QualitySample {
        rtt_ms: stats.estimated_rtt_ms.map(f64::from),
        rtt_variance_ms: stats.estimated_rtt_variance_ms.map(f64::from),
        fps: stats.performance.submitted_fps,
        missing_frames_percent: stats.performance.missing_frames_percent,
        video_mbps: stats.performance.video_mbps,
    }
}

pub fn start(app: AppHandle, context: AppContext) {
    tauri::async_runtime::spawn(async move {
        let mut tick = tokio::time::interval(SAMPLE_INTERVAL);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut current: Option<QualityAccumulator> = None;
        loop {
            tick.tick().await;
            let (stats, instance_id) = {
                let moonlight = app.state::<MoonlightManager>();
                let stats = moonlight.runtime.latest_statistics();
                let instance_id = moonlight
                    .active_stream_instance_id
                    .lock()
                    .ok()
                    .and_then(|guard| *guard);
                (stats, instance_id)
            };
            let live = stats.state == "streaming" && stats.sampled_at.elapsed() <= STALE_AFTER;
            let active = instance_id.filter(|_| live);

            if current
                .as_ref()
                .is_some_and(|session| Some(session.instance_id) != active)
            {
                if let Some(session) = current.take() {
                    finish(&app, &context, session).await;
                }
            }
            let Some(instance_id) = active else {
                crate::services::play_history::expire_unconfirmed(Utc::now());
                continue;
            };
            crate::services::play_history::mark_streaming(instance_id);
            if current.is_none() {
                let placement = placement_for(&*context.state.read().await, instance_id);
                current = Some(QualityAccumulator::new(instance_id, placement, Utc::now()));
            }
            if let Some(session) = current.as_mut() {
                session.add(sample_from(&stats));
            }
        }
    });
}

async fn finish(app: &AppHandle, context: &AppContext, session: QualityAccumulator) {
    let ended_at = Utc::now();
    crate::services::play_history::stream_ended(context, session.instance_id, ended_at).await;
    let Some(record) = session.finish(ended_at) else {
        return;
    };
    let saved = record.clone();
    if let Err(error) = context
        .update_state(|state| push_record(&mut state.quality_history, record))
        .await
    {
        warn!("could not persist stream quality record: {error}");
        return;
    }
    if let Err(error) = app.emit(QUALITY_RECORDED_EVENT, saved) {
        warn!("failed to emit quality record: {error}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::app_state::{OfferCandidate, ProvisionedServerState};

    #[test]
    fn placement_uses_the_offer_the_instance_was_rented_from() {
        let mut state = PersistedAppState::default();
        let mut record = ProvisionedServerState::new(42);
        record.offer_id = Some(7);
        state.provisioned_servers.push(record);
        state.selected_offer = Some(
            serde_json::from_value::<OfferCandidate>(serde_json::json!({
                "id": 7, "hostId": 99, "hostLabel": "", "locationLabel": "", "city": "Austin",
                "region": "Texas", "country": "US", "latitude": 0.0, "longitude": 0.0,
                "reliability": 0.99, "gpuName": "RTX 4090", "gpuRamMb": 0, "gpuCount": 1,
                "hourlyPrice": 0.4, "availableStorageGb": 100, "estimatedDistanceKm": 0.0, "score": 0.0
            }))
            .unwrap(),
        );

        let placement = placement_for(&state, 42);
        assert_eq!(placement.host_id, Some(99));
        assert_eq!(placement.region, "Texas");
        assert_eq!(placement.provider, "vast");

        let unknown = placement_for(&state, 43);
        assert_eq!(unknown.host_id, None);
        assert!(unknown.country.is_empty());
    }
}
