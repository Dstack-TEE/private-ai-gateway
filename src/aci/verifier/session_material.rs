//! Reviewed session-material shapes. Unknown profile claims are preserved by
//! the caller; the TDX projection has an exact, versioned field set.

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;

use super::quote::VerifiedQuote;
use crate::aci::digest;

/// Fixed-size lowercase hex in the reviewed material wire representation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Hex<const N: usize>(pub [u8; N]);

impl<const N: usize> Serialize for Hex<N> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&hex::encode(self.0))
    }
}

impl<'de, const N: usize> Deserialize<'de> for Hex<N> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        Self::parse(&value)
            .ok_or_else(|| serde::de::Error::custom("expected fixed-size lowercase hex"))
    }
}

impl<const N: usize> Hex<N> {
    pub(super) fn parse(value: &str) -> Option<Self> {
        let mut bytes = [0; N];
        if !lower_hex(value) || hex::decode_to_slice(value, &mut bytes).is_err() {
            return None;
        }
        Some(Self(bytes))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum TdxReportFormat {
    Td10,
    Td15,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct TdxReportMaterial {
    pub report_format: TdxReportFormat,
    pub tee_tcb_svn: Hex<16>,
    pub mr_seam: Hex<48>,
    pub mr_signer_seam: Hex<48>,
    pub seam_attributes: Hex<8>,
    pub td_attributes: Hex<8>,
    pub xfam: Hex<8>,
    pub mr_td: Hex<48>,
    pub mr_config_id: Hex<48>,
    pub mr_owner: Hex<48>,
    pub mr_owner_config: Hex<48>,
    pub rt_mr0: Hex<48>,
    pub rt_mr1: Hex<48>,
    pub rt_mr2: Hex<48>,
    pub rt_mr3: Hex<48>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present"
    )]
    pub tee_tcb_svn2: Option<Hex<16>>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present"
    )]
    pub mr_service_td: Option<Hex<48>>,
}

// Optional TD15 fields may be absent, but explicit null is not a report field.
fn present<'de, D: Deserializer<'de>, T: Deserialize<'de>>(d: D) -> Result<Option<T>, D::Error> {
    T::deserialize(d).map(Some)
}

impl TdxReportMaterial {
    pub(super) fn from_report(report: &dcap_qvl::quote::Report) -> Option<Self> {
        use dcap_qvl::quote::Report;
        let base = match report {
            Report::TD10(base) => base,
            Report::TD15(report) => &report.base,
            _ => return None,
        };
        let (report_format, tee_tcb_svn2, mr_service_td) = match report {
            Report::TD15(report) => (
                TdxReportFormat::Td15,
                Some(Hex(report.tee_tcb_svn2)),
                Some(Hex(report.mr_service_td)),
            ),
            _ => (TdxReportFormat::Td10, None, None),
        };
        Some(Self {
            report_format,
            tee_tcb_svn: Hex(base.tee_tcb_svn),
            mr_seam: Hex(base.mr_seam),
            mr_signer_seam: Hex(base.mr_signer_seam),
            seam_attributes: Hex(base.seam_attributes),
            td_attributes: Hex(base.td_attributes),
            xfam: Hex(base.xfam),
            mr_td: Hex(base.mr_td),
            mr_config_id: Hex(base.mr_config_id),
            mr_owner: Hex(base.mr_owner),
            mr_owner_config: Hex(base.mr_owner_config),
            rt_mr0: Hex(base.rt_mr0),
            rt_mr1: Hex(base.rt_mr1),
            rt_mr2: Hex(base.rt_mr2),
            rt_mr3: Hex(base.rt_mr3),
            tee_tcb_svn2,
            mr_service_td,
        })
    }

    fn valid(&self) -> bool {
        match self.report_format {
            TdxReportFormat::Td10 => self.tee_tcb_svn2.is_none() && self.mr_service_td.is_none(),
            TdxReportFormat::Td15 => self.tee_tcb_svn2.is_some() && self.mr_service_td.is_some(),
        }
    }

    pub(super) fn digest(&self) -> Option<String> {
        let value = serde_json::to_value(self).ok()?;
        Some(digest::sha256_hex(&digest::jcs_bytes(&value).ok()?))
    }
}

#[derive(Serialize, Deserialize)]
pub(crate) struct TdxMaterial {
    #[serde(deserialize_with = "object")]
    pub tdx_report: TdxReportMaterial,
    pub tdx_report_sha256: String,
}

impl TdxMaterial {
    pub(super) fn from_quote(quote: &VerifiedQuote) -> Option<Self> {
        let tdx_report = TdxReportMaterial::from_report(&quote.report)?;
        let tdx_report_sha256 = tdx_report.digest()?;
        Some(Self {
            tdx_report,
            tdx_report_sha256,
        })
    }

    fn valid(&self) -> bool {
        self.tdx_report.valid()
            && self.tdx_report.digest().as_ref() == Some(&self.tdx_report_sha256)
    }
}

// Serde also accepts struct fields as sequences; these JSON shapes require objects.
fn object<'de, D: Deserializer<'de>, T: serde::de::DeserializeOwned>(d: D) -> Result<T, D::Error> {
    let fields = serde_json::Map::<String, Value>::deserialize(d)?;
    T::deserialize(Value::Object(fields)).map_err(serde::de::Error::custom)
}

// Unlike Option<T>, this requires the field even when its value is null.
#[derive(Serialize, Deserialize)]
#[serde(untagged)]
enum Nullable<T> {
    Value(T),
    Null,
}

#[derive(Serialize, Deserialize)]
pub(crate) struct AciServiceMaterial {
    pub session_material_version: String,
    #[serde(flatten)]
    pub tdx: TdxMaterial,
    pub tcb_status: String,
    pub advisory_ids: Vec<String>,
    pub measured_compose_hash: Hex<32>,
    pub measured_app_id: String,
    pub published_source_provenance: serde_json::Map<String, Value>,
    #[serde(deserialize_with = "object")]
    pub receipt_key_custody: ReceiptCustody,
}

#[derive(Serialize, Deserialize)]
pub(crate) struct ReceiptCustody {
    pub provider: String,
    pub purpose: String,
    pub receipt_public_key: Hex<32>,
    pub kms_public_key: Hex<33>,
    pub app_public_key: Hex<33>,
    pub root_public_key: Hex<33>,
}

impl ReceiptCustody {
    pub(super) fn from_verified(
        custody: &aci_verify::dstack::VerifiedReceiptCustody,
    ) -> Option<Self> {
        Some(Self {
            provider: custody.provider.clone(),
            purpose: custody.purpose.clone(),
            receipt_public_key: Hex::parse(&custody.receipt_public_key)?,
            kms_public_key: Hex::parse(&custody.kms_public_key)?,
            app_public_key: Hex::parse(&custody.app_public_key)?,
            root_public_key: Hex::parse(&custody.root_public_key)?,
        })
    }

    fn valid(&self) -> bool {
        self.provider == "dstack-kms"
            && self.purpose == "aci.receipt.ed25519.v1"
            && [
                &self.kms_public_key,
                &self.app_public_key,
                &self.root_public_key,
            ]
            .iter()
            .all(|key| matches!(key.0[0], 2 | 3))
    }
}

#[derive(Serialize, Deserialize)]
struct NearAiMaterial {
    #[serde(flatten)]
    tdx: TdxMaterial,
    trust_boundary: String,
    signing_address: String,
    compose_hash: Hex<32>,
    gateway_tls_spki_sha256: Hex<32>,
    gateway_verified: bool,
    tcb_status: Nullable<String>,
    #[serde(deserialize_with = "object")]
    gateway_gpu_check: GatewayGpuCheck,
}

#[derive(Serialize, Deserialize)]
struct GatewayGpuCheck {
    present: bool,
    accepted: bool,
}

#[derive(Serialize, Deserialize)]
struct PhalaDirectMaterial {
    #[serde(flatten)]
    tdx: TdxMaterial,
    trust_boundary: String,
    evidence_scope: String,
    canonical_model_id: String,
    signing_address: String,
    compose_hash: Hex<32>,
    attestation_version: u64,
    tls_spki_from_report_data: bool,
    report_data_nonce_matched: bool,
    compose_hash_verified: bool,
    tdx_debug_mode: bool,
    os_image_hash: Nullable<String>,
    os_image_version: Nullable<String>,
    gpu_arch: Nullable<String>,
    tcb_status: Nullable<String>,
    os_image_is_dev: Nullable<bool>,
    production_os_image: Nullable<bool>,
    gpu_evidence_nonce_matched: Nullable<bool>,
    gpu_verified: bool,
    gpu_evidence_present: bool,
}

#[derive(Serialize, Deserialize)]
struct ChutesMaterial {
    #[serde(flatten)]
    tdx: TdxMaterial,
    trust_boundary: String,
    evidence_scope: String,
    chute_id: String,
    canonical_model_id: String,
    instance_id: String,
    measurement: String,
    tcb_status: Nullable<String>,
    advisory_ids: Vec<String>,
    gpu_verified: bool,
    gpu_evidence_present: bool,
    gpu_arch: Nullable<String>,
    gpu_evidence_nonce_matched: Nullable<bool>,
}

#[derive(Serialize, Deserialize)]
struct PrivatemodeMaterial {
    trust_boundary: String,
    attestation_scope: String,
    request_encryption: String,
    success_response_authentication: String,
    inference_secret_policy: String,
    manifest_mode: String,
    manifest_observation: String,
    observed_manifest_sha256: Hex<32>,
    manifest_observed_at: String,
    manifest_bound_to_active_secret: bool,
}

fn nonempty(values: &[&str]) -> bool {
    values.iter().all(|s| !s.is_empty())
}

fn sorted_unique(values: &[String]) -> bool {
    values.windows(2).all(|pair| pair[0] < pair[1])
}

fn lower_hex(value: &str) -> bool {
    !value.is_empty()
        && value.len().is_multiple_of(2)
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn signing_address(value: &str) -> bool {
    let value = value.strip_prefix("0x").unwrap_or(value);
    value.len() == 40 && value.bytes().all(|b| b.is_ascii_hexdigit())
}

fn parse<T: serde::de::DeserializeOwned>(claims: &Value, valid: impl FnOnce(T) -> bool) -> bool {
    T::deserialize(claims).is_ok_and(valid)
}

pub(crate) fn valid_profile(provider: &str, claims: &Value) -> bool {
    match provider {
        "aci-service" => parse(claims, |m: AciServiceMaterial| {
            let p = &m.published_source_provenance;
            m.tdx.valid()
                && !m.tcb_status.is_empty()
                && sorted_unique(&m.advisory_ids)
                && lower_hex(&m.measured_app_id)
                && m.receipt_key_custody.valid()
                && (p.get("image_digest").is_some_and(Value::is_string)
                    || (p.get("repo_url").is_some_and(Value::is_string)
                        && p.get("repo_commit").is_some_and(Value::is_string)))
        }),
        "near-ai" => parse(claims, |m: NearAiMaterial| {
            m.tdx.valid()
                && !m.trust_boundary.is_empty()
                && signing_address(&m.signing_address)
                && m.gateway_verified
        }),
        "phala-direct" => parse(claims, |m: PhalaDirectMaterial| {
            m.tdx.valid()
                && nonempty(&[&m.trust_boundary, &m.evidence_scope, &m.canonical_model_id])
                && signing_address(&m.signing_address)
                && m.attestation_version == 2
                && m.tls_spki_from_report_data
                && m.report_data_nonce_matched
                && m.compose_hash_verified
                && !m.tdx_debug_mode
        }),
        "chutes" => parse(claims, |m: ChutesMaterial| {
            m.tdx.valid()
                && nonempty(&[
                    &m.trust_boundary,
                    &m.evidence_scope,
                    &m.chute_id,
                    &m.canonical_model_id,
                    &m.instance_id,
                    &m.measurement,
                ])
                && sorted_unique(&m.advisory_ids)
        }),
        "privatemode" => parse(claims, |m: PrivatemodeMaterial| {
            nonempty(&[
                &m.trust_boundary,
                &m.attestation_scope,
                &m.request_encryption,
                &m.success_response_authentication,
                &m.inference_secret_policy,
                &m.manifest_mode,
                &m.manifest_observation,
                &m.manifest_observed_at,
            ]) && !m.manifest_bound_to_active_secret
        }),
        _ => false,
    }
}
