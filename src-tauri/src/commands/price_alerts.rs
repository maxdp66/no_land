use tauri::{AppHandle, State};
use uuid::Uuid;

use crate::{
    errors::{AppError, FrontendError},
    models::{
        app_state::PersistedAppState,
        price_alerts::{validate_alert, PriceAlert, MAX_PRICE_ALERTS},
    },
    services::{app_context::AppContext, price_watch},
};

#[tauri::command]
pub async fn save_price_alert(
    gpu_query: String,
    country_code: String,
    max_hourly_usd: f64,
    app: AppHandle,
    context: State<'_, AppContext>,
) -> Result<PersistedAppState, FrontendError> {
    let gpu_query = gpu_query.trim().to_string();
    let country_code = country_code.trim().to_uppercase();
    validate_alert(&gpu_query, &country_code, max_hourly_usd).map_err(AppError::InvalidInput)?;
    if context.state.read().await.price_alerts.len() >= MAX_PRICE_ALERTS {
        return Err(AppError::InvalidInput(format!(
            "You can keep up to {MAX_PRICE_ALERTS} price alerts; delete one first"
        ))
        .into());
    }
    let state = context
        .update_state(|state| {
            state.price_alerts.push(PriceAlert {
                id: Uuid::new_v4().to_string(),
                gpu_query,
                country_code,
                max_hourly_usd,
                enabled: true,
                last_notified_at: None,
                last_notified_price: None,
            });
        })
        .await?;
    // Check right away so the user does not wait for the next interval.
    let check_context = context.inner().clone();
    tauri::async_runtime::spawn(async move {
        price_watch::check_once(&app, &check_context).await;
    });
    Ok(state)
}

#[tauri::command]
pub async fn delete_price_alert(
    alert_id: String,
    context: State<'_, AppContext>,
) -> Result<PersistedAppState, FrontendError> {
    Ok(context
        .update_state(|state| state.price_alerts.retain(|alert| alert.id != alert_id))
        .await?)
}

#[tauri::command]
pub async fn set_price_alert_enabled(
    alert_id: String,
    enabled: bool,
    context: State<'_, AppContext>,
) -> Result<PersistedAppState, FrontendError> {
    Ok(context
        .update_state(|state| {
            if let Some(alert) = state.price_alerts.iter_mut().find(|alert| alert.id == alert_id) {
                alert.enabled = enabled;
                if enabled {
                    alert.last_notified_at = None;
                    alert.last_notified_price = None;
                }
            }
        })
        .await?)
}
