use chrono::Utc;
use tauri::State;
use uuid::Uuid;

use crate::{
    errors::{AppError, FrontendError},
    models::{
        app_state::PersistedAppState,
        presets::{normalize_preset_name, upsert_preset, ServerPreset, StreamPresetSettings},
    },
    services::app_context::AppContext,
};

/// Save the current server preferences and stream quality under `name`.
#[tauri::command]
pub async fn save_server_preset(
    name: String,
    context: State<'_, AppContext>,
) -> Result<PersistedAppState, FrontendError> {
    let name = normalize_preset_name(&name).map_err(AppError::InvalidInput)?;
    let mut outcome = Ok(());
    let state = context
        .update_state(|state| {
            let preset = ServerPreset {
                id: Uuid::new_v4().to_string(),
                name,
                created_at: Utc::now(),
                server_preferences: state.server_preferences.clone(),
                stream: StreamPresetSettings::from_preferences(&state.moonlight_preferences),
            };
            outcome = upsert_preset(&mut state.server_presets, preset);
        })
        .await?;
    outcome.map_err(AppError::InvalidInput)?;
    Ok(state)
}

#[tauri::command]
pub async fn delete_server_preset(
    preset_id: String,
    context: State<'_, AppContext>,
) -> Result<PersistedAppState, FrontendError> {
    Ok(context
        .update_state(|state| state.server_presets.retain(|preset| preset.id != preset_id))
        .await?)
}

/// Make a preset's server preferences and stream quality the current ones.
#[tauri::command]
pub async fn apply_server_preset(
    preset_id: String,
    context: State<'_, AppContext>,
) -> Result<PersistedAppState, FrontendError> {
    let preset = context
        .state
        .read()
        .await
        .server_presets
        .iter()
        .find(|preset| preset.id == preset_id)
        .cloned()
        .ok_or_else(|| AppError::NotFound("That preset no longer exists".to_string()))?;
    Ok(context
        .update_state(|state| {
            state.server_preferences = preset.server_preferences.clone();
            preset.stream.apply_to(&mut state.moonlight_preferences);
            // The previous selection may not match the new filters.
            state.selected_offer = None;
            state.last_error = None;
        })
        .await?)
}
