use std::collections::HashSet;

use super::helpers::legacy_signature_text;
use super::{
    AciService, LegacySignatureResult, ReceiptOwner, ServiceError, UpstreamVerificationRequest,
};
use crate::aci::keys::{LegacySignature, LEGACY_ALGO_ECDSA};
use crate::aci::receipt::{ReceiptError, SignedReceipt, EVENT_RESPONSE_RETURNED};
use crate::aggregator::session::AttestedSession;
use crate::aggregator::session_store::sort_sessions_newest_first;

impl AciService {
    pub fn get_receipt_by_receipt_id(&self, id: &str) -> Option<SignedReceipt> {
        self.receipt_store
            .get_by_receipt_id(id, self.clock.now_secs())
    }

    pub fn get_receipt_by_chat_id(&self, id: &str) -> Option<SignedReceipt> {
        self.receipt_store.get_by_chat_id(id, self.clock.now_secs())
    }

    /// legacy `/v1/signature/{chat_id}` wrapper: sign
    /// `request_hash:response_hash` (bare hex) lifted from the stored receipt
    /// payload with the legacy signing key.
    pub fn legacy_signature_for_receipt(
        &self,
        receipt: &SignedReceipt,
        signing_algo: Option<&str>,
    ) -> Result<LegacySignatureResult, ServiceError> {
        let Some(text) = legacy_signature_text(receipt) else {
            return Err(ReceiptError::MissingRequiredEvent(EVENT_RESPONSE_RETURNED).into());
        };
        let LegacySignature {
            signing_algo,
            signing_address,
            signature,
        } = self
            .keys
            .sign_legacy_message(signing_algo.unwrap_or(LEGACY_ALGO_ECDSA), &text)?;
        Ok(LegacySignatureResult {
            text,
            signature,
            signing_address,
            signing_algo,
        })
    }

    /// Read the recorded owner for a receipt, if any.
    pub fn owner_of_receipt(&self, receipt_id: &str) -> Option<ReceiptOwner> {
        self.receipt_store
            .owner_of(receipt_id, self.clock.now_secs())
    }

    /// Resolve a session by its full `bare 64-hex` id (retention window).
    pub fn get_attested_session(&self, session_id: &str) -> Option<AttestedSession> {
        self.session_store
            .get_session(session_id, self.clock.now_secs())
    }

    /// List the sessions accepted by the verifier's current cached state.
    pub fn list_current_sessions(
        &self,
        requests: &[UpstreamVerificationRequest],
    ) -> Result<Vec<AttestedSession>, ServiceError> {
        let Some(verifier) = self.upstream_verifier.as_ref() else {
            return Ok(Vec::new());
        };
        let now = self.clock.now_secs();
        let mut ids = HashSet::new();
        let mut sessions = Vec::new();
        for event in requests.iter().filter_map(|r| verifier.cached(r)) {
            for sealed in self.record_attested_upstream_session(&event)? {
                if !ids.insert(sealed.session_id.clone()) {
                    continue;
                }
                if let Some(session) = self
                    .session_store
                    .get_session(&sealed.session_id, now)
                    .filter(|s| now < s.document().expires_at)
                {
                    sessions.push(session);
                }
            }
        }
        sort_sessions_newest_first(&mut sessions);
        Ok(sessions)
    }

    /// E2EE protocol versions this workload has actually wired.
    pub fn supported_e2ee_versions(&self) -> &[String] {
        &self.config.service_capabilities.supported_e2ee_versions
    }

    /// legacy `X-Signing-Algo` keys, for the legacy report/signature
    /// surfaces. Never part of the ACI keyset.
    pub fn legacy_e2ee_keys(&self) -> Vec<crate::aci::types::KeyedPublicKey> {
        self.keys.legacy_e2ee_keys()
    }
}
