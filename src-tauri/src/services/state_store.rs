use std::collections::HashMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde_json::{Map, Value};
use tokio::fs;
use tracing::{info, warn};

use crate::{
    errors::{AppError, AppResult},
    models::app_state::{
        ConnectionProvider, NetworkEndpoint, PathAvailability, PersistedAppState, TransportKind,
        WireGuardSetupStatus,
    },
    utils::atomic_file::write_atomically,
};

#[async_trait]
pub trait StateStore: Send + Sync {
    async fn load_state(&self) -> AppResult<PersistedAppState>;
    async fn save_state(&self, state: &PersistedAppState) -> AppResult<()>;
    fn path(&self) -> &Path;
}

// ---------------------------------------------------------------------------
// Secret fields of PersistedAppState
//
// Secrets (app password, Vast API key, Twitch client secret, Backblaze
// application key, rclone crypt password) are kept in memory on
// PersistedAppState so every reader and the frontend contract keep working,
// but `JsonStateStore` never writes them to state.json when the OS credential
// store (keyring) accepts them. Instead each stored secret is recorded in a
// non-secret top-level marker object (`secretStorage`) so the next load knows
// to hydrate it from the keyring.
//
// Migration: a state.json that still contains a plaintext secret is moved into
// the keyring on the next save (which `load_state` performs immediately) and
// the plaintext is removed from the file.
//
// Fallback: if the keyring is unavailable (e.g. headless Linux without a
// Secret Service), the secret stays in state.json in plaintext so the app keeps
// working, a warning is logged (never the secret itself) and migration is
// retried on the next app start.
// ---------------------------------------------------------------------------

const SECRET_KEYRING_SERVICE: &str = "com.noland.connect.app-state";
const SECRET_STORAGE_KEY: &str = "secretStorage";
const SECRET_STORAGE_KEYRING: &str = "keyring";

/// Minimal secret backend abstraction so the state store can be tested
/// without touching the real OS credential store.
pub trait SecretBackend: Send + Sync + fmt::Debug {
    fn get(&self, account: &str) -> Result<Option<String>, String>;
    fn set(&self, account: &str, value: &str) -> Result<(), String>;
    fn delete(&self, account: &str) -> Result<(), String>;
}

/// OS credential store (macOS Keychain, Windows Credential Manager, Linux
/// Secret Service) via the `keyring` crate.
#[cfg_attr(test, allow(dead_code))]
#[derive(Debug, Default)]
pub struct OsKeyringSecretBackend;

#[cfg_attr(test, allow(dead_code))]
impl OsKeyringSecretBackend {
    fn entry(account: &str) -> Result<keyring::Entry, String> {
        keyring::Entry::new(SECRET_KEYRING_SERVICE, account).map_err(|error| error.to_string())
    }
}

impl SecretBackend for OsKeyringSecretBackend {
    fn get(&self, account: &str) -> Result<Option<String>, String> {
        match Self::entry(account)?.get_password() {
            Ok(value) => Ok(Some(value)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(error) => Err(error.to_string()),
        }
    }

    fn set(&self, account: &str, value: &str) -> Result<(), String> {
        let entry = Self::entry(account)?;
        entry
            .set_password(value)
            .map_err(|error| error.to_string())?;
        // Mirror the Cloudflare TURN pattern: only trust the write after the
        // same entry reads back the same value (some platforms accept the
        // write but redirect or drop it).
        match entry.get_password() {
            Ok(stored) if stored == value => Ok(()),
            Ok(_) => Err("value read back from secure storage did not match".to_string()),
            Err(error) => Err(format!("could not read back value: {error}")),
        }
    }

    fn delete(&self, account: &str) -> Result<(), String> {
        match Self::entry(account)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(error) => Err(error.to_string()),
        }
    }
}

/// In-memory backend used by tests.
#[cfg(test)]
#[derive(Debug, Default)]
pub struct InMemorySecretBackend {
    values: Mutex<HashMap<String, String>>,
    unavailable: bool,
}

#[cfg(test)]
impl InMemorySecretBackend {
    /// A backend whose every operation fails, emulating a missing keyring.
    pub fn unavailable() -> Self {
        Self {
            values: Mutex::new(HashMap::new()),
            unavailable: true,
        }
    }

    fn snapshot(&self) -> HashMap<String, String> {
        self.values.lock().unwrap().clone()
    }
}

#[cfg(test)]
impl SecretBackend for InMemorySecretBackend {
    fn get(&self, account: &str) -> Result<Option<String>, String> {
        if self.unavailable {
            return Err("secure storage unavailable".to_string());
        }
        Ok(self
            .values
            .lock()
            .map_err(|_| "poisoned".to_string())?
            .get(account)
            .cloned())
    }

    fn set(&self, account: &str, value: &str) -> Result<(), String> {
        if self.unavailable {
            return Err("secure storage unavailable".to_string());
        }
        self.values
            .lock()
            .map_err(|_| "poisoned".to_string())?
            .insert(account.to_string(), value.to_string());
        Ok(())
    }

    fn delete(&self, account: &str) -> Result<(), String> {
        if self.unavailable {
            return Err("secure storage unavailable".to_string());
        }
        self.values
            .lock()
            .map_err(|_| "poisoned".to_string())?
            .remove(account);
        Ok(())
    }
}

struct SecretField {
    /// Keyring account name and marker key in `secretStorage`.
    account: &'static str,
    /// camelCase JSON path inside the serialized PersistedAppState.
    json_path: &'static [&'static str],
    /// Whether the stripped JSON value is `null` (Option field) instead of "".
    nullable: bool,
    get: fn(&PersistedAppState) -> &str,
    set: fn(&mut PersistedAppState, String),
}

fn get_app_password(state: &PersistedAppState) -> &str {
    &state.credentials.app_password
}
fn set_app_password(state: &mut PersistedAppState, value: String) {
    state.credentials.app_password = value;
}
fn get_vast_api_key(state: &PersistedAppState) -> &str {
    &state.credentials.vast_api_key
}
fn set_vast_api_key(state: &mut PersistedAppState, value: String) {
    state.credentials.vast_api_key = value;
}
fn get_tensordock_api_key(state: &PersistedAppState) -> &str {
    &state.credentials.tensordock_api_key
}
fn set_tensordock_api_key(state: &mut PersistedAppState, value: String) {
    state.credentials.tensordock_api_key = value;
}
fn get_twitch_client_secret(state: &PersistedAppState) -> &str {
    &state.credentials.twitch_client_secret
}
fn set_twitch_client_secret(state: &mut PersistedAppState, value: String) {
    state.credentials.twitch_client_secret = value;
}
fn get_backblaze_application_key(state: &PersistedAppState) -> &str {
    &state.shared_storage.settings.backblaze_application_key
}
fn set_backblaze_application_key(state: &mut PersistedAppState, value: String) {
    state.shared_storage.settings.backblaze_application_key = value;
}
fn get_crypt_password(state: &PersistedAppState) -> &str {
    state
        .shared_storage
        .settings
        .crypt_password
        .as_deref()
        .unwrap_or("")
}
fn set_crypt_password(state: &mut PersistedAppState, value: String) {
    state.shared_storage.settings.crypt_password = (!value.is_empty()).then_some(value);
}

const SECRET_FIELDS: &[SecretField] = &[
    SecretField {
        account: "credentials.appPassword",
        json_path: &["credentials", "appPassword"],
        nullable: false,
        get: get_app_password,
        set: set_app_password,
    },
    SecretField {
        account: "credentials.vastApiKey",
        json_path: &["credentials", "vastApiKey"],
        nullable: false,
        get: get_vast_api_key,
        set: set_vast_api_key,
    },
    SecretField {
        account: "credentials.tensordockApiKey",
        json_path: &["credentials", "tensordockApiKey"],
        nullable: false,
        get: get_tensordock_api_key,
        set: set_tensordock_api_key,
    },
    SecretField {
        account: "credentials.twitchClientSecret",
        json_path: &["credentials", "twitchClientSecret"],
        nullable: false,
        get: get_twitch_client_secret,
        set: set_twitch_client_secret,
    },
    SecretField {
        account: "sharedStorage.settings.backblazeApplicationKey",
        json_path: &["sharedStorage", "settings", "backblazeApplicationKey"],
        nullable: false,
        get: get_backblaze_application_key,
        set: set_backblaze_application_key,
    },
    SecretField {
        account: "sharedStorage.settings.cryptPassword",
        json_path: &["sharedStorage", "settings", "cryptPassword"],
        nullable: true,
        get: get_crypt_password,
        set: set_crypt_password,
    },
];

/// What the store knows about a secret's location after load/save.
/// A field missing from the cache is "unknown" and treated like `Unreadable`
/// (never delete anything we have not confirmed).
#[derive(Clone, PartialEq, Eq)]
enum SecretSlot {
    /// Nothing stored anywhere.
    Absent,
    /// Stored in the keyring with this value.
    Stored(String),
    /// Keyring write failed; this value is kept in plaintext in state.json.
    Plaintext(String),
    /// Marker says the keyring holds it, but it could not be read.
    Unreadable,
}

impl fmt::Debug for SecretSlot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Never print secret values.
        let name = match self {
            Self::Absent => "Absent",
            Self::Stored(_) => "Stored(<redacted>)",
            Self::Plaintext(_) => "Plaintext(<redacted>)",
            Self::Unreadable => "Unreadable",
        };
        f.write_str(name)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MarkerAction {
    Set,
    Remove,
    Keep,
}

#[derive(Debug, Clone, Copy)]
struct SecretSaveDecision {
    strip: bool,
    marker: MarkerAction,
}

type SecretCache = Arc<Mutex<HashMap<&'static str, SecretSlot>>>;

/// Synchronise in-memory secret values with the backend. Blocking: call via
/// `spawn_blocking`.
fn sync_secrets_for_save(
    backend: &dyn SecretBackend,
    cache: &SecretCache,
    values: &[String],
) -> Vec<SecretSaveDecision> {
    let mut cache = match cache.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    SECRET_FIELDS
        .iter()
        .zip(values)
        .map(|(field, value)| {
            let slot = cache.get(field.account).cloned();
            if value.is_empty() {
                return match slot {
                    Some(SecretSlot::Stored(_)) => {
                        if let Err(error) = backend.delete(field.account) {
                            warn!(
                                secret = field.account,
                                %error,
                                "Could not remove a cleared secret from secure storage"
                            );
                        }
                        cache.insert(field.account, SecretSlot::Absent);
                        SecretSaveDecision {
                            strip: false,
                            marker: MarkerAction::Remove,
                        }
                    }
                    Some(SecretSlot::Absent) | Some(SecretSlot::Plaintext(_)) => {
                        cache.insert(field.account, SecretSlot::Absent);
                        SecretSaveDecision {
                            strip: false,
                            marker: MarkerAction::Remove,
                        }
                    }
                    // Unknown or unreadable: the keyring may still hold the
                    // real value; never drop the marker or delete it.
                    Some(SecretSlot::Unreadable) | None => SecretSaveDecision {
                        strip: false,
                        marker: MarkerAction::Keep,
                    },
                };
            }

            match slot {
                Some(SecretSlot::Stored(stored)) if &stored == value => SecretSaveDecision {
                    strip: true,
                    marker: MarkerAction::Set,
                },
                // Keyring already rejected this exact value this session;
                // don't retry on every save (retried on next app start).
                Some(SecretSlot::Plaintext(plain)) if &plain == value => SecretSaveDecision {
                    strip: false,
                    marker: MarkerAction::Remove,
                },
                previous => match backend.set(field.account, value) {
                    Ok(()) => {
                        if previous.is_none() {
                            info!(
                                secret = field.account,
                                "Stored a secret in the OS credential store"
                            );
                        }
                        cache.insert(field.account, SecretSlot::Stored(value.clone()));
                        SecretSaveDecision {
                            strip: true,
                            marker: MarkerAction::Set,
                        }
                    }
                    Err(error) => {
                        warn!(
                            secret = field.account,
                            %error,
                            "OS credential store unavailable; keeping this secret in state.json (plaintext fallback)"
                        );
                        cache.insert(field.account, SecretSlot::Plaintext(value.clone()));
                        SecretSaveDecision {
                            strip: false,
                            marker: MarkerAction::Remove,
                        }
                    }
                },
            }
        })
        .collect()
}

/// Hydrate secrets that state.json says live in the keyring. Blocking.
/// Returns the hydrated values per field (None = leave as loaded).
fn hydrate_secrets_on_load(
    backend: &dyn SecretBackend,
    cache: &SecretCache,
    loaded_values: &[String],
    markers: &[bool],
) -> Vec<Option<String>> {
    let mut cache = match cache.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    SECRET_FIELDS
        .iter()
        .zip(loaded_values.iter().zip(markers))
        .map(|(field, (loaded, has_marker))| {
            if !loaded.is_empty() {
                // Plaintext still present in state.json: leave the cache unknown
                // so the save that follows load migrates it into the keyring.
                cache.remove(field.account);
                return None;
            }
            if !has_marker {
                cache.insert(field.account, SecretSlot::Absent);
                return None;
            }
            match backend.get(field.account) {
                Ok(Some(value)) => {
                    cache.insert(field.account, SecretSlot::Stored(value.clone()));
                    Some(value)
                }
                Ok(None) => {
                    warn!(
                        secret = field.account,
                        "Secret marked as stored in secure storage was not found there"
                    );
                    cache.insert(field.account, SecretSlot::Absent);
                    None
                }
                Err(error) => {
                    warn!(
                        secret = field.account,
                        %error,
                        "Could not read a secret from secure storage; it will be unavailable until the next successful load"
                    );
                    cache.insert(field.account, SecretSlot::Unreadable);
                    None
                }
            }
        })
        .collect()
}

fn secret_markers(raw_root: &Value) -> Vec<bool> {
    let markers = raw_root.get(SECRET_STORAGE_KEY);
    SECRET_FIELDS
        .iter()
        .map(|field| {
            markers
                .and_then(|value| value.get(field.account))
                .and_then(Value::as_str)
                == Some(SECRET_STORAGE_KEYRING)
        })
        .collect()
}

fn strip_secret_from_json(root: &mut Value, field: &SecretField) {
    let Some((last, parents)) = field.json_path.split_last() else {
        return;
    };
    let mut cursor = root;
    for segment in parents {
        match cursor.get_mut(*segment) {
            Some(next) => cursor = next,
            None => return,
        }
    }
    if let Value::Object(map) = cursor {
        if map.contains_key(*last) {
            let replacement = if field.nullable {
                Value::Null
            } else {
                Value::String(String::new())
            };
            map.insert((*last).to_string(), replacement);
        }
    }
}

#[derive(Debug, Clone)]
pub struct JsonStateStore {
    state_path: PathBuf,
    current_version: u32,
    secret_backend: Arc<dyn SecretBackend>,
    secret_cache: SecretCache,
}

impl JsonStateStore {
    pub fn new(state_path: PathBuf, current_version: u32) -> Self {
        // Unit tests must never touch the developer's real OS keyring.
        #[cfg(test)]
        let backend: Arc<dyn SecretBackend> = Arc::new(InMemorySecretBackend::default());
        #[cfg(not(test))]
        let backend: Arc<dyn SecretBackend> = Arc::new(OsKeyringSecretBackend);
        Self::with_secret_backend(state_path, current_version, backend)
    }

    pub fn with_secret_backend(
        state_path: PathBuf,
        current_version: u32,
        secret_backend: Arc<dyn SecretBackend>,
    ) -> Self {
        Self {
            state_path,
            current_version,
            secret_backend,
            secret_cache: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    fn mark_all_secrets_absent(&self) {
        let mut cache = match self.secret_cache.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        for field in SECRET_FIELDS {
            cache.insert(field.account, SecretSlot::Absent);
        }
    }

    async fn hydrate_secrets(
        &self,
        state: &mut PersistedAppState,
        raw_root: &Value,
    ) -> AppResult<()> {
        let markers = secret_markers(raw_root);
        let loaded: Vec<String> = SECRET_FIELDS
            .iter()
            .map(|field| (field.get)(state).to_string())
            .collect();
        let backend = self.secret_backend.clone();
        let cache = self.secret_cache.clone();
        let hydrated = tokio::task::spawn_blocking(move || {
            hydrate_secrets_on_load(backend.as_ref(), &cache, &loaded, &markers)
        })
        .await
        .map_err(|error| AppError::State(format!("Secret hydration task failed: {error}")))?;
        for (field, value) in SECRET_FIELDS.iter().zip(hydrated) {
            if let Some(value) = value {
                (field.set)(state, value);
            }
        }
        Ok(())
    }

    async fn ensure_parent_exists(&self) -> AppResult<()> {
        if let Some(parent) = self.state_path.parent() {
            fs::create_dir_all(parent).await?;
        }
        Ok(())
    }

    fn migrate_value(&self, raw_value: Value) -> AppResult<PersistedAppState> {
        let mut baseline = serde_json::to_value(PersistedAppState::default())?;
        reconcile_json(&mut baseline, &raw_value);

        let mut migrated: PersistedAppState = serde_json::from_value(baseline)?;
        let current_instance_id = migrated.instance.instance_id;
        let direct_endpoint = (!migrated.wireguard.endpoint_host.trim().is_empty()
            && migrated.wireguard.endpoint_port != 0)
            .then(|| NetworkEndpoint {
                host: migrated.wireguard.endpoint_host.clone(),
                port: migrated.wireguard.endpoint_port,
            });
        for server in &mut migrated.provisioned_servers {
            server.connection_provider = ConnectionProvider::Wireguard;
            server.embedded_moonlight_pipeline_enabled = true;
            if server.embedded_moonlight_host_id.trim().is_empty() {
                server.embedded_moonlight_host_id = format!("instance-{}", server.instance_id);
            }
            if Some(server.instance_id) == current_instance_id
                && server.network.direct.endpoint.is_none()
            {
                server.network.direct.endpoint = direct_endpoint.clone();
                if server.network.direct.endpoint.is_some()
                    && migrated.post_wireguard_setup.wireguard_setup_status
                        == WireGuardSetupStatus::Connected
                {
                    server.network.direct.availability = PathAvailability::Ready;
                    server.network.active_transport = Some(TransportKind::Direct);
                }
            }
        }
        migrated.connection_provider = ConnectionProvider::Wireguard;
        if migrated.ssh.ssh_username == "root" && migrated.ssh.ssh_password == "user" {
            migrated.ssh.ssh_password = "password".to_string();
        }
        migrated.version = self.current_version;
        migrated.server_preferences.template_hash =
            if migrated.server_preferences.template_hash.is_empty() {
                "566868bff8b15eef891ee706acbbb5e5".to_string()
            } else {
                migrated.server_preferences.template_hash
            };

        Ok(migrated)
    }

    async fn recover_invalid_state(&self, error: &AppError) -> AppResult<PersistedAppState> {
        let backup_path = self.state_path.with_extension(format!(
            "invalid-{}.json",
            chrono::Utc::now().format("%Y%m%dT%H%M%SZ")
        ));

        fs::rename(&self.state_path, &backup_path).await?;
        warn!(
            state_path = %self.state_path.display(),
            backup_path = %backup_path.display(),
            %error,
            "Recovered an invalid persisted state file"
        );

        let state = PersistedAppState::default();
        self.mark_all_secrets_absent();
        self.save_state(&state).await?;
        Ok(state)
    }

    async fn load_existing_root_map(&self) -> AppResult<Map<String, Value>> {
        if !self.state_path.exists() {
            return Ok(Map::new());
        }

        let contents = fs::read_to_string(&self.state_path).await?;
        let raw_value: Value = serde_json::from_str(&contents).map_err(|error| {
            AppError::Serialization(format!(
                "Failed to parse state file at {}: {error}",
                self.state_path.display()
            ))
        })?;

        match raw_value {
            Value::Object(map) => Ok(map),
            _ => Err(AppError::Serialization(format!(
                "State file at {} is not a JSON object",
                self.state_path.display()
            ))),
        }
    }
}

#[async_trait]
impl StateStore for JsonStateStore {
    async fn load_state(&self) -> AppResult<PersistedAppState> {
        self.ensure_parent_exists().await?;

        if !self.state_path.exists() {
            // A fresh (or reset) state starts without secrets; stale keyring
            // entries are ignored because no `secretStorage` marker exists.
            let state = PersistedAppState::default();
            self.mark_all_secrets_absent();
            self.save_state(&state).await?;
            return Ok(state);
        }

        let contents = fs::read_to_string(&self.state_path).await?;
        let raw_value: Value = match serde_json::from_str(&contents) {
            Ok(value) => value,
            Err(error) => {
                let error = AppError::Serialization(format!(
                    "Failed to parse state file at {}: {error}",
                    self.state_path.display()
                ));
                return self.recover_invalid_state(&error).await;
            }
        };

        let raw_root = raw_value.clone();
        let mut migrated = match self.migrate_value(raw_value) {
            Ok(state) => state,
            Err(error @ AppError::Serialization(_)) => {
                return self.recover_invalid_state(&error).await;
            }
            Err(error) => return Err(error),
        };
        // Hydrate keyring-held secrets; plaintext secrets still present in
        // state.json are migrated into the keyring by the save below.
        self.hydrate_secrets(&mut migrated, &raw_root).await?;
        self.save_state(&migrated).await?;
        Ok(migrated)
    }

    async fn save_state(&self, state: &PersistedAppState) -> AppResult<()> {
        self.ensure_parent_exists().await?;

        let mut root_map = match self.load_existing_root_map().await {
            Ok(map) => map,
            Err(AppError::Serialization(_)) if self.state_path.exists() => Map::new(),
            Err(error) => return Err(error),
        };

        let mut next_value = serde_json::to_value(state)?;

        let secret_values: Vec<String> = SECRET_FIELDS
            .iter()
            .map(|field| (field.get)(state).to_string())
            .collect();
        let backend = self.secret_backend.clone();
        let cache = self.secret_cache.clone();
        let decisions = tokio::task::spawn_blocking(move || {
            sync_secrets_for_save(backend.as_ref(), &cache, &secret_values)
        })
        .await
        .map_err(|error| AppError::State(format!("Secret storage task failed: {error}")))?;
        for (field, decision) in SECRET_FIELDS.iter().zip(&decisions) {
            if decision.strip {
                strip_secret_from_json(&mut next_value, field);
            }
        }

        let next_map = match next_value {
            Value::Object(map) => map,
            _ => {
                return Err(AppError::Serialization(
                    "PersistedAppState did not serialize to a JSON object".to_string(),
                ))
            }
        };

        for (key, value) in next_map {
            root_map.insert(key, value);
        }

        let mut markers = match root_map.remove(SECRET_STORAGE_KEY) {
            Some(Value::Object(map)) => map,
            _ => Map::new(),
        };
        for (field, decision) in SECRET_FIELDS.iter().zip(&decisions) {
            match decision.marker {
                MarkerAction::Set => {
                    markers.insert(
                        field.account.to_string(),
                        Value::String(SECRET_STORAGE_KEYRING.to_string()),
                    );
                }
                MarkerAction::Remove => {
                    markers.remove(field.account);
                }
                MarkerAction::Keep => {}
            }
        }
        if !markers.is_empty() {
            root_map.insert(SECRET_STORAGE_KEY.to_string(), Value::Object(markers));
        }

        let body = serde_json::to_vec_pretty(&Value::Object(root_map))?;
        let state_path = self.state_path.clone();
        tokio::task::spawn_blocking(move || write_atomically(&state_path, &body))
            .await
            .map_err(|error| {
                AppError::State(format!("Atomic state writer task failed: {error}"))
            })??;
        Ok(())
    }

    fn path(&self) -> &Path {
        &self.state_path
    }
}

fn reconcile_json(target: &mut Value, source: &Value) {
    match (target, source) {
        (Value::Object(target_map), Value::Object(source_map)) => {
            for (key, source_value) in source_map {
                match target_map.get_mut(key) {
                    Some(target_value) => reconcile_json(target_value, source_value),
                    None => {
                        // Preserve unknown top-level fields when saving later, but never let
                        // unknown fields influence typed deserialization of PersistedAppState.
                        target_map.insert(key.clone(), source_value.clone());
                    }
                }
            }
        }
        (Value::Array(target_array), Value::Array(source_array)) => {
            if let Some(template) = target_array.first().cloned() {
                *target_array = source_array
                    .iter()
                    .map(|source_item| {
                        let mut item = template.clone();
                        reconcile_json(&mut item, source_item);
                        item
                    })
                    .collect();
            } else {
                *target_array = source_array.clone();
            }
        }
        (target_slot @ Value::Null, source_value) => {
            *target_slot = source_value.clone();
        }
        (target_slot @ Value::Bool(_), source_value @ Value::Bool(_))
        | (target_slot @ Value::Number(_), source_value @ Value::Number(_))
        | (target_slot @ Value::String(_), source_value @ Value::String(_)) => {
            *target_slot = source_value.clone();
        }
        // If an older or manually edited state file has the wrong shape for a
        // known field, keep the current default for that field instead of making
        // the whole state fail to deserialize and resetting the user.
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::Arc;

    use serde_json::json;
    use tokio::fs;

    use super::{InMemorySecretBackend, JsonStateStore, StateStore};
    use crate::models::app_state::{
        ConnectionProvider, OrchestrationState, PersistedAppState, ProvisionedServerState,
        TransportKind, WireGuardSetupStatus,
    };

    fn temp_state_path(name: &str) -> PathBuf {
        let base = std::env::temp_dir().join(format!(
            "noland-json-state-store-{name}-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&base).unwrap();
        base.join("state.json")
    }

    #[test]
    fn every_state_enables_embedded_moonlight_for_existing_servers() {
        let store = JsonStateStore::new(PathBuf::from("state.json"), 2);
        let mut state = PersistedAppState::default();
        state.version = 2;
        let mut server = ProvisionedServerState::new(42);
        server.embedded_moonlight_pipeline_enabled = false;
        server.embedded_moonlight_host_id.clear();
        state.provisioned_servers.push(server);

        let migrated = store
            .migrate_value(serde_json::to_value(state).unwrap())
            .unwrap();
        let migrated_server = migrated.provisioned_servers.first().unwrap();

        assert_eq!(migrated.version, 2);
        assert!(migrated_server.embedded_moonlight_pipeline_enabled);
        assert_eq!(migrated_server.embedded_moonlight_host_id, "instance-42");
    }

    #[test]
    fn retired_provider_state_maps_to_managed_tunnel_state() {
        let store = JsonStateStore::new(PathBuf::from("state.json"), 2);
        let migrated = store
            .migrate_value(json!({
                "version": 2,
                "connectionProvider": "tailscale",
                "orchestrationState": "TailscaleConnected"
            }))
            .unwrap();

        assert_eq!(migrated.connection_provider, ConnectionProvider::Wireguard);
        assert_eq!(
            migrated.orchestration_state,
            OrchestrationState::WireGuardConnected
        );
    }

    #[test]
    fn v2_active_tunnel_migrates_to_a_verified_direct_transport() {
        let store = JsonStateStore::new(PathBuf::from("state.json"), 3);
        let mut state = PersistedAppState::default();
        state.version = 2;
        state.instance.instance_id = Some(42);
        state.wireguard.endpoint_host = "203.0.113.20".to_string();
        state.wireguard.endpoint_port = 51820;
        state.post_wireguard_setup.wireguard_setup_status = WireGuardSetupStatus::Connected;
        state
            .provisioned_servers
            .push(ProvisionedServerState::new(42));

        let migrated = store
            .migrate_value(serde_json::to_value(state).unwrap())
            .unwrap();
        let network = &migrated.provisioned_servers[0].network;
        assert_eq!(migrated.version, 3);
        assert_eq!(network.active_transport, Some(TransportKind::Direct));
        assert_eq!(
            network.direct.endpoint.as_ref().unwrap().host,
            "203.0.113.20"
        );
        assert_eq!(network.direct.endpoint.as_ref().unwrap().port, 51820);
    }

    #[tokio::test]
    async fn invalid_state_is_backed_up_and_replaced() {
        let path = temp_state_path("recover-invalid");
        fs::write(&path, b"{ truncated").await.unwrap();

        let store = JsonStateStore::new(path.clone(), 3);
        let recovered = store.load_state().await.unwrap();

        assert_eq!(recovered.version, PersistedAppState::default().version);
        let persisted: serde_json::Value =
            serde_json::from_slice(&fs::read(&path).await.unwrap()).unwrap();
        assert!(persisted.is_object());

        let backup_count = std::fs::read_dir(path.parent().unwrap())
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with("state.invalid-")
            })
            .count();
        assert_eq!(backup_count, 1);
    }

    #[tokio::test]
    async fn incompatible_state_fields_are_reconciled_without_resetting_user_state() {
        let path = temp_state_path("reconcile-incompatible");
        fs::write(
            &path,
            serde_json::to_vec(&json!({
                "version": 1,
                "onboardingCompleted": true,
                "credentials": {
                    "appUsername": "felipe",
                    "appPassword": "secret",
                    "vastApiKey": "vast-key"
                },
                "moonlightPreferences": {
                    "width": "invalid",
                    "height": 1664
                },
                "serverPreferences": {
                    "storageGb": "bad",
                    "templateHash": "abc123"
                }
            }))
            .unwrap(),
        )
        .await
        .unwrap();

        let store = JsonStateStore::new(path.clone(), 3);
        let recovered = store.load_state().await.unwrap();

        assert!(recovered.onboarding_completed);
        assert_eq!(recovered.credentials.app_username, "felipe");
        assert_eq!(recovered.credentials.vast_api_key, "vast-key");
        assert_eq!(recovered.version, 3);
        assert_eq!(
            recovered.moonlight_preferences.width,
            PersistedAppState::default().moonlight_preferences.width
        );
        assert_eq!(recovered.moonlight_preferences.height, 1664);
        assert_eq!(
            recovered.server_preferences.storage_gb,
            PersistedAppState::default().server_preferences.storage_gb
        );
        assert_eq!(recovered.server_preferences.template_hash, "abc123");
    }

    fn read_root(path: &std::path::Path) -> serde_json::Value {
        serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
    }

    fn state_with_secrets() -> PersistedAppState {
        let mut state = PersistedAppState::default();
        state.credentials.app_username = "felipe".to_string();
        state.credentials.app_password = "app-secret".to_string();
        state.credentials.vast_api_key = "vast-secret".to_string();
        state.credentials.twitch_client_secret = "twitch-secret".to_string();
        state.credentials.tensordock_api_key = "tensordock-secret".to_string();
        state.shared_storage.settings.backblaze_application_key = "b2-secret".to_string();
        state.shared_storage.settings.crypt_password = Some("crypt-secret".to_string());
        state
    }

    fn assert_no_secret_in_file(path: &std::path::Path) {
        let raw = std::fs::read_to_string(path).unwrap();
        for secret in [
            "app-secret",
            "vast-secret",
            "twitch-secret",
            "tensordock-secret",
            "b2-secret",
            "crypt-secret",
        ] {
            assert!(!raw.contains(secret), "{secret} leaked into state.json");
        }
    }

    #[tokio::test]
    async fn secrets_are_kept_out_of_state_json_and_hydrated_from_keyring() {
        let path = temp_state_path("secrets-roundtrip");
        let _ = std::fs::remove_file(&path);
        let backend = Arc::new(InMemorySecretBackend::default());
        let store = JsonStateStore::with_secret_backend(path.clone(), 3, backend.clone());
        store.load_state().await.unwrap();

        let state = state_with_secrets();
        // In-memory serialization (sent to the frontend) still carries values.
        let in_memory = serde_json::to_value(&state).unwrap();
        assert_eq!(in_memory["credentials"]["vastApiKey"], "vast-secret");

        store.save_state(&state).await.unwrap();
        assert_no_secret_in_file(&path);
        let root = read_root(&path);
        assert_eq!(root["credentials"]["appUsername"], "felipe");
        assert_eq!(root["credentials"]["vastApiKey"], "");
        assert!(root["sharedStorage"]["settings"]["cryptPassword"].is_null());
        assert_eq!(root["secretStorage"]["credentials.vastApiKey"], "keyring");
        assert_eq!(
            backend
                .snapshot()
                .get("credentials.appPassword")
                .map(String::as_str),
            Some("app-secret")
        );

        // A fresh store (new app start) hydrates from the keyring.
        let reopened = JsonStateStore::with_secret_backend(path.clone(), 3, backend.clone());
        let loaded = reopened.load_state().await.unwrap();
        assert_eq!(loaded.credentials.app_password, "app-secret");
        assert_eq!(loaded.credentials.vast_api_key, "vast-secret");
        assert_eq!(loaded.credentials.twitch_client_secret, "twitch-secret");
        assert_eq!(loaded.credentials.tensordock_api_key, "tensordock-secret");
        assert_eq!(
            loaded.shared_storage.settings.backblaze_application_key,
            "b2-secret"
        );
        assert_eq!(
            loaded.shared_storage.settings.crypt_password.as_deref(),
            Some("crypt-secret")
        );
        assert_no_secret_in_file(&path);

        // Clearing a secret removes it from the keyring and the marker.
        let mut cleared = loaded.clone();
        cleared.credentials.vast_api_key.clear();
        reopened.save_state(&cleared).await.unwrap();
        assert!(!backend.snapshot().contains_key("credentials.vastApiKey"));
        assert!(read_root(&path)["secretStorage"]
            .get("credentials.vastApiKey")
            .is_none());
    }

    #[tokio::test]
    async fn plaintext_secrets_are_migrated_into_keyring_on_load() {
        let path = temp_state_path("secrets-migrate");
        fs::write(
            &path,
            serde_json::to_vec(&json!({
                "version": 3,
                "onboardingCompleted": true,
                "credentials": {
                    "appUsername": "felipe",
                    "appPassword": "app-secret",
                    "vastApiKey": "vast-secret"
                }
            }))
            .unwrap(),
        )
        .await
        .unwrap();

        let backend = Arc::new(InMemorySecretBackend::default());
        let store = JsonStateStore::with_secret_backend(path.clone(), 3, backend.clone());
        let loaded = store.load_state().await.unwrap();

        assert_eq!(loaded.credentials.app_password, "app-secret");
        assert_eq!(loaded.credentials.vast_api_key, "vast-secret");
        assert_no_secret_in_file(&path);
        let stored = backend.snapshot();
        assert_eq!(
            stored.get("credentials.vastApiKey").map(String::as_str),
            Some("vast-secret")
        );
        assert_eq!(
            stored.get("credentials.appPassword").map(String::as_str),
            Some("app-secret")
        );
    }

    #[tokio::test]
    async fn unavailable_keyring_falls_back_to_plaintext() {
        let path = temp_state_path("secrets-fallback");
        let _ = std::fs::remove_file(&path);
        let backend = Arc::new(InMemorySecretBackend::unavailable());
        let store = JsonStateStore::with_secret_backend(path.clone(), 3, backend);
        store.load_state().await.unwrap();
        store.save_state(&state_with_secrets()).await.unwrap();

        let root = read_root(&path);
        assert_eq!(root["credentials"]["vastApiKey"], "vast-secret");
        assert!(root.get("secretStorage").is_none());

        let reopened = JsonStateStore::with_secret_backend(
            path.clone(),
            3,
            Arc::new(InMemorySecretBackend::unavailable()),
        );
        let loaded = reopened.load_state().await.unwrap();
        assert_eq!(loaded.credentials.vast_api_key, "vast-secret");
    }

    #[tokio::test]
    async fn unreadable_keyring_does_not_drop_secret_marker() {
        let path = temp_state_path("secrets-unreadable");
        let _ = std::fs::remove_file(&path);
        let backend = Arc::new(InMemorySecretBackend::default());
        let store = JsonStateStore::with_secret_backend(path.clone(), 3, backend.clone());
        store.load_state().await.unwrap();
        store.save_state(&state_with_secrets()).await.unwrap();

        // Next start: keyring temporarily unavailable.
        let broken = JsonStateStore::with_secret_backend(
            path.clone(),
            3,
            Arc::new(InMemorySecretBackend::unavailable()),
        );
        let loaded = broken.load_state().await.unwrap();
        assert!(loaded.credentials.vast_api_key.is_empty());
        broken.save_state(&loaded).await.unwrap();
        assert_eq!(
            read_root(&path)["secretStorage"]["credentials.vastApiKey"],
            "keyring"
        );

        // Keyring back: secret is recovered.
        let healed = JsonStateStore::with_secret_backend(path.clone(), 3, backend);
        let loaded = healed.load_state().await.unwrap();
        assert_eq!(loaded.credentials.vast_api_key, "vast-secret");
    }

    #[tokio::test]
    async fn save_state_preserves_moonlight_category() {
        let path = temp_state_path("preserve-moonlight");
        fs::write(
            &path,
            serde_json::to_vec_pretty(&json!({
                "version": 1,
                "moonligConf": {
                    "schemaVersion": 1,
                    "hosts": {
                        "instance-1": {
                            "hostId": "instance-1"
                        }
                    }
                }
            }))
            .unwrap(),
        )
        .await
        .unwrap();

        let store = JsonStateStore::new(path.clone(), 1);
        let mut state = PersistedAppState::default();
        state.onboarding_completed = true;
        store.save_state(&state).await.unwrap();

        let root: serde_json::Value =
            serde_json::from_slice(&fs::read(&path).await.unwrap()).unwrap();
        assert_eq!(
            root.get("moonligConf")
                .and_then(|value| value.get("hosts"))
                .and_then(|value| value.get("instance-1"))
                .and_then(|value| value.get("hostId"))
                .and_then(|value| value.as_str()),
            Some("instance-1")
        );
        assert_eq!(
            root.get("onboardingCompleted")
                .and_then(|value| value.as_bool()),
            Some(true)
        );
    }
}
