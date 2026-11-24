//! Offline replay of a control-plane upstreams array.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::Arc;

use serde_json::{value::RawValue, Value};
use serde_path_to_error::Segment;

use super::builders::{add_model_routes, build_provider_verifier, build_state};
use super::validation::{unique_upstream_models, validate_config_with};
use super::{ProviderSessionRegistry, UpstreamConfig, UpstreamProvider, UpstreamRuntimeOptions};
use crate::aci::upstream::{ModelRouterBackend, PrivatemodeProxyDeployment};

#[test]
#[ignore = "requires UPSTREAM_CONFIG_CHECK_FILE pointing to a local JSON array"]
fn replay_from_file() {
    let path = std::env::var_os("UPSTREAM_CONFIG_CHECK_FILE")
        .expect("set UPSTREAM_CONFIG_CHECK_FILE to the upstreams JSON file");
    let input = std::fs::read(path).unwrap_or_else(|_| panic!("ERROR field=config_file"));
    let mut options = UpstreamRuntimeOptions::default();
    if let Some(path) = std::env::var_os("UPSTREAM_CONFIG_CHECK_PRIVATEMODE_POLICY") {
        options.privatemode_proxy =
            Some(load_privatemode_policy(path.into()).unwrap_or_else(|| {
                panic!("ERROR provider=\"privatemode\" field=privatemode_proxy")
            }));
    }

    let (ok, report) = check(&input, &options);
    print!("{report}");
    assert!(ok, "upstream config rejected");
}

fn load_privatemode_policy(path: PathBuf) -> Option<Arc<PrivatemodeProxyDeployment>> {
    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Policy {
        base_url: String,
        manifest_log_path: PathBuf,
        credential_path: PathBuf,
        credential_sha256: String,
        proxy_image_digest: String,
    }

    let input = std::fs::read(path).ok()?;
    let policy: Policy = serde_json::from_slice(&input).ok()?;
    PrivatemodeProxyDeployment::new(
        policy.base_url,
        policy.manifest_log_path,
        policy.credential_path,
        policy.credential_sha256,
        policy.proxy_image_digest,
    )
    .ok()
    .map(Arc::new)
}

fn provider_name(provider: UpstreamProvider) -> String {
    serde_json::to_string(&provider).expect("provider is a serializable unit variant")
}

fn check(input: &[u8], options: &UpstreamRuntimeOptions) -> (bool, String) {
    // This is the same type and JSON deserializer as the pull response's
    // upstreams member and the admin PUT body.
    let Ok(config) = serde_json::from_slice::<Vec<UpstreamConfig>>(input) else {
        return (false, parse_errors(input));
    };

    let ok = build_state(&config, options).is_ok();
    let mut report = if ok {
        String::from("OK\n")
    } else {
        config_errors(&config, options)
    };
    report.push_str(&summary(&config));
    (ok, report)
}

fn parse_errors(input: &[u8]) -> String {
    let Ok(rows) = serde_json::from_slice::<Vec<&RawValue>>(input) else {
        return String::from("ERROR field=json\n");
    };

    // Only known schema keys are printable. Serde errors, deeper path
    // segments (including map keys), and unrecognized providers can echo input.
    const FIELDS: &[&str] = &[
        "name",
        "provider",
        "base_url",
        "path",
        "streaming_usage",
        "models",
        "bearer_token",
        "basic_auth",
        "accepted_subjects",
        "accepted_image_digests",
        "accepted_dstack_kms_root_public_keys",
        "pccs_url",
        "verifier_cache_seconds",
        "connect_timeout_seconds",
        "read_timeout_seconds",
        "verifier_request_timeout_seconds",
        "verification_refresh_seconds",
        "session_refresh_seconds",
        "chutes_e2ee_api_base",
        "chutes_chute_ids",
        "chutes_e2ee_discovery_rounds",
        "chutes_e2ee_discovery_interval_seconds",
        "zg_chain_rpc_url",
        "zg_serving_contract",
    ];
    let mut report = String::new();
    for row in rows {
        let mut json = serde_json::Deserializer::from_str(row.get());
        let Err(error) = serde_path_to_error::deserialize::<_, UpstreamConfig>(&mut json) else {
            continue;
        };
        let field = match error.path().iter().next() {
            Some(Segment::Map { key }) => FIELDS
                .iter()
                .copied()
                .find(|field| key == field)
                .unwrap_or("<unknown-field>"),
            _ => {
                let message = error.inner().to_string();
                let field = message
                    .strip_prefix("missing field `")
                    .or_else(|| message.strip_prefix("duplicate field `"))
                    .and_then(|message| message.split_once('`').map(|(field, _)| field));
                FIELDS
                    .iter()
                    .copied()
                    .find(|known| Some(*known) == field)
                    .unwrap_or("json")
            }
        };
        let row: Value = serde_json::from_str(row.get()).unwrap_or(Value::Null);
        let name = row
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("<unknown>");
        let provider = match row.get("provider") {
            None if row.is_object() => provider_name(UpstreamProvider::default()),
            Some(value) => serde_json::from_value(value.clone())
                .map(provider_name)
                .unwrap_or_else(|_| String::from("\"<unknown>\"")),
            None => String::from("\"<unknown>\""),
        };
        append_error(&mut report, name, &provider, field);
    }
    if report.is_empty() {
        report.push_str("ERROR field=json\n");
    }
    report
}

fn append_error(report: &mut String, name: &str, provider: &str, field: &str) {
    use std::fmt::Write;
    writeln!(
        report,
        "ERROR upstream={name:?} provider={provider} field={field}"
    )
    .expect("writing to a String is infallible");
}

fn config_errors(config: &[UpstreamConfig], options: &UpstreamRuntimeOptions) -> String {
    let mut errors = BTreeSet::new();
    let mut invalid = BTreeSet::new();
    validate_config_with(config, |cfg, field, _| {
        invalid.insert(cfg.name.clone());
        errors.insert((cfg.name.clone(), provider_name(cfg.provider), field));
        Ok(())
    })
    .expect("the diagnostic callback always succeeds");

    let sessions = ProviderSessionRegistry::new(config);
    let mut router = ModelRouterBackend::new("model-router");
    for cfg in config.iter().filter(|cfg| !invalid.contains(&cfg.name)) {
        if let Err(error) = add_model_routes(&mut router, cfg, options, &sessions) {
            errors.insert((
                cfg.name.clone(),
                provider_name(cfg.provider),
                error.into_parts().0,
            ));
        }
        if let Err(error) = build_provider_verifier(cfg, config, options, &sessions) {
            errors.insert((
                cfg.name.clone(),
                provider_name(cfg.provider),
                error.into_parts().0,
            ));
        }
    }

    let mut report = String::new();
    for (name, provider, field) in errors {
        // A policy may require one of several fields.
        for field in field.split('|') {
            append_error(&mut report, &name, &provider, field);
        }
    }
    if report.is_empty() {
        report.push_str("ERROR field=config\n");
    }
    report
}

fn summary(config: &[UpstreamConfig]) -> String {
    use std::fmt::Write;

    let mut report = String::new();
    let mut providers = BTreeMap::new();
    for cfg in config {
        *providers.entry(provider_name(cfg.provider)).or_insert(0) += 1;
    }

    write!(report, "provider_counts").unwrap();
    for (provider, count) in providers {
        write!(report, " {provider}={count}").unwrap();
    }
    report.push('\n');

    for cfg in config {
        let provider = provider_name(cfg.provider);
        let subjects = cfg.accepted_subjects.as_deref().unwrap_or_default();
        // A subject may be unprefixed or use any prefix. Only established
        // kinds can be output safely, including for rejected configurations.
        let mut counts = BTreeMap::from([
            ("app-id", 0),
            ("dstack-compose", 0),
            ("tdx-boot-chain", 0),
            ("tdx-measurement", 0),
            ("tinfoil-release", 0),
            ("other", 0),
        ]);
        for subject in subjects {
            let kind = subject.split_once(':').map_or("other", |(kind, _)| kind);
            if let Some(count) = counts.get_mut(kind) {
                *count += 1;
            } else {
                *counts.entry("other").or_default() += 1;
            }
        }
        let review = match cfg.provider {
            UpstreamProvider::NearAi => Some("near-compare-exact-model-ids-with-catalog"),
            UpstreamProvider::PhalaDirect if !subjects.is_empty() => Some("phala-subjects"),
            UpstreamProvider::Tinfoil if counts["tinfoil-release"] > 0 => {
                Some("tinfoil-release-pins")
            }
            UpstreamProvider::ZeroGPrivateComputer => Some("0g-boot-chain-pins"),
            _ => None,
        };
        write!(
            report,
            "upstream={:?} provider={provider} model_ids={:?} accepted_subjects=",
            cfg.name,
            unique_upstream_models(cfg)
        )
        .unwrap();
        for (index, (kind, count)) in counts.iter().enumerate() {
            if index > 0 {
                report.push(',');
            }
            write!(report, "{kind}:{count}").unwrap();
        }
        if let Some(review) = review {
            write!(report, " review={review}").unwrap();
        }
        report.push('\n');
    }
    report
}

#[test]
fn replays_and_redacts_every_provider() {
    use serde_json::json;

    let directory = tempfile::tempdir().unwrap();
    let credential = directory.path().join("fixture-credential");
    std::fs::write(&credential, "fixture-privatemode-credential").unwrap();
    let policy = directory.path().join("fixture-policy.json");
    std::fs::write(
        &policy,
        json!({
            "base_url": "http://privatemode-proxy:8080",
            "manifest_log_path": directory.path().join("fixture-manifest"),
            "credential_path": credential,
            "credential_sha256": crate::aci::digest::sha256_hex(b"fixture-privatemode-credential"),
            "proxy_image_digest": "22".repeat(32),
        })
        .to_string(),
    )
    .unwrap();
    let options = UpstreamRuntimeOptions {
        privatemode_proxy: load_privatemode_policy(policy),
        ..UpstreamRuntimeOptions::default()
    };

    let fixture_pin = "a".repeat(64);
    let root = k256::ecdsa::SigningKey::from_slice(&[1; 32]).unwrap();
    let kms_key = hex::encode(root.verifying_key().to_encoded_point(true).as_bytes());
    let mut rows = [
        "openai-compatible",
        "anthropic",
        "aci-service",
        "chutes",
        "tinfoil",
        "near-ai",
        "secret-ai",
        "privatemode",
        "phala-direct",
        "armet-ai",
        "0g-private-computer",
    ]
    .map(|provider| {
        json!({
            "name": provider,
            "provider": provider,
            "base_url": format!("https://{provider}.example"),
            "models": {"public": "provider-model"},
        })
    });
    rows[0]["bearer_token"] = json!("fixture-user:fixture-basic-secret");
    rows[0]["basic_auth"] = json!(true);
    rows[0]["base_url"] = json!("https://openai.example?key=fixture-query-secret");
    rows[1]["path"] = json!("/v1/messages");
    rows[2]["accepted_subjects"] = json!(["app-id:0xfixture-app"]);
    rows[2]["accepted_dstack_kms_root_public_keys"] = json!([kms_key]);
    rows[4]["accepted_subjects"] = json!([format!("tinfoil-release:sha256:{fixture_pin}")]);
    rows[5]["models"] =
        json!({"near-public": "Org/Example-Model", "near-alias": "lowercase-alias"});
    rows[5]["bearer_token"] = json!("fixture-near-token");
    rows[6]["accepted_subjects"] = json!(["fixture-unknown-prefix:fixture-workload"]);
    rows[7]["base_url"] = json!("http://privatemode-proxy:8080");
    rows[8]["accepted_subjects"] = json!([
        "app-id:0xfixture-app",
        format!("dstack-compose:sha256:{fixture_pin}")
    ]);
    rows[9]["accepted_subjects"] = json!([format!("tdx-measurement:sha256:{fixture_pin}")]);
    rows[10]["accepted_subjects"] = json!([format!("tdx-boot-chain:sha256:{fixture_pin}")]);
    rows[10]["bearer_token"] = json!("fixture-0g-token");
    rows[10]["zg_chain_rpc_url"] = json!("https://0g-rpc.example?key=fixture-rpc-secret");
    rows[10]["zg_serving_contract"] = json!(format!("0x{}", "ab".repeat(20)));

    let (ok, report) = check(&serde_json::to_vec(&rows).unwrap(), &options);
    assert!(ok, "{report}");
    assert!(report.starts_with("OK\nprovider_counts"));
    assert_eq!(report.lines().count(), 13);
    for row in &rows {
        let provider = &row["provider"];
        assert!(report
            .lines()
            .nth(1)
            .unwrap()
            .contains(&format!("{provider}=1")));
        assert!(report.contains(&format!(
            "upstream={provider} provider={provider} model_ids="
        )));
    }
    for expected in [
        r#"model_ids=["lowercase-alias", "Org/Example-Model"]"#,
        "review=near-compare-exact-model-ids-with-catalog",
        "review=phala-subjects",
        "review=tinfoil-release-pins",
        "review=0g-boot-chain-pins",
        "tdx-boot-chain:1",
    ] {
        assert!(report.contains(expected), "{report}");
    }

    rows[2]["accepted_dstack_kms_root_public_keys"] = json!(["invalid-fixture-kms-key"]);
    rows[7]["base_url"] = json!("http://wrong-proxy.example?token=fixture-proxy-secret");
    rows[8]["accepted_subjects"] = json!(["dstack-compose:sha256:fixture-invalid-pin"]);
    rows[10]["base_url"] = json!("http://0g.example");
    rows[10]["bearer_token"] = Value::Null;
    rows[10]["accepted_subjects"] = json!(["fixture-invalid-prefix:secret"]);
    let (ok, rejected) = check(&serde_json::to_vec(&rows).unwrap(), &options);
    assert!(!ok);
    assert_eq!(
        rejected
            .lines()
            .take_while(|line| line.starts_with("ERROR"))
            .collect::<Vec<_>>(),
        [
            r#"ERROR upstream="0g-private-computer" provider="0g-private-computer" field=accepted_subjects"#,
            r#"ERROR upstream="0g-private-computer" provider="0g-private-computer" field=base_url"#,
            r#"ERROR upstream="0g-private-computer" provider="0g-private-computer" field=bearer_token"#,
            r#"ERROR upstream="aci-service" provider="aci-service" field=accepted_dstack_kms_root_public_keys"#,
            r#"ERROR upstream="phala-direct" provider="phala-direct" field=accepted_subjects"#,
            r#"ERROR upstream="privatemode" provider="privatemode" field=base_url"#,
        ]
    );
    for output in [&report, &rejected] {
        assert!(!output.contains("fixture-"), "{output}");
        assert!(!output.contains(&fixture_pin) && !output.contains(&kms_key));
        assert!(!output.contains("://") && !output.contains("basic_auth"));
    }
}

#[test]
fn labels_deserialization_errors_without_echoing_input() {
    let input = br#"[
        {"name":"bad-pin","provider":"phala-direct","base_url":"https://example.test",
         "models":{"public":"provider-model"},"accepted_subjects":[{"fixture-pin-secret": true}]},
        {"name":"missing-url","models":{"public":"provider-model"}},
        {"name":"bad-provider","provider":"fixture-provider-secret","base_url":"https://example.test",
         "models":{"public":"provider-model"}},
        {"name":"unknown-key","base_url":"https://example.test","models":{},
         "fixture-key-secret":"fixture-value-secret"},
        {"name":"duplicate-token","bearer_token":"fixture-secret","base_url":"https://example.test",
         "models":{},"bearer_token":"fixture-other-secret"}
    ]"#;
    let (ok, report) = check(input, &UpstreamRuntimeOptions::default());
    assert!(!ok);
    assert_eq!(
        report.lines().collect::<Vec<_>>(),
        [
            r#"ERROR upstream="bad-pin" provider="phala-direct" field=accepted_subjects"#,
            r#"ERROR upstream="missing-url" provider="openai-compatible" field=base_url"#,
            r#"ERROR upstream="bad-provider" provider="<unknown>" field=provider"#,
            r#"ERROR upstream="unknown-key" provider="openai-compatible" field=<unknown-field>"#,
            r#"ERROR upstream="duplicate-token" provider="openai-compatible" field=bearer_token"#,
        ]
    );
}
