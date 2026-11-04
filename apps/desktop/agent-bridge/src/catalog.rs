//! The verified remote catalog (`GET /v1/models` through the ACI verifier) is
//! the only source of model truth. Entries are validated and preserved as the
//! service lists them; nothing is added or inferred. The display name shown
//! in the app and agent pickers is derived from each entry and is part of the
//! catalog revision, so connected agents pick up a new name like any other
//! catalog update.

use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};

pub(crate) const IMAGES_GENERATIONS_PATH: &str = "/v1/images/generations";
pub(crate) const AUDIO_TRANSCRIPTIONS_PATH: &str = "/v1/audio/transcriptions";

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
    /// System One relay surface.
    SystemOne,
    Images,
    AudioTranscriptions,
}

impl Surface {
    pub fn path(self) -> &'static str {
        match self {
            Surface::ChatCompletions => "/v1/chat/completions",
            Surface::Messages => "/v1/messages",
            Surface::Responses => "/v1/responses",
            Surface::SystemOne => "/v1/systemone",
            Surface::Images => IMAGES_GENERATIONS_PATH,
            Surface::AudioTranscriptions => AUDIO_TRANSCRIPTIONS_PATH,
        }
    }

    fn output_modality(self) -> &'static str {
        match self {
            Surface::SystemOne => "decisions",
            Surface::Images => "image",
            Surface::AudioTranscriptions => "transcription",
            Surface::ChatCompletions | Surface::Messages | Surface::Responses => "text",
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CatalogModel {
    pub remote: RemoteModel,
    display_name: String,
}

impl CatalogModel {
    pub fn supports(&self, surface: Surface) -> bool {
        self.outputs(surface.output_modality())
            && !(matches!(
                surface,
                Surface::ChatCompletions | Surface::Messages | Surface::Responses
            ) && self.outputs("transcription"))
    }

    fn outputs(&self, modality: &str) -> bool {
        self.remote
            .extra
            .get("output_modalities")
            // Text and decisions accept legacy catalogs without modality metadata;
            // images and transcription require an explicit advertisement.
            .map_or(matches!(modality, "text" | "decisions"), |modalities| {
                modalities
                    .as_array()
                    .is_some_and(|modalities| modalities.iter().any(|value| value == modality))
            })
    }

    pub fn id(&self) -> &str {
        &self.remote.id
    }
    pub fn display_name(&self) -> &str {
        &self.display_name
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
    /// floats, so `0.00000005` becomes exactly the f64 nearest `0.05`. The
    /// result keeps `f64::DIGITS` significant digits, which serde_json's
    /// default parser reads back exactly for per-million prices from 1e-8 up
    /// to 1e15 (a significand below 2^53 and a decimal exponent within ±22).
    /// A longer form can read back as a neighbouring f64, and a fresh
    /// connection would then differ from its own record.
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
        let price: f64 = format!("{price:.*e}", f64::DIGITS as usize - 1)
            .parse()
            .ok()?;
        (price.is_finite() && price >= 0.0).then_some(price)
    }
}

/// The name people see; requests always use the unchanged `id`. `[TEE]` marks
/// models the service reports with `is_tee`, which the RedPill and Phala
/// presets set on every model. A custom ACI service may also route to
/// upstreams without a TEE, so an unmarked model gets no suffix.
fn display_name(remote: &RemoteModel) -> String {
    let name = remote
        .name
        .as_deref()
        .filter(|name| !name.trim().is_empty())
        .unwrap_or(&remote.id);
    if remote.extra.get("is_tee").and_then(Value::as_bool) == Some(true) && !name.ends_with("[TEE]")
    {
        format!("{name} [TEE]")
    } else {
        name.to_string()
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
            let model = CatalogModel {
                display_name: display_name(&entry),
                remote: entry,
            };
            let canonical = serde_json::to_vec(&model).map_err(|error| error.to_string())?;
            hasher.update((canonical.len() as u64).to_be_bytes());
            hasher.update(&canonical);
            models.push(model);
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
