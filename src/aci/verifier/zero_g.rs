//! Verifier for 0G Private Computer providers, run before anything is sealed.
//!
//! The 0G router names candidates; nothing it says is trusted. For each
//! private-tier candidate the verifier reads the provider's on-chain service
//! record from an operator-chosen RPC, fetches the provider broker's TDX quote
//! directly, verifies it to Intel's root, requires an allowlisted dstack boot
//! chain, and requires the quote's `report_data` signer to be the on-chain
//! acknowledged `teeSignerAddress`. A candidate whose record declares
//! `TargetSeparated` (the broker forwards plaintext to a separate target) is
//! refused. Each provider that passes yields one enforced
//! `e2ee_public_key_sha256` binding over the encryption key its quote binds.

use std::collections::HashMap;
use std::sync::RwLock;
use std::time::Duration;

use async_trait::async_trait;
use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use dcap_qvl::quote::Report;
use serde::Deserialize;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use sha3::Keccak256;
use tokio::time::Instant;

use super::quote::{parse_quote_evidence, verify_quote_to_root};
use super::{current_unix_secs, CachedProviderEvent};
use crate::aci::digest;
use crate::aci::receipt::{ChannelBinding, UpstreamVerifiedEvent, VerificationResult};
use crate::aci::upstream::{
    broker_v1_base, parse_report_data, read_limited, zero_g_client, VerifiedProvider,
    TRUST_MODE_HEADER, TRUST_MODE_PRIVATE, ZERO_G_E2EE_ALGORITHM, ZERO_G_PROVIDER,
};
use crate::aggregator::service::{UpstreamVerificationRequest, UpstreamVerifier};

const VERIFIER_ID: &str = "0g-private-computer/native-e2ee/v1";
const SERVICE_TYPE: &str = "chatbot";
const MAX_CANDIDATES: usize = 8;
const MAX_CONTROL_BODY_BYTES: usize = 1 << 20;
const MAX_QUOTE_BODY_BYTES: usize = 4 << 20;

/// One allowlisted dstack OS image, identified by its boot chain: MRTD
/// (firmware), RTMR1 (kernel) and RTMR2 (cmdline, which pins the rootfs
/// dm-verity hash, and initrd). RTMR0 (VM shape) and RTMR3 (the app) vary per
/// deployment and are excluded.
struct BootChain {
    image: &'static str,
    mrtd: &'static str,
    rtmr1: &'static str,
    rtmr2: &'static str,
}

/// The production dstack images 0G providers run. Values are 0G's provider
/// allowlist (`client/evidence/brokerimages.json` at 0g-pc-e2ee
/// release-2026.10.19.2), which records for each entry the published release,
/// its `os_image_hash`, and a dstack-mr derivation from that artifact rather
/// than from a running provider. dstack-nvidia-0.5.9 is the same production
/// image (`os_image_hash` 806a352e…) PAG already accepts for Phala hosts.
const ALLOWED_BOOT_CHAINS: &[BootChain] = &[
    BootChain {
        image: "dstack-nvidia-0.5.9",
        mrtd: "b24d3b24e9e3c16012376b52362ca09856c4adecb709d5fac33addf1c47e193da075b125b6c364115771390a5461e217",
        rtmr1: "07e6f51aa763abfe75c3ddfbf4f425fe3f0ceff66d807a75e049303dce9addf68e7218729bd419638af63a370f65878c",
        rtmr2: "f1c82667a354194467cd8419efd14a714560dd9b85b4c13b25c11e44bf4e126248c2255fad58c303fb0ca2921765d53a",
    },
    BootChain {
        image: "dstack-nvidia-0.5.4.1",
        mrtd: "b24d3b24e9e3c16012376b52362ca09856c4adecb709d5fac33addf1c47e193da075b125b6c364115771390a5461e217",
        rtmr1: "6e1afb7464ed0b941e8f5bf5b725cf1df9425e8105e3348dca52502f27c453f3018a28b90749cf05199d5a17820101a7",
        rtmr2: "89e73cedf48f976ffebe8ac1129790ff59a0f52d54d969cb73455b1a79793f1dc16edc3b1fccc0fd65ea5905774bbd57",
    },
];

#[derive(Debug, Deserialize)]
struct PreviewResponse {
    #[serde(default)]
    providers: Vec<PreviewCandidate>,
}

#[derive(Debug, Clone, Deserialize)]
pub(super) struct PreviewCandidate {
    pub address: String,
    #[serde(default)]
    pub provider_identity: Option<String>,
    #[serde(default)]
    pub canonical_id: Option<String>,
    #[serde(default)]
    pub model_id: Option<String>,
}

/// The fields of the InferenceServing `Service` struct this verifier reads.
struct ServiceRecord {
    url: String,
    model: String,
    verifiability: String,
    additional_info: String,
    tee_signer: String,
    acknowledged: bool,
}

/// A candidate that passed every check.
pub(super) struct VerifiedCandidate {
    pub provider: VerifiedProvider,
    pub claims: Map<String, Value>,
    pub evidence: Value,
}

pub struct ZeroGProviderVerifier {
    router_base: String,
    api_key: Option<String>,
    chain_rpc_url: String,
    serving_contract: String,
    pccs_url: String,
    client: reqwest::Client,
    cache_ttl_seconds: u64,
    cache: RwLock<HashMap<String, CachedProviderEvent>>,
    verify_lock: tokio::sync::Mutex<()>,
    #[cfg(test)]
    quote_status: Option<QuoteStatusCheck>,
}

/// Test stand-in for DCAP-to-root verification: raw quote to TCB status.
#[cfg(test)]
type QuoteStatusCheck = fn(&[u8]) -> Result<String, String>;

impl ZeroGProviderVerifier {
    pub fn new(
        router_base: impl Into<String>,
        chain_rpc_url: impl Into<String>,
        serving_contract: impl Into<String>,
        pccs_url: Option<String>,
        connect_timeout_seconds: u64,
        request_timeout_seconds: u64,
        cache_ttl_seconds: u64,
    ) -> Result<Self, String> {
        let client = zero_g_client(connect_timeout_seconds)
            .timeout(Duration::from_secs(request_timeout_seconds))
            .build()
            .map_err(|e| e.to_string())?;
        Ok(Self {
            router_base: router_base.into().trim().trim_end_matches('/').to_string(),
            api_key: None,
            chain_rpc_url: chain_rpc_url.into(),
            serving_contract: serving_contract.into(),
            pccs_url: pccs_url.unwrap_or_else(|| dcap_qvl::PHALA_PCCS_URL.to_string()),
            client,
            cache_ttl_seconds,
            cache: RwLock::new(HashMap::new()),
            verify_lock: tokio::sync::Mutex::new(()),
            #[cfg(test)]
            quote_status: None,
        })
    }

    pub fn with_api_key(mut self, api_key: impl Into<String>) -> Self {
        self.api_key = Some(api_key.into());
        self
    }

    #[cfg(test)]
    pub(super) fn with_quote_status(mut self, check: QuoteStatusCheck) -> Self {
        self.quote_status = Some(check);
        self
    }

    async fn verify_model(&self, request: &UpstreamVerificationRequest) -> UpstreamVerifiedEvent {
        let mut event = UpstreamVerifiedEvent {
            upstream_name: request.upstream_name.clone(),
            provider_type: Some(ZERO_G_PROVIDER.to_string()),
            model_id: request.model_id.clone(),
            url_origin: Some(self.router_base.clone()),
            verifier_id: VERIFIER_ID.to_string(),
            result: VerificationResult::Failed,
            required: request.required,
            ..Default::default()
        };
        let candidates = match self.preview(&request.model_id).await {
            Ok(candidates) => candidates,
            Err(reason) => {
                event.reason = Some(reason);
                return event;
            }
        };
        let mut bindings = Vec::new();
        let mut instances = Map::new();
        let mut evidence = Vec::new();
        let mut rejected = Vec::new();
        for candidate in candidates {
            match self.verify_candidate(&candidate, &request.model_id).await {
                Ok(verified) => {
                    let address = verified.provider.provider_address.clone();
                    let key = hex::decode(&verified.provider.e2ee_public_key).unwrap_or_default();
                    bindings.push(ChannelBinding::E2eePublicKeySha256 {
                        provider: ZERO_G_PROVIDER.to_string(),
                        key_id: Some(address.clone()),
                        algorithm: ZERO_G_E2EE_ALGORITHM.to_string(),
                        public_key_sha256: hex::encode(Sha256::digest(key)),
                    });
                    let mut instance = match serde_json::to_value(&verified.provider) {
                        Ok(Value::Object(instance)) => instance,
                        _ => Map::new(),
                    };
                    instance.extend(verified.claims);
                    instances.insert(address, Value::Object(instance));
                    evidence.push(verified.evidence);
                }
                Err(reason) => {
                    tracing::warn!(
                        target: "zero_g",
                        provider = %candidate.address,
                        %reason,
                        "0G candidate rejected before sealing"
                    );
                    rejected.push(format!("{}: {reason}", candidate.address));
                }
            }
        }
        if bindings.is_empty() {
            event.reason = Some(format!(
                "no 0G private-tier candidate passed verification ({})",
                rejected.join("; ")
            ));
            return event;
        }
        let evidence = serde_json::to_vec(&evidence).unwrap_or_default();
        event.result = VerificationResult::Verified;
        event.channel_bindings = bindings;
        event.evidence = Some(json!({
            "type": "0g_provider_quotes_and_service_records",
            "digest": digest::sha256_hex(&evidence),
            "data": format!("data:application/json;base64,{}", BASE64.encode(&evidence)),
        }));
        event.provider_claims = Some(json!({
            "trust_boundary": "0g-provider-broker-tee",
            "router_trust_mode": TRUST_MODE_PRIVATE,
            "request_encryption": "0g-pc-v1-hpke-field-sealing",
            "response_authentication": "zg-sig-v1-recovered-to-onchain-signer-before-release",
            "onchain_target_separated": false,
            "plaintext_destination_verified": false,
            "instances": instances,
        }));
        event
    }

    /// Ask the router for private-tier candidates. The trust-mode header is
    /// set here, by code; the body carries only the model and service type.
    async fn preview(&self, model: &str) -> Result<Vec<PreviewCandidate>, String> {
        let api_key = self
            .api_key
            .as_deref()
            .filter(|key| !key.trim().is_empty())
            .ok_or("0G verification requires bearer_token in upstream config")?;
        let response = self
            .client
            .post(format!("{}/v1/routing/preview", self.router_base))
            .bearer_auth(api_key)
            .header(TRUST_MODE_HEADER, TRUST_MODE_PRIVATE)
            .json(&json!({"model": model, "service_type": SERVICE_TYPE}))
            .send()
            .await
            .map_err(|e| format!("0G route preview failed: {e}"))?;
        let status = response.status().as_u16();
        if status != 200 {
            return Err(format!("0G route preview returned {status}"));
        }
        let body = read_limited(response, MAX_CONTROL_BODY_BYTES).await?;
        let preview: PreviewResponse = serde_json::from_slice(&body)
            .map_err(|e| format!("malformed 0G route preview: {e}"))?;
        let mut candidates: Vec<PreviewCandidate> = Vec::new();
        for candidate in preview.providers {
            // One provider address is one broker, one key and one signer.
            if !candidates
                .iter()
                .any(|seen| seen.address.eq_ignore_ascii_case(&candidate.address))
            {
                candidates.push(candidate);
            }
        }
        candidates.truncate(MAX_CANDIDATES);
        if candidates.is_empty() {
            return Err(format!(
                "0G route preview offered no provider for {model:?}"
            ));
        }
        Ok(candidates)
    }

    pub(super) async fn verify_candidate(
        &self,
        candidate: &PreviewCandidate,
        model: &str,
    ) -> Result<VerifiedCandidate, String> {
        if !is_address(&candidate.address) {
            return Err("router offered an invalid provider address".to_string());
        }
        for offered in [&candidate.canonical_id, &candidate.model_id]
            .into_iter()
            .flatten()
        {
            if offered != model {
                return Err(format!("router offered model {offered:?}"));
            }
        }
        let address = candidate.address.to_ascii_lowercase();
        let (service, service_raw) = self.service_record(&address).await?;
        if !service.acknowledged {
            return Err("on-chain teeSignerAddress is not acknowledged".to_string());
        }
        if service.model != model {
            return Err(format!("on-chain service serves model {:?}", service.model));
        }
        check_not_target_separated(&service.additional_info)?;

        let quote_url = format!("{}/quote?legacy=false", broker_v1_base(&service.url)?);
        let response = self
            .client
            .get(&quote_url)
            .send()
            .await
            .map_err(|e| format!("quote fetch failed: {e}"))?;
        let status = response.status().as_u16();
        if status != 200 {
            return Err(format!("quote endpoint returned {status}"));
        }
        let reply: Value =
            serde_json::from_slice(&read_limited(response, MAX_QUOTE_BODY_BYTES).await?)
                .map_err(|e| format!("malformed quote reply: {e}"))?;
        let (raw_quote, quote) = parse_quote_evidence(&reply).map_err(|e| e.to_string())?;
        let td = match &quote.report {
            Report::TD10(report) => report,
            Report::TD15(report) => &report.base,
            Report::SgxEnclave(_) => return Err("quote is not a TDX quote".to_string()),
        };
        let tcb_status = self.verify_quote(&raw_quote).await?;
        let image = ALLOWED_BOOT_CHAINS
            .iter()
            .find(|chain| {
                hex::encode(td.mr_td) == chain.mrtd
                    && hex::encode(td.rt_mr1) == chain.rtmr1
                    && hex::encode(td.rt_mr2) == chain.rtmr2
            })
            .ok_or("quote boot chain is not an allowlisted dstack image")?
            .image;
        let report_data = parse_report_data(&td.report_data).map_err(|e| e.to_string())?;
        if !report_data.signer.eq_ignore_ascii_case(&service.tee_signer) {
            return Err(format!(
                "quote signer {} is not the on-chain teeSignerAddress {}",
                report_data.signer, service.tee_signer
            ));
        }

        let mut claims = Map::new();
        claims.insert("tcb_status".into(), tcb_status.into());
        claims.insert("os_image".into(), image.into());
        if let Some(compose_hash) = compose_hash_from_mr_config_id(&td.mr_config_id) {
            claims.insert("compose_hash".into(), compose_hash.into());
        }
        claims.insert("onchain_verifiability".into(), service.verifiability.into());
        Ok(VerifiedCandidate {
            provider: VerifiedProvider {
                provider_address: address.clone(),
                provider_identity: candidate.provider_identity.clone(),
                endpoint: service.url,
                model: service.model,
                signing_address: report_data.signer,
                e2ee_public_key: hex::encode(report_data.enc_pub),
            },
            claims,
            evidence: json!({
                "provider_address": address,
                "quote": reply.get("quote"),
                "event_log": reply.get("event_log"),
                "vm_config": reply.get("vm_config"),
                "service_record": format!("0x{}", hex::encode(service_raw)),
            }),
        })
    }

    /// DCAP-verify the quote to Intel's root; dcap-qvl refuses invalid TCB
    /// statuses and debug TDs, and the status it returns is recorded.
    async fn verify_quote(&self, raw_quote: &[u8]) -> Result<String, String> {
        #[cfg(test)]
        if let Some(check) = self.quote_status {
            return check(raw_quote);
        }
        verify_quote_to_root(raw_quote, &self.pccs_url, current_unix_secs(), "tdx")
            .await
            .map(|verified| verified.status)
            .map_err(|e| e.to_string())
    }

    /// `getService(address)` on the InferenceServing contract, through the
    /// operator's RPC rather than the router.
    async fn service_record(&self, address: &str) -> Result<(ServiceRecord, Vec<u8>), String> {
        let selector = hex::encode(&Keccak256::digest(b"getService(address)")[..4]);
        let call = format!("0x{selector}{:0>64}", &address[2..]);
        let response = self
            .client
            .post(&self.chain_rpc_url)
            .json(&json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "eth_call",
                "params": [{"to": self.serving_contract, "data": call}, "latest"],
            }))
            .send()
            .await
            .map_err(|e| format!("chain RPC request failed: {e}"))?;
        let status = response.status().as_u16();
        if status != 200 {
            return Err(format!("chain RPC returned {status}"));
        }
        let reply: Value =
            serde_json::from_slice(&read_limited(response, MAX_CONTROL_BODY_BYTES).await?)
                .map_err(|e| format!("malformed chain RPC reply: {e}"))?;
        if let Some(error) = reply.get("error") {
            return Err(format!("chain RPC error: {error}"));
        }
        let raw = reply
            .get("result")
            .and_then(Value::as_str)
            .and_then(|result| hex::decode(result.trim_start_matches("0x")).ok())
            .ok_or("chain RPC returned no result")?;
        Ok((decode_service(&raw)?, raw))
    }

    fn cached_entry(&self, model: &str) -> Option<CachedProviderEvent> {
        if self.cache_ttl_seconds == 0 {
            return None;
        }
        self.cache
            .read()
            .expect("0G verifier cache poisoned")
            .get(model)
            .filter(|cached| Instant::now() < cached.expires_at)
            .cloned()
    }

    async fn verify_and_cache(
        &self,
        request: UpstreamVerificationRequest,
    ) -> UpstreamVerifiedEvent {
        let started = Instant::now();
        let event = self.verify_model(&request).await;
        // A failed refresh keeps the still-valid entry.
        if self.cache_ttl_seconds > 0 && event.result == VerificationResult::Verified {
            self.cache
                .write()
                .expect("0G verifier cache poisoned")
                .insert(
                    request.model_id.clone(),
                    CachedProviderEvent {
                        expires_at: started + Duration::from_secs(self.cache_ttl_seconds),
                        event: event.clone(),
                    },
                );
        }
        event
    }
}

#[async_trait]
impl UpstreamVerifier for ZeroGProviderVerifier {
    async fn verify(&self, request: UpstreamVerificationRequest) -> UpstreamVerifiedEvent {
        if let Some(event) = self.cached(&request) {
            return event;
        }
        let _verify_guard = self.verify_lock.lock().await;
        if let Some(event) = self.cached(&request) {
            return event;
        }
        self.verify_and_cache(request).await
    }

    fn cached(&self, request: &UpstreamVerificationRequest) -> Option<UpstreamVerifiedEvent> {
        self.cached_entry(&request.model_id)
            .map(|cached| cached.event_for(request))
    }

    fn cache_remaining(&self, request: &UpstreamVerificationRequest) -> Option<Duration> {
        self.cached_entry(&request.model_id)?
            .expires_at
            .checked_duration_since(Instant::now())
    }

    async fn refresh(&self, request: UpstreamVerificationRequest) -> UpstreamVerifiedEvent {
        let _verify_guard = self.verify_lock.lock().await;
        self.verify_and_cache(request).await
    }

    fn invalidate(&self, request: &UpstreamVerificationRequest) {
        self.cache
            .write()
            .expect("0G verifier cache poisoned")
            .remove(&request.model_id);
    }
}

/// The record must state `TargetSeparated: false`: a separated target means
/// the broker forwards plaintext beyond its own TEE, and an absent or
/// unreadable declaration cannot establish that it does not.
fn check_not_target_separated(additional_info: &str) -> Result<(), String> {
    let info: Value = serde_json::from_str(additional_info)
        .map_err(|_| "on-chain additionalInfo is not JSON".to_string())?;
    match info.get("TargetSeparated") {
        Some(Value::Bool(false)) => Ok(()),
        Some(Value::Bool(true)) => Err(
            "on-chain record declares TargetSeparated: plaintext leaves the provider TEE".into(),
        ),
        _ => Err("on-chain record does not declare TargetSeparated: false".to_string()),
    }
}

/// dstack's v1 `mr_config_id`: `0x01 ‖ compose_hash(32) ‖ zero padding`.
fn compose_hash_from_mr_config_id(mr_config_id: &[u8; 48]) -> Option<String> {
    (mr_config_id[0] == 1 && mr_config_id[33..].iter().all(|b| *b == 0))
        .then(|| hex::encode(&mr_config_id[1..33]))
}

fn is_address(value: &str) -> bool {
    value.len() == 42
        && value.starts_with("0x")
        && value[2..].bytes().all(|b| b.is_ascii_hexdigit())
}

/// Decode the ABI-encoded `getService` return: one dynamic `Service` tuple
/// `(provider, serviceType, url, inputPrice, outputPrice, updatedAt, model,
/// verifiability, additionalInfo, teeSignerAddress, teeSignerAcknowledged)`.
/// Any misalignment fails closed.
fn decode_service(raw: &[u8]) -> Result<ServiceRecord, String> {
    let base = abi_offset(raw, 0)?;
    let head = |slot: usize| abi_word(raw, base + slot * 32);
    let string = |slot: usize| -> Result<String, String> {
        let start = base + abi_offset(raw, base + slot * 32)?;
        let len = abi_offset(raw, start)?;
        let data = raw
            .get(start + 32..start + 32 + len)
            .ok_or("getService string out of range")?;
        String::from_utf8(data.to_vec()).map_err(|_| "getService string is not UTF-8".to_string())
    };
    let signer = head(9)?;
    if signer[..12].iter().any(|b| *b != 0) {
        return Err("getService teeSignerAddress slot is not an address".to_string());
    }
    let acknowledged = head(10)?;
    if acknowledged[..31].iter().any(|b| *b != 0) || acknowledged[31] > 1 {
        return Err("getService teeSignerAcknowledged slot is not a bool".to_string());
    }
    Ok(ServiceRecord {
        url: string(2)?,
        model: string(6)?,
        verifiability: string(7)?,
        additional_info: string(8)?,
        tee_signer: format!("0x{}", hex::encode(&signer[12..])),
        acknowledged: acknowledged[31] == 1,
    })
}

fn abi_word(raw: &[u8], offset: usize) -> Result<&[u8], String> {
    raw.get(offset..offset + 32)
        .ok_or_else(|| "getService return is truncated".to_string())
}

fn abi_offset(raw: &[u8], offset: usize) -> Result<usize, String> {
    let word = abi_word(raw, offset)?;
    if word[..24].iter().any(|b| *b != 0) {
        return Err("getService offset out of range".to_string());
    }
    let value = u64::from_be_bytes(word[24..].try_into().expect("8 bytes"));
    usize::try_from(value)
        .ok()
        .filter(|value| *value <= raw.len())
        .ok_or_else(|| "getService offset out of range".to_string())
}
