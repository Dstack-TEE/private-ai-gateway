//! The verified remote catalog (`GET /v1/models` through the ACI sidecar) is
//! the only source of model truth. Entries are validated and preserved as the
//! service lists them; nothing is added or inferred.

use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::collections::HashSet;

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

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EndpointInventory {
    schema_version: u32,
    endpoint: String,
    checked_at: chrono::DateTime<chrono::Utc>,
    results: Vec<EndpointObservation>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct EndpointObservation {
    model: String,
    endpoint: String,
    status: ObservationStatus,
    reason: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    http_status: Option<u16>,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum ObservationStatus {
    Supported,
    Unavailable,
    Inconclusive,
}

impl EndpointInventory {
    pub fn is_newer_than(&self, other: &Self) -> bool {
        self.checked_at > other.checked_at
    }

    pub fn parse(bytes: &[u8]) -> Result<Self, String> {
        let inventory: Self = serde_json::from_slice(bytes)
            .map_err(|_| "Invalid model endpoint inventory".to_string())?;
        let mut pairs = HashSet::new();
        if inventory.schema_version != 1
            || inventory.endpoint != "https://tee.redpill.ai"
            || inventory.results.is_empty()
            || inventory.results.len() > 10_000
            || inventory.results.iter().any(|entry| {
                entry.model.trim().is_empty()
                    || entry.model.len() > 256
                    || entry.reason.len() > 256
                    || !matches!(
                        entry.endpoint.as_str(),
                        "/v1/chat/completions" | "/v1/messages" | "/v1/responses"
                    )
                    || !pairs.insert((&entry.model, &entry.endpoint))
                    || entry
                        .http_status
                        .is_some_and(|status| !(100..600).contains(&status))
                    || (entry.status == ObservationStatus::Supported
                        && !entry
                            .http_status
                            .is_some_and(|status| (200..300).contains(&status)))
            })
        {
            return Err("Invalid model endpoint inventory".to_string());
        }
        let models: HashSet<_> = inventory.results.iter().map(|entry| &entry.model).collect();
        if pairs.len() != models.len() * 3 {
            return Err("Incomplete model endpoint inventory".to_string());
        }
        Ok(inventory)
    }

    pub fn bundled() -> Result<Self, String> {
        Self::parse(include_bytes!("endpoint-support.json"))
    }
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
    /// Local endpoint observations, separate from the verified remote metadata.
    /// None means this endpoint has no compatibility inventory.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supported_surfaces: Option<Vec<Surface>>,
}

impl CatalogModel {
    pub fn supports(&self, surface: Surface) -> bool {
        self.supported_surfaces
            .as_ref()
            .is_none_or(|surfaces| surfaces.contains(&surface))
    }

    pub fn id(&self) -> &str {
        &self.remote.id
    }
    pub fn display_name(&self) -> &str {
        self.remote.name.as_deref().unwrap_or(&self.remote.id)
    }

    pub fn bool_field(&self, name: &str) -> Option<bool> {
        self.remote.extra.get(name).and_then(Value::as_bool)
    }

    pub fn string_field(&self, name: &str) -> Option<String> {
        self.remote
            .extra
            .get(name)
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .map(str::to_string)
    }

    pub fn string_array(&self, name: &str) -> Vec<String> {
        self.remote
            .extra
            .get(name)
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .map(str::to_string)
            .collect()
    }

    /// Remote pricing is per token. The UI presents the same validated value
    /// per one million tokens and omits malformed or negative fields.
    pub fn price_per_million(&self, name: &str) -> Option<f64> {
        let value = self.remote.extra.get("pricing")?.get(name)?;
        let per_token = match value {
            Value::Number(value) => value.as_f64()?,
            Value::String(value) => value.parse::<f64>().ok()?,
            _ => return None,
        };
        (per_token.is_finite() && per_token >= 0.0).then_some(per_token * 1_000_000.0)
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
            models.push(CatalogModel {
                remote: entry,
                supported_surfaces: None,
            });
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

    pub fn for_surface(&self, surface: Surface) -> Self {
        let mut catalog = self.clone();
        catalog.models.retain(|model| model.supports(surface));
        catalog
    }

    /// Apply only to the two shared preset services, never a custom route that
    /// happens to expose the same model IDs. Unknown observations are excluded.
    pub fn has_endpoint_inventory(endpoint: &str) -> bool {
        let Ok(url) = reqwest::Url::parse(endpoint) else {
            return false;
        };
        if url.scheme() != "https"
            || !url.username().is_empty()
            || url.password().is_some()
            || url.port_or_known_default() != Some(443)
            || url.query().is_some()
            || url.fragment().is_some()
            || !matches!(url.path(), "/" | "/v1" | "/v1/")
            || !matches!(
                url.host_str(),
                Some("tee.redpill.ai" | "inference.phala.com")
            )
        {
            return false;
        }
        true
    }

    pub fn apply_endpoint_inventory(
        &mut self,
        endpoint: &str,
        inventory: &EndpointInventory,
    ) -> Result<(), String> {
        if !Self::has_endpoint_inventory(endpoint) {
            return Ok(());
        }
        for model in &mut self.models {
            model.supported_surfaces = Some(
                [
                    Surface::ChatCompletions,
                    Surface::Messages,
                    Surface::Responses,
                ]
                .into_iter()
                .filter(|surface| {
                    inventory.results.iter().any(|entry| {
                        entry.model == model.id()
                            && entry.endpoint == surface.path()
                            && entry.status == ObservationStatus::Supported
                    })
                })
                .collect(),
            );
        }
        let bytes = serde_json::to_vec(&self.models).map_err(|error| error.to_string())?;
        self.revision = format!("{:x}", Sha256::digest(bytes));
        Ok(())
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
mod tests;
