//! Format-preserving edits on agent config files: JSON and JSON5 (everything
//! but the edited value kept via the `jsonc-parser` CST) and TOML (comments
//! and layout kept via `toml_edit`). A path is a key sequence;
//! containers along it are created on demand and pruned again when a removal
//! leaves them empty, so a disconnect leaves no empty shells behind. YAML
//! lists can also be edited by keyed item (`EntryKey`), where edits are limited
//! to what restores byte for byte.

use jsonc_parser::cst::{CstInputValue, CstRootNode};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use toml_edit::{DocumentMut, Item, Table, TableLike};
use yaml_edit::{path::YamlPath, AsYaml, Mapping, Sequence, YamlFile, YamlNode};

/// Read-only JSONC inspection. Never render this value back over a user's file:
/// the regular JSON writer does not preserve comments.
pub(crate) fn parse_jsonc(text: &str) -> Result<Value, String> {
    if text.is_empty() {
        return Ok(Value::Object(Map::new()));
    }
    let value: Value = jsonc_parser::parse_to_serde_value(
        text,
        &jsonc_parser::ParseOptions {
            allow_comments: true,
            allow_trailing_commas: true,
            ..json_options(true)
        },
    )
    .map_err(|_| "not valid JSONC".to_string())?;
    if !value.is_object() {
        return Err("not a JSONC object".to_string());
    }
    Ok(value)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    Json,
    Json5,
    Toml,
    Yaml,
    /// YAML whose root is a list, such as a Cordis patch list.
    YamlList,
}

/// The scalar and structured values a projection writes.
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

    pub(crate) fn from_json(value: &Value) -> Option<Self> {
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

    pub(crate) fn to_json(&self) -> Value {
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
    // JSON source, edited through the CST like JSON5 but parsed strictly.
    Json(String),
    // Store source, not Rc-backed CST nodes: clones are independent and Send.
    Json5(String),
    Toml(DocumentMut),
    Yaml(YamlFile),
    /// A live view of one mapping item of a YAML list: edits through it change
    /// the document it came from, which is the one to render.
    YamlItem(Mapping),
}

/// A mapping item of a YAML list, selected by the value of one of its keys,
/// such as `{id: llm-pi-ai}` in a Cordis patch list.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EntryKey {
    pub key: String,
    pub id: String,
}

impl EntryKey {
    /// The item whose `id` is `id`.
    pub fn id(id: &str) -> Self {
        Self {
            key: "id".to_string(),
            id: id.to_string(),
        }
    }
}

impl ConfigDoc {
    /// Parse a config file; a missing or empty file is an empty document.
    pub fn parse(format: Format, text: &str) -> Result<Self, String> {
        match format {
            Format::Json | Format::Json5 => {
                let strict = format == Format::Json;
                let text = if text.trim().is_empty() { "{}\n" } else { text };
                json_root(text, strict)?;
                let text = text.to_string();
                Ok(if strict {
                    Self::Json(text)
                } else {
                    Self::Json5(text)
                })
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
                    .map_err(|_| "not valid YAML".to_string())
            }
            Format::YamlList => {
                let source = if text.trim().is_empty() { "[]\n" } else { text };
                let file = source
                    .parse::<YamlFile>()
                    .map_err(|_| "not valid YAML".to_string())?;
                let mut documents = file.documents();
                if documents.next().and_then(|doc| doc.as_sequence()).is_none()
                    || documents.next().is_some()
                {
                    return Err("not a single YAML list".to_string());
                }
                Ok(Self::Yaml(file))
            }
        }
    }

    pub fn render(&self) -> Result<String, String> {
        match self {
            Self::Json5(text) | Self::Json(text) => Ok(text.clone()),
            Self::Toml(doc) => Ok(doc.to_string()),
            Self::Yaml(doc) => {
                let text = doc.to_string();
                text.parse::<YamlFile>()
                    .map_err(|_| "YAML edit produced invalid syntax".to_string())?;
                Ok(text)
            }
            Self::YamlItem(_) => Err("render the document that holds this item".to_string()),
        }
    }

    /// The YAML node at `path` below the document root or the list item.
    fn yaml_node(&self, path: &[&str]) -> Option<YamlNode> {
        match self {
            Self::Yaml(file) => {
                let document = file.documents().next()?;
                if path.is_empty() {
                    document
                        .as_mapping()
                        .map(YamlNode::Mapping)
                        .or_else(|| document.as_sequence().map(YamlNode::Sequence))
                } else {
                    document.try_get_path(&path.join(".")).ok()
                }
            }
            Self::YamlItem(item) if path.is_empty() => Some(YamlNode::Mapping(item.clone())),
            Self::YamlItem(item) => item.try_get_path(&path.join(".")).ok(),
            _ => None,
        }
    }

    pub fn get_value(&self, path: &[&str]) -> Option<ConfigValue> {
        match self {
            Self::Json(_) | Self::Json5(_) => {
                ConfigValue::from_json(json_get(&self.json_tree()?, path)?)
            }
            Self::Toml(doc) => ConfigValue::from_toml(toml_get(doc.as_item(), path)?),
            Self::Yaml(_) | Self::YamlItem(_) => yaml_value(self.yaml_node(path)?),
        }
    }

    /// The value of a JSON document; other formats have none.
    pub fn as_json(&self) -> Option<Value> {
        match self {
            Self::Json(text) => json_value(text, true).ok(),
            _ => None,
        }
    }

    /// The value of a JSON or JSON5 document, its numbers read as connection
    /// records read them.
    fn json_tree(&self) -> Option<Value> {
        match self {
            Self::Json(text) => json_value(text, true).ok(),
            Self::Json5(text) => json_value(text, false).ok().map(recorded),
            _ => None,
        }
    }

    pub fn get_str(&self, path: &[&str]) -> Option<String> {
        match self.get_value(path)? {
            ConfigValue::Str(text) => Some(text),
            _ => None,
        }
    }

    pub fn contains(&self, path: &[&str]) -> bool {
        match self {
            Self::Json(_) | Self::Json5(_) => self
                .json_tree()
                .is_some_and(|root| json_get(&root, path).is_some()),
            Self::Toml(doc) => toml_get(doc.as_item(), path).is_some(),
            Self::Yaml(_) | Self::YamlItem(_) => !path.is_empty() && self.yaml_node(path).is_some(),
        }
    }

    pub fn set_value(&mut self, path: &[&str], value: &ConfigValue) -> Result<(), String> {
        let (leaf, parents) = split_leaf(path)?;
        let strict = matches!(self, Self::Json(_));
        match self {
            Self::Json(text) | Self::Json5(text) => {
                *text = edit_json(text, path, Some(value.to_json()), strict)?;
            }
            Self::Toml(doc) => {
                toml_container(doc.as_item_mut(), parents)?.insert(leaf, value.to_toml()?);
            }
            Self::Yaml(file) => yaml_set(&yaml_document(file)?, path, value)?,
            Self::YamlItem(item) => yaml_set(item, path, value)?,
        }
        Ok(())
    }

    pub fn set_str(&mut self, path: &[&str], value: &str) -> Result<(), String> {
        self.set_value(path, &ConfigValue::Str(value.to_string()))
    }

    pub fn is_table(&self, path: &[&str]) -> bool {
        match self {
            Self::Json(_) | Self::Json5(_) => self
                .json_tree()
                .is_some_and(|root| json_get(&root, path).is_some_and(Value::is_object)),
            Self::Toml(doc) => {
                toml_get(doc.as_item(), path).is_some_and(|item| item.as_table_like().is_some())
            }
            Self::Yaml(_) | Self::YamlItem(_) => {
                !path.is_empty() && self.yaml_node(path).is_some_and(|node| node.is_mapping())
            }
        }
    }

    /// Remove the key at `path`, then prune containers left empty above it.
    /// Inside a list item only the key itself is removed.
    pub fn remove(&mut self, path: &[&str]) -> Result<(), String> {
        let Ok((leaf, parents)) = split_leaf(path) else {
            return Ok(());
        };
        let emptied = match self {
            Self::Json5(text) => {
                // Do not prune JSON5 parents: an empty object may contain user comments.
                *text = edit_json(text, path, None, false)?;
                None
            }
            Self::Json(text) => {
                // Strict JSON has no comments, so an emptied object holds nothing.
                *text = edit_json(text, path, None, true)?;
                json_value(text, true)
                    .ok()
                    .and_then(|root| json_get(&root, parents)?.as_object().map(Map::is_empty))
            }
            Self::Toml(doc) => toml_get_mut(doc.as_item_mut(), parents)
                .and_then(|item| item.as_table_like_mut())
                .map(|table| {
                    table.remove(leaf);
                    table.is_empty()
                }),
            Self::YamlItem(_) => return self.remove_exact(path),
            Self::Yaml(file) => {
                let Some(doc) = file.documents().next() else {
                    return Ok(());
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
                            .is_some_and(|mapping| {
                                mapping.is_empty()
                                    && !mapping.as_node().is_some_and(|node| {
                                        node.descendants_with_tokens().any(|element| {
                                            element.kind() == yaml_edit::SyntaxKind::COMMENT
                                        })
                                    })
                            });
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
            self.remove(parents)?;
        }
        Ok(())
    }

    /// Remove exactly the key at `path`, never its parents. Other formats
    /// fall back to [`Self::remove`].
    pub fn remove_exact(&mut self, path: &[&str]) -> Result<(), String> {
        let path_text = path.join(".");
        let removed = match self {
            Self::Yaml(file) => file
                .documents()
                .next()
                .map(|doc| doc.try_remove_path(&path_text)),
            Self::YamlItem(item) => Some(item.try_remove_path(&path_text)),
            _ => return self.remove(path),
        };
        match removed {
            Some(Err(yaml_edit::path::PathError::NotFound { .. })) | Some(Ok(_)) | None => Ok(()),
            Some(Err(_)) => Err(format!("cannot edit YAML path {path_text}")),
        }
    }

    /// Create an empty mapping at `path`. In YAML it is written as a block
    /// mapping once it has entries, so keys added to it and removed again
    /// restore it byte for byte.
    pub fn create_mapping(&mut self, path: &[&str]) -> Result<(), String> {
        let path_text = path.join(".");
        match self {
            Self::Yaml(file) => {
                yaml_document(file)?.try_set_path(&path_text, Mapping::new_pending_block())
            }
            Self::YamlItem(item) => item.try_set_path(&path_text, Mapping::new_pending_block()),
            _ => return self.set_value(path, &ConfigValue::Json(Value::Object(Map::new()))),
        }
        .map_err(|_| format!("cannot edit YAML path {path_text}"))
    }

    /// A new, empty YAML mapping document written in block style.
    pub fn new_yaml_mapping() -> Self {
        let file = YamlFile::new();
        file.ensure_document();
        Self::Yaml(file)
    }

    /// The source text of the YAML scalar at `path`, quoting included.
    pub fn scalar_source(&self, path: &[&str]) -> Option<String> {
        match self.yaml_node(path)? {
            YamlNode::Scalar(scalar) if !path.is_empty() => Some(scalar.to_string()),
            _ => None,
        }
    }

    /// Put back a YAML scalar from the source text [`Self::scalar_source`] gave.
    pub fn set_scalar_source(&mut self, path: &[&str], source: &str) -> Result<(), String> {
        let file = source
            .parse::<YamlFile>()
            .map_err(|_| "The recorded YAML scalar is invalid".to_string())?;
        let scalar = file
            .documents()
            .next()
            .and_then(|doc| doc.as_scalar())
            .filter(|scalar| scalar.to_string() == source)
            .ok_or_else(|| "The recorded YAML scalar is invalid".to_string())?;
        let path_text = path.join(".");
        match self {
            Self::Yaml(file) => yaml_document(file)?.try_set_path(&path_text, scalar),
            Self::YamlItem(item) => item.try_set_path(&path_text, scalar),
            _ => return Err("scalar sources are YAML only".to_string()),
        }
        .map_err(|_| format!("cannot edit YAML path {path_text}"))
    }

    /// Whether the YAML collection at `path` (`[]` is the root or the item)
    /// is written in flow style and has entries.
    pub fn is_flow(&self, path: &[&str]) -> bool {
        match self.yaml_node(path) {
            Some(YamlNode::Mapping(mapping)) => mapping.is_flow_style() && !mapping.is_empty(),
            Some(YamlNode::Sequence(list)) => list.is_flow_style() && !list.is_empty(),
            _ => false,
        }
    }

    fn yaml_list(&self) -> Result<Sequence, String> {
        match self {
            Self::Yaml(file) => file
                .documents()
                .next()
                .and_then(|doc| doc.as_sequence())
                .ok_or_else(|| "The document is not a YAML list".to_string()),
            _ => Err("The document is not a YAML list".to_string()),
        }
    }

    /// The mapping items of a YAML list, as read-only views.
    pub fn items(&self) -> Result<Vec<ConfigDoc>, String> {
        Ok(self
            .yaml_list()?
            .values()
            .filter_map(|item| match item {
                YamlNode::Mapping(mapping) => Some(Self::YamlItem(mapping)),
                _ => None,
            })
            .collect())
    }

    /// The position of the item `entry` selects; found by reading only. More
    /// than one match is ambiguous, because the consumer would apply both.
    pub fn entry_index(&self, entry: &EntryKey) -> Result<Option<usize>, String> {
        let mut found = None;
        for (index, item) in self.yaml_list()?.values().enumerate() {
            let YamlNode::Mapping(mapping) = item else {
                continue;
            };
            let Some(YamlNode::Scalar(value)) = mapping.get(entry.key.as_str()) else {
                continue;
            };
            if value.as_string() == entry.id && found.replace(index).is_some() {
                return Err(format!(
                    "The list has more than one item with {}: {}",
                    entry.key, entry.id
                ));
            }
        }
        Ok(found)
    }

    /// A live view of the item `entry` selects, if the list has one.
    pub fn entry(&self, entry: &EntryKey) -> Result<Option<ConfigDoc>, String> {
        let Some(index) = self.entry_index(entry)? else {
            return Ok(None);
        };
        match self.yaml_list()?.get(index) {
            Some(YamlNode::Mapping(mapping)) => Ok(Some(Self::YamlItem(mapping))),
            _ => Ok(None),
        }
    }

    /// Append `content`, a JSON object, as a new last item of the list. Check
    /// [`Self::list_editable`] on the user's list before the first append.
    pub fn insert_entry(&mut self, content: &Value) -> Result<(), String> {
        let list = self.yaml_list()?;
        list.insert(list.len(), yaml_input(content.clone())?);
        Ok(())
    }

    /// Remove the item `entry` selects. Items appended together are removed
    /// in the order they were appended.
    pub fn remove_entry(&mut self, entry: &EntryKey) -> Result<(), String> {
        if let Some(index) = self.entry_index(entry)? {
            self.yaml_list()?.remove(index);
        }
        Ok(())
    }

    /// Whether items can be appended to this list and removed again byte for
    /// byte; the error says what to change.
    //
    // yaml-edit 0.3.2 edits lists and items byte for byte only through
    // `Sequence::insert`/`remove` of a whole item and `Mapping` key additions,
    // removals and scalar replacements. Relax these checks if it learns more:
    // G1: re-inserting a saved block item loses its nested indentation:
    //     `- id: b\n  config:\n    k: 1\n` comes back as `  config:\nk: 1`.
    // G2: `Mapping::set` with a saved block value over-indents it and drops its
    //     leading comment; `insert_at_index_preserving` renders
    //     `    \nreasoningEffort: max\n`.
    // G3: in a non-empty flow list, removing the last item leaves
    //     `[{id: a}, ]`, and appending after `[\n  {id: a},\n]` gives `,\n,`.
    // G4: appending to a list without a trailing newline adds one to the
    //     previous item that removing the new item does not take back.
    // G5: an appended item starts at column 1 even when the list is indented,
    //     so `    - id: a\n` gains `- id: b`, which YAML parsers reject.
    pub fn list_editable(&self) -> Result<(), String> {
        let list = self.yaml_list()?;
        let text = self.render()?;
        if !text.is_empty() && !text.ends_with('\n') {
            return Err("it does not end with a newline; add one".to_string());
        }
        if list.is_flow_style() && !list.is_empty() {
            return Err(
                "its list is written in flow style ([...]); write it as a block list (- item)"
                    .to_string(),
            );
        }
        if !list.is_flow_style() && list.start_position(&text).column != 1 {
            return Err(
                "its list is indented; start each `- ` item at the beginning of the line"
                    .to_string(),
            );
        }
        Ok(())
    }
}

fn yaml_document(file: &YamlFile) -> Result<yaml_edit::Document, String> {
    file.documents()
        .next()
        .ok_or_else(|| "YAML document is empty".to_string())
}

fn yaml_set(target: &impl YamlPath, path: &[&str], value: &ConfigValue) -> Result<(), String> {
    let path = path.join(".");
    match value {
        ConfigValue::Str(value) => target.try_set_path(&path, value.as_str()),
        ConfigValue::Number(value) => {
            let value =
                i64::try_from(*value).map_err(|_| "number too large for YAML".to_string())?;
            target.try_set_path(&path, value)
        }
        ConfigValue::Bool(value) => target.try_set_path(&path, *value),
        ConfigValue::List(_) | ConfigValue::Json(_) => {
            target.try_set_path(&path, yaml_input(value.to_json())?)
        }
    }
    .map_err(|_| format!("cannot edit YAML path {path}"))
}

fn validate_unique_keys(node: &jsonc_parser::cst::CstNode, format: &str) -> Result<(), String> {
    if let Some(object) = node.as_object() {
        let mut keys = std::collections::HashSet::new();
        for property in object.properties() {
            let name = property
                .name()
                .ok_or_else(|| format!("unsupported {format} property name"))?;
            let key = name
                .decoded_value()
                .ok()
                .ok_or_else(|| format!("unsupported {format} property name"))?;
            // The CST parser's loose-word mode is wider than JSON5. Accept a
            // conservative identifier subset; quoted Unicode keys remain valid.
            if matches!(name, jsonc_parser::cst::ObjectPropName::Word(_))
                && (key.is_empty()
                    || !key.bytes().enumerate().all(|(index, byte)| {
                        byte == b'_'
                            || byte == b'$'
                            || byte.is_ascii_alphabetic()
                            || (index > 0 && byte.is_ascii_digit())
                    }))
            {
                return Err(
                    "unsupported JSON5 unquoted property name; quote the name explicitly"
                        .to_string(),
                );
            }
            if !keys.insert(key) {
                return Err(format!(
                    "duplicate {format} property names are unsafe to edit"
                ));
            }
        }
    }
    for child in node.children() {
        validate_unique_keys(&child, format)?;
    }
    Ok(())
}

fn cst_input(value: Value) -> CstInputValue {
    match value {
        Value::Null => CstInputValue::Null,
        Value::Bool(value) => CstInputValue::Bool(value),
        Value::Number(value) => CstInputValue::Number(value.to_string()),
        Value::String(value) => CstInputValue::String(value),
        Value::Array(values) => CstInputValue::Array(values.into_iter().map(cst_input).collect()),
        Value::Object(values) => CstInputValue::Object(
            values
                .into_iter()
                .map(|(key, value)| (key, cst_input(value)))
                .collect(),
        ),
    }
}

/// The value of a JSON object source: strict JSON is read by serde_json, as
/// connection records are; JSON5 by the CST parser's JSON5 options.
fn json_value(text: &str, strict: bool) -> Result<Value, String> {
    let (value, format): (Result<Value, _>, _) = if strict {
        (
            serde_json::from_str(text).map_err(|_| "not valid JSON"),
            "JSON",
        )
    } else {
        (
            jsonc_parser::parse_to_serde_value(text, &json_options(false))
                .map_err(|_| "invalid or unsupported JSON5 syntax"),
            "JSON5",
        )
    };
    let value = value?;
    if !value.is_object() {
        return Err(format!("not a {format} object"));
    }
    Ok(value)
}

fn json_options(strict: bool) -> jsonc_parser::ParseOptions {
    if strict {
        jsonc_parser::ParseOptions {
            allow_comments: false,
            allow_trailing_commas: false,
            allow_loose_object_property_names: false,
            allow_missing_commas: false,
            allow_single_quoted_strings: false,
            allow_hexadecimal_numbers: false,
            allow_unary_plus_numbers: false,
        }
    } else {
        jsonc_parser::ParseOptions {
            allow_missing_commas: false,
            ..Default::default()
        }
    }
}

fn json_root(text: &str, strict: bool) -> Result<CstRootNode, String> {
    json_value(text, strict)?;
    let (format, invalid) = if strict {
        ("JSON", "not valid JSON")
    } else {
        ("JSON5", "invalid or unsupported JSON5 syntax")
    };
    let root = CstRootNode::parse(text, &json_options(strict)).map_err(|_| invalid.to_string())?;
    if let Some(value) = root.value() {
        validate_unique_keys(&value, format)?;
    }
    Ok(root)
}

fn edit_json(
    text: &str,
    path: &[&str],
    value: Option<Value>,
    strict: bool,
) -> Result<String, String> {
    let updated = edit_cst(json_root(text, strict)?, text, path, value)?;
    json_value(&updated, strict)?;
    Ok(updated)
}

/// Sets or removes the value at `path`; the rest of the source stays as it is.
fn edit_cst(
    root: CstRootNode,
    text: &str,
    path: &[&str],
    value: Option<Value>,
) -> Result<String, String> {
    let (leaf, parents) = split_leaf(path)?;
    let mut object = root
        .object_value()
        .ok_or_else(|| "not a JSON object".to_string())?;
    for key in parents {
        object = if value.is_some() {
            object
                .object_value_or_create(key)
                .ok_or_else(|| not_a_table(parents))?
        } else {
            let Some(child) = object.object_value(key) else {
                return Ok(text.to_string());
            };
            child
        };
    }
    match (object.get(leaf), value) {
        (Some(property), Some(value)) => property.set_value(cst_input(value)),
        (None, Some(value)) => {
            object.append(leaf, cst_input(value));
        }
        (Some(property), None) => property.remove(),
        (None, None) => {}
    }
    Ok(root.to_string())
}

fn yaml_value(node: yaml_edit::YamlNode) -> Option<ConfigValue> {
    ConfigValue::from_json(&recorded(yaml_json(&node)?))
}

/// Connection records are serde_json documents and are compared with config
/// values. JSON configs are read by serde_json too; JSON5 and YAML numbers
/// are read by other parsers, which can decode a long literal such as
/// `0.049999999999999996` to a different f64 than serde_json does. Reading
/// those values back through serde_json gives both sides of a comparison the
/// same decoder. Older records only match because that decoder is serde_json's
/// default one, not `float_roundtrip`; a unit test pins this.
fn recorded(value: Value) -> Value {
    serde_json::from_str(&value.to_string()).unwrap_or(value)
}

/// Inspect structured YAML without resolving aliases, tags or merge keys.
fn yaml_json(node: &yaml_edit::YamlNode) -> Option<Value> {
    use yaml_edit::{ScalarType, ScalarValue, YamlNode};
    match node {
        YamlNode::Mapping(mapping) => {
            let mut values = Map::new();
            for (key, value) in mapping {
                let key = key.as_scalar()?;
                if ScalarValue::from_scalar(key).scalar_type() != ScalarType::String {
                    return None;
                }
                let key = key.as_string();
                if key == "<<" || values.insert(key, yaml_json(&value)?).is_some() {
                    return None;
                }
            }
            Some(Value::Object(values))
        }
        YamlNode::Sequence(sequence) => sequence
            .into_iter()
            .map(|value| yaml_json(&value))
            .collect::<Option<Vec<_>>>()
            .map(Value::Array),
        YamlNode::Scalar(scalar) => {
            let value = ScalarValue::from_scalar(scalar);
            match value.scalar_type() {
                ScalarType::String => Some(Value::String(scalar.as_string())),
                ScalarType::Null => Some(Value::Null),
                ScalarType::Boolean => value.to_bool().map(Value::Bool),
                ScalarType::Integer => value.to_i64().map(|number| Value::Number(number.into())),
                ScalarType::Float => value
                    .to_f64()
                    .and_then(serde_json::Number::from_f64)
                    .map(Value::Number),
                _ => None,
            }
        }
        _ => None,
    }
}

/// Construct only a new value subtree. The surrounding document stays a CST.
fn yaml_input(value: Value) -> Result<yaml_edit::YamlNode, String> {
    use yaml_edit::YamlNode;
    // JSON is a YAML flow value, valid beneath either block or flow parents.
    // Encode only the newly supplied value, never the existing user document.
    let fragment = serde_json::to_string(&value).map_err(|_| "Cannot encode YAML value")?;
    let file = fragment
        .parse::<YamlFile>()
        .map_err(|_| "Cannot construct YAML value")?;
    let doc = file
        .documents()
        .next()
        .ok_or("Cannot construct YAML value")?;
    let node = doc
        .as_mapping()
        .map(YamlNode::Mapping)
        .or_else(|| doc.as_sequence().map(YamlNode::Sequence))
        .or_else(|| doc.as_scalar().map(YamlNode::Scalar))
        .ok_or_else(|| "Cannot construct YAML value".to_string())?;
    if yaml_json(&node).as_ref() != Some(&value) {
        return Err("The YAML value cannot be represented without semantic loss".to_string());
    }
    Ok(node)
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
mod tests;
