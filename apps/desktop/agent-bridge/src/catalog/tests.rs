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
    assert!(catalog.models[0].supports(Surface::SystemOne));
    let list = catalog.openai_list();
    assert_eq!(list["data"][0]["is_tee"], json!(true));
    assert_eq!(list["data"][0]["supported_features"], json!(["tools"]));
    assert!(Catalog::from_remote(&json!({ "data": [] }), 1).is_err());
    assert!(Catalog::from_remote(&json!({ "data": [{ "name": "no id" }] }), 1).is_err());
}

#[test]
fn output_modalities_gate_surfaces_and_agent_projections() {
    let catalog = Catalog::from_remote(
        &json!({ "data": [
            { "id": "decisions", "output_modalities": ["decisions"] },
            { "id": "text", "output_modalities": ["text"] },
            { "id": "legacy" }
        ]}),
        1,
    )
    .unwrap();
    let decisions = catalog.get("decisions").unwrap();
    assert!(decisions.supports(Surface::SystemOne));
    assert!(!decisions.supports(Surface::Responses));

    let text = catalog.get("text").unwrap();
    assert!(!text.supports(Surface::SystemOne));
    assert!(text.supports(Surface::Responses));

    let legacy = catalog.get("legacy").unwrap();
    assert!(legacy.supports(Surface::SystemOne));
    assert!(legacy.supports(Surface::Responses));
}

#[test]
fn images_require_explicit_image_output() {
    let original = Catalog::from_remote(
        &json!({ "data": [
            { "id": "images", "output_modalities": ["image"] },
            { "id": "mixed", "output_modalities": ["text", "image"] },
            { "id": "text", "output_modalities": ["text"] },
            { "id": "legacy" },
            { "id": "empty", "output_modalities": [] },
            { "id": "malformed", "output_modalities": "image" },
            { "id": "null", "output_modalities": null }
        ]}),
        1,
    )
    .unwrap();
    for surface in [
        Surface::ChatCompletions,
        Surface::Messages,
        Surface::Responses,
    ] {
        assert!(!original.get("images").unwrap().supports(surface));
    }
    let images = original.for_surface(Surface::Images);
    let ids: Vec<_> = images.models.iter().map(CatalogModel::id).collect();
    assert_eq!(ids, ["images", "mixed"]);
    assert_eq!(Surface::Images.path(), "/v1/images/generations");
}

#[test]
fn transcriptions_require_only_transcription_output() {
    let original = Catalog::from_remote(
        &json!({ "data": [
            { "id": "audio", "input_modalities": ["audio"], "output_modalities": ["transcription"] },
            { "id": "mixed-output", "output_modalities": ["text", "transcription"] },
            { "id": "gemini", "input_modalities": ["text", "audio", "image", "video"], "output_modalities": ["text"] },
            { "id": "missing-output", "input_modalities": ["audio"] },
            { "id": "legacy" },
            { "id": "malformed-output", "output_modalities": "transcription" }
        ]}),
        1,
    )
    .unwrap();
    for surface in [
        Surface::ChatCompletions,
        Surface::Messages,
        Surface::Responses,
    ] {
        for id in ["audio", "mixed-output", "malformed-output"] {
            assert!(
                !original.get(id).unwrap().supports(surface),
                "{id}: {surface:?}"
            );
        }
        for id in ["gemini", "missing-output", "legacy"] {
            assert!(
                original.get(id).unwrap().supports(surface),
                "{id}: {surface:?}"
            );
        }
    }
    let audio = original.for_surface(Surface::AudioTranscriptions);
    let ids: Vec<_> = audio.models.iter().map(CatalogModel::id).collect();
    assert_eq!(ids, ["audio", "mixed-output"]);
}

#[test]
fn tee_models_show_a_tee_suffix_while_ids_and_listed_entries_stay_unchanged() {
    let catalog = Catalog::from_remote(
        &json!({ "data": [
            { "id": "openai/gpt-oss-120b", "name": "OpenAI: GPT OSS 120B", "is_tee": true },
            { "id": "phala/qwen", "name": " ", "is_tee": true },
            { "id": "tagged", "name": "Tagged [TEE]", "is_tee": true },
            { "id": "plain", "name": "Plain", "is_tee": false },
            { "id": "unmarked" }
        ]}),
        1,
    )
    .unwrap();
    let names: Vec<_> = catalog
        .models
        .iter()
        .map(|model| (model.id(), model.display_name()))
        .collect();
    assert_eq!(
        names,
        [
            ("openai/gpt-oss-120b", "OpenAI: GPT OSS 120B [TEE]"),
            ("phala/qwen", "phala/qwen [TEE]"),
            ("tagged", "Tagged [TEE]"),
            ("plain", "Plain"),
            ("unmarked", "unmarked"),
        ]
    );
    let list = catalog.openai_list();
    assert_eq!(list["data"][0]["id"], json!("openai/gpt-oss-120b"));
    assert_eq!(list["data"][0]["name"], json!("OpenAI: GPT OSS 120B"));
}

#[test]
fn removed_models_are_reported_not_replaced() {
    let before = Catalog::from_remote(&remote(), 1).unwrap();
    let after =
        Catalog::from_remote(&json!({ "data": [{ "id": "openai/gpt-oss-20b" }] }), 2).unwrap();
    assert_eq!(after.removed_since(&before), ["meta/llama"]);
    assert_ne!(after.revision, before.revision);
}

#[test]
fn malformed_optional_metadata_is_omitted() {
    let catalog = Catalog::from_remote(
        &json!({ "data": [{
            "id": "model-a",
            "is_tee": "yes",
            "supported_features": ["tools", 4, ""],
            "pricing": {
                "prompt": "0.000002",
                "completion": -1,
                "input_cache_read": "NaN",
                "input_cache_write": 5e-8
            }
        }] }),
        1,
    )
    .unwrap();
    let model = &catalog.models[0];
    assert_eq!(model.bool_field("is_tee"), None);
    assert_eq!(model.string_array("supported_features"), ["tools"]);
    assert_eq!(model.price_per_million("prompt"), Some(2.0));
    assert_eq!(model.price_per_million("completion"), None);
    assert_eq!(model.price_per_million("input_cache_read"), None);
    // The decimal point moves in the text; floats are not multiplied.
    assert_eq!(model.price_per_million("input_cache_write"), Some(0.05));
}
