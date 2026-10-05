//! Named server + stream setups the user can re-apply in one click.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::models::app_state::{MoonlightPreferences, ServerPreferences};

pub const MAX_PRESETS: usize = 20;
pub const MAX_PRESET_NAME_CHARS: usize = 40;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct StreamPresetSettings {
    pub bitrate: u32,
    pub fps: u32,
    pub width: u32,
    pub height: u32,
}

impl StreamPresetSettings {
    pub fn from_preferences(preferences: &MoonlightPreferences) -> Self {
        Self {
            bitrate: preferences.bitrate,
            fps: preferences.fps,
            width: preferences.width,
            height: preferences.height,
        }
    }

    pub fn apply_to(&self, preferences: &mut MoonlightPreferences) {
        preferences.bitrate = self.bitrate;
        preferences.fps = self.fps;
        preferences.width = self.width;
        preferences.height = self.height;
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerPreset {
    pub id: String,
    pub name: String,
    pub created_at: DateTime<Utc>,
    pub server_preferences: ServerPreferences,
    pub stream: StreamPresetSettings,
}

pub fn normalize_preset_name(raw: &str) -> Result<String, String> {
    let name = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    if name.is_empty() {
        return Err("Give the preset a name".to_string());
    }
    if name.chars().count() > MAX_PRESET_NAME_CHARS {
        return Err(format!(
            "Preset names can be at most {MAX_PRESET_NAME_CHARS} characters"
        ));
    }
    Ok(name)
}

/// Insert a preset, replacing one with the same name (case-insensitive).
pub fn upsert_preset(presets: &mut Vec<ServerPreset>, preset: ServerPreset) -> Result<(), String> {
    if let Some(existing) = presets
        .iter_mut()
        .find(|existing| existing.name.eq_ignore_ascii_case(&preset.name))
    {
        let id = existing.id.clone();
        let created_at = existing.created_at;
        *existing = ServerPreset {
            id,
            created_at,
            ..preset
        };
        return Ok(());
    }
    if presets.len() >= MAX_PRESETS {
        return Err(format!(
            "You can keep up to {MAX_PRESETS} presets; delete one first"
        ));
    }
    presets.push(preset);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn preset(name: &str, storage_gb: u32) -> ServerPreset {
        let server_preferences = ServerPreferences {
            storage_gb,
            ..ServerPreferences::default()
        };
        ServerPreset {
            id: format!("id-{name}"),
            name: name.to_string(),
            created_at: Utc::now(),
            server_preferences,
            stream: StreamPresetSettings {
                bitrate: 50_000,
                fps: 120,
                width: 2560,
                height: 1440,
            },
        }
    }

    #[test]
    fn names_are_trimmed_and_bounded() {
        assert_eq!(normalize_preset_name("  4090   EU  ").unwrap(), "4090 EU");
        assert!(normalize_preset_name("   ").is_err());
        assert!(normalize_preset_name(&"x".repeat(41)).is_err());
    }

    #[test]
    fn same_name_replaces_but_keeps_identity() {
        let mut presets = vec![preset("Cyberpunk", 100)];
        upsert_preset(&mut presets, preset("cyberpunk", 250)).unwrap();
        assert_eq!(presets.len(), 1);
        assert_eq!(presets[0].id, "id-Cyberpunk");
        assert_eq!(presets[0].server_preferences.storage_gb, 250);
    }

    #[test]
    fn preset_count_is_capped() {
        let mut presets = (0..MAX_PRESETS)
            .map(|index| preset(&format!("p{index}"), 100))
            .collect::<Vec<_>>();
        assert!(upsert_preset(&mut presets, preset("one more", 100)).is_err());
        // Replacing an existing one is still allowed at the cap.
        assert!(upsert_preset(&mut presets, preset("p3", 120)).is_ok());
    }

    #[test]
    fn stream_settings_round_trip() {
        let mut preferences = MoonlightPreferences::default();
        let settings = StreamPresetSettings {
            bitrate: 80_000,
            fps: 144,
            width: 3440,
            height: 1440,
        };
        settings.apply_to(&mut preferences);
        assert_eq!(StreamPresetSettings::from_preferences(&preferences), settings);
    }
}
