use super::*;

#[test]
fn downloaded_inventory_rejects_unknown_schema_duplicate_and_partial_observations() {
    let valid = serde_json::to_value(EndpointInventory::bundled().unwrap()).unwrap();
    for malformed in [
        {
            let mut value = valid.clone();
            value["schemaVersion"] = json!(1);
            value
        },
        {
            let mut value = valid.clone();
            value["schemaVersion"] = json!(3);
            value
        },
        {
            let mut value = valid.clone();
            value["checkedAt"] = json!("2026-02-30T00:00:00Z");
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

    let mut temporary = valid.clone();
    temporary["results"][0]["status"] = json!("supported");
    temporary["results"][0]["reason"] = json!("temporary_server_error");
    temporary["results"][0]["httpStatus"] = json!(503);
    assert!(EndpointInventory::parse(&serde_json::to_vec(&temporary).unwrap()).is_ok());

    temporary["results"][0]["httpStatus"] = json!(400);
    assert!(EndpointInventory::parse(&serde_json::to_vec(&temporary).unwrap()).is_err());

    temporary["results"][0]["httpStatus"] = json!(501);
    assert!(EndpointInventory::parse(&serde_json::to_vec(&temporary).unwrap()).is_err());
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
    let inventory = EndpointInventory::bundled().unwrap();
    for endpoint in ["https://tee.redpill.ai", "https://inference.phala.com/v1"] {
        let mut catalog = original.clone();
        catalog
            .apply_endpoint_inventory(endpoint, &inventory)
            .unwrap();
        assert_ne!(catalog.revision, original.revision);
        assert_eq!(catalog.openai_list(), original.openai_list());
        assert!(catalog
            .get("new-unprobed-model")
            .unwrap()
            .agent_surfaces
            .as_ref()
            .is_some_and(Vec::is_empty));
        for surface in [
            Surface::ChatCompletions,
            Surface::Messages,
            Surface::Responses,
        ] {
            assert!(catalog
                .for_surface(surface)
                .get("new-unprobed-model")
                .is_none());
            assert!(catalog
                .for_agent_surface(surface)
                .get("new-unprobed-model")
                .is_none());
        }
    }
    for endpoint in [
        "https://example.com",
        "https://tee.redpill.ai/custom",
        "https://tee.redpill.ai:444",
    ] {
        let mut catalog = original.clone();
        catalog
            .apply_endpoint_inventory(endpoint, &inventory)
            .unwrap();
        assert_eq!(catalog.revision, original.revision);
        assert!(catalog
            .models
            .iter()
            .all(|model| model.supported_surfaces.is_none()));
    }
}

#[test]
fn agent_projections_require_all_checks_without_blocking_direct_api_access() {
    let mut value = serde_json::to_value(EndpointInventory::bundled().unwrap()).unwrap();
    for entry in value["results"].as_array_mut().unwrap() {
        entry["checks"] = json!({
            "streaming": {"status": "supported", "reason": "valid_event_stream", "httpStatus": 200},
            "tools": {"status": "supported", "reason": "valid_streamed_tool_call", "httpStatus": 200},
            "toolResult": {"status": "supported", "reason": "valid_tool_result_response", "httpStatus": 200}
        });
        if entry["model"] == "z-ai/glm-5.3" && entry["endpoint"] == "/v1/responses" {
            entry["checks"]["tools"]["status"] = json!("inconclusive");
        }
    }
    let inventory = EndpointInventory::parse(&serde_json::to_vec(&value).unwrap()).unwrap();
    let mut older = value.clone();
    older["checkedAt"] = json!((inventory.checked_at - chrono::TimeDelta::seconds(1)).to_rfc3339());
    let older = EndpointInventory::parse(&serde_json::to_vec(&older).unwrap()).unwrap();
    assert!(!older.is_newer_than(&inventory));
    let mut catalog = Catalog::from_remote(&json!({"data": [{"id": "z-ai/glm-5.3"}]}), 1).unwrap();
    catalog
        .apply_endpoint_inventory("https://tee.redpill.ai", &inventory)
        .unwrap();
    let model = &catalog.models[0];
    assert!(model.supports(Surface::Responses));
    assert!(!model.supports_agent(Surface::Responses));
    assert!(catalog
        .for_agent_surface(Surface::Responses)
        .models
        .is_empty());
    assert!(model.supports_agent(Surface::Messages));

    value["results"][0]["checks"]
        .as_object_mut()
        .unwrap()
        .remove("toolResult");
    assert!(EndpointInventory::parse(&serde_json::to_vec(&value).unwrap()).is_err());
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
