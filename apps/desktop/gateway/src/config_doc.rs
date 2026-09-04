//! Format-preserving edits on agent config files: JSON (key order kept) and
//! TOML (comments and layout kept via `toml_edit`). A path is a key sequence;
//! containers along it are created on demand and pruned again when a removal
//! leaves them empty, so a disconnect leaves no empty shells behind.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use toml_edit::{DocumentMut, Item, Table, TableLike};
use yaml_edit::{path::YamlPath, YamlFile};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    Json,
    Toml,
    Yaml,
}

/// The scalar shapes a projection writes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ConfigValue {
    Str(String),
    Number(u64),
    Bool(bool),
    List(Vec<String>),
    Json(Value),
}

impl ConfigValue {
    pub fn display(&self) -> String {
        match self {
            ConfigValue::Str(value) => value.clone(),
            ConfigValue::Number(value) => value.to_string(),
            ConfigValue::Bool(value) => value.to_string(),
            ConfigValue::List(values) => {
                serde_json::to_string(values).unwrap_or_else(|_| values.join(", "))
            }
            ConfigValue::Json(value) => serde_json::to_string(value).unwrap_or_default(),
        }
    }

    fn from_json(value: &Value) -> Option<Self> {
        match value {
            Value::String(text) => Some(ConfigValue::Str(text.clone())),
            Value::Number(number) => number.as_u64().map(ConfigValue::Number),
            Value::Bool(value) => Some(ConfigValue::Bool(*value)),
            Value::Array(items) => items
                .iter()
                .map(|item| item.as_str().map(str::to_string))
                .collect::<Option<Vec<_>>>()
                .map(ConfigValue::List)
                .or_else(|| Some(ConfigValue::Json(value.clone()))),
            Value::Object(_) => Some(ConfigValue::Json(value.clone())),
            Value::Null => None,
        }
    }

    fn to_json(&self) -> Value {
        match self {
            ConfigValue::Str(text) => Value::String(text.clone()),
            ConfigValue::Number(number) => Value::from(*number),
            ConfigValue::Bool(value) => Value::from(*value),
            ConfigValue::List(items) => Value::Array(
                items
                    .iter()
                    .map(|item| Value::String(item.clone()))
                    .collect(),
            ),
            ConfigValue::Json(value) => value.clone(),
        }
    }

    fn from_toml(item: &Item) -> Option<Self> {
        if let Some(text) = item.as_str() {
            return Some(ConfigValue::Str(text.to_string()));
        }
        if let Some(number) = item.as_integer() {
            return u64::try_from(number).ok().map(ConfigValue::Number);
        }
        if let Some(value) = item.as_bool() {
            return Some(ConfigValue::Bool(value));
        }
        item.as_array().and_then(|array| {
            array
                .iter()
                .map(|value| value.as_str().map(str::to_string))
                .collect::<Option<Vec<_>>>()
                .map(ConfigValue::List)
        })
    }

    fn to_toml(&self) -> Result<Item, String> {
        Ok(match self {
            ConfigValue::Str(text) => toml_edit::value(text.as_str()),
            ConfigValue::Number(number) => toml_edit::value(
                i64::try_from(*number).map_err(|_| "number too large for TOML".to_string())?,
            ),
            ConfigValue::Bool(value) => toml_edit::value(*value),
            ConfigValue::List(items) => {
                let mut array = toml_edit::Array::new();
                for item in items {
                    array.push(item.as_str());
                }
                toml_edit::value(array)
            }
            ConfigValue::Json(_) => {
                return Err("complex JSON values cannot be written to TOML".to_string())
            }
        })
    }
}

#[derive(Clone, Debug)]
pub enum ConfigDoc {
    Json(Value),
    Toml(DocumentMut),
    Yaml(YamlFile),
}

impl ConfigDoc {
    /// Parse a config file; a missing or empty file is an empty document.
    pub fn parse(format: Format, text: &str) -> Result<Self, String> {
        match format {
            Format::Json => {
                if text.trim().is_empty() {
                    return Ok(Self::Json(Value::Object(Map::new())));
                }
                let value: Value =
                    serde_json::from_str(text).map_err(|_| "not valid JSON".to_string())?;
                if !value.is_object() {
                    return Err("not a JSON object".to_string());
                }
                Ok(Self::Json(value))
            }
            Format::Toml => text
                .parse::<DocumentMut>()
                .map(Self::Toml)
                .map_err(|_| "not valid TOML".to_string()),
            Format::Yaml => {
                let source = if text.trim().is_empty() { "{}\n" } else { text };
                source
                    .parse::<YamlFile>()
                    .map(Self::Yaml)
                    .map_err(|error| format!("not valid YAML: {error}"))
            }
        }
    }

    pub fn render(&self) -> Result<String, String> {
        match self {
            Self::Json(value) => serde_json::to_string_pretty(value)
                .map(|text| text + "\n")
                .map_err(|error| error.to_string()),
            Self::Toml(doc) => Ok(doc.to_string()),
            Self::Yaml(doc) => Ok(doc.to_string()),
        }
    }

    pub fn get_value(&self, path: &[&str]) -> Option<ConfigValue> {
        match self {
            Self::Json(root) => ConfigValue::from_json(json_get(root, path)?),
            Self::Toml(doc) => ConfigValue::from_toml(toml_get(doc.as_item(), path)?),
            Self::Yaml(file) => yaml_value(
                file.documents()
                    .next()?
                    .try_get_path(&path.join("."))
                    .ok()?,
            ),
        }
    }

    pub fn get_str(&self, path: &[&str]) -> Option<String> {
        match self.get_value(path)? {
            ConfigValue::Str(text) => Some(text),
            _ => None,
        }
    }

    pub fn set_value(&mut self, path: &[&str], value: &ConfigValue) -> Result<(), String> {
        let (leaf, parents) = split_leaf(path)?;
        match self {
            Self::Json(root) => {
                json_container(root, parents)?.insert(leaf.to_string(), value.to_json());
            }
            Self::Toml(doc) => {
                toml_container(doc.as_item_mut(), parents)?.insert(leaf, value.to_toml()?);
            }
            Self::Yaml(file) => {
                let doc = file
                    .documents()
                    .next()
                    .ok_or_else(|| "YAML document is empty".to_string())?;
                let path = path.join(".");
                match value {
                    ConfigValue::Str(value) => doc.try_set_path(&path, value.as_str()),
                    ConfigValue::Number(value) => {
                        let value = i64::try_from(*value)
                            .map_err(|_| "number too large for YAML".to_string())?;
                        doc.try_set_path(&path, value)
                    }
                    ConfigValue::Bool(value) => doc.try_set_path(&path, *value),
                    ConfigValue::List(_) | ConfigValue::Json(_) => {
                        return Err("complex values are not used in YAML projections".to_string())
                    }
                }
                .map_err(|error| format!("cannot edit YAML path {path}: {error}"))?;
            }
        }
        Ok(())
    }

    pub fn set_str(&mut self, path: &[&str], value: &str) -> Result<(), String> {
        self.set_value(path, &ConfigValue::Str(value.to_string()))
    }

    pub fn is_table(&self, path: &[&str]) -> bool {
        match self {
            Self::Json(root) => json_get(root, path).is_some_and(Value::is_object),
            Self::Toml(doc) => {
                toml_get(doc.as_item(), path).is_some_and(|item| item.as_table_like().is_some())
            }
            Self::Yaml(file) => file.documents().next().is_some_and(|doc| {
                doc.try_get_path(&path.join("."))
                    .is_ok_and(|node| node.is_mapping())
            }),
        }
    }

    /// Remove the key at `path`, then prune containers left empty above it.
    pub fn remove(&mut self, path: &[&str]) {
        let Ok((leaf, parents)) = split_leaf(path) else {
            return;
        };
        let emptied = match self {
            Self::Json(root) => json_get_mut(root, parents)
                .and_then(Value::as_object_mut)
                .map(|map| {
                    map.remove(leaf);
                    map.is_empty()
                }),
            Self::Toml(doc) => toml_get_mut(doc.as_item_mut(), parents)
                .and_then(|item| item.as_table_like_mut())
                .map(|table| {
                    table.remove(leaf);
                    table.is_empty()
                }),
            Self::Yaml(file) => {
                let Some(doc) = file.documents().next() else {
                    return;
                };
                let full = path.join(".");
                let removed = doc.try_remove_path(&full).is_ok();
                if removed {
                    for length in (1..path.len()).rev() {
                        let parent = path[..length].join(".");
                        let empty = doc
                            .try_get_path(&parent)
                            .ok()
                            .and_then(|node| node.as_mapping().cloned())
                            .is_some_and(|mapping| mapping.is_empty());
                        if empty {
                            let _ = doc.try_remove_path(&parent);
                        } else {
                            break;
                        }
                    }
                }
                None
            }
        };
        if emptied == Some(true) && !parents.is_empty() {
            self.remove(parents);
        }
    }
}

fn yaml_value(node: yaml_edit::YamlNode) -> Option<ConfigValue> {
    if let Some(value) = node.to_bool() {
        return Some(ConfigValue::Bool(value));
    }
    if let Some(value) = node.to_i64().and_then(|value| u64::try_from(value).ok()) {
        return Some(ConfigValue::Number(value));
    }
    node.as_scalar()
        .map(|scalar| ConfigValue::Str(scalar.as_string()))
}

fn split_leaf<'a>(path: &'a [&'a str]) -> Result<(&'a str, &'a [&'a str]), String> {
    path.split_last()
        .map(|(leaf, parents)| (*leaf, parents))
        .ok_or_else(|| "empty config path".to_string())
}

fn not_a_table(path: &[&str]) -> String {
    format!("{} is not a table", path.join("."))
}

fn json_get<'a>(root: &'a Value, path: &[&str]) -> Option<&'a Value> {
    path.iter().try_fold(root, |node, key| node.get(*key))
}

fn json_get_mut<'a>(root: &'a mut Value, path: &[&str]) -> Option<&'a mut Value> {
    path.iter().try_fold(root, |node, key| node.get_mut(*key))
}

fn json_container<'a>(
    root: &'a mut Value,
    path: &[&str],
) -> Result<&'a mut Map<String, Value>, String> {
    let mut node = root;
    for key in path {
        node = node
            .as_object_mut()
            .ok_or_else(|| not_a_table(path))?
            .entry(*key)
            .or_insert_with(|| Value::Object(Map::new()));
    }
    node.as_object_mut().ok_or_else(|| not_a_table(path))
}

fn toml_get<'a>(root: &'a Item, path: &[&str]) -> Option<&'a Item> {
    path.iter().try_fold(root, |node, key| node.get(*key))
}

fn toml_get_mut<'a>(root: &'a mut Item, path: &[&str]) -> Option<&'a mut Item> {
    path.iter().try_fold(root, |node, key| node.get_mut(*key))
}

fn toml_container<'a>(root: &'a mut Item, path: &[&str]) -> Result<&'a mut dyn TableLike, String> {
    let mut node = root;
    for key in path {
        let table = node.as_table_like_mut().ok_or_else(|| not_a_table(path))?;
        if table.get(key).is_none() {
            // Implicit tables only print a header once they hold values, so
            // `[model_providers.x]` renders as a single header.
            let mut child = Table::new();
            child.set_implicit(true);
            table.insert(key, Item::Table(child));
        }
        node = table.get_mut(key).ok_or_else(|| not_a_table(path))?;
    }
    node.as_table_like_mut().ok_or_else(|| not_a_table(path))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn toml_lists_and_numbers_round_trip_and_prune() {
        let mut doc = ConfigDoc::parse(Format::Toml, "model = \"x\"\n").unwrap();
        let args = ConfigValue::List(vec!["--agent-token".into(), "codex".into()]);
        doc.set_value(&["model_providers", "p", "auth", "args"], &args)
            .unwrap();
        doc.set_value(
            &["model_providers", "p", "auth", "timeout_ms"],
            &ConfigValue::Number(5),
        )
        .unwrap();
        let text = doc.render().unwrap();
        assert!(text.contains("[model_providers.p.auth]"));
        assert!(text.contains("args = [\"--agent-token\", \"codex\"]"));
        assert_eq!(
            doc.get_value(&["model_providers", "p", "auth", "args"]),
            Some(args)
        );
        doc.remove(&["model_providers", "p", "auth", "args"]);
        doc.remove(&["model_providers", "p", "auth", "timeout_ms"]);
        assert_eq!(doc.render().unwrap(), "model = \"x\"\n");
    }

    #[test]
    fn json_numbers_are_typed() {
        let mut doc = ConfigDoc::parse(Format::Json, "{}").unwrap();
        doc.set_value(&["limit", "context"], &ConfigValue::Number(4096))
            .unwrap();
        assert_eq!(
            doc.render().unwrap(),
            "{\n  \"limit\": {\n    \"context\": 4096\n  }\n}\n"
        );
        assert_eq!(
            doc.get_value(&["limit", "context"]),
            Some(ConfigValue::Number(4096))
        );
    }

    #[test]
    fn yaml_edits_preserve_comments_and_prune_owned_tables() {
        let mut doc = ConfigDoc::parse(Format::Yaml, "# user comment\ntheme: dark\n").unwrap();
        doc.set_value(
            &["providers", "private-ai-gateway", "discover_models"],
            &ConfigValue::Bool(true),
        )
        .unwrap();
        assert_eq!(
            doc.get_value(&["providers", "private-ai-gateway", "discover_models"]),
            Some(ConfigValue::Bool(true))
        );
        doc.remove(&["providers", "private-ai-gateway", "discover_models"]);
        let text = doc.render().unwrap();
        assert!(text.contains("# user comment"));
        assert!(text.contains("theme: dark"));
        assert!(!text.contains("private-ai-gateway"));
    }
}
