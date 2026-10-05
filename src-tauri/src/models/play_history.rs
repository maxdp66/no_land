//! Desktop-side play time per launched application.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

pub const MAX_PLAY_ENTRIES: usize = 200;
/// A single session longer than this is treated as a stuck measurement.
const MAX_SESSION_SECONDS: f64 = 24.0 * 3600.0;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PlayStats {
    pub app_id: String,
    pub display_name: String,
    pub total_play_seconds: f64,
    pub launch_count: u32,
    pub last_played_at: DateTime<Utc>,
}

pub fn record_launch(history: &mut Vec<PlayStats>, app_id: &str, display_name: &str, now: DateTime<Utc>) {
    match history.iter_mut().find(|entry| entry.app_id == app_id) {
        Some(entry) => {
            entry.launch_count += 1;
            entry.last_played_at = now;
            if !display_name.trim().is_empty() {
                entry.display_name = display_name.to_string();
            }
        }
        None => history.push(PlayStats {
            app_id: app_id.to_string(),
            display_name: display_name.to_string(),
            total_play_seconds: 0.0,
            launch_count: 1,
            last_played_at: now,
        }),
    }
    if history.len() > MAX_PLAY_ENTRIES {
        history.sort_by(|left, right| right.last_played_at.cmp(&left.last_played_at));
        history.truncate(MAX_PLAY_ENTRIES);
    }
}

pub fn add_play_time(history: &mut [PlayStats], app_id: &str, started_at: DateTime<Utc>, ended_at: DateTime<Utc>) {
    let seconds = (ended_at - started_at).num_milliseconds() as f64 / 1000.0;
    if !(seconds > 0.0 && seconds <= MAX_SESSION_SECONDS) {
        return;
    }
    if let Some(entry) = history.iter_mut().find(|entry| entry.app_id == app_id) {
        entry.total_play_seconds += seconds;
        entry.last_played_at = entry.last_played_at.max(ended_at);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(hour: u32) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(&format!("2026-10-05T{hour:02}:00:00Z"))
            .unwrap()
            .with_timezone(&Utc)
    }

    #[test]
    fn launches_and_play_time_accumulate() {
        let mut history = Vec::new();
        record_launch(&mut history, "cp2077", "Cyberpunk 2077", at(1));
        add_play_time(&mut history, "cp2077", at(1), at(3));
        record_launch(&mut history, "cp2077", "Cyberpunk 2077", at(5));
        add_play_time(&mut history, "cp2077", at(5), at(6));

        assert_eq!(history.len(), 1);
        assert_eq!(history[0].launch_count, 2);
        assert!((history[0].total_play_seconds - 3.0 * 3600.0).abs() < 1e-6);
        assert_eq!(history[0].last_played_at, at(6));
    }

    #[test]
    fn implausible_durations_are_ignored() {
        let mut history = Vec::new();
        record_launch(&mut history, "game", "Game", at(2));
        add_play_time(&mut history, "game", at(3), at(2));
        add_play_time(&mut history, "unknown", at(1), at(2));
        assert_eq!(history[0].total_play_seconds, 0.0);
    }

    #[test]
    fn history_keeps_the_most_recent_entries() {
        let mut history = Vec::new();
        for index in 0..=MAX_PLAY_ENTRIES {
            let when = at(0) + chrono::Duration::minutes(index as i64);
            record_launch(&mut history, &format!("app{index}"), "", when);
        }
        assert_eq!(history.len(), MAX_PLAY_ENTRIES);
        assert!(!history.iter().any(|entry| entry.app_id == "app0"));
    }
}
