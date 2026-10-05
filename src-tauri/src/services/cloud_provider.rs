//! Provider-dispatching GPU cloud client.
//!
//! `CloudClient` exposes the operations the app performs on rented machines
//! (search, create, inspect, list, stop, destroy, SSH key sync) and routes
//! each one to the provider that owns the offer or instance. Instances are
//! addressed by local id (see `models::provider`); Vast ids pass straight
//! through and TensorDock ids are resolved through the persisted
//! `provider_instance_refs` table, which this client keeps up to date.

use std::sync::Arc;

use tokio::sync::RwLock;
use tracing::warn;

use crate::{
    errors::{AppError, AppResult},
    models::{
        app_state::OfferCandidate,
        provider::{
            is_foreign_local_id, remember_instance_ref, resolve_instance, CloudProviderKind,
            ProviderInstanceRef,
        },
        vast::{VastInstance, VastOffer, VastSshKey},
    },
    services::{
        app_context::AppContext,
        tensordock_api::{
            TensorDockApiClient, TensorDockInstance, TensorDockOfferRef,
            TENSORDOCK_MIN_STORAGE_GB,
        },
        vast_api::VastApiClient,
    },
};

/// Filters for an offer search across providers.
#[derive(Debug, Clone)]
pub struct OfferSearch<'a> {
    pub min_reliability: f64,
    pub limit: usize,
    pub storage_gb: u32,
    pub geolocation_country_code: Option<&'a str>,
    pub require_verified: bool,
    pub require_datacenter: bool,
    pub require_avx: bool,
}

/// Instances from every provider that answered, plus the providers that
/// failed. Callers that delete local records for missing instances must
/// only do so when `failed` is empty.
#[derive(Debug, Default)]
pub struct InstanceListing {
    pub instances: Vec<VastInstance>,
    pub failed: Vec<(CloudProviderKind, AppError)>,
}

#[derive(Clone)]
pub struct CloudClient {
    context: Option<AppContext>,
    vast: Option<VastApiClient>,
    tensordock: Option<TensorDockApiClient>,
    refs: Arc<RwLock<Vec<ProviderInstanceRef>>>,
}

impl CloudClient {
    /// Client for every provider with an API key in the persisted state.
    pub async fn from_context(context: &AppContext) -> AppResult<Self> {
        let state = context.state.read().await;
        let vast_key = state.credentials.vast_api_key.trim().to_string();
        let tensordock_key = state.credentials.tensordock_api_key.trim().to_string();
        let refs = state.provider_instance_refs.clone();
        drop(state);

        let client = Self {
            context: Some(context.clone()),
            vast: (!vast_key.is_empty()).then(|| {
                VastApiClient::new(
                    context.http_client.clone(),
                    context.config.vast_base_url.clone(),
                    vast_key,
                )
            }),
            tensordock: (!tensordock_key.is_empty()).then(|| {
                TensorDockApiClient::new(
                    context.http_client.clone(),
                    context.config.tensordock_base_url.clone(),
                    tensordock_key,
                )
            }),
            refs: Arc::new(RwLock::new(refs)),
        };
        if !client.has_any_provider() {
            return Err(AppError::InvalidInput(
                "No GPU provider API key is configured. Add a Vast.ai or TensorDock key in Settings."
                    .to_string(),
            ));
        }
        Ok(client)
    }

    /// Vast-only client, for code paths and tests that predate providers.
    pub fn from_vast(vast: VastApiClient) -> Self {
        Self {
            context: None,
            vast: Some(vast),
            tensordock: None,
            refs: Arc::new(RwLock::new(Vec::new())),
        }
    }

    pub fn has_any_provider(&self) -> bool {
        self.vast.is_some() || self.tensordock.is_some()
    }

    pub fn has_vast(&self) -> bool {
        self.vast.is_some()
    }

    fn vast(&self) -> AppResult<&VastApiClient> {
        self.vast.as_ref().ok_or_else(|| {
            AppError::InvalidInput("Missing Vast.ai API key. Add it in Settings.".to_string())
        })
    }

    fn tensordock(&self) -> AppResult<&TensorDockApiClient> {
        self.tensordock.as_ref().ok_or_else(|| {
            AppError::InvalidInput("Missing TensorDock API key. Add it in Settings.".to_string())
        })
    }

    /// Provider that owns a local instance id.
    pub async fn provider_for_instance(&self, instance_id: u64) -> Option<CloudProviderKind> {
        let refs = self.refs.read().await;
        resolve_instance(&refs, instance_id).map(|resolved| resolved.provider)
    }

    async fn tensordock_remote_id(&self, instance_id: u64) -> AppResult<String> {
        let refs = self.refs.read().await;
        resolve_instance(&refs, instance_id)
            .filter(|resolved| resolved.provider == CloudProviderKind::Tensordock)
            .map(|resolved| resolved.remote_id)
            .ok_or_else(|| {
                AppError::NotFound(format!(
                    "Instance {instance_id} is not a known TensorDock instance"
                ))
            })
    }

    /// Remember TensorDock UUIDs so later calls by local id can resolve them.
    async fn remember(&self, instances: &[TensorDockInstance]) {
        let mut changed = false;
        {
            let mut refs = self.refs.write().await;
            for instance in instances {
                changed |= remember_instance_ref(
                    &mut refs,
                    CloudProviderKind::Tensordock,
                    &instance.remote_id,
                );
            }
        }
        if !changed {
            return;
        }
        let Some(context) = &self.context else {
            return;
        };
        let refs = self.refs.read().await.clone();
        if let Err(error) = context
            .update_state(|state| {
                for reference in refs {
                    if !state
                        .provider_instance_refs
                        .iter()
                        .any(|existing| existing.local_id == reference.local_id)
                    {
                        state.provider_instance_refs.push(reference);
                    }
                }
            })
            .await
        {
            warn!("failed to persist provider instance ids: {error}");
        }
    }

    pub async fn search_offers(&self, search: &OfferSearch<'_>) -> AppResult<Vec<VastOffer>> {
        let mut offers = Vec::new();
        let mut errors = Vec::new();
        if let Some(vast) = &self.vast {
            match vast
                .search_offers(
                    search.min_reliability,
                    search.limit,
                    search.storage_gb,
                    search.geolocation_country_code,
                    search.require_verified,
                    search.require_datacenter,
                    search.require_avx,
                )
                .await
            {
                Ok(found) => offers.extend(found),
                Err(error) => errors.push(error),
            }
        }
        if let Some(tensordock) = &self.tensordock {
            match tensordock
                .search_offers(search.storage_gb, search.geolocation_country_code)
                .await
            {
                Ok(found) => offers.extend(
                    found
                        .into_iter()
                        .filter(|offer| offer.reliability >= search.min_reliability.min(0.95)),
                ),
                Err(error) => {
                    warn!("TensorDock offer search failed (continuing): {error}");
                    errors.push(error);
                }
            }
        }
        if offers.is_empty() {
            if let Some(error) = errors.into_iter().next() {
                return Err(error);
            }
        }
        Ok(offers)
    }

    /// Rent an offer. `ssh_public_key` is used by providers that take the
    /// key at creation time; Vast uses the account key synced beforehand.
    pub async fn create_instance(
        &self,
        offer: &OfferCandidate,
        template_hash: &str,
        storage_gb: u32,
        label: &str,
        env_vars: Option<serde_json::Value>,
        ssh_public_key: &str,
    ) -> AppResult<VastInstance> {
        match CloudProviderKind::parse(&offer.provider) {
            Some(CloudProviderKind::Vast) => {
                self.vast()?
                    .create_instance(offer.id, template_hash, storage_gb, label, env_vars)
                    .await
            }
            Some(CloudProviderKind::Tensordock) => {
                let offer_ref: TensorDockOfferRef =
                    serde_json::from_str(&offer.provider_offer_ref).map_err(|error| {
                        AppError::InvalidInput(format!(
                            "Selected TensorDock offer is missing deployment details ({error}). Refresh offers and select it again."
                        ))
                    })?;
                let created = self
                    .tensordock()?
                    .create_instance(
                        &offer_ref,
                        storage_gb.max(TENSORDOCK_MIN_STORAGE_GB),
                        label,
                        ssh_public_key,
                    )
                    .await?;
                self.remember(std::slice::from_ref(&created)).await;
                Ok(created.instance)
            }
            None => Err(AppError::InvalidInput(format!(
                "Unsupported GPU provider '{}'",
                offer.provider
            ))),
        }
    }

    pub async fn get_instance(&self, instance_id: u64) -> AppResult<VastInstance> {
        if !is_foreign_local_id(instance_id) {
            return self.vast()?.get_instance(instance_id).await;
        }
        let remote_id = self.tensordock_remote_id(instance_id).await?;
        let instance = self.tensordock()?.get_instance(&remote_id).await?;
        Ok(instance.instance)
    }

    /// Instances across providers. Fails if any configured provider fails,
    /// so callers never mistake an outage for "everything was destroyed".
    pub async fn list_instances(&self) -> AppResult<Vec<VastInstance>> {
        let listing = self.list_instances_partial().await;
        if let Some((_, error)) = listing.failed.into_iter().next() {
            return Err(error);
        }
        Ok(listing.instances)
    }

    pub async fn list_instances_partial(&self) -> InstanceListing {
        let mut listing = InstanceListing::default();
        if let Some(vast) = &self.vast {
            match vast.list_instances().await {
                Ok(instances) => listing.instances.extend(instances),
                Err(error) => listing.failed.push((CloudProviderKind::Vast, error)),
            }
        }
        if let Some(tensordock) = &self.tensordock {
            match tensordock.list_instances().await {
                Ok(instances) => {
                    self.remember(&instances).await;
                    listing
                        .instances
                        .extend(instances.into_iter().map(|instance| instance.instance));
                }
                Err(error) => listing.failed.push((CloudProviderKind::Tensordock, error)),
            }
        }
        listing
    }

    pub async fn pause_instance(&self, instance_id: u64) -> AppResult<VastInstance> {
        if !is_foreign_local_id(instance_id) {
            return self.vast()?.pause_instance(instance_id).await;
        }
        let remote_id = self.tensordock_remote_id(instance_id).await?;
        let tensordock = self.tensordock()?;
        tensordock.stop_instance(&remote_id).await?;
        Ok(tensordock.get_instance(&remote_id).await?.instance)
    }

    pub async fn destroy_instance(&self, instance_id: u64) -> AppResult<()> {
        if !is_foreign_local_id(instance_id) {
            return self.vast()?.destroy_instance(instance_id).await;
        }
        let remote_id = self.tensordock_remote_id(instance_id).await?;
        self.tensordock()?.delete_instance(&remote_id).await
    }

    pub async fn list_ssh_keys(&self) -> AppResult<Vec<VastSshKey>> {
        match &self.vast {
            Some(vast) => vast.list_ssh_keys().await,
            None => Ok(Vec::new()),
        }
    }

    /// Upload the key to account-level key stores (Vast). No-op for
    /// providers that take the key at creation time.
    pub async fn upload_ssh_key(&self, public_key: &str) -> AppResult<()> {
        match &self.vast {
            Some(vast) => vast.upload_ssh_key(public_key).await,
            None => Ok(()),
        }
    }

    pub async fn attach_ssh_key(&self, instance_id: u64, public_key: &str) -> AppResult<()> {
        if is_foreign_local_id(instance_id) {
            // TensorDock installs the key at creation; there is no attach call.
            return Ok(());
        }
        self.vast()?.attach_ssh_key(instance_id, public_key).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn vast_only_client_routes_foreign_ids_to_a_clear_error() {
        let client = CloudClient::from_vast(VastApiClient::new(
            reqwest::Client::new(),
            "http://127.0.0.1:9".into(),
            "key".into(),
        ));
        assert!(client.has_vast() && client.has_any_provider());
        let foreign = crate::models::provider::foreign_local_id(CloudProviderKind::Tensordock, "x");
        let error = client.get_instance(foreign).await.unwrap_err();
        assert!(matches!(error, AppError::NotFound(_)), "{error}");
        assert!(client.attach_ssh_key(foreign, "ssh-ed25519 AAAA").await.is_ok());
    }
}
