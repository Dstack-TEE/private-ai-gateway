use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceProvenance {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repo_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repo_commit: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub image_digest: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GatewayIdentity {
    pub tee_type: String,
    pub trust_level: String,
    pub keyset_digest: String,
    pub keyset_not_after: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tls_spki: Option<String>,
    pub source: SourceProvenance,
    pub serving: String,
    pub supported_e2ee_versions: Vec<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VerificationCheck {
    pub id: String,
    pub section: String,
    pub title: String,
    pub status: String,
    pub detail: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RequestActivity {
    pub method: String,
    pub path: String,
    pub status: u16,
    pub streamed: bool,
    pub verified: Option<bool>,
    pub detail: String,
    pub at: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReceiptSummary {
    pub receipt_id: String,
    pub path: String,
    pub status: u16,
    pub streamed: bool,
    pub truncated: bool,
    pub at: u64,
    pub verified: Option<bool>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GatewayState {
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remote_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub proxy_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub control_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub identity: Option<GatewayIdentity>,
    pub checks: Vec<VerificationCheck>,
    pub activity: Vec<RequestActivity>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl Default for GatewayState {
    fn default() -> Self {
        Self {
            status: "stopped".to_string(),
            remote_url: None,
            proxy_url: None,
            control_url: None,
            identity: None,
            checks: Vec::new(),
            activity: Vec::new(),
            error: None,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StartGatewayConfig {
    pub remote_url: String,
    pub require_production_os: bool,
}

#[derive(Debug, Deserialize)]
pub struct RawReceiptSummary {
    pub receipt_id: String,
    pub path: String,
    pub status: u16,
    pub streamed: bool,
    pub truncated: bool,
    pub at: u64,
    pub verified: Option<bool>,
}

impl From<RawReceiptSummary> for ReceiptSummary {
    fn from(value: RawReceiptSummary) -> Self {
        Self {
            receipt_id: value.receipt_id,
            path: value.path,
            status: value.status,
            streamed: value.streamed,
            truncated: value.truncated,
            at: value.at,
            verified: value.verified,
        }
    }
}
