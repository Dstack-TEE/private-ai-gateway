use super::*;

#[test]
fn json5_cst_preserves_unrelated_syntax_and_rejects_ambiguous_edits() {
    let source = "{\n // keep this comment\n untouched: 'single quoted',\n nested: { /* keep inside */ flag: true, },\n}\n";
    let original = ConfigDoc::parse(Format::Json5, source).unwrap();
    let mut doc = original.clone();
    doc.set_value(
        &["models", "providers", "gateway"],
        &ConfigValue::Json(serde_json::json!({"api":"openai-completions"})),
    )
    .unwrap();
    let output = doc.render().unwrap();
    assert!(output.contains("untouched: 'single quoted',"));
    assert!(output.contains("nested: { /* keep inside */ flag: true, }"));
    assert!(output.contains("// keep this comment"));
    assert_eq!(original.render().unwrap(), source);
    assert!(matches!(doc.get_value(&[]), Some(ConfigValue::Json(_))));
    doc.remove(&["models", "providers", "gateway"]).unwrap();
    assert!(doc.get_value(&["models", "providers", "gateway"]).is_none());
    let before = doc.render().unwrap();
    assert!(doc.set_str(&["untouched", "bad"], "bad").is_err());
    assert_eq!(doc.render().unwrap(), before);
    for source in [
        "{bad-key:1}",
        "{1:2}",
        "{a:1,a:2}",
        "{nested:{a:1,a:2}}",
        "{a:Infinity}",
        "{a:NaN}",
        "{a:1 b:2}",
    ] {
        assert!(ConfigDoc::parse(Format::Json5, source).is_err(), "{source}");
    }
}

#[test]
fn jsonc_inspection_accepts_comments_not_json5_or_malformed_json() {
    assert_eq!(
        parse_jsonc("{/* keep */\"url\":\"https://host/a//b\", // keep too\n}").unwrap(),
        serde_json::json!({"url": "https://host/a//b"}),
    );
    for invalid in [
        "{unquoted:1}",
        "{'single':1}",
        "{\"a\":1 \"b\":2}",
        "{\"a\":0xff}",
        "{\"a\":+1}",
        "{",
        "[]",
        "null",
        "// comment only",
        " \n",
    ] {
        assert!(parse_jsonc(invalid).is_err(), "{invalid}");
    }
}

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
    doc.remove(&["model_providers", "p", "auth", "args"])
        .unwrap();
    doc.remove(&["model_providers", "p", "auth", "timeout_ms"])
        .unwrap();
    assert_eq!(doc.render().unwrap(), "model = \"x\"\n");
}

#[test]
fn json_numbers_are_typed() {
    let mut doc = ConfigDoc::parse(Format::Json, "").unwrap();
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
fn json_edits_keep_every_untouched_byte() {
    let source = "{\r\n    \"z\" :  \"\\u00e9\",\r\n    \"price\": 0.049999999999999996,\r\n    \"env\": {\"KEEP\": \"1\"},\r\n    \"model\": \"other\"\r\n}  \r\n";
    let original = ConfigDoc::parse(Format::Json, source).unwrap();
    assert_eq!(original.render().unwrap(), source);

    // Setting a value changes only that value.
    let mut doc = original.clone();
    doc.set_str(&["model"], "private-ai-proxy/x").unwrap();
    assert_eq!(
        doc.render().unwrap(),
        source.replace("\"other\"", "\"private-ai-proxy/x\"")
    );
    doc.set_str(&["model"], "other").unwrap();
    assert_eq!(doc.render().unwrap(), source);

    // A new nested key is appended, and removing it prunes the containers it
    // created, restoring the source byte for byte.
    doc.set_value(
        &["provider", "private-ai-proxy", "options"],
        &ConfigValue::Json(serde_json::json!({"baseURL": "http://127.0.0.1:1/v1"})),
    )
    .unwrap();
    let edited = doc.render().unwrap();
    assert!(
        edited.starts_with(&source[..source.find("\r\n}").unwrap()]),
        "{edited}"
    );
    assert_eq!(
        doc.get_str(&["provider", "private-ai-proxy", "options", "baseURL"])
            .as_deref(),
        Some("http://127.0.0.1:1/v1")
    );
    doc.remove(&["provider", "private-ai-proxy", "options"])
        .unwrap();
    assert_eq!(doc.render().unwrap(), source);
    doc.remove(&["env", "KEEP"]).unwrap();
    assert!(!doc.contains(&["env"]));

    assert_eq!(
        ConfigDoc::parse(Format::Json, "{\"a\":{\"b\":1,\"b\":2}}").unwrap_err(),
        "duplicate JSON property names are unsafe to edit"
    );
    for source in [
        "{\"a\":1,\"a\":2}",
        "{/* c */}",
        "{\"a\":1,}",
        "[]",
        "{a:1}",
    ] {
        assert!(ConfigDoc::parse(Format::Json, source).is_err(), "{source}");
    }
}

#[test]
fn yaml_edits_preserve_comments_and_prune_owned_tables() {
    let mut doc = ConfigDoc::parse(Format::Yaml, "# user comment\ntheme: dark\n").unwrap();
    doc.set_value(
        &["providers", "private-ai-proxy", "discover_models"],
        &ConfigValue::Bool(true),
    )
    .unwrap();
    assert_eq!(
        doc.get_value(&["providers", "private-ai-proxy", "discover_models"]),
        Some(ConfigValue::Bool(true))
    );
    doc.remove(&["providers", "private-ai-proxy", "discover_models"])
        .unwrap();
    let text = doc.render().unwrap();
    assert!(text.contains("# user comment"));
    assert!(text.contains("theme: dark"));
    assert!(!text.contains("private-ai-proxy"));
}

/// `recorded` relies on serde_json's default float parsing, which reads the
/// 17-digit literal older builds wrote as the f64 nearest `0.05`. Enabling
/// serde_json's `float_roundtrip` anywhere in the build (features are unified)
/// would read it exactly and break recovery of those connections.
#[test]
fn serde_json_float_parsing_is_not_roundtrip() {
    let exact: f64 = "0.049999999999999996".parse().unwrap();
    let parsed: f64 = serde_json::from_str("0.049999999999999996").unwrap();
    assert_ne!(exact, 0.05);
    assert_eq!(parsed, 0.05);
}

fn key(id: &str) -> EntryKey {
    EntryKey {
        key: "id".to_string(),
        id: id.to_string(),
    }
}

/// The keyed-item operations the projection relies on restore every byte:
/// appending and removing a whole item, adding and removing a key inside an
/// existing item, and replacing a scalar from its recorded source text.
#[test]
fn yaml_list_items_round_trip_byte_for_byte() {
    let lists = [
        "# my patches\n- id: a # keep\n  config:\n    x: 1\n",
        "-   id: a\n    config:\n        x: 1\n",
        "- id: a\r\n  config:\r\n    x: 1\r\n",
        "- id: a\n# trailing comment\n",
        "- {id: a, config: {x: 1}}\n",
        "[]\n",
        "# only a comment above\n[]\n",
    ];
    let ours = serde_json::json!({"id": "ours", "config": {"providers": {"p": {"models": [{"id": "m"}]}}}});
    for source in lists {
        let mut doc = ConfigDoc::parse(Format::YamlList, source).unwrap();
        doc.list_editable().unwrap();
        doc.insert_entry(&ours).unwrap();
        doc.insert_entry(&serde_json::json!({"id": "second", "disabled": true}))
            .unwrap();
        let written = doc.render().unwrap();
        let reparsed = ConfigDoc::parse(Format::YamlList, &written).unwrap();
        let item = reparsed.entry(&key("ours")).unwrap().unwrap();
        assert_eq!(
            item.get_value(&["config", "providers", "p", "models"]),
            Some(ConfigValue::Json(serde_json::json!([{"id": "m"}]))),
            "{source:?}"
        );
        doc.remove_entry(&key("ours")).unwrap();
        doc.remove_entry(&key("second")).unwrap();
        assert_eq!(doc.render().unwrap(), source, "{source:?}");
    }

    let source = "# head\n- id: a\n  config:\n    providers:\n      mine: {k: v} # c\n    model: 'deepseek-flash' # mine\n    effort: \"max\"\n- id: b\n";
    let doc = ConfigDoc::parse(Format::YamlList, source).unwrap();
    let mut item = doc.entry(&key("a")).unwrap().unwrap();
    item.set_value(
        &["config", "providers", "ours"],
        &ConfigValue::Json(serde_json::json!({"api": "x"})),
    )
    .unwrap();
    item.set_value(&["disabled"], &ConfigValue::Bool(false))
        .unwrap();
    let model = item.scalar_source(&["config", "model"]).unwrap();
    let effort = item.scalar_source(&["config", "effort"]).unwrap();
    assert_eq!(
        (model.as_str(), effort.as_str()),
        ("'deepseek-flash'", "\"max\"")
    );
    item.set_str(&["config", "model"], "ours/model").unwrap();
    item.set_str(&["config", "effort"], "low").unwrap();
    let written = doc.render().unwrap();
    assert!(written.contains("mine: {k: v} # c"), "{written}");
    item.remove(&["config", "providers", "ours"]).unwrap();
    item.remove(&["disabled"]).unwrap();
    item.set_scalar_source(&["config", "model"], &model)
        .unwrap();
    item.set_scalar_source(&["config", "effort"], &effort)
        .unwrap();
    assert_eq!(doc.render().unwrap(), source);
}

#[test]
fn yaml_list_edits_refuse_what_cannot_be_restored() {
    // Duplicate ids are ambiguous; they are found without editing anything.
    let duplicated = "- id: a\n- id: b\n- id: a\n";
    let doc = ConfigDoc::parse(Format::YamlList, duplicated).unwrap();
    assert!(doc.entry(&key("a")).is_err());
    assert!(doc.entry(&key("b")).unwrap().is_some());
    assert!(doc.entry(&key("c")).unwrap().is_none());
    assert_eq!(doc.render().unwrap(), duplicated);
    for source in [
        "- id: a",
        "[{id: a}]\n",
        "[\n  {id: a},\n]\n",
        "    - id: a\n",
        "# indented\n  - id: a\n",
    ] {
        let doc = ConfigDoc::parse(Format::YamlList, source).unwrap();
        assert!(doc.list_editable().is_err(), "{source:?}");
    }
    for source in ["a: 1\n", "- a\n---\n- b\n", "# comment only\n"] {
        assert!(
            ConfigDoc::parse(Format::YamlList, source).is_err(),
            "{source:?}"
        );
    }
    let flow = ConfigDoc::parse(Format::YamlList, "- {id: a, config: {x: 1}}\n").unwrap();
    let item = flow.entry(&key("a")).unwrap().unwrap();
    assert!(item.is_flow(&[]) && item.is_flow(&["config"]));
    let block = ConfigDoc::parse(Format::YamlList, "- id: a\n  config: {}\n").unwrap();
    let item = block.entry(&key("a")).unwrap().unwrap();
    assert!(!item.is_flow(&[]) && !item.is_flow(&["config"]));
}
