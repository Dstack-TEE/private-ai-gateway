use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::RwLock;

use super::ReceiptOwner;
use crate::aci::types::Receipt;

/// stores request bodies — only the receipt (which holds hashes, not content).
pub trait ReceiptStore: Send + Sync {
    /// Store a signed receipt. `now` is the current second, used to evict
    /// already-expired entries on write so the store stays bounded without a
    /// background sweep; `expires_at` is this receipt's retention deadline.
    /// `owner` is the requester's hashed bearer credential, or `None` for
    /// anonymous calls. The store MUST keep the owner alongside the receipt so
    /// lookups can authenticate.
    fn put(&self, receipt: Receipt, owner: Option<ReceiptOwner>, now: u64, expires_at: u64);
    fn get_by_receipt_id(&self, receipt_id: &str, now: u64) -> Option<Receipt>;
    fn get_by_chat_id(&self, chat_id: &str, now: u64) -> Option<Receipt>;
    /// Return the owner recorded at `put` time, if any.
    fn owner_of(&self, receipt_id: &str, now: u64) -> Option<ReceiptOwner>;
}

/// In-memory receipt store. Active eviction on write keeps the footprint bounded
/// by `requests/sec × receipt_ttl_seconds` — independent of read traffic — so a
/// write-heavy, read-light workload does not grow it without limit. (A receipt
/// is signed and self-verifying, so a store that holds the only copy is a
/// convenience cache for the pull API, not a durable record; durable retention
/// would be a separate store.)
#[derive(Default)]
pub struct InMemoryReceiptStore {
    inner: RwLock<InMemoryReceiptStoreInner>,
}

#[derive(Default)]
struct InMemoryReceiptStoreInner {
    by_receipt: HashMap<String, StoredReceipt>,
    by_chat: HashMap<String, String>,
    /// `expires_at` → receipt ids, so eviction costs only what actually expired
    /// (O(k)) instead of scanning the whole store on every write.
    by_expiry: BTreeMap<u64, HashSet<String>>,
}

struct StoredReceipt {
    receipt: Receipt,
    owner: Option<ReceiptOwner>,
    expires_at: u64,
}

impl InMemoryReceiptStoreInner {
    fn insert(&mut self, receipt: Receipt, owner: Option<ReceiptOwner>, expires_at: u64) {
        let receipt_id = receipt.receipt_id.clone();
        // Receipt ids are unique in practice, but drop any prior entry first so a
        // collision can never leave a dangling expiry hint.
        self.remove(&receipt_id);
        if let Some(chat_id) = receipt.chat_id.clone() {
            // A reused chat id supersedes the prior mapping; the older receipt
            // stays reachable by its own id until it expires.
            self.by_chat.insert(chat_id, receipt_id.clone());
        }
        self.by_expiry
            .entry(expires_at)
            .or_default()
            .insert(receipt_id.clone());
        self.by_receipt.insert(
            receipt_id,
            StoredReceipt {
                receipt,
                owner,
                expires_at,
            },
        );
    }

    /// Remove one receipt from all three indexes.
    fn remove(&mut self, receipt_id: &str) {
        if let Some(entry) = self.by_receipt.remove(receipt_id) {
            self.drop_chat_mapping(&entry, receipt_id);
            self.drop_expiry_hint(receipt_id, entry.expires_at);
        }
    }

    /// Drop the chat→receipt mapping for a removed entry, but only if it still
    /// points at this receipt — a later receipt may have reused the chat id and
    /// overwritten the mapping, and that newer entry must survive.
    fn drop_chat_mapping(&mut self, entry: &StoredReceipt, receipt_id: &str) {
        if let Some(chat_id) = entry.receipt.chat_id.as_deref() {
            if self.by_chat.get(chat_id).map(String::as_str) == Some(receipt_id) {
                self.by_chat.remove(chat_id);
            }
        }
    }

    fn drop_expiry_hint(&mut self, receipt_id: &str, expires_at: u64) {
        if let Some(ids) = self.by_expiry.get_mut(&expires_at) {
            ids.remove(receipt_id);
            if ids.is_empty() {
                self.by_expiry.remove(&expires_at);
            }
        }
    }

    /// Drop every receipt whose deadline is at or before `now`. Whole buckets are
    /// safe to pop here: every id in a bucket shares the same expired deadline.
    fn evict_expired(&mut self, now: u64) {
        while let Some((&expires_at, _)) = self.by_expiry.first_key_value() {
            if expires_at > now {
                break;
            }
            let (_, ids) = self
                .by_expiry
                .pop_first()
                .expect("first_key_value just returned a bucket");
            for id in ids {
                if let Some(entry) = self.by_receipt.remove(&id) {
                    self.drop_chat_mapping(&entry, &id);
                }
            }
        }
    }
}

impl ReceiptStore for InMemoryReceiptStore {
    fn put(&self, receipt: Receipt, owner: Option<ReceiptOwner>, now: u64, expires_at: u64) {
        let mut guard = self.inner.write().expect("receipt store poisoned");
        guard.evict_expired(now);
        guard.insert(receipt, owner, expires_at);
    }

    fn get_by_receipt_id(&self, receipt_id: &str, now: u64) -> Option<Receipt> {
        let mut guard = self.inner.write().expect("receipt store poisoned");
        let expires_at = guard.by_receipt.get(receipt_id)?.expires_at;
        if now >= expires_at {
            guard.remove(receipt_id);
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
            guard.remove(&receipt_id);
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
            guard.remove(receipt_id);
            return None;
        }
        guard
            .by_receipt
            .get(receipt_id)
            .and_then(|entry| entry.owner.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::aci::types::{Receipt, ReceiptSignature};

    fn receipt(id: &str, chat_id: &str) -> Receipt {
        Receipt {
            api_version: "aci/1".to_string(),
            receipt_id: id.to_string(),
            chat_id: Some(chat_id.to_string()),
            workload_id: "wl".to_string(),
            workload_keyset_digest: "sha256:0".to_string(),
            endpoint: "/v1/chat/completions".to_string(),
            method: "POST".to_string(),
            served_at: 0,
            event_log: vec![],
            signature: ReceiptSignature {
                algo: "ed25519".to_string(),
                key_id: "k".to_string(),
                value_hex: "00".to_string(),
            },
        }
    }

    fn len(store: &InMemoryReceiptStore) -> usize {
        store.inner.read().unwrap().by_receipt.len()
    }

    #[test]
    fn put_actively_evicts_expired_without_a_read() {
        // The risk this guards: writes far outpace reads, so without active
        // eviction the map would grow with total request count. A later write
        // must reclaim earlier expired receipts on its own.
        let store = InMemoryReceiptStore::default();
        store.put(receipt("a", "ca"), None, 1_000, 2_000); // expires at 2_000
        store.put(receipt("b", "cb"), None, 1_000, 2_000);
        assert_eq!(len(&store), 2);

        // A write at now=3_000 is past both deadlines: they are evicted, leaving
        // only the freshly written record — no read required.
        store.put(receipt("c", "cc"), None, 3_000, 9_000);
        assert_eq!(len(&store), 1);
        assert!(store.get_by_receipt_id("a", 3_000).is_none());
        assert_eq!(
            store.get_by_receipt_id("c", 3_000).map(|r| r.receipt_id),
            Some("c".to_string())
        );
    }

    #[test]
    fn reused_chat_id_keeps_pointing_at_the_newest_receipt() {
        // Two receipts share a chat id; the second supersedes the mapping.
        // Evicting the older receipt must not delete the chat mapping that now
        // points at the newer one.
        let store = InMemoryReceiptStore::default();
        store.put(receipt("old", "conv"), None, 1_000, 2_000); // expires 2_000
        store.put(receipt("new", "conv"), None, 1_000, 9_000); // same chat id

        // A write past the old deadline evicts "old"...
        store.put(receipt("filler", "cf"), None, 3_000, 9_000);
        assert!(store.get_by_receipt_id("old", 3_000).is_none());
        // ...but the chat mapping still resolves to "new".
        assert_eq!(
            store.get_by_chat_id("conv", 3_000).map(|r| r.receipt_id),
            Some("new".to_string())
        );
    }
}
