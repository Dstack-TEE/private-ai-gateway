use super::*;

#[test]
fn downloaded_inventory_rejects_unknown_schema_duplicate_and_partial_observations() {
    let valid = serde_json::to_value(EndpointInventory::bundled().unwrap()).unwrap();
    for malformed in [
        {
            let mut value = valid.clone();
            value["schemaVersion"] = json!(2);
            value
        },
        {
            let mut value = valid.clone();
            value["results"][0]["status"] = json!("maybe");
            value
        },
        {
            let mut value = valid.clone();
            value["results"][0]["endpoint"] = json!("/admin");
            value
        },
        {
            let mut value = valid.clone();
            value["results"][0] = value["results"][1].clone();
            value
        },
        {
            let mut value = valid.clone();
            value["results"].as_array_mut().unwrap().pop();
            value
        },
    ] {
        assert!(EndpointInventory::parse(&serde_json::to_vec(&malformed).unwrap()).is_err());
    }
}

#[test]
fn observations_are_scoped_and_never_add_unlisted_models() {
    let original = Catalog::from_remote(
        &json!({"data": [
            {"id": "z-ai/glm-5.3"},
            {"id": "meta-llama/llama-3.3-70b-instruct"},
            {"id": "new-unprobed-model"}
        ]}),
        1,
    )
    .unwrap();
    for endpoint in ["https://tee.redpill.ai", "https://inference.phala.com/v1"] {
        let mut catalog = original.clone();
        catalog
            .apply_endpoint_inventory(endpoint, &EndpointInventory::bundled().unwrap())
            .unwrap();
        assert_ne!(catalog.revision, original.revision);
        assert_eq!(catalog.openai_list(), original.openai_list());
        assert_eq!(catalog.for_surface(Surface::Responses).models.len(), 1);
        assert_eq!(catalog.for_surface(Surface::Messages).models.len(), 2);
        assert_eq!(
            catalog.for_surface(Surface::ChatCompletions).models.len(),
            2
        );
    }
    for endpoint in [
        "https://example.com",
        "https://tee.redpill.ai/custom",
        "https://tee.redpill.ai:444",
    ] {
        let mut catalog = original.clone();
        catalog
            .apply_endpoint_inventory(endpoint, &EndpointInventory::bundled().unwrap())
            .unwrap();
        assert_eq!(catalog.revision, original.revision);
        assert!(catalog
            .models
            .iter()
            .all(|model| model.supported_surfaces.is_none()));
    }
}
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
                "input_cache_read": "NaN"
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
}
