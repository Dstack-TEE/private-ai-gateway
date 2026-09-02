//! The verified remote catalog (`GET /v1/models` through the ACI sidecar) is
//! the only source of model truth. Entries are validated and preserved as the
//! service lists them; nothing is added or inferred.

use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct RemoteModel {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_length: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_output_length: Option<u64>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Surface {
    ChatCompletions,
    Messages,
    Responses,
}

impl Surface {
    pub fn path(self) -> &'static str {
        match self {
            Surface::ChatCompletions => "/v1/chat/completions",
            Surface::Messages => "/v1/messages",
            Surface::Responses => "/v1/responses",
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CatalogModel {
    pub remote: RemoteModel,
}

impl CatalogModel {
    pub fn id(&self) -> &str {
        &self.remote.id
    }
    pub fn display_name(&self) -> &str {
        self.remote.name.as_deref().unwrap_or(&self.remote.id)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Catalog {
    pub revision: String,
    pub fetched_at: u64,
    pub models: Vec<CatalogModel>,
}

impl Catalog {
    pub fn from_remote(body: &Value, fetched_at: u64) -> Result<Self, String> {
        let data = body
            .get("data")
            .and_then(Value::as_array)
            .ok_or_else(|| "the model list has no `data` array".to_string())?;
        let entries: Vec<RemoteModel> = serde_json::from_value(Value::Array(data.clone()))
            .map_err(|error| format!("the model list is malformed: {error}"))?;
        if entries.is_empty() {
            return Err("the service returned no models".to_string());
        }
        let mut hasher = Sha256::new();
        let mut models = Vec::with_capacity(entries.len());
        for entry in entries {
            if entry.id.trim().is_empty() {
                return Err("the model list contains an entry with an empty id".to_string());
            }
            if models
                .iter()
                .any(|model: &CatalogModel| model.id() == entry.id)
            {
                continue;
            }
            let canonical = serde_json::to_vec(&entry).map_err(|error| error.to_string())?;
            hasher.update((canonical.len() as u64).to_be_bytes());
            hasher.update(&canonical);
            models.push(CatalogModel { remote: entry });
        }
        let revision = hasher
            .finalize()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        Ok(Self {
            revision,
            fetched_at,
            models,
        })
    }

    pub fn get(&self, id: &str) -> Option<&CatalogModel> {
        self.models.iter().find(|model| model.id() == id)
    }

    pub fn removed_since(&self, previous: &Catalog) -> Vec<String> {
        previous
            .models
            .iter()
            .filter(|model| self.get(model.id()).is_none())
            .map(|model| model.id().to_string())
            .collect()
    }

    pub fn openai_list(&self) -> Value {
        let data: Vec<Value> = self
            .models
            .iter()
            .filter_map(|model| serde_json::to_value(&model.remote).ok())
            .map(|mut entry| {
                if let Some(object) = entry.as_object_mut() {
                    object
                        .entry("object")
                        .or_insert_with(|| Value::String("model".to_string()));
                }
                entry
            })
            .collect();
        json!({ "object": "list", "data": data })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn remote() -> Value {
        json!({ "data": [
            { "id": "openai/gpt-oss-20b", "name": "GPT OSS 20B", "context_length": 131072, "supported_features": ["tools"], "is_tee": true },
            { "id": "meta/llama" }, { "id": "meta/llama" }
        ]})
    }

    #[test]
    fn catalog_preserves_verified_entries_as_listed() {
        let catalog = Catalog::from_remote(&remote(), 1).unwrap();
        assert_eq!(catalog.models.len(), 2);
        assert!(catalog.get("openai/gpt-oss-20b").is_some());
        let list = catalog.openai_list();
        assert_eq!(list["data"][0]["is_tee"], json!(true));
        assert_eq!(list["data"][0]["supported_features"], json!(["tools"]));
        assert!(Catalog::from_remote(&json!({ "data": [] }), 1).is_err());
        assert!(Catalog::from_remote(&json!({ "data": [{ "name": "no id" }] }), 1).is_err());
    }

    #[test]
    fn removed_models_are_reported_not_replaced() {
        let before = Catalog::from_remote(&remote(), 1).unwrap();
        let after =
            Catalog::from_remote(&json!({ "data": [{ "id": "openai/gpt-oss-20b" }] }), 2).unwrap();
        assert_eq!(after.removed_since(&before), ["meta/llama"]);
        assert_ne!(after.revision, before.revision);
    }
}
