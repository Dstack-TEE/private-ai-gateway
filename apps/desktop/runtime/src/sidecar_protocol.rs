//! JSON-lines contract between `private-ai-proxy serve` and the persistent
//! desktop backend.

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct IdentityEvent {
    pub trust_level: String,
    pub tee_type: String,
    pub keyset_digest: String,
    pub keyset_not_after: u64,
    pub tls_spki: Option<String>,
    pub source_provenance: SourceProvenance,
    pub service_capabilities: ServiceCapabilities,
    pub verification: Value,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct SourceProvenance {
    pub repo_url: Option<String>,
    pub repo_commit: Option<String>,
    pub image_digest: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ServiceCapabilities {
    pub serving: String,
    pub supported_e2ee_versions: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RequestCompleteEvent {
    pub method: String,
    pub path: String,
    pub status: u16,
    pub streamed: bool,
    pub receipt_id: Option<String>,
    pub verified: Option<bool>,
    pub detail: String,
    pub tag: Option<String>,
    pub rewritten: Option<bool>,
    pub local_policy_applied: Option<bool>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServeEvent {
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
        #[serde(skip_serializing_if = "Option::is_none")]
        code: Option<String>,
        reason: String,
    },
    Fatal {
        message: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn identity() -> IdentityEvent {
        IdentityEvent {
            trust_level: "hardware_verified".to_string(),
            tee_type: "tdx".to_string(),
            keyset_digest: "sha256:keyset".to_string(),
            keyset_not_after: 42,
            tls_spki: Some("sha256:spki".to_string()),
            source_provenance: SourceProvenance::default(),
            service_capabilities: ServiceCapabilities {
                serving: "aggregator".to_string(),
                supported_e2ee_versions: vec!["2".to_string()],
            },
            verification: json!({ "checks": [] }),
        }
    }

    #[test]
    fn events_round_trip_through_the_shared_contract() {
        let events = [
            ServeEvent::Ready {
                identity: identity(),
                remote_url: "https://tee.example".to_string(),
                proxy_url: "http://127.0.0.1:4181".to_string(),
                control_url: "http://127.0.0.1:4182".to_string(),
                policy: json!({ "enforce_verified": true }),
            },
            ServeEvent::IdentityUpdated {
                identity: identity(),
            },
            ServeEvent::RequestComplete {
                request: RequestCompleteEvent {
                    method: "POST".to_string(),
                    path: "/v1/responses".to_string(),
                    status: 200,
                    streamed: true,
                    receipt_id: Some("receipt-1".to_string()),
                    verified: Some(true),
                    detail: "receipt verified".to_string(),
                    tag: Some("pap:req:session:codex".to_string()),
                    rewritten: Some(false),
                    local_policy_applied: Some(true),
                },
            },
        ];

        for event in events {
            let value = serde_json::to_value(&event).unwrap();
            let decoded: ServeEvent = serde_json::from_value(value.clone()).unwrap();
            assert_eq!(serde_json::to_value(decoded).unwrap(), value);
        }
    }
}
