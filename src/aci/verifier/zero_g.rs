//! Verifier for 0G Private Computer providers. Every private-tier candidate
//! the router offers is checked before anything is sealed to it: its on-chain
//! record (acknowledged signer, model, `TargetSeparated: false`), its broker's
//! DCAP-verified TDX quote and dstack boot chain, and quote signer equal to
//! the on-chain signer. Each survivor yields an `e2ee_public_key_sha256`
//! binding over the key its quote commits to.

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
    broker_api, readiness_client, UpstreamError, VerifiedProvider, TRUST_MODE_HEADER,
    ZERO_G_E2EE_ALGORITHM, ZERO_G_PROVIDER,
};
use crate::aggregator::service::{UpstreamVerificationRequest, UpstreamVerifier};

const MAX_CANDIDATES: usize = 8;

/// Boot chains (MRTD, RTMR1, RTMR2) of the production dstack images 0G
/// providers run, from 0G's provider allowlist (`client/evidence/
/// brokerimages.json` at 0g-pc-e2ee release-2026.10.19.2), which derives each
/// entry with dstack-mr from the published release.
const ALLOWED_BOOT_CHAINS: &[(&str, [&str; 3])] = &[
    (
        "dstack-nvidia-0.5.9",
        [
            "b24d3b24e9e3c16012376b52362ca09856c4adecb709d5fac33addf1c47e193da075b125b6c364115771390a5461e217",
            "07e6f51aa763abfe75c3ddfbf4f425fe3f0ceff66d807a75e049303dce9addf68e7218729bd419638af63a370f65878c",
            "f1c82667a354194467cd8419efd14a714560dd9b85b4c13b25c11e44bf4e126248c2255fad58c303fb0ca2921765d53a",
        ],
    ),
    (
        "dstack-nvidia-0.5.4.1",
        [
            "b24d3b24e9e3c16012376b52362ca09856c4adecb709d5fac33addf1c47e193da075b125b6c364115771390a5461e217",
            "6e1afb7464ed0b941e8f5bf5b725cf1df9425e8105e3348dca52502f27c453f3018a28b90749cf05199d5a17820101a7",
            "89e73cedf48f976ffebe8ac1129790ff59a0f52d54d969cb73455b1a79793f1dc16edc3b1fccc0fd65ea5905774bbd57",
        ],
    ),
];

#[derive(Debug, Clone, Deserialize)]
pub(super) struct PreviewCandidate {
    pub address: String,
    #[serde(default)]
    pub provider_identity: Option<String>,
    #[serde(default)]
    pub canonical_id: Option<String>,
}

#[derive(Deserialize)]
struct PreviewResponse {
    #[serde(default)]
    providers: Vec<PreviewCandidate>,
}

/// Test stand-in for DCAP-to-root verification: raw quote to TCB status.
#[cfg(test)]
type QuoteStatusCheck = fn(&[u8]) -> Result<String, String>;

pub struct ZeroGProviderVerifier {
    router_base: String,
    api_key: String,
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

impl ZeroGProviderVerifier {
    pub fn new(
        router_base: impl Into<String>,
        api_key: impl Into<String>,
        chain_rpc_url: impl Into<String>,
        serving_contract: impl Into<String>,
        pccs_url: Option<String>,
        request_timeout_seconds: u64,
        cache_ttl_seconds: u64,
    ) -> Result<Self, UpstreamError> {
        Ok(Self {
            router_base: router_base.into().trim_end_matches('/').to_string(),
            api_key: api_key.into(),
            chain_rpc_url: chain_rpc_url.into(),
            serving_contract: serving_contract.into(),
            pccs_url: pccs_url.unwrap_or_else(|| dcap_qvl::PHALA_PCCS_URL.to_string()),
            client: readiness_client(
                super::DEFAULT_VERIFIER_CONNECT_TIMEOUT_SECONDS,
                request_timeout_seconds,
            )?,
            cache_ttl_seconds,
            cache: RwLock::new(HashMap::new()),
            verify_lock: tokio::sync::Mutex::new(()),
            #[cfg(test)]
            quote_status: None,
        })
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
            verifier_id: "0g-private-computer/native-e2ee/v1".to_string(),
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
        let mut instances = Map::new();
        let mut evidence = Vec::new();
        let mut rejected = Vec::new();
        for candidate in candidates {
            match self.verify_candidate(&candidate, &request.model_id).await {
                Ok((provider, claims, quote)) => {
                    let key = hex::decode(&provider.e2ee_public_key).unwrap_or_default();
                    event
                        .channel_bindings
                        .push(ChannelBinding::E2eePublicKeySha256 {
                            provider: ZERO_G_PROVIDER.to_string(),
                            key_id: Some(provider.provider_address.clone()),
                            algorithm: ZERO_G_E2EE_ALGORITHM.to_string(),
                            public_key_sha256: hex::encode(Sha256::digest(key)),
                        });
                    let mut instance = match serde_json::to_value(&provider) {
                        Ok(Value::Object(instance)) => instance,
                        _ => Map::new(),
                    };
                    instance.extend(claims);
                    instances.insert(provider.provider_address, Value::Object(instance));
                    evidence.push(quote);
                }
                Err(reason) => {
                    tracing::warn!(target: "zero_g", provider = %candidate.address, %reason, "0G candidate rejected before sealing");
                    rejected.push(format!("{}: {reason}", candidate.address));
                }
            }
        }
        if instances.is_empty() {
            event.reason = Some(format!(
                "no 0G private-tier candidate passed verification ({})",
                rejected.join("; ")
            ));
            return event;
        }
        let evidence = serde_json::to_vec(&evidence).unwrap_or_default();
        event.result = VerificationResult::Verified;
        event.evidence = Some(json!({
            "digest": digest::sha256_hex(&evidence),
            "data": format!("data:application/json;base64,{}", BASE64.encode(&evidence)),
        }));
        event.provider_claims = Some(json!({
            "trust_boundary": "0g-provider-broker-tee",
            "instances": instances,
        }));
        event
    }

    async fn preview(&self, model: &str) -> Result<Vec<PreviewCandidate>, String> {
        let response = self
            .client
            .post(format!("{}/v1/routing/preview", self.router_base))
            .bearer_auth(&self.api_key)
            .header(TRUST_MODE_HEADER, "private")
            .json(&json!({"model": model, "service_type": "chatbot"}))
            .send()
            .await
            .and_then(reqwest::Response::error_for_status)
            .map_err(|e| format!("0G route preview failed: {e}"))?;
        let preview: PreviewResponse = response
            .json()
            .await
            .map_err(|e| format!("malformed 0G route preview: {e}"))?;
        let mut candidates: Vec<PreviewCandidate> = Vec::new();
        for candidate in preview.providers {
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
    ) -> Result<(VerifiedProvider, Map<String, Value>, Value), String> {
        if candidate
            .canonical_id
            .as_deref()
            .is_some_and(|id| id != model)
        {
            return Err("router offered a different model".to_string());
        }
        let address = candidate.address.to_ascii_lowercase();
        if address.len() != 42
            || !address.starts_with("0x")
            || !address[2..].bytes().all(|b| b.is_ascii_hexdigit())
        {
            return Err("router offered an invalid provider address".to_string());
        }
        let service = self.service_record(&address).await?;
        let [url, onchain_model, verifiability, additional_info] = &service.strings;
        if !service.acknowledged {
            return Err("on-chain teeSignerAddress is not acknowledged".to_string());
        }
        if onchain_model != model {
            return Err(format!("on-chain service serves model {onchain_model:?}"));
        }
        let info: Value = serde_json::from_str(additional_info).unwrap_or_default();
        if info.get("TargetSeparated") != Some(&Value::Bool(false)) {
            return Err("on-chain record does not declare TargetSeparated: false".to_string());
        }

        let response = self
            .client
            .get(format!("{}/quote?legacy=false", broker_api(url)?))
            .send()
            .await
            .map_err(|e| format!("quote fetch failed: {e}"))?;
        if response.status() != reqwest::StatusCode::OK {
            return Err(format!("quote endpoint returned {}", response.status()));
        }
        let reply: Value = response
            .json()
            .await
            .map_err(|e| format!("malformed quote reply: {e}"))?;
        let (raw_quote, quote) = parse_quote_evidence(&reply).map_err(|e| e.to_string())?;
        let td = match &quote.report {
            Report::TD10(report) => report,
            Report::TD15(report) => &report.base,
            Report::SgxEnclave(_) => return Err("quote is not a TDX quote".to_string()),
        };
        let tcb_status = self.verify_quote(&raw_quote).await?;
        let boot_chain = [td.mr_td, td.rt_mr1, td.rt_mr2].map(hex::encode);
        let (os_image, _) = ALLOWED_BOOT_CHAINS
            .iter()
            .find(|(_, chain)| *chain == boot_chain)
            .ok_or("quote boot chain is not an allowlisted dstack image")?;
        // report_data (§4.2): enc_pub(32) ‖ signer(20) ‖ version u32 BE = 1 ‖ zero(8).
        let report_data = &td.report_data;
        if report_data[52..56] != [0, 0, 0, 1] || report_data[56..].iter().any(|b| *b != 0) {
            return Err("quote report_data is not the 0G v1 layout".to_string());
        }
        let signer = format!("0x{}", hex::encode(&report_data[32..52]));
        if signer != service.signer {
            return Err(format!(
                "quote signer {signer} is not the on-chain teeSignerAddress {}",
                service.signer
            ));
        }

        let mut claims = Map::new();
        claims.insert("tcb_status".into(), tcb_status.into());
        claims.insert("os_image".into(), (*os_image).into());
        claims.insert("onchain_verifiability".into(), verifiability.clone().into());
        claims.insert("plaintext_destination_verified".into(), false.into());
        if td.mr_config_id[0] == 1 && td.mr_config_id[33..].iter().all(|b| *b == 0) {
            claims.insert(
                "compose_hash".into(),
                hex::encode(&td.mr_config_id[1..33]).into(),
            );
        }
        let provider = VerifiedProvider {
            provider_address: address.clone(),
            provider_identity: candidate.provider_identity.clone(),
            endpoint: url.clone(),
            model: model.to_string(),
            signing_address: signer,
            e2ee_public_key: hex::encode(&report_data[..32]),
        };
        let evidence = json!({
            "provider_address": address,
            "quote": reply.get("quote"),
            "event_log": reply.get("event_log"),
            "service_record": service.raw,
        });
        Ok((provider, claims, evidence))
    }

    /// DCAP to Intel's root; dcap-qvl refuses invalid TCB statuses and debug TDs.
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

    /// `getService(address)` on the InferenceServing contract through the
    /// operator's RPC, never the router.
    async fn service_record(&self, address: &str) -> Result<ServiceRecord, String> {
        let selector = hex::encode(&Keccak256::digest(b"getService(address)")[..4]);
        let reply: Value = self
            .client
            .post(&self.chain_rpc_url)
            .json(&json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "eth_call",
                "params": [{"to": self.serving_contract, "data": format!("0x{selector}{:0>64}", &address[2..])}, "latest"],
            }))
            .send()
            .await
            .and_then(reqwest::Response::error_for_status)
            .map_err(|e| format!("chain RPC failed: {e}"))?
            .json()
            .await
            .map_err(|e| format!("malformed chain RPC reply: {e}"))?;
        let raw = reply["result"].as_str().unwrap_or_default().to_string();
        let bytes = hex::decode(raw.trim_start_matches("0x"))
            .map_err(|_| "chain RPC returned no result".to_string())?;
        decode_service(&bytes, raw).ok_or_else(|| "getService return does not decode".to_string())
    }

    fn cached_entry(&self, model: &str) -> Option<CachedProviderEvent> {
        let cache = self.cache.read().expect("0G verifier cache poisoned");
        cache
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
        if self.cache_ttl_seconds > 0 && event.result == VerificationResult::Verified {
            self.cache
                .write()
                .expect("0G verifier cache poisoned")
                .insert(
                    request.model_id,
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
        let mut cache = self.cache.write().expect("0G verifier cache poisoned");
        cache.remove(&request.model_id);
    }
}

struct ServiceRecord {
    /// `url`, `model`, `verifiability`, `additionalInfo`.
    strings: [String; 4],
    signer: String,
    acknowledged: bool,
    raw: String,
}

/// Decode the ABI `getService` return: one dynamic tuple `(provider,
/// serviceType, url, inputPrice, outputPrice, updatedAt, model, verifiability,
/// additionalInfo, teeSignerAddress, teeSignerAcknowledged)`.
fn decode_service(bytes: &[u8], raw: String) -> Option<ServiceRecord> {
    let word = |offset: usize| bytes.get(offset..offset + 32);
    let number = |offset: usize| {
        let word = word(offset)?;
        let value = u64::from_be_bytes(word[24..].try_into().ok()?);
        // Bounded by the return's length, so the offset sums below cannot overflow.
        (word[..24].iter().all(|b| *b == 0) && value <= bytes.len() as u64)
            .then_some(value as usize)
    };
    let base = number(0)?;
    let string = |slot: usize| {
        let start = base + number(base + slot * 32)?;
        let len = number(start)?;
        let data = bytes.get(start + 32..start + 32 + len)?;
        String::from_utf8(data.to_vec()).ok()
    };
    let signer = word(base + 9 * 32)?;
    let acknowledged = number(base + 10 * 32)?;
    (signer[..12].iter().all(|b| *b == 0) && acknowledged <= 1).then_some(())?;
    Some(ServiceRecord {
        strings: [string(2)?, string(6)?, string(7)?, string(8)?],
        signer: format!("0x{}", hex::encode(&signer[12..])),
        acknowledged: acknowledged == 1,
        raw,
    })
}
