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
