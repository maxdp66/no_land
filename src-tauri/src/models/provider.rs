//! GPU cloud provider identity.
//!
//! The app was built around Vast.ai, whose instance and offer ids are small
//! integers, and those `u64` ids are used as keys throughout the persisted
//! state, the commands and the frontend. Other providers use string ids
//! (TensorDock and Shadeform use UUIDs). Rather than re-keying everything, each foreign id
//! is mapped to a stable *local* id in a reserved range that Vast ids never
//! reach, and the mapping is persisted so the provider's own id can be
//! recovered for API calls.

use serde::{Deserialize, Serialize};

/// Local ids at or above this value belong to a non-Vast provider. Vast
/// contract ids are far below 2^52, and every local id stays below 2^53 so
/// it is exactly representable as a JavaScript number.
pub const FOREIGN_LOCAL_ID_BASE: u64 = 1 << 52;
const FOREIGN_LOCAL_ID_MASK: u64 = (1 << 51) - 1;

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "lowercase")]
pub enum CloudProviderKind {
    #[default]
    Vast,
    Tensordock,
    Shadeform,
}

impl CloudProviderKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Vast => "vast",
            Self::Tensordock => "tensordock",
            Self::Shadeform => "shadeform",
        }
    }

    pub fn display_name(self) -> &'static str {
        match self {
            Self::Vast => "Vast.ai",
            Self::Tensordock => "TensorDock",
            Self::Shadeform => "Shadeform",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "" | "vast" | "vast.ai" | "vastai" => Some(Self::Vast),
            "tensordock" => Some(Self::Tensordock),
            "shadeform" => Some(Self::Shadeform),
            _ => None,
        }
    }
}

pub fn default_provider_name() -> String {
    CloudProviderKind::Vast.as_str().to_string()
}

/// Stable local id for a provider-scoped string id (FNV-1a, 64-bit).
pub fn foreign_local_id(provider: CloudProviderKind, remote_id: &str) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in provider
        .as_str()
        .bytes()
        .chain(std::iter::once(b':'))
        .chain(remote_id.trim().bytes())
    {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    FOREIGN_LOCAL_ID_BASE | (hash & FOREIGN_LOCAL_ID_MASK)
}

pub fn is_foreign_local_id(id: u64) -> bool {
    id >= FOREIGN_LOCAL_ID_BASE
}

/// Persisted link between a local instance id and the provider's own id.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ProviderInstanceRef {
    pub local_id: u64,
    pub provider: CloudProviderKind,
    pub remote_id: String,
}

/// Which provider owns an instance, and its provider-side id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedInstance {
    pub provider: CloudProviderKind,
    pub remote_id: String,
}

pub fn resolve_instance(refs: &[ProviderInstanceRef], local_id: u64) -> Option<ResolvedInstance> {
    if !is_foreign_local_id(local_id) {
        return Some(ResolvedInstance {
            provider: CloudProviderKind::Vast,
            remote_id: local_id.to_string(),
        });
    }
    refs.iter()
        .find(|reference| reference.local_id == local_id)
        .map(|reference| ResolvedInstance {
            provider: reference.provider,
            remote_id: reference.remote_id.clone(),
        })
}

/// Record (or refresh) a mapping. Returns `true` when the list changed.
pub fn remember_instance_ref(
    refs: &mut Vec<ProviderInstanceRef>,
    provider: CloudProviderKind,
    remote_id: &str,
) -> bool {
    if provider == CloudProviderKind::Vast {
        return false;
    }
    let local_id = foreign_local_id(provider, remote_id);
    if refs
        .iter()
        .any(|reference| reference.local_id == local_id && reference.remote_id == remote_id)
    {
        return false;
    }
    refs.retain(|reference| reference.local_id != local_id);
    refs.push(ProviderInstanceRef {
        local_id,
        provider,
        remote_id: remote_id.to_string(),
    });
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn foreign_ids_are_stable_js_safe_and_outside_vast_range() {
        let uuid = "6b7e8f0a-1c2d-4e5f-9a8b-7c6d5e4f3a2b";
        let id = foreign_local_id(CloudProviderKind::Tensordock, uuid);
        assert_eq!(id, foreign_local_id(CloudProviderKind::Tensordock, uuid));
        assert!(is_foreign_local_id(id));
        assert!(id < (1 << 53), "must be exactly representable in JS");
        assert_ne!(
            id,
            foreign_local_id(CloudProviderKind::Tensordock, "another-uuid")
        );
        assert!(!is_foreign_local_id(28_812_345));
    }

    #[test]
    fn resolve_falls_back_to_vast_for_small_ids() {
        let resolved = resolve_instance(&[], 883).unwrap();
        assert_eq!(resolved.provider, CloudProviderKind::Vast);
        assert_eq!(resolved.remote_id, "883");
    }

    #[test]
    fn remembered_foreign_refs_resolve() {
        let mut refs = Vec::new();
        assert!(remember_instance_ref(&mut refs, CloudProviderKind::Tensordock, "abc"));
        assert!(!remember_instance_ref(&mut refs, CloudProviderKind::Tensordock, "abc"));
        assert!(!remember_instance_ref(&mut refs, CloudProviderKind::Vast, "1"));
        let local = foreign_local_id(CloudProviderKind::Tensordock, "abc");
        let resolved = resolve_instance(&refs, local).unwrap();
        assert_eq!(resolved.provider, CloudProviderKind::Tensordock);
        assert_eq!(resolved.remote_id, "abc");
        assert!(resolve_instance(&refs, local + 1).is_none());
    }

    #[test]
    fn provider_names_parse() {
        assert_eq!(CloudProviderKind::parse(""), Some(CloudProviderKind::Vast));
        assert_eq!(
            CloudProviderKind::parse("TensorDock"),
            Some(CloudProviderKind::Tensordock)
        );
        assert_eq!(CloudProviderKind::parse("runpod"), None);
        assert_eq!(
            serde_json::to_string(&CloudProviderKind::Tensordock).unwrap(),
            "\"tensordock\""
        );
    }
}
