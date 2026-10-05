//! Background price watch: periodically searches offers and emits
//! `price:alert` when an enabled watch finds a cheap enough match.

use std::time::Duration;

use chrono::Utc;
use tauri::{AppHandle, Emitter};
use tracing::warn;

use crate::{
    models::price_alerts::evaluate_alerts,
    services::{
        app_context::AppContext,
        cloud_provider::{CloudClient, OfferSearch},
        offer_selector::OfferSelector,
    },
};

const CHECK_INTERVAL: Duration = Duration::from_secs(15 * 60);
pub const PRICE_ALERT_EVENT: &str = "price:alert";

pub async fn run_price_watch(app: AppHandle, context: AppContext) {
    let mut interval = tokio::time::interval(CHECK_INTERVAL);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        interval.tick().await;
        check_once(&app, &context).await;
    }
}

pub async fn check_once(app: &AppHandle, context: &AppContext) {
    let state = context.state.read().await.clone();
    if !state.price_alerts.iter().any(|alert| alert.enabled) {
        return;
    }
    let Ok(cloud) = CloudClient::from_context(context).await else {
        return;
    };
    let offers = match cloud
        .search_offers(&OfferSearch {
            min_reliability: state.server_preferences.min_reliability,
            limit: context.config.offers_search_limit,
            storage_gb: state.server_preferences.storage_gb,
            geolocation_country_code: None,
            require_verified: false,
            require_datacenter: false,
            require_avx: false,
        })
        .await
    {
        Ok(offers) => offers,
        Err(error) => {
            warn!("price watch offer search failed: {error}");
            return;
        }
    };
    let ranked = OfferSelector {
        scoring: context.config.scoring.clone(),
    }
    .rank_offers(offers, &state.location);

    let mut matches = Vec::new();
    let result = context
        .update_state(|state| {
            matches = evaluate_alerts(&mut state.price_alerts, &ranked, Utc::now());
        })
        .await;
    if let Err(error) = result {
        warn!("price watch could not persist alert state: {error}");
        return;
    }
    for found in matches {
        if let Err(error) = app.emit(PRICE_ALERT_EVENT, found) {
            warn!("failed to emit price alert: {error}");
        }
    }
}
