//! Attributes stream time to the application launched for that stream.

use std::{collections::HashMap, sync::Mutex, sync::OnceLock};

use chrono::{DateTime, Utc};
use tracing::warn;

use crate::{models::play_history, services::app_context::AppContext};

/// A launched app waiting for (or attached to) its stream.
#[derive(Debug, Clone)]
struct CurrentPlay {
    app_id: String,
    launched_at: DateTime<Utc>,
    /// Set once the quality recorder has seen this instance streaming.
    streaming: bool,
}

/// Launches whose stream never started are dropped after this long, so
/// a failed launch is not credited with a later, unrelated session.
const UNCONFIRMED_PLAY_TTL_SECONDS: i64 = 180;

fn current_plays() -> &'static Mutex<HashMap<u64, CurrentPlay>> {
    static PLAYS: OnceLock<Mutex<HashMap<u64, CurrentPlay>>> = OnceLock::new();
    PLAYS.get_or_init(Default::default)
}

fn plays() -> std::sync::MutexGuard<'static, HashMap<u64, CurrentPlay>> {
    current_plays()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// The recorder saw `instance_id` streaming.
pub fn mark_streaming(instance_id: u64) {
    if let Some(play) = plays().get_mut(&instance_id) {
        play.streaming = true;
    }
}

/// Forget launches that never produced a stream.
pub fn expire_unconfirmed(now: DateTime<Utc>) {
    plays().retain(|_, play| {
        play.streaming || (now - play.launched_at).num_seconds() < UNCONFIRMED_PLAY_TTL_SECONDS
    });
}

pub async fn record_launch(
    context: &AppContext,
    instance_id: u64,
    app_id: &str,
    display_name: &str,
) {
    let now = Utc::now();
    plays().insert(
        instance_id,
        CurrentPlay {
            app_id: app_id.to_string(),
            launched_at: now,
            streaming: false,
        },
    );
    if let Err(error) = context
        .update_state(|state| {
            play_history::record_launch(&mut state.play_history, app_id, display_name, now)
        })
        .await
    {
        warn!("could not record launch for play history: {error}");
    }
}

/// Called when the stream on `instance_id` ends.
pub async fn stream_ended(context: &AppContext, instance_id: u64, ended_at: DateTime<Utc>) {
    let Some(play) = plays().remove(&instance_id) else {
        return;
    };
    if !play.streaming {
        return;
    }
    let (app_id, started_at) = (play.app_id, play.launched_at);
    if let Err(error) = context
        .update_state(|state| {
            play_history::add_play_time(&mut state.play_history, &app_id, started_at, ended_at)
        })
        .await
    {
        warn!("could not record play time: {error}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unconfirmed_launches_expire_but_streaming_ones_stay() {
        let now = Utc::now();
        plays().insert(
            9001,
            CurrentPlay {
                app_id: "a".into(),
                launched_at: now - chrono::Duration::minutes(10),
                streaming: false,
            },
        );
        plays().insert(
            9002,
            CurrentPlay {
                app_id: "b".into(),
                launched_at: now - chrono::Duration::minutes(10),
                streaming: false,
            },
        );
        mark_streaming(9002);
        expire_unconfirmed(now);
        assert!(!plays().contains_key(&9001));
        assert!(plays().contains_key(&9002));
        plays().remove(&9002);
    }
}
