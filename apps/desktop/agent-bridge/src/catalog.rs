//! The verified remote catalog (`GET /v1/models` through the ACI verifier) is
//! the only source of model truth. Entries are validated and preserved as the
//! service lists them; nothing is added or inferred.

use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::collections::HashSet;

const ENDPOINT_INVENTORY_SCHEMA_VERSION: u32 = 2;

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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    reasoning_effort: Option<String>,
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
    checks: EndpointChecks,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct EndpointChecks {
    streaming: EndpointCheck,
    tools: EndpointCheck,
    tool_result: EndpointCheck,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct EndpointCheck {
    status: ObservationStatus,
    reason: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    http_status: Option<u16>,
}

impl EndpointChecks {
    fn entries(&self) -> [&EndpointCheck; 3] {
        [&self.streaming, &self.tools, &self.tool_result]
    }

    fn supported(&self) -> bool {
        self.entries()
            .iter()
            .all(|check| check.status == ObservationStatus::Supported)
    }
}

fn supports_compatibility(
    status: ObservationStatus,
    reason: &str,
    http_status: Option<u16>,
) -> bool {
    status == ObservationStatus::Supported
        && http_status.is_some_and(|status| {
            (200..300).contains(&status)
                || (status == 429 && reason == "temporary_rate_or_quota_limit")
                || ((matches!(status, 408 | 425) || (500..600).contains(&status))
                    && status != 501
                    && reason == "temporary_server_error")
        })
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
        if inventory.schema_version != ENDPOINT_INVENTORY_SCHEMA_VERSION
            || inventory.endpoint != "https://tee.redpill.ai"
            || inventory
                .reasoning_effort
                .as_deref()
                .is_some_and(|effort| !matches!(effort, "low" | "medium" | "high"))
            || inventory.results.is_empty()
            || inventory.results.len() > 10_000
            || inventory.results.iter().any(|entry| {
                entry.model.trim().is_empty()
                    || entry.model.len() > 256
                    || entry.reason.len() > 256
                    || entry.checks.entries().iter().any(|check| {
                        check.reason.is_empty()
                            || check.reason.len() > 256
                            || check
                                .http_status
                                .is_some_and(|status| !(100..600).contains(&status))
                            || (check.status == ObservationStatus::Supported
                                && !supports_compatibility(
                                    check.status,
                                    &check.reason,
                                    check.http_status,
                                ))
                    })
                    || !matches!(
                        entry.endpoint.as_str(),
                        "/v1/chat/completions" | "/v1/messages" | "/v1/responses"
                    )
                    || !pairs.insert((&entry.model, &entry.endpoint))
                    || entry
                        .http_status
                        .is_some_and(|status| !(100..600).contains(&status))
                    || (entry.status == ObservationStatus::Supported
                        && !supports_compatibility(entry.status, &entry.reason, entry.http_status))
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
        // Released apps refresh from this repository path, so the file stays there.
        Self::parse(include_bytes!("../../gateway/src/endpoint-support.json"))
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
    /// None means agent capabilities have not been probed for this endpoint.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_surfaces: Option<Vec<Surface>>,
}

impl CatalogModel {
    pub fn supports(&self, surface: Surface) -> bool {
        self.supported_surfaces
            .as_ref()
            .is_none_or(|surfaces| surfaces.contains(&surface))
    }

    pub fn supports_agent(&self, surface: Surface) -> bool {
        self.supports(surface)
            && self
                .agent_surfaces
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
    /// per one million tokens and omits malformed or negative fields. The
    /// decimal point is shifted in the price text instead of multiplying
    /// floats, so `0.00000005` becomes exactly the f64 nearest `0.05`, whose
    /// shortest form every JSON and YAML parser reads back identically.
    pub fn price_per_million(&self, name: &str) -> Option<f64> {
        let text = match self.remote.extra.get("pricing")?.get(name)? {
            Value::Number(value) => value.to_string(),
            Value::String(value) => value.trim().to_string(),
            _ => return None,
        };
        let (mantissa, exponent) = match text.split_once(['e', 'E']) {
            Some((mantissa, exponent)) => (mantissa, exponent.parse::<i32>().ok()?),
            None => (text.as_str(), 0),
        };
        let price: f64 = format!("{mantissa}e{}", exponent.checked_add(6)?)
            .parse()
            .ok()?;
        (price.is_finite() && price >= 0.0).then_some(price)
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
                agent_surfaces: None,
            });
        }
        let revision = hex::encode(hasher.finalize());
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

    pub fn for_agent_surface(&self, surface: Surface) -> Self {
        let mut catalog = self.for_surface(surface);
        catalog.models.retain(|model| model.supports_agent(surface));
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
            model.agent_surfaces = Some(
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
                            && entry.checks.supported()
                    })
                })
                .collect(),
            );
        }
        let bytes = serde_json::to_vec(&self.models).map_err(|error| error.to_string())?;
        self.revision = hex::encode(Sha256::digest(bytes));
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
