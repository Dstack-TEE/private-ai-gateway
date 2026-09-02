//! The verified remote catalog (`GET /v1/models` through the ACI sidecar) is
//! the only source of model truth. Nothing is added to it and nothing is
//! inferred from a model's name or from generic feature flags. The standard
//! catalog declares no per-model protocol capabilities, so every surface the
//! service exposes is reported as gateway-routed with capability undeclared;
//! a future versioned capability extension can tighten this without changing
//! callers.

use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};

/// One entry of the service catalog. Fields the service returns but this app
/// does not interpret ride along in `extra`, so the local `/v1/models` relays
/// the verified list without loss.
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

/// The wire surfaces the local proxy exposes.
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

/// What can honestly be said about serving a model on a surface. A plain
/// `/v1/models` proves availability only; a surface is `Declared` solely when
/// the service publishes the versioned `aci_capabilities` extension for it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "level", content = "version", rename_all = "snake_case")]
pub enum Support {
    /// The service declares (extension version) that every catalog model is
    /// served on this surface.
    Declared(u32),
    /// Nothing is declared; requests are refused rather than guessed.
    Undeclared,
}

impl Support {
    pub fn allows_requests(&self) -> bool {
        matches!(self, Support::Declared(_))
    }

    pub fn label(&self) -> &'static str {
        match self {
            Support::Declared(_) => "declared",
            Support::Undeclared => "undeclared",
        }
    }
}

/// The service's versioned capability declaration (`aci_capabilities`).
/// Absent on services that predate it, which leaves every surface undeclared.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Capabilities {
    pub version: u32,
    #[serde(default)]
    pub surfaces: std::collections::BTreeMap<String, String>,
}

impl Capabilities {
    pub const CURRENT_VERSION: u32 = 1;

    fn support(&self, surface: Surface) -> Support {
        let key = match surface {
            Surface::ChatCompletions => "chat_completions",
            Surface::Messages => "messages",
            Surface::Responses => "responses",
        };
        if self.version == Self::CURRENT_VERSION
            && self.surfaces.get(key).map(String::as_str) == Some("all")
        {
            Support::Declared(self.version)
        } else {
            Support::Undeclared
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CatalogModel {
    pub remote: RemoteModel,
    pub chat_completions: Support,
    pub messages: Support,
    pub responses: Support,
}

impl CatalogModel {
    fn derive(remote: RemoteModel, capabilities: Option<&Capabilities>) -> Self {
        let support = |surface| {
            capabilities.map_or(Support::Undeclared, |declared| declared.support(surface))
        };
        Self {
            chat_completions: support(Surface::ChatCompletions),
            messages: support(Surface::Messages),
            responses: support(Surface::Responses),
            remote,
        }
    }

    pub fn id(&self) -> &str {
        &self.remote.id
    }

    pub fn display_name(&self) -> &str {
        self.remote.name.as_deref().unwrap_or(&self.remote.id)
    }

    pub fn support(&self, surface: Surface) -> &Support {
        match surface {
            Surface::ChatCompletions => &self.chat_completions,
            Surface::Messages => &self.messages,
            Surface::Responses => &self.responses,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Catalog {
    /// SHA-256 of the service entries; changes whenever the service list does.
    pub revision: String,
    pub fetched_at: u64,
    pub models: Vec<CatalogModel>,
    /// The service's capability declaration, when it publishes one.
    #[serde(default)]
    pub capabilities: Option<Capabilities>,
}

impl Catalog {
    /// Build from the service's `/v1/models` body. Malformed entries fail the
    /// whole refresh rather than being dropped.
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
        let capabilities = match body.get("aci_capabilities") {
            None => None,
            Some(value) => Some(
                serde_json::from_value::<Capabilities>(value.clone())
                    .map_err(|error| format!("the capability declaration is malformed: {error}"))?,
            ),
        };
        let mut hasher = Sha256::new();
        if let Some(capabilities) = &capabilities {
            let canonical = serde_json::to_vec(capabilities).map_err(|error| error.to_string())?;
            hasher.update((canonical.len() as u64).to_be_bytes());
            hasher.update(&canonical);
        }
        let mut models: Vec<CatalogModel> = Vec::with_capacity(entries.len());
        for entry in entries {
            if entry.id.trim().is_empty() {
                return Err("the model list contains an entry with an empty id".to_string());
            }
            if models.iter().any(|model| model.id() == entry.id) {
                continue;
            }
            let canonical = serde_json::to_vec(&entry).map_err(|error| error.to_string())?;
            hasher.update((canonical.len() as u64).to_be_bytes());
            hasher.update(&canonical);
            models.push(CatalogModel::derive(entry, capabilities.as_ref()));
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
            capabilities,
        })
    }

    /// Whether the service declares every catalog model on `surface`.
    pub fn declares(&self, surface: Surface) -> bool {
        self.capabilities
            .as_ref()
            .is_some_and(|capabilities| capabilities.support(surface).allows_requests())
    }

    pub fn get(&self, id: &str) -> Option<&CatalogModel> {
        self.models.iter().find(|model| model.id() == id)
    }

    /// Ids present before but no longer served.
    pub fn removed_since(&self, previous: &Catalog) -> Vec<String> {
        previous
            .models
            .iter()
            .filter(|model| self.get(model.id()).is_none())
            .map(|model| model.id().to_string())
            .collect()
    }

    /// The OpenAI-style list: the service entries as returned, plus `object`.
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
            { "id": "openai/gpt-oss-20b", "name": "GPT OSS 20B", "context_length": 131072,
              "max_output_length": 131072, "supported_features": ["tools"], "is_tee": true },
            { "id": "meta/llama" },
            { "id": "meta/llama" }
        ]})
    }

    #[test]
    fn catalog_is_exactly_what_the_service_returned() {
        let catalog = Catalog::from_remote(&remote(), 1).unwrap();
        assert_eq!(catalog.models.len(), 2);
        assert!(catalog.get("openai/gpt-oss-20b").is_some());
        assert!(catalog.get("gpt-5").is_none());
        let list = catalog.openai_list();
        assert_eq!(list["data"][0]["is_tee"], json!(true));
        assert_eq!(list["data"][0]["supported_features"], json!(["tools"]));
        assert_eq!(list["data"][0]["object"], json!("model"));
        assert_eq!(list["data"].as_array().unwrap().len(), 2);

        assert!(Catalog::from_remote(&json!({ "data": [] }), 1).is_err());
        assert!(Catalog::from_remote(&json!({ "data": [{ "name": "no id" }] }), 1).is_err());
        assert!(Catalog::from_remote(&json!({ "data": [{ "id": " " }] }), 1).is_err());
        assert!(Catalog::from_remote(&json!({ "models": [] }), 1).is_err());
    }

    #[test]
    fn surfaces_are_undeclared_without_the_capability_extension() {
        let catalog = Catalog::from_remote(&remote(), 1).unwrap();
        for model in &catalog.models {
            for surface in [
                Surface::ChatCompletions,
                Surface::Messages,
                Surface::Responses,
            ] {
                assert_eq!(model.support(surface), &Support::Undeclared);
                assert!(!model.support(surface).allows_requests());
            }
        }
        assert!(!catalog.declares(Surface::Messages));
    }

    #[test]
    fn the_versioned_extension_declares_surfaces_exactly() {
        let mut body = remote();
        body["aci_capabilities"] = json!({
            "version": 1,
            "surfaces": { "chat_completions": "all", "messages": "all", "responses": "undeclared" }
        });
        let catalog = Catalog::from_remote(&body, 1).unwrap();
        assert!(catalog.declares(Surface::ChatCompletions));
        assert!(catalog.declares(Surface::Messages));
        assert!(!catalog.declares(Surface::Responses));
        assert_eq!(catalog.models[0].messages, Support::Declared(1));
        assert_eq!(catalog.models[0].responses, Support::Undeclared);

        // An unknown version or malformed declaration never widens support.
        body["aci_capabilities"]["version"] = json!(2);
        assert!(!Catalog::from_remote(&body, 1)
            .unwrap()
            .declares(Surface::Messages));
        body["aci_capabilities"] = json!("yes");
        assert!(Catalog::from_remote(&body, 1).is_err());
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
