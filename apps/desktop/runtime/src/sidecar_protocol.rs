//! Versioned JSON-lines contract between `private-ai-proxy serve` and the
//! persistent desktop backend.

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const EVENT_SCHEMA_VERSION: u64 = 1;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct IdentityEvent {
    pub trust_level: String,
    pub tee_type: String,
    pub keyset_digest: String,
    pub keyset_not_after: u64,
    #[serde(default)]
    pub tls_spki: Option<String>,
    #[serde(default)]
    pub source_provenance: SourceProvenance,
    #[serde(default)]
    pub service_capabilities: ServiceCapabilities,
    pub verification: Value,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct SourceProvenance {
    #[serde(default)]
    pub repo_url: Option<String>,
    #[serde(default)]
    pub repo_commit: Option<String>,
    #[serde(default)]
    pub image_digest: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ServiceCapabilities {
    #[serde(default)]
    pub serving: String,
    #[serde(default)]
    pub supported_e2ee_versions: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RequestCompleteEvent {
    pub method: String,
    pub path: String,
    pub status: u16,
    #[serde(default)]
    pub streamed: bool,
    #[serde(default)]
    pub receipt_id: Option<String>,
    #[serde(default)]
    pub verified: Option<bool>,
    #[serde(default)]
    pub detail: String,
    #[serde(default)]
    pub tag: Option<String>,
    #[serde(default)]
    pub rewritten: Option<bool>,
    #[serde(default)]
    pub local_policy_applied: Option<bool>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServeEventKind {
    Ready {
        #[serde(flatten)]
        identity: IdentityEvent,
        remote_url: String,
        proxy_url: String,
        control_url: String,
        policy: Value,
    },
    IdentityUpdated {
        #[serde(flatten)]
        identity: IdentityEvent,
    },
    RequestComplete {
        #[serde(flatten)]
        request: RequestCompleteEvent,
    },
    Blocked {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        code: Option<String>,
        reason: String,
    },
    Fatal {
        message: String,
    },
    #[serde(other)]
    Unknown,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ServeEvent {
    pub schema_version: u64,
    #[serde(flatten)]
    pub kind: ServeEventKind,
}

impl ServeEvent {
    pub fn new(kind: ServeEventKind) -> Self {
        Self {
            schema_version: EVENT_SCHEMA_VERSION,
            kind,
        }
    }
}
