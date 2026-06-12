use super::*;

use super::helpers::*;
use crate::aci::keys::{LegacySignature, LEGACY_ALGO_ECDSA};
use crate::aci::receipt::{ReceiptError, EVENT_RESPONSE_RETURNED};
use crate::aci::types::Receipt;
use crate::aggregator::session::AttestedSession;

impl AciService {
    pub fn get_receipt_by_receipt_id(&self, id: &str) -> Option<Receipt> {
        self.receipt_store
            .get_by_receipt_id(id, self.clock.now_secs())
    }

    pub fn get_receipt_by_chat_id(&self, id: &str) -> Option<Receipt> {
        self.receipt_store.get_by_chat_id(id, self.clock.now_secs())
    }

    pub fn legacy_signature_for_receipt(
        &self,
        receipt: &Receipt,
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

    pub fn get_attested_session(&self, session_id: &str) -> Option<AttestedSession> {
        self.session_store
            .get_session(session_id, self.clock.now_secs())
    }

    /// List attested sessions (TEE channels), optionally filtered by provider
    /// (the upstream config name). A model→channel lookup belongs to the caller,
    /// since a session is per-channel, not per-model.
    pub fn list_attested_sessions(&self, provider: Option<&str>) -> Vec<AttestedSession> {
        self.session_store
            .list_sessions(provider, self.clock.now_secs())
    }

    /// E2EE protocol versions this workload has actually wired.
    pub fn supported_e2ee_versions(&self) -> &[String] {
        &self.config.service_capabilities.supported_e2ee_versions
    }
}
