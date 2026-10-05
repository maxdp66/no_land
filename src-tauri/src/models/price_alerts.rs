//! Price watches: notify when an offer matching a GPU/region filter drops
//! to or below a target hourly price.

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};

use crate::models::app_state::OfferCandidate;

pub const MAX_PRICE_ALERTS: usize = 10;
/// Do not re-notify for the same watch more often than this unless the
/// price drops further.
pub const RENOTIFY_AFTER_HOURS: i64 = 6;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PriceAlert {
    pub id: String,
    /// Case-insensitive substring of the GPU name, e.g. "4090". Empty = any.
    pub gpu_query: String,
    /// ISO country code, or empty for anywhere.
    #[serde(default)]
    pub country_code: String,
    pub max_hourly_usd: f64,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub last_notified_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub last_notified_price: Option<f64>,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PriceAlertMatch {
    pub alert_id: String,
    pub gpu_query: String,
    pub max_hourly_usd: f64,
    pub offer_id: u64,
    pub provider: String,
    pub gpu_name: String,
    pub location_label: String,
    pub hourly_price: f64,
}

pub fn validate_alert(gpu_query: &str, country_code: &str, max_hourly_usd: f64) -> Result<(), String> {
    if gpu_query.chars().count() > 40 {
        return Err("GPU filter is too long".to_string());
    }
    let code = country_code.trim();
    if !code.is_empty() && !(code.len() == 2 && code.chars().all(|ch| ch.is_ascii_alphabetic())) {
        return Err("Use a two-letter country code, or leave it empty for anywhere".to_string());
    }
    if !max_hourly_usd.is_finite() || max_hourly_usd <= 0.0 || max_hourly_usd > 100.0 {
        return Err("Target price must be between $0 and $100 per hour".to_string());
    }
    Ok(())
}

pub fn offer_matches(alert: &PriceAlert, offer: &OfferCandidate) -> bool {
    let query = alert.gpu_query.trim().to_ascii_lowercase();
    let gpu_ok = query.is_empty()
        || offer
            .gpu_name
            .to_ascii_lowercase()
            .replace(' ', "")
            .contains(&query.replace(' ', ""));
    let code = alert.country_code.trim();
    let country_ok = code.is_empty() || offer.country.trim().eq_ignore_ascii_case(code);
    gpu_ok && country_ok && offer.hourly_price > 0.0 && offer.hourly_price <= alert.max_hourly_usd
}

/// Cheapest matching offer for each enabled alert that is due to notify.
/// Marks those alerts as notified.
pub fn evaluate_alerts(
    alerts: &mut [PriceAlert],
    offers: &[OfferCandidate],
    now: DateTime<Utc>,
) -> Vec<PriceAlertMatch> {
    let mut matches = Vec::new();
    for alert in alerts.iter_mut().filter(|alert| alert.enabled) {
        let Some(best) = offers
            .iter()
            .filter(|offer| offer_matches(alert, offer))
            .min_by(|left, right| left.hourly_price.total_cmp(&right.hourly_price))
        else {
            continue;
        };
        let cooled_down = alert
            .last_notified_at
            .is_none_or(|at| now - at >= Duration::hours(RENOTIFY_AFTER_HOURS));
        let cheaper = alert
            .last_notified_price
            .is_none_or(|price| best.hourly_price < price - 0.005);
        if !(cooled_down || cheaper) {
            continue;
        }
        alert.last_notified_at = Some(now);
        alert.last_notified_price = Some(best.hourly_price);
        matches.push(PriceAlertMatch {
            alert_id: alert.id.clone(),
            gpu_query: alert.gpu_query.clone(),
            max_hourly_usd: alert.max_hourly_usd,
            offer_id: best.id,
            provider: best.provider.clone(),
            gpu_name: best.gpu_name.clone(),
            location_label: best.location_label.clone(),
            hourly_price: best.hourly_price,
        });
    }
    matches
}

#[cfg(test)]
mod tests {
    use super::*;

    fn offer(id: u64, gpu: &str, country: &str, price: f64) -> OfferCandidate {
        serde_json::from_value(serde_json::json!({
            "id": id, "hostId": null, "hostLabel": "", "locationLabel": format!("City, {country}"),
            "city": "City", "region": "", "country": country, "latitude": 0.0, "longitude": 0.0,
            "reliability": 0.99, "gpuName": gpu, "gpuRamMb": 24576, "gpuCount": 1,
            "hourlyPrice": price, "availableStorageGb": 100, "estimatedDistanceKm": 0.0, "score": 0.0
        }))
        .unwrap()
    }

    fn alert(query: &str, country: &str, max: f64) -> PriceAlert {
        PriceAlert {
            id: "a".into(),
            gpu_query: query.into(),
            country_code: country.into(),
            max_hourly_usd: max,
            enabled: true,
            last_notified_at: None,
            last_notified_price: None,
        }
    }

    fn at(hour: u32) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(&format!("2026-10-05T{hour:02}:00:00Z"))
            .unwrap()
            .with_timezone(&Utc)
    }

    #[test]
    fn matches_gpu_country_and_price() {
        let watch = alert("4090", "US", 0.5);
        assert!(offer_matches(&watch, &offer(1, "RTX 4090", "US", 0.45)));
        assert!(!offer_matches(&watch, &offer(1, "RTX 4090", "DE", 0.45)));
        assert!(!offer_matches(&watch, &offer(1, "RTX 4090", "US", 0.55)));
        assert!(!offer_matches(&watch, &offer(1, "RTX 3090", "US", 0.30)));
        assert!(offer_matches(&alert("rtx4090", "", 0.5), &offer(1, "RTX 4090", "JP", 0.5)));
    }

    #[test]
    fn notifies_cheapest_once_then_on_cooldown_or_drop() {
        let mut alerts = vec![alert("4090", "", 0.6)];
        let offers = vec![offer(1, "RTX 4090", "US", 0.55), offer(2, "RTX 4090", "CA", 0.41)];
        let first = evaluate_alerts(&mut alerts, &offers, at(1));
        assert_eq!(first.len(), 1);
        assert_eq!(first[0].offer_id, 2);

        assert!(evaluate_alerts(&mut alerts, &offers, at(2)).is_empty(), "within cooldown");
        let cheaper = vec![offer(3, "RTX 4090", "US", 0.35)];
        assert_eq!(evaluate_alerts(&mut alerts, &cheaper, at(3)).len(), 1, "price dropped");
        assert_eq!(evaluate_alerts(&mut alerts, &cheaper, at(10)).len(), 1, "cooldown elapsed");
    }

    #[test]
    fn disabled_alerts_never_fire() {
        let mut alerts = vec![PriceAlert {
            enabled: false,
            ..alert("", "", 10.0)
        }];
        assert!(evaluate_alerts(&mut alerts, &[offer(1, "A100", "US", 1.0)], at(1)).is_empty());
    }

    #[test]
    fn validation_rejects_bad_inputs() {
        assert!(validate_alert("4090", "US", 0.5).is_ok());
        assert!(validate_alert("4090", "USA", 0.5).is_err());
        assert!(validate_alert("4090", "", 0.0).is_err());
        assert!(validate_alert("4090", "", f64::NAN).is_err());
    }
}
