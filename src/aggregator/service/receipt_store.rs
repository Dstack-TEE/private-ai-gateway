use std::sync::RwLock;

use super::ReceiptOwner;
use crate::aci::types::Receipt;

/// stores request bodies — only the receipt (which holds hashes, not content).
pub trait ReceiptStore: Send + Sync {
    /// Store a signed receipt. `owner` is the requester's hashed bearer
    /// credential, or `None` for anonymous calls. The store MUST keep
    /// the owner alongside the receipt so lookups can authenticate.
    fn put(&self, receipt: Receipt, owner: Option<ReceiptOwner>, expires_at: u64);
    fn get_by_receipt_id(&self, receipt_id: &str, now: u64) -> Option<Receipt>;
    fn get_by_chat_id(&self, chat_id: &str, now: u64) -> Option<Receipt>;
    /// Return the owner recorded at `put` time, if any.
    fn owner_of(&self, receipt_id: &str, now: u64) -> Option<ReceiptOwner>;
}

#[derive(Default)]
pub struct InMemoryReceiptStore {
    inner: RwLock<InMemoryReceiptStoreInner>,
}

#[derive(Default)]
struct InMemoryReceiptStoreInner {
    by_receipt: std::collections::HashMap<String, StoredReceipt>,
    by_chat: std::collections::HashMap<String, String>,
}

struct StoredReceipt {
    receipt: Receipt,
    owner: Option<ReceiptOwner>,
    expires_at: u64,
}

impl ReceiptStore for InMemoryReceiptStore {
    fn put(&self, receipt: Receipt, owner: Option<ReceiptOwner>, expires_at: u64) {
        let mut guard = self.inner.write().expect("receipt store poisoned");
        if let Some(cid) = receipt.chat_id.clone() {
            guard.by_chat.insert(cid, receipt.receipt_id.clone());
        }
        guard.by_receipt.insert(
            receipt.receipt_id.clone(),
            StoredReceipt {
                receipt,
                owner,
                expires_at,
            },
        );
    }

    fn get_by_receipt_id(&self, receipt_id: &str, now: u64) -> Option<Receipt> {
        let mut guard = self.inner.write().expect("receipt store poisoned");
        let expires_at = guard.by_receipt.get(receipt_id)?.expires_at;
        if now >= expires_at {
            remove_receipt_locked(&mut guard, receipt_id);
            return None;
        }
        guard
            .by_receipt
            .get(receipt_id)
            .map(|entry| entry.receipt.clone())
    }

    fn get_by_chat_id(&self, chat_id: &str, now: u64) -> Option<Receipt> {
        let mut guard = self.inner.write().expect("receipt store poisoned");
        let receipt_id = guard.by_chat.get(chat_id)?.clone();
        let expires_at = guard.by_receipt.get(&receipt_id)?.expires_at;
        if now >= expires_at {
            remove_receipt_locked(&mut guard, &receipt_id);
            return None;
        }
        guard
            .by_receipt
            .get(&receipt_id)
            .map(|entry| entry.receipt.clone())
    }

    fn owner_of(&self, receipt_id: &str, now: u64) -> Option<ReceiptOwner> {
        let mut guard = self.inner.write().expect("receipt store poisoned");
        let expires_at = guard.by_receipt.get(receipt_id)?.expires_at;
        if now >= expires_at {
            remove_receipt_locked(&mut guard, receipt_id);
            return None;
        }
        guard
            .by_receipt
            .get(receipt_id)
            .and_then(|entry| entry.owner.clone())
    }
}

fn remove_receipt_locked(inner: &mut InMemoryReceiptStoreInner, receipt_id: &str) {
    if let Some(entry) = inner.by_receipt.remove(receipt_id) {
        if let Some(chat_id) = entry.receipt.chat_id {
            inner.by_chat.remove(&chat_id);
        }
    }
}
