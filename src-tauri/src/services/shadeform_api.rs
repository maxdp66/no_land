//! Shadeform client.
//!
//! Shadeform resells virtual machines from many GPU clouds (Lambda, Massed
//! Compute, Hyperstack, DataCrunch, Scaleway and others) behind one API, so a
//! single integration reaches several sources of capacity. Every instance is a
//! full VM or bare-metal machine with a public IP, which the provisioning
//! pipeline needs (systemd, a headless NVIDIA display and inbound UDP for
//! WireGuard/GameStream).
//!
//! Offers and instances are converted to the shared `VastOffer` /
//! `VastInstance` shapes, and Shadeform's UUIDs are mapped to local `u64` ids
//! with [`foreign_local_id`], like TensorDock's.
//!
//! The request and response shapes follow Shadeform's v1 API as used by
//! SkyPilot's Shadeform provider: `X-API-KEY` auth, `GET /instances/types`,
//! `POST /instances/create`, `GET /instances`, `GET /instances/{id}/info`,
//! `POST /instances/{id}/delete`, `GET /sshkeys` and `POST /sshkeys/add`.
//! Shadeform cannot stop an instance; it can only delete it.

use std::time::Instant;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tracing::{info, warn};

use crate::{
    errors::{AppError, AppResult},
    models::{
        provider::{foreign_local_id, CloudProviderKind},
        vast::{VastInstance, VastOffer},
    },
    services::tensordock_api::{country_code_for, json_shape},
};

pub const DEFAULT_SHADEFORM_BASE_URL: &str = "https://api.shadeform.ai/v1";
/// Login user Shadeform creates on its images when an instance reports none.
pub const SHADEFORM_DEFAULT_SSH_USER: &str = "shadeform";
const SSH_KEY_NAME: &str = "noland-connect";

/// Clouds behind Shadeform that are not usable here: container platforms
/// without inbound UDP, and providers the app already integrates directly.
const EXCLUDED_CLOUDS: &[&str] = &["runpod", "vastai", "vast", "tensordock"];

/// Datacenter GPUs without an NVENC video encoder, which Sunshine needs.
const NO_NVENC_GPU_PREFIXES: &[&str] = &[
    "A100", "A800", "H100", "H200", "H800", "GH200", "B100", "B200", "GB200", "MI", "GAUDI",
];

/// Everything needed to deploy one offer, serialized into
/// `VastOffer::provider_offer_ref`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ShadeformOfferRef {
    pub cloud: String,
    pub region: String,
    pub shade_instance_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub os: Option<String>,
}

/// A Shadeform instance plus its provider-side id and login user.
#[derive(Debug, Clone)]
pub struct ShadeformInstance {
    pub remote_id: String,
    pub ssh_user: String,
    pub deleted: bool,
    pub instance: VastInstance,
}

#[derive(Clone)]
pub struct ShadeformApiClient {
    http: reqwest::Client,
    base_url: String,
    api_key: String,
}

impl ShadeformApiClient {
    pub fn new(http: reqwest::Client, base_url: String, api_key: String) -> Self {
        Self {
            http,
            base_url,
            api_key,
        }
    }

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.base_url.trim_end_matches('/'), path)
    }

    async fn request(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<Value>,
    ) -> AppResult<Value> {
        let url = self.url(path);
        let started = Instant::now();
        let mut request = self
            .http
            .request(method.clone(), &url)
            .header("X-API-KEY", &self.api_key);
        if let Some(body) = body {
            request = request.json(&body);
        }
        let response = request
            .send()
            .await
            .map_err(|error| AppError::Api(format!("Shadeform {method} {url} failed: {error}")))?;
        let status = response.status();
        let text = response.text().await.map_err(|error| {
            AppError::Api(format!(
                "Shadeform {method} {url} response read failed: {error}"
            ))
        })?;
        let parsed = if text.trim().is_empty() {
            Value::Null
        } else {
            serde_json::from_str::<Value>(&text).unwrap_or(Value::String(text))
        };
        info!(
            "Shadeform {} {} -> {} in {}ms",
            method,
            url,
            status,
            started.elapsed().as_millis()
        );

        if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
            return Err(AppError::Authentication);
        }
        if !status.is_success() {
            let detail = error_detail(&parsed);
            warn!("Shadeform {method} {url} -> {status}: {detail}");
            if status == reqwest::StatusCode::NOT_FOUND {
                return Err(AppError::NotFound(format!("Shadeform {path}: {detail}")));
            }
            return Err(AppError::Api(format!(
                "Shadeform {method} {path} -> {status}: {detail}"
            )));
        }
        Ok(parsed)
    }

    pub async fn search_offers(
        &self,
        storage_gb: u32,
        country_code: Option<&str>,
    ) -> AppResult<Vec<VastOffer>> {
        let body = self
            .request(
                reqwest::Method::GET,
                "/instances/types?available=true",
                None,
            )
            .await?;
        let offers = parse_instance_types(&body, storage_gb)
            .into_iter()
            .filter(|offer| match country_code {
                Some(code) if !code.trim().is_empty() && !code.eq_ignore_ascii_case("GLOBAL") => {
                    offer.country.eq_ignore_ascii_case(code.trim())
                }
                _ => true,
            })
            .collect::<Vec<_>>();
        info!("Shadeform returned {} offers", offers.len());
        Ok(offers)
    }

    /// Id of the account SSH key matching `public_key`, adding it if missing.
    async fn ensure_ssh_key(&self, public_key: &str) -> AppResult<String> {
        let wanted = key_material(public_key).ok_or_else(|| {
            AppError::InvalidInput("The managed SSH public key is empty or malformed".to_string())
        })?;
        let body = self.request(reqwest::Method::GET, "/sshkeys", None).await?;
        let keys = body
            .get("ssh_keys")
            .or_else(|| body.get("sshkeys"))
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        if let Some(id) = keys.iter().find_map(|key| {
            let material = key
                .get("public_key")
                .and_then(Value::as_str)
                .and_then(key_material)?;
            (material == wanted)
                .then(|| key.get("id").and_then(value_as_string))
                .flatten()
        }) {
            return Ok(id);
        }

        let body = self
            .request(
                reqwest::Method::POST,
                "/sshkeys/add",
                Some(json!({ "name": SSH_KEY_NAME, "public_key": public_key.trim() })),
            )
            .await?;
        body.get("id").and_then(value_as_string).ok_or_else(|| {
            AppError::Api(format!(
                "Shadeform add SSH key response did not include an id: {}",
                json_shape(&body, 2)
            ))
        })
    }

    pub async fn create_instance(
        &self,
        offer_ref: &ShadeformOfferRef,
        label: &str,
        ssh_public_key: &str,
    ) -> AppResult<ShadeformInstance> {
        let ssh_key_id = self.ensure_ssh_key(ssh_public_key).await?;
        let payload = create_instance_payload(offer_ref, label, &ssh_key_id);
        let body = self
            .request(reqwest::Method::POST, "/instances/create", Some(payload))
            .await?;
        let remote_id = body.get("id").and_then(value_as_string).ok_or_else(|| {
            AppError::Api(format!(
                "Shadeform create instance response did not include an id: {}",
                json_shape(&body, 2)
            ))
        })?;
        info!("Shadeform create_instance accepted id={remote_id}");
        self.get_instance(&remote_id).await
    }

    pub async fn get_instance(&self, remote_id: &str) -> AppResult<ShadeformInstance> {
        let body = self
            .request(
                reqwest::Method::GET,
                &format!("/instances/{remote_id}/info"),
                None,
            )
            .await?;
        parse_instance(&body, Some(remote_id)).ok_or_else(|| {
            AppError::Api(format!(
                "Shadeform instance payload missing expected fields for {remote_id}"
            ))
        })
    }

    /// Live instances. Deleted ones are left out so reconciliation drops them.
    pub async fn list_instances(&self) -> AppResult<Vec<ShadeformInstance>> {
        let body = self
            .request(reqwest::Method::GET, "/instances", None)
            .await?;
        let items = body
            .get("instances")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        Ok(items
            .iter()
            .filter_map(|item| parse_instance(item, None))
            .filter(|instance| !instance.deleted)
            .collect())
    }

    pub async fn delete_instance(&self, remote_id: &str) -> AppResult<()> {
        match self
            .request(
                reqwest::Method::POST,
                &format!("/instances/{remote_id}/delete"),
                None,
            )
            .await
        {
            Ok(_) | Err(AppError::NotFound(_)) => Ok(()),
            Err(error) => Err(error),
        }
    }

    pub async fn check_credentials(&self) -> AppResult<()> {
        self.request(reqwest::Method::GET, "/instances", None)
            .await
            .map(|_| ())
    }
}

/// Request body for `POST /instances/create`.
pub fn create_instance_payload(
    offer_ref: &ShadeformOfferRef,
    label: &str,
    ssh_key_id: &str,
) -> Value {
    let mut payload = json!({
        "cloud": offer_ref.cloud,
        "region": offer_ref.region,
        "shade_instance_type": offer_ref.shade_instance_type,
        "shade_cloud": true,
        "name": instance_name(label),
        "ssh_key_id": ssh_key_id,
    });
    if let Some(os) = offer_ref.os.as_deref().filter(|os| !os.trim().is_empty()) {
        payload["os"] = Value::String(os.to_string());
    }
    payload
}

/// Shadeform names allow letters, digits and dashes.
fn instance_name(label: &str) -> String {
    let name = label
        .trim()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>();
    let name = name
        .split('-')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    if name.is_empty() {
        "noland-connect".to_string()
    } else {
        name.chars().take(60).collect()
    }
}

/// `type base64` of an OpenSSH public key, ignoring the comment.
fn key_material(public_key: &str) -> Option<String> {
    let mut parts = public_key.split_whitespace();
    let kind = parts.next()?;
    let data = parts.next()?;
    Some(format!("{kind} {data}"))
}

fn error_detail(body: &Value) -> String {
    for key in ["error", "message", "detail", "msg"] {
        if let Some(text) = body.get(key).and_then(Value::as_str) {
            return text.to_string();
        }
    }
    match body {
        Value::String(text) => text.chars().take(300).collect(),
        Value::Null => "empty response".to_string(),
        other => other.to_string().chars().take(300).collect(),
    }
}

fn value_as_string(value: &Value) -> Option<String> {
    value
        .as_str()
        .map(ToString::to_string)
        .or_else(|| value.as_u64().map(|number| number.to_string()))
        .filter(|text| !text.trim().is_empty())
}

fn number(value: &Value, key: &str) -> Option<f64> {
    let raw = value.get(key)?;
    raw.as_f64()
        .or_else(|| raw.as_str().and_then(|text| text.trim().parse().ok()))
}

fn text(value: &Value, key: &str) -> String {
    value
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_string()
}

fn gpu_has_nvenc(gpu_type: &str) -> bool {
    let normalized = gpu_type.to_ascii_uppercase().replace(['_', '-', ' '], "");
    let normalized = normalized.strip_prefix("NVIDIA").unwrap_or(&normalized);
    !NO_NVENC_GPU_PREFIXES
        .iter()
        .any(|prefix| normalized.starts_with(prefix))
}

/// "A6000" → "RTX A6000", "RTX4090" → "RTX 4090", "L40S" stays.
fn pretty_gpu_name(gpu_type: &str) -> String {
    let name = gpu_type.trim().replace('_', " ");
    let upper = name.to_ascii_uppercase();
    if let Some(rest) = upper.strip_prefix("RTX") {
        let rest = rest.trim();
        if !rest.is_empty() {
            return format!("RTX {rest}");
        }
    }
    if ["A4000", "A4500", "A5000", "A6000"].contains(&upper.as_str()) {
        return format!("RTX {upper}");
    }
    name
}

/// ISO code for a Shadeform region such as `us-east-1`, `canada-1` or
/// `helsinki-finland-1`, preferring a display name like `US, Virginia`.
fn region_country(region: &str, display_name: &str) -> String {
    let display = display_name.trim();
    if let Some((head, _)) = display.split_once(',') {
        let head = head.trim();
        if head.len() == 2 && head.chars().all(|c| c.is_ascii_uppercase()) {
            return head.to_string();
        }
    }
    let tokens = |raw: &str| -> Vec<String> {
        raw.split(|c: char| !c.is_ascii_alphanumeric())
            .filter(|token| !token.is_empty())
            .map(str::to_ascii_lowercase)
            .collect()
    };
    let region_tokens = tokens(region);
    // Two-letter tokens are only trusted as the region's leading code
    // ("us-east-1"); elsewhere they are too often ordinary words.
    if let Some(first) = region_tokens.first().filter(|token| token.len() == 2) {
        let code = country_code_for(first);
        if !code.is_empty() {
            return code.to_string();
        }
    }
    let display_tokens = tokens(display);
    for token in region_tokens.iter().chain(display_tokens.iter()) {
        if token.len() < 3 {
            continue;
        }
        let code = match country_code_for(token) {
            "" => city_country(token),
            code => code,
        };
        if !code.is_empty() {
            return code.to_string();
        }
    }
    String::new()
}

fn city_country(token: &str) -> &'static str {
    match token {
        "virginia" | "ohio" | "oregon" | "california" | "texas" | "dallas" | "utah" | "kansas"
        | "nevada" | "arizona" | "georgia" | "iowa" | "chicago" | "seattle" | "atlanta"
        | "newyork" | "jersey" | "carolina" | "washington" | "denver" | "phoenix" | "miami"
        | "losangeles" | "sanjose" | "ashburn" | "columbus" | "portland" | "boston" | "usa" => "US",
        "montreal" | "toronto" | "vancouver" | "quebec" => "CA",
        "helsinki" => "FI",
        "stockholm" => "SE",
        "oslo" => "NO",
        "reykjavik" | "keflavik" => "IS",
        "london" | "manchester" => "GB",
        "frankfurt" | "berlin" | "munich" => "DE",
        "amsterdam" => "NL",
        "paris" => "FR",
        "warsaw" => "PL",
        "madrid" => "ES",
        "milan" => "IT",
        "tokyo" | "osaka" => "JP",
        "mumbai" => "IN",
        "sydney" | "melbourne" => "AU",
        "seoul" => "KR",
        _ => "",
    }
}

/// Rough centroid of a country, so distance ranking works for providers that
/// report no coordinates. Continental US uses its geographic center.
pub(crate) fn country_centroid(code: &str) -> Option<(f64, f64)> {
    Some(match code.trim().to_ascii_uppercase().as_str() {
        "US" => (39.8, -98.6),
        "CA" => (45.5, -75.0),
        "MX" => (23.6, -102.5),
        "BR" => (-14.2, -51.9),
        "GB" => (52.4, -1.5),
        "IE" => (53.4, -8.2),
        "FR" => (46.2, 2.2),
        "DE" => (51.2, 10.4),
        "NL" => (52.1, 5.3),
        "BE" => (50.5, 4.5),
        "CH" => (46.8, 8.2),
        "AT" => (47.5, 14.6),
        "IT" => (41.9, 12.6),
        "ES" => (40.5, -3.7),
        "PT" => (39.4, -8.2),
        "SE" => (60.1, 18.6),
        "NO" => (60.5, 8.5),
        "FI" => (61.9, 25.7),
        "DK" => (56.3, 9.5),
        "IS" => (64.9, -19.0),
        "PL" => (51.9, 19.1),
        "CZ" => (49.8, 15.5),
        "EE" => (58.6, 25.0),
        "LV" => (56.9, 24.6),
        "LT" => (55.2, 23.9),
        "RO" => (45.9, 25.0),
        "BG" => (42.7, 25.5),
        "HU" => (47.2, 19.5),
        "GR" => (39.1, 21.8),
        "UA" => (48.4, 31.2),
        "TR" => (39.0, 35.2),
        "IL" => (31.0, 34.9),
        "AE" => (23.4, 53.8),
        "IN" => (20.6, 79.0),
        "SG" => (1.35, 103.8),
        "JP" => (36.2, 138.3),
        "KR" => (35.9, 127.8),
        "TW" => (23.7, 121.0),
        "HK" => (22.3, 114.2),
        "AU" => (-25.3, 133.8),
        "NZ" => (-40.9, 174.9),
        "ZA" => (-30.6, 22.9),
        _ => return None,
    })
}

/// Offers from `GET /instances/types`: one per instance type and available
/// region, for single-GPU types with an NVENC encoder.
pub fn parse_instance_types(body: &Value, storage_gb: u32) -> Vec<VastOffer> {
    let types = body
        .get("instance_types")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    let mut offers = Vec::new();
    let (mut excluded_cloud, mut no_nvenc, mut multi_gpu, mut unavailable) = (0, 0, 0, 0);
    for item in &types {
        let cloud = text(item, "cloud");
        let shade_instance_type = text(item, "shade_instance_type");
        if cloud.is_empty() || shade_instance_type.is_empty() {
            continue;
        }
        if EXCLUDED_CLOUDS.contains(&cloud.to_ascii_lowercase().as_str()) {
            excluded_cloud += 1;
            continue;
        }
        let config = item.get("configuration").cloned().unwrap_or(Value::Null);
        let gpu_type = text(&config, "gpu_type");
        if gpu_type.is_empty() || !gpu_has_nvenc(&gpu_type) {
            no_nvenc += 1;
            continue;
        }
        if number(&config, "num_gpus").unwrap_or(1.0) != 1.0 {
            multi_gpu += 1;
            continue;
        }
        // Prices are in cents per hour.
        let hourly = number(item, "hourly_price").unwrap_or_default() / 100.0;
        if hourly <= 0.0 {
            continue;
        }
        let vram_gb = number(&config, "vram_per_gpu_in_gb").unwrap_or_default();
        let vcpus = number(&config, "vcpus").unwrap_or_default();
        let disk_gb = number(&config, "storage_in_gb").unwrap_or_default();
        let os = preferred_os(config.get("os_options"));

        for availability in item
            .get("availability")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            if availability.get("available").and_then(Value::as_bool) != Some(true) {
                unavailable += 1;
                continue;
            }
            let region = text(availability, "region");
            if region.is_empty() {
                continue;
            }
            let display_name = text(availability, "display_name");
            let country = region_country(&region, &display_name);
            let (latitude, longitude) = country_centroid(&country).unwrap_or_default();
            let offer_ref = ShadeformOfferRef {
                cloud: cloud.clone(),
                region: region.clone(),
                shade_instance_type: shade_instance_type.clone(),
                os: os.clone(),
            };
            let location = if display_name.is_empty() {
                region.clone()
            } else {
                display_name.clone()
            };
            offers.push(VastOffer {
                id: foreign_local_id(
                    CloudProviderKind::Shadeform,
                    &format!("{cloud}/{shade_instance_type}/{region}"),
                ),
                host_id: None,
                host_label: format!("Shadeform · {cloud} · {location}"),
                city: String::new(),
                region: location,
                country,
                latitude,
                longitude,
                reliability: 0.95,
                gpu_ram_mb: (vram_gb * 1024.0).round() as u64,
                gpu_name: pretty_gpu_name(&gpu_type),
                gpu_count: 1,
                cpu_name: String::new(),
                cpu_cores: vcpus,
                internet_down_mbps: 0.0,
                internet_up_mbps: 0.0,
                hourly_price: hourly,
                compute_hourly_price: hourly,
                storage_hourly_price: 0.0,
                available_storage_gb: if disk_gb > 0.0 {
                    disk_gb as u32
                } else {
                    storage_gb
                },
                raw_geolocation: region.clone(),
                time_remaining_hours: 0.0,
                is_verified: true,
                is_datacenter: true,
                offer_type: "on-demand".to_string(),
                has_static_ip: true,
                has_avx: true,
                provider: CloudProviderKind::Shadeform.as_str().to_string(),
                provider_offer_ref: serde_json::to_string(&offer_ref).unwrap_or_default(),
            });
        }
    }
    let message = format!(
        "Shadeform instance types: {} type(s), {} offer(s); skipped {} on excluded clouds, {} without NVENC, {} multi-GPU, {} unavailable regions",
        types.len(),
        offers.len(),
        excluded_cloud,
        no_nvenc,
        multi_gpu,
        unavailable
    );
    if offers.is_empty() {
        warn!("{message}; response shape: {}", json_shape(body, 3));
    } else {
        info!("{message}");
    }
    offers
}

/// Prefer Ubuntu 24.04, then 22.04, from an instance type's OS options.
/// `None` lets Shadeform pick its default image.
fn preferred_os(options: Option<&Value>) -> Option<String> {
    let options = options?
        .as_array()?
        .iter()
        .filter_map(|option| {
            option
                .as_str()
                .map(ToString::to_string)
                .or_else(|| option.get("name").and_then(value_as_string))
        })
        .collect::<Vec<_>>();
    ["24.04", "24_04", "2404", "22.04", "22_04", "2204"]
        .iter()
        .find_map(|version| {
            options.iter().find(|option| {
                let lower = option.to_ascii_lowercase();
                lower.contains("ubuntu") && lower.contains(version)
            })
        })
        .cloned()
}

fn normalize_status(raw: &str) -> String {
    match raw.trim().to_ascii_lowercase().as_str() {
        "active" | "running" => "running",
        "creating" | "pending" | "pending_provider" | "provisioning" | "" => "loading",
        "deleting" => "destroying",
        "deleted" => "deleted",
        "error" | "failed" => "error",
        other => return other.to_string(),
    }
    .to_string()
}

/// Parse one instance from `GET /instances` or `GET /instances/{id}/info`.
pub fn parse_instance(value: &Value, fallback_id: Option<&str>) -> Option<ShadeformInstance> {
    let remote_id = value
        .get("id")
        .and_then(value_as_string)
        .or_else(|| fallback_id.map(ToString::to_string))?;
    let raw_status = text(value, "status");
    let status = normalize_status(&raw_status);
    let ip = text(value, "ip");
    let ssh_port = number(value, "ssh_port")
        .and_then(|port| u16::try_from(port as u64).ok())
        .filter(|port| *port > 0)
        .unwrap_or(if ip.is_empty() { 0 } else { 22 });
    let ssh_user = match text(value, "ssh_user") {
        user if user.is_empty() => SHADEFORM_DEFAULT_SSH_USER.to_string(),
        user => user,
    };
    let config = value.get("configuration").cloned().unwrap_or(Value::Null);
    let gpu_type = text(&config, "gpu_type");
    let hourly = number(value, "hourly_price").unwrap_or_default() / 100.0;
    let wireguard_listen_port = 51820;
    let network_probe_listen_port = 6201;
    let local_id = foreign_local_id(CloudProviderKind::Shadeform, &remote_id);
    let ssh_command = if ip.is_empty() {
        String::new()
    } else {
        format!("ssh -p {ssh_port} root@{ip}")
    };
    let has_ip = !ip.is_empty();

    Some(ShadeformInstance {
        remote_id,
        ssh_user,
        deleted: status == "deleted",
        instance: VastInstance {
            id: local_id,
            label: text(value, "name"),
            status,
            ssh_host: ip.clone(),
            ssh_port,
            // Shadeform instances have a public IP with no port mapping.
            wireguard_port: if has_ip { wireguard_listen_port } else { 0 },
            wireguard_listen_port,
            wireguard_host_ip: ip.clone(),
            network_probe_port: if has_ip { network_probe_listen_port } else { 0 },
            network_probe_listen_port,
            network_probe_host_ip: ip.clone(),
            ssh_command,
            public_ip: ip,
            gpu_name: if gpu_type.is_empty() {
                "Unknown GPU".to_string()
            } else {
                pretty_gpu_name(&gpu_type)
            },
            hourly_price: hourly,
            compute_hourly_price: hourly,
            storage_hourly_price: 0.0,
            image_runtype: "vm".to_string(),
            hosting_type: format!("shadeform:{}", text(value, "cloud")),
            provider: CloudProviderKind::Shadeform.as_str().to_string(),
            interruptible: false,
            intended_status: String::new(),
            status_message: text(value, "status_details"),
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn types_body() -> Value {
        json!({
            "instance_types": [
                {
                    "cloud": "massedcompute",
                    "shade_instance_type": "A6000",
                    "cloud_instance_type": "gpu_1x_a6000",
                    "hourly_price": 57,
                    "configuration": {
                        "gpu_type": "A6000", "num_gpus": 1, "vcpus": 6, "memory_in_gb": 48,
                        "vram_per_gpu_in_gb": 48, "storage_in_gb": 256,
                        "os_options": ["ubuntu20.04_cuda12.0_shade_os", "ubuntu22.04_cuda12.2_shade_os"]
                    },
                    "availability": [
                        { "region": "us-central-2", "available": true, "display_name": "US, Kansas City, MO" },
                        { "region": "helsinki-finland-1", "available": true },
                        { "region": "us-east-1", "available": false }
                    ]
                },
                {
                    "cloud": "lambda",
                    "shade_instance_type": "H100",
                    "hourly_price": 249,
                    "configuration": { "gpu_type": "H100", "num_gpus": 1, "vram_per_gpu_in_gb": 80 },
                    "availability": [{ "region": "us-west-1", "available": true }]
                },
                {
                    "cloud": "hyperstack",
                    "shade_instance_type": "A6000x2",
                    "hourly_price": 100,
                    "configuration": { "gpu_type": "A6000", "num_gpus": 2 },
                    "availability": [{ "region": "canada-1", "available": true }]
                },
                {
                    "cloud": "runpod",
                    "shade_instance_type": "RTX4090",
                    "hourly_price": 40,
                    "configuration": { "gpu_type": "RTX4090", "num_gpus": 1 },
                    "availability": [{ "region": "us-1", "available": true }]
                }
            ]
        })
    }

    #[test]
    fn parses_single_gpu_nvenc_offers_in_available_regions() {
        let offers = parse_instance_types(&types_body(), 100);
        assert_eq!(
            offers.len(),
            2,
            "H100, multi-GPU, runpod and unavailable regions skipped"
        );

        let us = &offers[0];
        assert_eq!(us.provider, "shadeform");
        assert_eq!(us.gpu_name, "RTX A6000");
        assert_eq!(us.gpu_ram_mb, 48 * 1024);
        assert_eq!(us.country, "US");
        assert!((us.hourly_price - 0.57).abs() < 1e-9);
        assert_eq!(us.available_storage_gb, 256);
        assert!(us.latitude != 0.0 && us.longitude != 0.0);
        assert!(us.id >= crate::models::provider::FOREIGN_LOCAL_ID_BASE);
        let offer_ref: ShadeformOfferRef = serde_json::from_str(&us.provider_offer_ref).unwrap();
        assert_eq!(offer_ref.cloud, "massedcompute");
        assert_eq!(offer_ref.region, "us-central-2");
        assert_eq!(offer_ref.shade_instance_type, "A6000");
        assert_eq!(
            offer_ref.os.as_deref(),
            Some("ubuntu22.04_cuda12.2_shade_os")
        );

        assert_eq!(offers[1].country, "FI");
        assert_ne!(offers[0].id, offers[1].id);
    }

    #[test]
    fn region_country_handles_codes_names_and_cities() {
        assert_eq!(region_country("us-east-1", ""), "US");
        assert_eq!(region_country("canada-1", ""), "CA");
        assert_eq!(region_country("helsinki-finland-1", ""), "FI");
        assert_eq!(region_country("x", "DE, Frankfurt"), "DE");
        assert_eq!(region_country("norway-1", ""), "NO");
        assert_eq!(region_country("mystery-9", ""), "");
        assert_eq!(
            region_country("europe-central-1", ""),
            "",
            "direction words are not countries"
        );
    }

    #[test]
    fn nvenc_filter_rejects_datacenter_only_gpus() {
        for gpu in [
            "A6000", "RTX4090", "L40S", "L4", "A10", "A10G", "T4", "A40", "V100",
        ] {
            assert!(gpu_has_nvenc(gpu), "{gpu}");
        }
        for gpu in [
            "A100", "A100_80G", "H100", "H200", "GH200", "B200", "MI300X", "GAUDI2",
        ] {
            assert!(!gpu_has_nvenc(gpu), "{gpu}");
        }
    }

    #[test]
    fn parses_active_instance() {
        let body = json!({
            "id": "d290f1ee-6c54-4b01-90e6-d701748f0851",
            "cloud": "massedcompute",
            "region": "us-central-2",
            "shade_instance_type": "A6000",
            "name": "noland-connect-session",
            "status": "active",
            "ip": "203.0.113.20",
            "ssh_user": "ubuntu",
            "ssh_port": 22,
            "hourly_price": 57,
            "configuration": { "gpu_type": "A6000", "num_gpus": 1 }
        });
        let parsed = parse_instance(&body, None).unwrap();
        assert_eq!(parsed.ssh_user, "ubuntu");
        assert!(!parsed.deleted);
        let instance = parsed.instance;
        assert!(instance.ssh_ready());
        assert!(instance.is_vm_runtime());
        assert_eq!(instance.public_ip, "203.0.113.20");
        assert_eq!(instance.wireguard_port, 51820);
        assert_eq!(instance.wireguard_endpoint_host(), "203.0.113.20");
        assert_eq!(instance.gpu_name, "RTX A6000");
        assert!((instance.hourly_price - 0.57).abs() < 1e-9);
        assert_eq!(
            instance.id,
            foreign_local_id(
                CloudProviderKind::Shadeform,
                "d290f1ee-6c54-4b01-90e6-d701748f0851"
            )
        );
    }

    #[test]
    fn pending_instance_is_loading_and_deleted_is_flagged() {
        let pending =
            parse_instance(&json!({"id": "a", "status": "pending_provider"}), None).unwrap();
        assert!(pending.instance.is_loading());
        assert_eq!(pending.ssh_user, SHADEFORM_DEFAULT_SSH_USER);
        assert_eq!(pending.instance.ssh_port, 0);
        let deleted = parse_instance(&json!({"id": "b", "status": "deleted"}), None).unwrap();
        assert!(deleted.deleted);
    }

    #[test]
    fn create_payload_names_type_region_key_and_os() {
        let payload = create_instance_payload(
            &ShadeformOfferRef {
                cloud: "massedcompute".into(),
                region: "us-central-2".into(),
                shade_instance_type: "A6000".into(),
                os: Some("ubuntu22.04_cuda12.2_shade_os".into()),
            },
            "Noland Connect Session",
            "key-1",
        );
        assert_eq!(payload["cloud"], "massedcompute");
        assert_eq!(payload["region"], "us-central-2");
        assert_eq!(payload["shade_instance_type"], "A6000");
        assert_eq!(payload["ssh_key_id"], "key-1");
        assert_eq!(payload["name"], "noland-connect-session");
        assert_eq!(payload["os"], "ubuntu22.04_cuda12.2_shade_os");
    }

    #[test]
    fn key_material_ignores_comments() {
        assert_eq!(
            key_material("ssh-ed25519 AAAAC3 user@host\n").as_deref(),
            Some("ssh-ed25519 AAAAC3")
        );
        assert_eq!(key_material("ssh-ed25519"), None);
    }
}
