use aci_verify::dstack::verify_dstack_event_log;
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

/// dstack v1 embeds the Compose hash. V3 commits to a launch document that
/// 0G's quote endpoint does not serve. Reconstruct it from RTMR3-measured boot
/// events, following 0G's pcverify (client/evidence/mrconfig.go).
pub(super) fn compose_hash(
    mr_config_id: &[u8; 48],
    rtmr3: &[u8; 48],
    reply: &Value,
) -> Option<String> {
    if mr_config_id[33..].iter().any(|byte| *byte != 0) {
        return None;
    }
    match mr_config_id[0] {
        1 => Some(hex::encode(&mr_config_id[1..33])),
        3 => compose_hash_v3(&mr_config_id[1..33], rtmr3, reply),
        _ => None,
    }
}

#[derive(Deserialize)]
struct KeyProvider {
    name: String,
    id: String,
}

fn compose_hash_v3(expected: &[u8], rtmr3: &[u8; 48], reply: &Value) -> Option<String> {
    let log = verify_dstack_event_log(reply, Some(rtmr3)).ok()?;
    let (mut app_id, mut compose_hash, mut gpu_policy_hash, mut instance_id, mut key_provider) =
        (None, None, None, None, None);
    let mut init_script_hashes = Vec::new();
    for entry in log.boot_events()? {
        let payload = entry.event_payload.as_slice();
        let field = match entry.event.as_str() {
            "app-id" => &mut app_id,
            "compose-hash" => &mut compose_hash,
            "gpu-policy-hash" => &mut gpu_policy_hash,
            "instance-id" => &mut instance_id,
            "key-provider" => &mut key_provider,
            "init-script-hash" => {
                init_script_hashes.push(hex::encode(<&[u8; 32]>::try_from(payload).ok()?));
                continue;
            }
            _ => continue,
        };
        if field.replace(payload).is_some() {
            return None;
        }
    }

    let compose_hash = hex::encode(<&[u8; 32]>::try_from(compose_hash?).ok()?);
    let key_provider: KeyProvider = serde_json::from_slice(key_provider?).ok()?;
    let kind = match key_provider.name.as_str() {
        "local-sgx" => "local",
        kind @ ("none" | "kms" | "tpm") => kind,
        _ => return None,
    };

    let mut document = json!({
        "version": 3, "compose_hash": compose_hash, "key_provider": kind
    });
    if let Some(instance_id) = instance_id.filter(|value| !value.is_empty()) {
        document["instance_id"] = hex::encode(instance_id).into();
    }
    let mut optional = vec![("init_script_hashes", json!(init_script_hashes))];
    if let Some(app_id) = app_id.filter(|value| !value.is_empty()) {
        optional.push(("app_id", hex::encode(app_id).into()));
    }
    if let Some(gpu_policy_hash) = gpu_policy_hash {
        optional.push(("gpu_policy_hash", hex::encode(gpu_policy_hash).into()));
    }
    if !key_provider.id.is_empty() {
        optional.push((
            "key_provider_id",
            hex::encode(hex::decode(key_provider.id).ok()?).into(),
        ));
    }

    // The guest allows optional fields to be absent. If present, their values
    // must match what it measured. Try at most 16 documents and only claim the
    // Compose hash when one exactly reproduces the signed register.
    for mask in 0..(1 << optional.len()) {
        let mut candidate = document.clone();
        for (index, (field, value)) in optional.iter().enumerate() {
            if mask & (1 << index) != 0 {
                candidate[field] = value.clone();
            }
        }
        let encoded = serde_json_canonicalizer::to_vec(&candidate).ok()?;
        let mut hash = Sha256::new();
        hash.update(b"dstack-mr-config-v3:\0");
        hash.update(encoded);
        if &hash.finalize()[..] == expected {
            return Some(compose_hash);
        }
    }
    None
}
