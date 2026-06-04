use std::env;
use std::fs;
use std::process;
use std::time::{SystemTime, UNIX_EPOCH};

use private_ai_gateway::aci::canonical::sha256_hex;
use private_ai_gateway::aci::keys::verify_receipt_signature;
use private_ai_gateway::aci::receipt::canonical_bytes_for_signing;
use private_ai_gateway::aci::types::{AttestationReport, Receipt, ReceiptEvent, ReceiptSignature};
use private_ai_gateway::aci::verifier::validate_aci_report_binding;
use serde_json::{json, Map, Value};

#[derive(Debug, Default)]
struct Args {
    report_path: String,
    receipt_path: String,
    nonce: Option<String>,
    request_body_path: Option<String>,
    response_body_path: Option<String>,
    skip_freshness: bool,
}

fn main() {
    if let Err(err) = run() {
        eprintln!("{err}");
        process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let args = parse_args()?;
    let report_bytes = fs::read(&args.report_path)
        .map_err(|e| format!("failed to read report {}: {e}", args.report_path))?;
    let report: AttestationReport = serde_json::from_slice(&report_bytes)
        .map_err(|e| format!("failed to parse report JSON: {e}"))?;
    let receipt_value = read_json_file(&args.receipt_path)?;
    let receipt = parse_receipt_response(receipt_value)?;

    let now_secs = if args.skip_freshness {
        report.attestation.freshness.fetched_at
    } else {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|e| format!("system time is before UNIX_EPOCH: {e}"))?
            .as_secs()
    };
    let validated = validate_aci_report_binding(
        &report,
        args.nonce.as_deref(),
        now_secs,
        Some(&report_bytes),
    )
    .map_err(|e| format!("ACI report binding failed: {e}"))?;

    let identity_match = receipt.workload_id == validated.workload_id
        && receipt.workload_keyset_digest == validated.workload_keyset_digest;
    let receipt_key = report
        .attestation
        .workload_keyset
        .receipt_signing_keys
        .iter()
        .find(|key| key.key_id == receipt.signature.key_id)
        .ok_or_else(|| {
            format!(
                "receipt signature key_id {:?} is not in the attested keyset",
                receipt.signature.key_id
            )
        })?;
    let canonical = canonical_bytes_for_signing(&receipt)
        .map_err(|e| format!("failed to canonicalize receipt for signing: {e}"))?;
    let signature = hex::decode(&receipt.signature.value_hex)
        .map_err(|e| format!("invalid receipt signature hex: {e}"))?;
    let receipt_signature_valid = verify_receipt_signature(receipt_key, &canonical, &signature);

    let request_hash_valid = match &args.request_body_path {
        Some(path) => Some(compare_body_hash(
            path,
            &receipt,
            "request.received",
            "body_hash",
        )?),
        None => None,
    };
    let response_hash_valid = match &args.response_body_path {
        Some(path) => {
            let body =
                fs::read(path).map_err(|e| format!("failed to read response body {path}: {e}"))?;
            let expected = sha256_hex(&body);
            let event = event_by_type(&receipt, "response.returned")
                .ok_or_else(|| "receipt missing response.returned event".to_string())?;
            let cleartext = event_field_str(event, "cleartext_hash");
            let wire = event_field_str(event, "wire_hash");
            Some(cleartext == Some(expected.as_str()) || wire == Some(expected.as_str()))
        }
        None => None,
    };

    let upstream_events = receipt
        .event_log
        .iter()
        .filter(|event| event.event_type == "upstream.verified")
        .map(|event| {
            json!({
                "seq": event.seq,
                "result": event_field_str(event, "result"),
                "vendor": event_field_str(event, "vendor"),
                "model_id": event_field_str(event, "model_id"),
                "verifier_id": event_field_str(event, "verifier_id"),
                "evidence": {
                    "digest": event
                    .fields
                    .get("evidence")
                    .and_then(|evidence| evidence.get("digest"))
                    .and_then(Value::as_str),
                },
                "channel_binding_count": event
                    .fields
                    .get("channel_bindings")
                    .and_then(Value::as_array)
                    .map_or(0, Vec::len),
            })
        })
        .collect::<Vec<_>>();
    let transparency_events = receipt
        .event_log
        .iter()
        .filter(|event| event.event_type.starts_with("transparency."))
        .map(|event| json!({ "seq": event.seq, "type": event.event_type }))
        .collect::<Vec<_>>();

    let verified = identity_match
        && receipt_signature_valid
        && request_hash_valid.unwrap_or(true)
        && response_hash_valid.unwrap_or(true);
    let summary = json!({
        "verified": verified,
        "report_binding_valid": true,
        "identity_match": identity_match,
        "receipt_signature_valid": receipt_signature_valid,
        "request_hash_valid": request_hash_valid,
        "response_hash_valid": response_hash_valid,
        "workload_id": validated.workload_id,
        "workload_keyset_digest": validated.workload_keyset_digest,
        "report_evidence": {
            "digest": validated
            .evidence
            .as_ref()
            .and_then(|evidence| evidence.get("digest"))
            .and_then(Value::as_str),
        },
        "receipt_id": receipt.receipt_id,
        "chat_id": receipt.chat_id,
        "upstream_events": upstream_events,
        "transparency_events": transparency_events,
    });
    println!(
        "{}",
        serde_json::to_string_pretty(&summary)
            .map_err(|e| format!("failed to serialize verification summary: {e}"))?
    );
    if verified {
        Ok(())
    } else {
        Err("ACI artifact verification failed".to_string())
    }
}

fn parse_args() -> Result<Args, String> {
    let mut args = Args::default();
    let mut iter = env::args().skip(1);
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--report" => args.report_path = next_arg(&mut iter, "--report")?,
            "--receipt" => args.receipt_path = next_arg(&mut iter, "--receipt")?,
            "--nonce" => args.nonce = Some(next_arg(&mut iter, "--nonce")?),
            "--request-body" => {
                args.request_body_path = Some(next_arg(&mut iter, "--request-body")?)
            }
            "--response-body" => {
                args.response_body_path = Some(next_arg(&mut iter, "--response-body")?)
            }
            "--skip-freshness" => args.skip_freshness = true,
            "--help" | "-h" => {
                println!(
                    "usage: cargo run --example verify_aci_artifacts -- \
                     --report report.json --receipt receipt.json [--nonce value] \
                     [--request-body request.json] [--response-body response.json] \
                     [--skip-freshness]"
                );
                process::exit(0);
            }
            other => return Err(format!("unknown argument: {other}")),
        }
    }
    if args.report_path.is_empty() {
        return Err("--report is required".to_string());
    }
    if args.receipt_path.is_empty() {
        return Err("--receipt is required".to_string());
    }
    Ok(args)
}

fn next_arg(iter: &mut impl Iterator<Item = String>, flag: &str) -> Result<String, String> {
    iter.next()
        .ok_or_else(|| format!("{flag} requires a value"))
}

fn read_json_file(path: &str) -> Result<Value, String> {
    let bytes = fs::read(path).map_err(|e| format!("failed to read {path}: {e}"))?;
    serde_json::from_slice(&bytes).map_err(|e| format!("failed to parse JSON {path}: {e}"))
}

fn parse_receipt_response(value: Value) -> Result<Receipt, String> {
    let receipt_value = value.get("receipt").cloned().unwrap_or(value);
    let obj = receipt_value
        .as_object()
        .ok_or_else(|| "receipt JSON must be an object".to_string())?;
    let signature = parse_signature(
        obj.get("signature")
            .ok_or_else(|| "receipt missing signature".to_string())?,
    )?;
    let event_log = obj
        .get("event_log")
        .and_then(Value::as_array)
        .ok_or_else(|| "receipt missing event_log array".to_string())?
        .iter()
        .map(parse_event)
        .collect::<Result<Vec<_>, _>>()?;

    Ok(Receipt {
        api_version: object_string(obj, "api_version")?,
        receipt_id: object_string(obj, "receipt_id")?,
        chat_id: match obj.get("chat_id") {
            Some(Value::Null) | None => None,
            Some(Value::String(s)) => Some(s.clone()),
            _ => return Err("receipt chat_id must be string or null".to_string()),
        },
        workload_id: object_string(obj, "workload_id")?,
        workload_keyset_digest: object_string(obj, "workload_keyset_digest")?,
        endpoint: object_string(obj, "endpoint")?,
        method: object_string(obj, "method")?,
        served_at: obj
            .get("served_at")
            .and_then(Value::as_u64)
            .ok_or_else(|| "receipt served_at must be an integer".to_string())?,
        event_log,
        signature,
    })
}

fn parse_signature(value: &Value) -> Result<ReceiptSignature, String> {
    let obj = value
        .as_object()
        .ok_or_else(|| "receipt signature must be an object".to_string())?;
    Ok(ReceiptSignature {
        algo: object_string(obj, "algo")?,
        key_id: object_string(obj, "key_id")?,
        value_hex: object_string(obj, "value")?,
    })
}

fn parse_event(value: &Value) -> Result<ReceiptEvent, String> {
    let obj = value
        .as_object()
        .ok_or_else(|| "receipt event must be an object".to_string())?;
    let seq = obj
        .get("seq")
        .and_then(Value::as_u64)
        .ok_or_else(|| "receipt event missing integer seq".to_string())?;
    let event_type = obj
        .get("type")
        .and_then(Value::as_str)
        .ok_or_else(|| "receipt event missing string type".to_string())?
        .to_string();

    if let Some(fields) = obj.get("fields") {
        return Ok(ReceiptEvent {
            seq,
            event_type,
            fields: fields.clone(),
        });
    }

    let mut fields = Map::new();
    for (key, value) in obj {
        if key != "seq" && key != "type" {
            fields.insert(key.clone(), value.clone());
        }
    }
    Ok(ReceiptEvent {
        seq,
        event_type,
        fields: Value::Object(fields),
    })
}

fn object_string(obj: &Map<String, Value>, key: &str) -> Result<String, String> {
    obj.get(key)
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| format!("missing string field {key:?}"))
}

fn event_by_type<'a>(receipt: &'a Receipt, event_type: &str) -> Option<&'a ReceiptEvent> {
    receipt
        .event_log
        .iter()
        .find(|event| event.event_type == event_type)
}

fn event_field_str<'a>(event: &'a ReceiptEvent, field: &str) -> Option<&'a str> {
    event.fields.get(field).and_then(Value::as_str)
}

fn compare_body_hash(
    path: &str,
    receipt: &Receipt,
    event_type: &str,
    field: &str,
) -> Result<bool, String> {
    let body = fs::read(path).map_err(|e| format!("failed to read body {path}: {e}"))?;
    let expected = sha256_hex(&body);
    let event = event_by_type(receipt, event_type)
        .ok_or_else(|| format!("receipt missing {event_type} event"))?;
    Ok(event_field_str(event, field) == Some(expected.as_str()))
}
