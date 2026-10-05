//! TensorDock (v2 Instances API) client.
//!
//! TensorDock rents full KVM virtual machines, which is what the provisioning
//! pipeline requires (systemd, a headless NVIDIA display and inbound UDP for
//! WireGuard/GameStream). Instances are deployed with a dedicated IP so every
//! port, including UDP, is reachable without per-port forwarding.
//!
//! Offers and instances are converted to the shared `VastOffer` /
//! `VastInstance` shapes. TensorDock's UUIDs are mapped to local `u64` ids
//! with [`foreign_local_id`]; the UUID is carried alongside so callers can
//! persist the mapping.
//!
//! The parsers accept both snake_case and camelCase keys and both the
//! array and map shapes TensorDock has used for GPU lists, because the
//! public documentation and the live API have not always agreed.

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
};

pub const DEFAULT_TENSORDOCK_BASE_URL: &str = "https://dashboard.tensordock.com";
/// Default login user on TensorDock Ubuntu images.
pub const TENSORDOCK_DEFAULT_SSH_USER: &str = "user";
const TENSORDOCK_IMAGE: &str = "ubuntu2404";
/// TensorDock rejects deployments with less storage than this.
pub const TENSORDOCK_MIN_STORAGE_GB: u32 = 100;
const DEFAULT_VCPUS: u32 = 8;
const DEFAULT_RAM_GB: u32 = 32;
/// Ports forwarded when a location has no dedicated IP: SSH, WireGuard (all
/// streaming runs through the tunnel) and the network probe. An external
/// port of 0 lets TensorDock pick one.
const FORWARDED_PORTS: [u16; 3] = [22, 51820, 6201];

/// Everything needed to deploy one offer, serialized into
/// `VastOffer::provider_offer_ref`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TensorDockOfferRef {
    pub location_id: String,
    pub gpu_v0_name: String,
    pub vcpu_count: u32,
    pub ram_gb: u32,
    /// `false` deploys with port forwards instead of a dedicated IP.
    #[serde(default = "default_true")]
    pub use_dedicated_ip: bool,
}

fn default_true() -> bool {
    true
}

/// A TensorDock instance plus its provider-side UUID.
#[derive(Debug, Clone)]
pub struct TensorDockInstance {
    pub remote_id: String,
    pub instance: VastInstance,
}

#[derive(Clone)]
pub struct TensorDockApiClient {
    http: reqwest::Client,
    base_url: String,
    api_key: String,
}

impl TensorDockApiClient {
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
            .bearer_auth(&self.api_key);
        if let Some(body) = body {
            request = request.json(&body);
        }
        let response = request
            .send()
            .await
            .map_err(|error| AppError::Api(format!("TensorDock {method} {url} failed: {error}")))?;
        let status = response.status();
        let text = response.text().await.map_err(|error| {
            AppError::Api(format!("TensorDock {method} {url} response read failed: {error}"))
        })?;
        let parsed = serde_json::from_str::<Value>(&text).unwrap_or(Value::String(text));
        info!(
            "TensorDock {} {} -> {} in {}ms",
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
            warn!("TensorDock {method} {url} -> {status}: {detail}");
            if status == reqwest::StatusCode::NOT_FOUND {
                return Err(AppError::NotFound(format!("TensorDock {path}: {detail}")));
            }
            return Err(AppError::Api(format!(
                "TensorDock {method} {path} -> {status}: {detail}"
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
            .request(reqwest::Method::GET, "/api/v2/locations", None)
            .await?;
        let storage_gb = storage_gb.max(TENSORDOCK_MIN_STORAGE_GB);
        let offers = parse_locations(&body, storage_gb)
            .into_iter()
            .filter(|offer| match country_code {
                Some(code) if !code.trim().is_empty() && !code.eq_ignore_ascii_case("GLOBAL") => {
                    country_matches(&offer.country, code)
                }
                _ => true,
            })
            .collect::<Vec<_>>();
        info!("TensorDock returned {} offers", offers.len());
        Ok(offers)
    }

    pub async fn create_instance(
        &self,
        offer_ref: &TensorDockOfferRef,
        storage_gb: u32,
        label: &str,
        ssh_public_key: &str,
    ) -> AppResult<TensorDockInstance> {
        let payload = create_instance_payload(offer_ref, storage_gb, label, ssh_public_key);
        let body = self
            .request(reqwest::Method::POST, "/api/v2/instances", Some(payload))
            .await?;
        let remote_id = body
            .pointer("/data/id")
            .or_else(|| body.get("id"))
            .and_then(value_as_string)
            .ok_or_else(|| {
                AppError::Api(format!(
                    "TensorDock create instance response did not include an id: {body}"
                ))
            })?;
        info!("TensorDock create_instance accepted id={remote_id}");
        self.get_instance(&remote_id).await
    }

    pub async fn get_instance(&self, remote_id: &str) -> AppResult<TensorDockInstance> {
        let body = self
            .request(
                reqwest::Method::GET,
                &format!("/api/v2/instances/{remote_id}"),
                None,
            )
            .await?;
        let envelope = body.get("data").unwrap_or(&body);
        parse_instance(envelope, Some(remote_id)).ok_or_else(|| {
            AppError::Api(format!(
                "TensorDock instance payload missing expected fields for {remote_id}"
            ))
        })
    }

    pub async fn list_instances(&self) -> AppResult<Vec<TensorDockInstance>> {
        let body = self
            .request(reqwest::Method::GET, "/api/v2/instances", None)
            .await?;
        let items = body
            .pointer("/data/instances")
            // The getting-started page shows this JSON:API list envelope.
            .or_else(|| body.pointer("/data/attributes/instances"))
            .or_else(|| body.get("instances"))
            .or_else(|| body.get("data"))
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let listed = items
            .iter()
            .filter_map(|item| parse_instance(item, None))
            .collect::<Vec<_>>();

        // The list only carries id, name and status. Fetch details (IP,
        // ports, GPU, rate) so refreshes do not blank saved SSH endpoints.
        let mut instances = Vec::with_capacity(listed.len());
        for instance in listed {
            if !needs_details(&instance) {
                instances.push(instance);
                continue;
            }
            match self.get_instance(&instance.remote_id).await {
                Ok(detailed) => instances.push(detailed),
                Err(error) => {
                    warn!(
                        "TensorDock instance {} details failed (using list entry): {error}",
                        instance.remote_id
                    );
                    instances.push(instance);
                }
            }
        }
        Ok(instances)
    }

    pub async fn stop_instance(&self, remote_id: &str) -> AppResult<()> {
        self.request(
            reqwest::Method::POST,
            &format!("/api/v2/instances/{remote_id}/stop"),
            None,
        )
        .await?;
        Ok(())
    }

    pub async fn delete_instance(&self, remote_id: &str) -> AppResult<()> {
        self.request(
            reqwest::Method::DELETE,
            &format!("/api/v2/instances/{remote_id}"),
            None,
        )
        .await?;
        Ok(())
    }

    /// TensorDock has no account wallet endpoint in the v2 Instances API.
    pub async fn check_credentials(&self) -> AppResult<()> {
        self.request(reqwest::Method::GET, "/api/v2/instances", None)
            .await
            .map(|_| ())
    }
}

/// Whether a listed instance lacks the network details the app relies on.
/// Deleted instances are skipped since they have none to fetch.
fn needs_details(instance: &TensorDockInstance) -> bool {
    instance.instance.public_ip.trim().is_empty()
        && !matches!(instance.instance.status.as_str(), "destroying" | "deleted")
}

/// Request body for `POST /api/v2/instances`.
pub fn create_instance_payload(
    offer_ref: &TensorDockOfferRef,
    storage_gb: u32,
    label: &str,
    ssh_public_key: &str,
) -> Value {
    let mut payload = json!({
        "data": {
            "type": "virtualmachine",
            "attributes": {
                "name": label,
                "type": "virtualmachine",
                "image": TENSORDOCK_IMAGE,
                "resources": {
                    "vcpu_count": offer_ref.vcpu_count,
                    "ram_gb": offer_ref.ram_gb,
                    "storage_gb": storage_gb.max(TENSORDOCK_MIN_STORAGE_GB),
                    "gpus": { offer_ref.gpu_v0_name.clone(): { "count": 1 } }
                },
                "location_id": offer_ref.location_id,
                "ssh_key": ssh_public_key.trim()
            }
        }
    });
    let attributes = &mut payload["data"]["attributes"];
    if offer_ref.use_dedicated_ip {
        // A dedicated IP exposes every port, including WireGuard's UDP port.
        attributes["useDedicatedIp"] = Value::Bool(true);
    } else {
        attributes["port_forwards"] = Value::Array(
            FORWARDED_PORTS
                .iter()
                .map(|port| json!({ "internal_port": port, "external_port": 0 }))
                .collect(),
        );
    }
    payload
}

/// Shell script run once over SSH as the image's default user, giving the
/// app's key root access and setting the desktop user's password, which the
/// Vast template otherwise does from `USER`/`PASS`.
pub fn access_bootstrap_script(target_user: &str, target_password: &str) -> String {
    let user = shell_quote(target_user);
    let credentials = shell_quote(&format!("{target_user}:{target_password}"));
    format!(
        r#"set -eu
sudo -n true
if [ "$(id -u)" -ne 0 ]; then
  sudo install -d -m 700 /root/.ssh
  cat ~/.ssh/authorized_keys | sudo tee -a /root/.ssh/authorized_keys >/dev/null
  sudo sort -u -o /root/.ssh/authorized_keys /root/.ssh/authorized_keys
  sudo chmod 600 /root/.ssh/authorized_keys
fi
if ! id -u {user} >/dev/null 2>&1; then sudo useradd -m -s /bin/bash -G sudo {user}; fi
echo {credentials} | sudo chpasswd
sudo install -d -m 755 /etc/ssh/sshd_config.d
echo 'PermitRootLogin prohibit-password' | sudo tee /etc/ssh/sshd_config.d/10-noland-root.conf >/dev/null
sudo systemctl reload ssh 2>/dev/null || sudo systemctl reload sshd 2>/dev/null || true
echo NOLAND_ACCESS_READY"#
    )
}

fn shell_quote(raw: &str) -> String {
    format!("'{}'", raw.replace('\'', r"'\''"))
}

fn error_detail(body: &Value) -> String {
    if let Some(errors) = body.get("errors").and_then(Value::as_array) {
        let joined = errors
            .iter()
            .filter_map(|error| {
                error
                    .get("detail")
                    .or_else(|| error.get("title"))
                    .and_then(Value::as_str)
            })
            .collect::<Vec<_>>()
            .join("; ");
        if !joined.is_empty() {
            return joined;
        }
    }
    for key in ["error", "message", "detail"] {
        if let Some(text) = body.get(key).and_then(Value::as_str) {
            return text.to_string();
        }
    }
    let raw = body.to_string();
    raw.chars().take(300).collect()
}

fn value_as_string(value: &Value) -> Option<String> {
    value
        .as_str()
        .map(ToString::to_string)
        .or_else(|| value.as_u64().map(|number| number.to_string()))
        .filter(|text| !text.trim().is_empty())
}

fn get_any<'a>(value: &'a Value, keys: &[&str]) -> Option<&'a Value> {
    keys.iter().find_map(|key| value.get(*key))
}

fn number_any(value: &Value, keys: &[&str]) -> Option<f64> {
    keys.iter().find_map(|key| {
        let raw = value.get(*key)?;
        raw.as_f64()
            .or_else(|| raw.as_str().and_then(|text| text.trim().parse().ok()))
    })
}

fn string_any(value: &Value, keys: &[&str]) -> String {
    keys.iter()
        .find_map(|key| value.get(*key).and_then(Value::as_str))
        .unwrap_or_default()
        .trim()
        .to_string()
}

fn bool_any(value: &Value, keys: &[&str]) -> Option<bool> {
    keys.iter()
        .find_map(|key| value.get(*key).and_then(Value::as_bool))
}

fn country_matches(country: &str, code: &str) -> bool {
    let country = country.trim();
    let code = code.trim();
    country.eq_ignore_ascii_case(code) || country_code_for(country).eq_ignore_ascii_case(code)
}

/// ISO 3166 alpha-2 code for the country names GPU providers commonly report.
pub(crate) fn country_code_for(country: &str) -> &'static str {
    match country.trim().to_ascii_lowercase().as_str() {
        "united states" | "united states of america" | "usa" | "us" => "US",
        "canada" | "ca" => "CA",
        "united kingdom" | "uk" | "great britain" | "gb" => "GB",
        "germany" | "de" => "DE",
        "france" | "fr" => "FR",
        "netherlands" | "the netherlands" | "nl" => "NL",
        "sweden" | "se" => "SE",
        "norway" | "no" => "NO",
        "finland" | "fi" => "FI",
        "poland" | "pl" => "PL",
        "spain" | "es" => "ES",
        "italy" | "it" => "IT",
        "czechia" | "czech republic" | "cz" => "CZ",
        "japan" | "jp" => "JP",
        "singapore" | "sg" => "SG",
        "australia" | "au" => "AU",
        "india" | "in" => "IN",
        "brazil" | "br" => "BR",
        "ireland" | "ie" => "IE",
        "switzerland" | "ch" => "CH",
        "belgium" | "be" => "BE",
        "austria" | "at" => "AT",
        "denmark" | "dk" => "DK",
        "portugal" | "pt" => "PT",
        "iceland" | "is" => "IS",
        "estonia" | "ee" => "EE",
        "latvia" | "lv" => "LV",
        "lithuania" | "lt" => "LT",
        "romania" | "ro" => "RO",
        "bulgaria" | "bg" => "BG",
        "hungary" | "hu" => "HU",
        "slovakia" | "sk" => "SK",
        "slovenia" | "si" => "SI",
        "croatia" | "hr" => "HR",
        "serbia" | "rs" => "RS",
        "greece" | "gr" => "GR",
        "luxembourg" | "lu" => "LU",
        "ukraine" | "ua" => "UA",
        "moldova" | "md" => "MD",
        "turkey" | "türkiye" | "turkiye" | "tr" => "TR",
        "israel" | "il" => "IL",
        "united arab emirates" | "uae" | "ae" => "AE",
        "south africa" | "za" => "ZA",
        "mexico" | "mx" => "MX",
        "chile" | "cl" => "CL",
        "argentina" | "ar" => "AR",
        "colombia" | "co" => "CO",
        "south korea" | "korea" | "republic of korea" | "kr" => "KR",
        "taiwan" | "tw" => "TW",
        "hong kong" | "hk" => "HK",
        "thailand" | "th" => "TH",
        "malaysia" | "my" => "MY",
        "indonesia" | "id" => "ID",
        "vietnam" | "viet nam" | "vn" => "VN",
        "philippines" | "ph" => "PH",
        "new zealand" | "nz" => "NZ",
        _ => "",
    }
}

/// GPU entries from either `[{"v0Name": ..}, ..]` or `{"<v0Name>": {..}}`.
fn gpu_entries(raw: Option<&Value>) -> Vec<(String, Value)> {
    match raw {
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(|item| {
                let name = string_any(item, &["v0Name", "v0_name", "gpuV0Name", "name", "id"]);
                (!name.is_empty()).then(|| (name, item.clone()))
            })
            .collect(),
        // Keys are the model; instance details also carry `v0Name` inside.
        Some(Value::Object(map)) => map
            .iter()
            .map(|(name, item)| {
                let inner = string_any(item, &["v0Name", "v0_name", "gpuV0Name"]);
                let name = if inner.is_empty() {
                    name.clone()
                } else {
                    inner
                };
                (name, item.clone())
            })
            .collect(),
        _ => Vec::new(),
    }
}

/// VRAM from a TensorDock GPU model id such as `geforcertx4090-pcie-24gb`.
fn gpu_ram_mb_from_name(name: &str) -> u64 {
    name.to_ascii_lowercase()
        .rsplit(['-', '_'])
        .find_map(|part| part.strip_suffix("gb")?.parse::<u64>().ok())
        .map(|gb| gb * 1024)
        .unwrap_or_default()
}

fn pretty_gpu_name(v0_name: &str) -> String {
    let base = v0_name.split('-').next().unwrap_or(v0_name);
    let lower = base.to_ascii_lowercase();
    let stripped = lower
        .strip_prefix("geforce")
        .or_else(|| lower.strip_prefix("nvidia"))
        .unwrap_or(&lower);
    let mut name = stripped.to_ascii_uppercase();
    // "RTX4090" → "RTX 4090", "RTXA6000" → "RTX A6000"; datacenter names
    // such as "A100" or "H100" are already in their usual form.
    for family in ["RTX", "GTX"] {
        if let Some(rest) = name.strip_prefix(family) {
            if !rest.is_empty() {
                name = format!("{family} {rest}");
                break;
            }
        }
    }
    if name.is_empty() {
        v0_name.to_string()
    } else {
        name
    }
}

/// Offers from `GET /api/v2/locations`: one per location and GPU model,
/// priced for one GPU plus the default vCPU/RAM and the requested storage.
pub fn parse_locations(body: &Value, storage_gb: u32) -> Vec<VastOffer> {
    let locations = body
        .pointer("/data/locations")
        .or_else(|| body.get("locations"))
        .or_else(|| body.get("data"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    let mut offers = Vec::new();
    let mut skipped = SkipCounts::default();
    for location in &locations {
        let location_id = get_any(location, &["id", "uuid", "location_id"])
            .and_then(value_as_string)
            .unwrap_or_default();
        if location_id.is_empty() {
            continue;
        }
        let city = string_any(location, &["city"]);
        let region = string_any(location, &["stateprovince", "state", "region"]);
        let country_name = string_any(location, &["country"]);
        // The server picker groups offers by ISO code, like Vast reports them.
        let country = match country_code_for(&country_name) {
            "" => country_name.clone(),
            code => code.to_string(),
        };
        let tier = number_any(location, &["tier"]).unwrap_or_default();

        for (gpu_v0_name, gpu) in gpu_entries(location.get("gpus")) {
            skipped.gpus_seen += 1;
            let sold_out = bool_any(&gpu, &["available", "isAvailable", "is_available"])
                == Some(false)
                || number_any(
                    &gpu,
                    &[
                        "max_count",
                        "maxCount",
                        "available_count",
                        "availableCount",
                        "available",
                        "count",
                    ],
                )
                .is_some_and(|count| count < 1.0);
            if sold_out {
                skipped.sold_out += 1;
                continue;
            }
            let network = gpu
                .get("network_features")
                .or_else(|| gpu.get("networkFeatures"))
                .cloned()
                .unwrap_or(Value::Null);
            let dedicated_ip = bool_any(
                &network,
                &["dedicated_ip_available", "dedicatedIpAvailable"],
            ) != Some(false);
            let port_forwarding = bool_any(
                &network,
                &["port_forwarding_available", "portForwardingAvailable"],
            ) == Some(true);
            if !dedicated_ip && !port_forwarding {
                // No way to reach WireGuard's UDP port from outside.
                skipped.no_dedicated_ip += 1;
                continue;
            }
            let resources = gpu.get("resources").cloned().unwrap_or(Value::Null);
            let pricing = gpu.get("pricing").cloned().unwrap_or(Value::Null);
            let max_vcpus = number_any(&resources, &["max_vcpus", "maxVcpus"]).unwrap_or(f64::MAX);
            let max_ram = number_any(&resources, &["max_ram_gb", "maxRamGb"]).unwrap_or(f64::MAX);
            let max_storage =
                number_any(&resources, &["max_storage_gb", "maxStorageGb"]).unwrap_or(f64::MAX);
            if max_storage < f64::from(storage_gb) {
                skipped.storage += 1;
                continue;
            }
            let vcpu_count = (f64::from(DEFAULT_VCPUS).min(max_vcpus).floor() as u32).max(2);
            let ram_gb = (f64::from(DEFAULT_RAM_GB).min(max_ram).floor() as u32).max(8);

            let gpu_hourly = number_any(
                &gpu,
                &[
                    "price_per_hr",
                    "pricePerHr",
                    "price_per_hour",
                    "pricePerHour",
                    "hourly_price",
                    "price",
                ],
            )
            .or_else(|| {
                number_any(
                    &pricing,
                    &["per_gpu_hr", "perGpuHr", "price_per_hr", "pricePerHr"],
                )
            })
            .unwrap_or(0.0);
            let per_vcpu = number_any(&pricing, &["per_vcpu_hr", "perVcpuHr"]).unwrap_or(0.0);
            let per_ram = number_any(&pricing, &["per_gb_ram_hr", "perGbRamHr"]).unwrap_or(0.0);
            let per_storage =
                number_any(&pricing, &["per_gb_storage_hr", "perGbStorageHr"]).unwrap_or(0.0);
            let compute =
                gpu_hourly + per_vcpu * f64::from(vcpu_count) + per_ram * f64::from(ram_gb);
            let storage = per_storage * f64::from(storage_gb);
            if compute <= 0.0 {
                skipped.no_price += 1;
                continue;
            }

            let offer_ref = TensorDockOfferRef {
                location_id: location_id.clone(),
                gpu_v0_name: gpu_v0_name.clone(),
                vcpu_count,
                ram_gb,
                use_dedicated_ip: dedicated_ip,
            };
            let display_name = string_any(&gpu, &["displayName", "display_name"]);
            let gpu_name = if display_name.is_empty() {
                pretty_gpu_name(&gpu_v0_name)
            } else {
                display_name
                    .trim_start_matches("NVIDIA ")
                    .trim_start_matches("GeForce ")
                    .to_string()
            };
            let host_label = [city.as_str(), region.as_str(), country_name.as_str()]
                .iter()
                .filter(|part| !part.is_empty())
                .copied()
                .collect::<Vec<_>>()
                .join(", ");

            offers.push(VastOffer {
                id: foreign_local_id(
                    CloudProviderKind::Tensordock,
                    &format!("{location_id}/{gpu_v0_name}"),
                ),
                host_id: None,
                host_label: if dedicated_ip {
                    format!("TensorDock {host_label}").trim().to_string()
                } else {
                    format!("TensorDock {host_label} (port-forwarded)")
                        .trim()
                        .to_string()
                },
                city: city.clone(),
                region: region.clone(),
                country: country.clone(),
                latitude: number_any(location, &["latitude", "lat"]).unwrap_or_default(),
                longitude: number_any(location, &["longitude", "lon", "lng"]).unwrap_or_default(),
                // TensorDock tiers run 1 (lowest) to 4 (highest); map to the
                // 0-1 reliability scale used for ranking.
                reliability: if tier > 0.0 {
                    (0.9 + tier.min(4.0) * 0.025).min(0.999)
                } else {
                    0.95
                },
                gpu_ram_mb: gpu_ram_mb_from_name(&gpu_v0_name),
                gpu_name,
                gpu_count: 1,
                cpu_name: String::new(),
                cpu_cores: f64::from(vcpu_count),
                internet_down_mbps: 0.0,
                internet_up_mbps: 0.0,
                hourly_price: compute + storage,
                compute_hourly_price: compute,
                storage_hourly_price: storage,
                available_storage_gb: if max_storage.is_finite() && max_storage < f64::from(u32::MAX) {
                    max_storage as u32
                } else {
                    storage_gb
                },
                raw_geolocation: host_label,
                time_remaining_hours: 0.0,
                is_verified: true,
                is_datacenter: tier >= 3.0,
                offer_type: "on-demand".to_string(),
                has_static_ip: dedicated_ip,
                has_avx: true,
                provider: CloudProviderKind::Tensordock.as_str().to_string(),
                provider_offer_ref: serde_json::to_string(&offer_ref).unwrap_or_default(),
            });
        }
    }
    let message = format!(
        "TensorDock locations: {} location(s), {} GPU type(s), {} offer(s); skipped {} sold out, {} without a dedicated IP or port forwarding, {} below {storage_gb} GB storage, {} without a price",
        locations.len(),
        skipped.gpus_seen,
        offers.len(),
        skipped.sold_out,
        skipped.no_dedicated_ip,
        skipped.storage,
        skipped.no_price,
    );
    if offers.is_empty() {
        // Log the response shape (keys only, never values) so a mismatch with
        // the live API is visible in diagnostics.
        warn!("{message}; response shape: {}", json_shape(body, 3));
    } else {
        info!("{message}");
    }
    offers
}

#[derive(Default)]
struct SkipCounts {
    gpus_seen: usize,
    sold_out: usize,
    no_dedicated_ip: usize,
    storage: usize,
    no_price: usize,
}

/// Keys and value types of a JSON document down to `depth`, with arrays
/// summarized by their first element. Contains no values.
pub(crate) fn json_shape(value: &Value, depth: usize) -> String {
    match value {
        Value::Object(map) if depth > 0 => format!(
            "{{{}}}",
            map.iter()
                .take(24)
                .map(|(key, item)| format!("{key}: {}", json_shape(item, depth - 1)))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        Value::Object(_) => "{..}".to_string(),
        Value::Array(items) => match items.first() {
            Some(first) if depth > 0 => {
                format!("[{} x {}]", items.len(), json_shape(first, depth - 1))
            }
            _ => format!("[{}]", items.len()),
        },
        Value::String(_) => "string".to_string(),
        Value::Number(_) => "number".to_string(),
        Value::Bool(_) => "bool".to_string(),
        Value::Null => "null".to_string(),
    }
}

fn normalize_status(raw: &str) -> String {
    match raw.trim().to_ascii_lowercase().as_str() {
        "running" => "running",
        "starting" | "provisioning" | "creating" | "pending" => "loading",
        "stopping" => "stopping",
        "stopped" | "stoppeddisassociated" | "stopped_disassociated" => "stopped",
        "deleting" | "deleted" => "destroying",
        "error" | "failed" => "error",
        "" => "loading",
        other => return other.to_string(),
    }
    .to_string()
}

/// Map of internal → external ports from any of the shapes TensorDock uses.
fn port_forwards(attributes: &Value) -> Vec<(u16, u16, Option<String>)> {
    let raw = get_any(attributes, &["port_forwards", "portForwards"]);
    let mut out = Vec::new();
    let parse_port = |value: Option<&Value>| -> Option<u16> {
        value
            .and_then(|value| {
                value
                    .as_u64()
                    .or_else(|| value.as_str().and_then(|text| text.trim().parse().ok()))
            })
            .and_then(|port| u16::try_from(port).ok())
    };
    match raw {
        Some(Value::Array(items)) => {
            for item in items {
                let internal = parse_port(get_any(item, &["internal_port", "internalPort", "internal"]));
                let external = parse_port(get_any(item, &["external_port", "externalPort", "external"]));
                let protocol = item
                    .get("protocol")
                    .and_then(Value::as_str)
                    .map(str::to_ascii_lowercase);
                if let (Some(internal), Some(external)) = (internal, external) {
                    out.push((internal, external, protocol));
                }
            }
        }
        Some(Value::Object(map)) => {
            // {"<external>": "<internal>"} in the v0 API.
            for (external, internal) in map {
                if let (Ok(external), Some(internal)) =
                    (external.parse::<u16>(), parse_port(Some(internal)))
                {
                    out.push((internal, external, None));
                }
            }
        }
        _ => {}
    }
    out
}

fn forwarded_port(forwards: &[(u16, u16, Option<String>)], internal: u16, protocol: &str) -> Option<u16> {
    forwards
        .iter()
        .find(|(port, _, proto)| {
            *port == internal && proto.as_deref().is_none_or(|actual| actual == protocol)
        })
        .map(|(_, external, _)| *external)
}

/// Parse one instance from a JSON:API envelope (`{type, id, attributes}`)
/// or a flat object.
pub fn parse_instance(value: &Value, fallback_id: Option<&str>) -> Option<TensorDockInstance> {
    let remote_id = get_any(value, &["id", "uuid", "instance_id"])
        .and_then(value_as_string)
        .or_else(|| fallback_id.map(ToString::to_string))?;
    let attributes = value
        .get("attributes")
        .filter(|attributes| attributes.is_object())
        .unwrap_or(value);

    let ip = string_any(attributes, &["ipAddress", "ip_address", "ip", "public_ip"]);
    let forwards = port_forwards(attributes);
    let dedicated_ip = bool_any(attributes, &["useDedicatedIp", "dedicated_ip", "dedicatedIp"])
        .unwrap_or(forwards.is_empty());
    let mapped = |internal: u16, protocol: &str| -> u16 {
        forwarded_port(&forwards, internal, protocol)
            .unwrap_or(if dedicated_ip { internal } else { 0 })
    };
    let ssh_port = mapped(22, "tcp").max(if ip.is_empty() { 0 } else { 22 });
    let wireguard_listen_port = 51820;
    let network_probe_listen_port = 6201;

    let gpu_name = attributes
        .get("resources")
        .and_then(|resources| resources.get("gpus"))
        .map(|gpus| gpu_entries(Some(gpus)))
        .and_then(|entries| entries.into_iter().next())
        .map(|(name, _)| pretty_gpu_name(&name))
        .unwrap_or_else(|| "Unknown GPU".to_string());

    let status = normalize_status(&string_any(attributes, &["status", "state"]));
    let hourly = number_any(attributes, &["rateHourly", "rate_hourly", "hourly_rate", "price"])
        .unwrap_or_default();
    let label = string_any(attributes, &["name", "label"]);

    let local_id = foreign_local_id(CloudProviderKind::Tensordock, &remote_id);
    let ssh_command = if ip.is_empty() {
        String::new()
    } else {
        format!("ssh -p {ssh_port} root@{ip}")
    };

    Some(TensorDockInstance {
        remote_id,
        instance: VastInstance {
            id: local_id,
            label,
            status,
            ssh_host: ip.clone(),
            ssh_port,
            wireguard_port: mapped(wireguard_listen_port, "udp"),
            wireguard_listen_port,
            wireguard_host_ip: ip.clone(),
            network_probe_port: mapped(network_probe_listen_port, "udp"),
            network_probe_listen_port,
            network_probe_host_ip: ip.clone(),
            ssh_command,
            public_ip: ip,
            gpu_name,
            hourly_price: hourly,
            compute_hourly_price: hourly,
            storage_hourly_price: 0.0,
            image_runtype: "vm".to_string(),
            hosting_type: "tensordock".to_string(),
            provider: CloudProviderKind::Tensordock.as_str().to_string(),
            interruptible: false,
            intended_status: String::new(),
            status_message: String::new(),
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn locations_body() -> Value {
        json!({
            "data": {
                "locations": [
                    {
                        "id": "loc-1",
                        "city": "Chubbuck",
                        "stateprovince": "Idaho",
                        "country": "United States",
                        "tier": 3,
                        "gpus": [
                            {
                                "v0Name": "geforcertx4090-pcie-24gb",
                                "displayName": "NVIDIA GeForce RTX 4090 PCIe 24GB",
                                "max_count": 4,
                                "price_per_hr": 0.35,
                                "resources": { "max_vcpus": 16, "max_ram_gb": 64, "max_storage_gb": 1000 },
                                "pricing": { "per_vcpu_hr": 0.003, "per_gb_ram_hr": 0.002, "per_gb_storage_hr": 0.00005 },
                                "network_features": { "dedicated_ip_available": true }
                            },
                            {
                                "v0Name": "rtxa4000-pcie-16gb",
                                "max_count": 2,
                                "price_per_hr": 0.15,
                                "network_features": { "dedicated_ip_available": false }
                            },
                            {
                                "v0Name": "h100-sxm5-80gb",
                                "max_count": 0,
                                "price_per_hr": 2.0
                            }
                        ]
                    },
                    {
                        "id": "loc-2",
                        "city": "Frankfurt",
                        "country": "Germany",
                        "gpus": {
                            "rtxa6000-pcie-48gb": { "max_count": 1, "price_per_hr": 0.5 }
                        }
                    }
                ]
            }
        })
    }

    #[test]
    fn parses_location_offers_and_skips_unusable_gpus() {
        let offers = parse_locations(&locations_body(), 100);
        assert_eq!(offers.len(), 2, "no-dedicated-IP and sold-out GPUs are skipped");

        let rtx = &offers[0];
        assert_eq!(rtx.provider, "tensordock");
        assert_eq!(rtx.gpu_name, "RTX 4090 PCIe 24GB");
        assert_eq!(rtx.gpu_ram_mb, 24 * 1024);
        assert_eq!(rtx.country, "US");
        assert!(
            rtx.host_label.ends_with("United States"),
            "{}",
            rtx.host_label
        );
        assert!(rtx.is_datacenter);
        let expected_compute = 0.35 + 0.003 * 8.0 + 0.002 * 32.0;
        assert!((rtx.compute_hourly_price - expected_compute).abs() < 1e-9);
        assert!((rtx.storage_hourly_price - 0.005).abs() < 1e-9);
        assert!(rtx.id >= crate::models::provider::FOREIGN_LOCAL_ID_BASE);

        let offer_ref: TensorDockOfferRef = serde_json::from_str(&rtx.provider_offer_ref).unwrap();
        assert_eq!(offer_ref.location_id, "loc-1");
        assert_eq!(offer_ref.gpu_v0_name, "geforcertx4090-pcie-24gb");
        assert_eq!(offer_ref.vcpu_count, 8);
        assert_eq!(offer_ref.ram_gb, 32);

        let a6000 = &offers[1];
        assert_eq!(a6000.gpu_name, "RTX A6000");
        assert_eq!(a6000.gpu_ram_mb, 48 * 1024);
    }

    #[test]
    fn offers_requiring_more_storage_than_available_are_skipped() {
        let body = json!({"data": {"locations": [{
            "id": "loc", "country": "Canada",
            "gpus": [{"v0Name": "geforcertx3090-pcie-24gb", "max_count": 1, "price_per_hr": 0.2,
                      "resources": {"max_storage_gb": 150}}]
        }]}});
        assert_eq!(parse_locations(&body, 100).len(), 1);
        assert!(parse_locations(&body, 200).is_empty());
    }

    #[test]
    fn availability_flags_and_alternate_price_keys_are_understood() {
        let body = json!({"data": {"locations": [{
            "id": "loc", "country": "Norway",
            "gpus": [
                {"v0Name": "rtxa5000-pcie-24gb", "available": true, "pricePerHour": 0.3},
                {"v0Name": "rtxa4000-pcie-16gb", "available": false, "price_per_hr": 0.2},
                {"v0Name": "l40s-pcie-48gb", "available_count": 0, "price_per_hr": 0.9}
            ]
        }]}});
        let offers = parse_locations(&body, 100);
        assert_eq!(offers.len(), 1);
        assert_eq!(offers[0].country, "NO");
        assert!((offers[0].compute_hourly_price - 0.3).abs() < 1e-9);
    }

    #[test]
    fn json_shape_reports_keys_without_values() {
        let shape = json_shape(
            &json!({"data": {"locations": [{"id": "secret-ish", "n": 1}]}}),
            4,
        );
        assert_eq!(shape, "{data: {locations: [1 x {id: string, n: number}]}}");
    }

    #[test]
    fn country_filter_accepts_iso_codes_and_names() {
        assert!(country_matches("United States", "US"));
        assert!(country_matches("Germany", "de"));
        assert!(country_matches("US", "us"));
        assert!(!country_matches("Germany", "US"));
    }

    #[test]
    fn parses_running_dedicated_ip_instance() {
        let body = json!({
            "type": "virtualmachine",
            "id": "6b7e8f0a-1c2d-4e5f",
            "attributes": {
                "name": "Noland Connect Session",
                "status": "running",
                "ipAddress": "203.0.113.10",
                "rateHourly": 0.44,
                "resources": { "gpus": { "geforcertx4090-pcie-24gb": { "count": 1 } } }
            }
        });
        let parsed = parse_instance(&body, None).unwrap();
        assert_eq!(parsed.remote_id, "6b7e8f0a-1c2d-4e5f");
        let instance = parsed.instance;
        assert!(instance.ssh_ready());
        assert!(instance.is_vm_runtime());
        assert_eq!(instance.ssh_host, "203.0.113.10");
        assert_eq!(instance.ssh_port, 22);
        assert_eq!(instance.wireguard_port, 51820);
        assert_eq!(instance.wireguard_endpoint_host(), "203.0.113.10");
        assert_eq!(instance.network_probe_port, 6201);
        assert_eq!(instance.gpu_name, "RTX 4090");
        assert!((instance.hourly_price - 0.44).abs() < 1e-9);
        assert_eq!(
            instance.id,
            foreign_local_id(CloudProviderKind::Tensordock, "6b7e8f0a-1c2d-4e5f")
        );
    }

    #[test]
    fn parses_flat_instance_from_official_docs() {
        // Shape of GET /api/v2/instances/{id} in TensorDock's API docs.
        let body = json!({
            "type": "instance",
            "id": "inst-1",
            "name": "noland",
            "status": "running",
            "ipAddress": "203.0.113.50",
            "portForwards": [],
            "resources": {
                "vcpu_count": 8, "ram_gb": 32, "storage_gb": 100,
                "gpus": { "RTX 4090": { "count": 1, "v0Name": "geforcertx4090-pcie-24gb" } }
            },
            "rateHourly": 0.62
        });
        let envelope = body.get("data").unwrap_or(&body);
        let parsed = parse_instance(envelope, Some("inst-1")).unwrap();
        assert_eq!(parsed.remote_id, "inst-1");
        assert_eq!(parsed.instance.public_ip, "203.0.113.50");
        assert_eq!(parsed.instance.ssh_port, 22);
        assert_eq!(parsed.instance.wireguard_port, 51820);
        assert_eq!(parsed.instance.gpu_name, "RTX 4090");
    }

    #[test]
    fn listed_instances_without_an_ip_need_details() {
        // Item shape of GET /api/v2/instances in TensorDock's API docs.
        let listed = parse_instance(
            &json!({
                "type": "VM",
                "id": "550e8400-e29b-41d4-a716-446655440000",
                "attributes": { "name": "My Instance", "status": "running" }
            }),
            None,
        )
        .unwrap();
        assert_eq!(listed.remote_id, "550e8400-e29b-41d4-a716-446655440000");
        assert!(listed.instance.public_ip.is_empty());
        assert!(needs_details(&listed));

        let detailed = parse_instance(
            &json!({"id": "a", "status": "running", "ipAddress": "203.0.113.9"}),
            None,
        )
        .unwrap();
        assert!(!needs_details(&detailed));
        let deleting = parse_instance(
            &json!({"id": "b", "attributes": {"status": "deleting"}}),
            None,
        )
        .unwrap();
        assert!(!needs_details(&deleting));
    }

    #[test]
    fn parses_official_locations_example() {
        // Response example of GET /api/v2/locations in TensorDock's API docs.
        let body = json!({"data": {"locations": [{
            "id": "loc-uuid-12345", "city": "Austin", "stateprovince": "Texas",
            "country": "United States", "tier": 3,
            "gpus": [
                {"v0Name": "h100-sxm5-80gb", "displayName": "H100 SXM5 80GB", "max_count": 8,
                 "price_per_hr": 2.2,
                 "resources": {"max_vcpus": 128, "max_ram_gb": 300, "max_storage_gb": 1000},
                 "pricing": {"per_vcpu_hr": 0.003, "per_gb_ram_hr": 0.002, "per_gb_storage_hr": 0.00005},
                 "network_features": {"dedicated_ip_available": true, "port_forwarding_available": true}},
                {"v0Name": "geforcertx4090-pcie-24gb", "displayName": "NVIDIA GeForce RTX 4090 PCIe 24GB",
                 "max_count": 4, "price_per_hr": 0.5,
                 "resources": {"max_vcpus": 32, "max_ram_gb": 128, "max_storage_gb": 2000},
                 "pricing": {"per_vcpu_hr": 0.003, "per_gb_ram_hr": 0.002, "per_gb_storage_hr": 0.00005},
                 "network_features": {"dedicated_ip_available": false, "port_forwarding_available": true}}
            ]
        }]}});
        let offers = parse_locations(&body, 100);
        assert_eq!(offers.len(), 2);
        assert_eq!(offers[0].country, "US");
        assert_eq!(offers[0].gpu_name, "H100 SXM5 80GB");
        assert!(offers[0].has_static_ip);
        // The 4090 has no dedicated IP but supports port forwarding.
        let forwarded = &offers[1];
        assert!(!forwarded.has_static_ip);
        assert!(forwarded.host_label.ends_with("(port-forwarded)"));
        let offer_ref: TensorDockOfferRef =
            serde_json::from_str(&forwarded.provider_offer_ref).unwrap();
        assert!(!offer_ref.use_dedicated_ip);
        assert!(parse_locations(&json!({"data": {"locations": []}}), 100).is_empty());
    }

    #[test]
    fn parses_port_forwarded_instance() {
        let body = json!({
            "id": "abc",
            "attributes": {
                "status": "starting",
                "ipAddress": "198.51.100.4",
                "portForwards": [
                    { "internal_port": 22, "external_port": 20022, "protocol": "tcp" },
                    { "internal_port": 51820, "external_port": 20051, "protocol": "udp" }
                ]
            }
        });
        let instance = parse_instance(&body, None).unwrap().instance;
        assert!(instance.is_loading());
        assert_eq!(instance.ssh_port, 20022);
        assert_eq!(instance.wireguard_port, 20051);
        assert_eq!(instance.network_probe_port, 0, "unforwarded UDP port is unavailable");
    }

    #[test]
    fn create_payload_requests_dedicated_ip_and_minimum_storage() {
        let payload = create_instance_payload(
            &TensorDockOfferRef {
                location_id: "loc-1".into(),
                gpu_v0_name: "geforcertx4090-pcie-24gb".into(),
                vcpu_count: 8,
                ram_gb: 32,
                use_dedicated_ip: true,
            },
            50,
            "Noland",
            "ssh-ed25519 AAAA test\n",
        );
        let attributes = &payload["data"]["attributes"];
        assert_eq!(attributes["useDedicatedIp"], true);
        assert_eq!(attributes["resources"]["storage_gb"], 100);
        assert_eq!(attributes["ssh_key"], "ssh-ed25519 AAAA test");
        assert_eq!(
            attributes["resources"]["gpus"]["geforcertx4090-pcie-24gb"]["count"],
            1
        );
        assert_eq!(attributes["location_id"], "loc-1");
    }

    #[test]
    fn port_forwarded_payload_forwards_ssh_wireguard_and_probe() {
        let payload = create_instance_payload(
            &TensorDockOfferRef {
                location_id: "loc-1".into(),
                gpu_v0_name: "geforcertx4090-pcie-24gb".into(),
                vcpu_count: 8,
                ram_gb: 32,
                use_dedicated_ip: false,
            },
            200,
            "Noland",
            "ssh-ed25519 AAAA",
        );
        let attributes = &payload["data"]["attributes"];
        assert!(attributes.get("useDedicatedIp").is_none());
        assert_eq!(
            attributes["port_forwards"],
            json!([
                { "internal_port": 22, "external_port": 0 },
                { "internal_port": 51820, "external_port": 0 },
                { "internal_port": 6201, "external_port": 0 }
            ])
        );
    }

    #[test]
    fn offer_refs_saved_before_port_forwarding_default_to_dedicated_ip() {
        let offer_ref: TensorDockOfferRef =
            serde_json::from_str(r#"{"locationId":"l","gpuV0Name":"g","vcpuCount":8,"ramGb":32}"#)
                .unwrap();
        assert!(offer_ref.use_dedicated_ip);
    }

    #[test]
    fn bootstrap_script_quotes_credentials() {
        let script = access_bootstrap_script("user", "pa'ss");
        assert!(script.contains(r"echo 'user:pa'\''ss' | sudo chpasswd"));
        assert!(script.contains("PermitRootLogin prohibit-password"));
        // Copying keys as root would append authorized_keys to itself.
        assert!(script.contains(r#"if [ "$(id -u)" -ne 0 ]; then"#));
        assert!(script.trim_end().ends_with("NOLAND_ACCESS_READY"));
    }

    #[test]
    fn error_detail_prefers_json_api_errors() {
        let body = json!({"errors": [{"status": "400", "title": "Bad", "detail": "Insufficient balance"}]});
        assert_eq!(error_detail(&body), "Insufficient balance");
    }
}
