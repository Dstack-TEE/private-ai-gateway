//! `private-ai-proxy send`: one verified chat completion, end to end.
//!
//! Verifies the service (fail closed), sends the prompt over an
//! SPKI-pinned connection capturing the exact wire bytes, then fetches
//! and verifies the receipt (spec 9.3) and the session it cites (9.2).

use std::io::{IsTerminal, Write};

use crate::aci::types::{PROVIDER_ACI_SESSION_IDS, PROVIDER_ACI_VERIFIED};
use desktop_core::sse::DataLines;
use serde_json::{json, Value};

use crate::args::SendArgs;
use crate::checks::{
    fetch_live_session, parse_receipt_document, run_response_checks, BodyDigest, UpstreamContext,
};
use crate::client::HttpResult;
use crate::verify::{verify_service, ServiceVerification};

const DEFAULT_PROMPT: &str = "Say hello and name the model serving this request.";

pub async fn run(args: SendArgs, require_production_os: bool) -> Result<i32, String> {
    let bearer = if args.api_key_stdin {
        if std::io::stdin().is_terminal() {
            return Err("Refusing to read the API key from a terminal with --api-key-stdin; pipe it in or set ACI_API_KEY.".into());
        }
        Some(crate::read_api_key(std::io::stdin())?)
    } else if let Some(key) = args.api_key.clone() {
        // Like the `aci` alias note, never mixed into JSON-mode stderr.
        if !args.json {
            tracing::warn!("warning: --api-key exposes the key to other local processes and will be removed in 0.3; use --api-key-stdin or ACI_API_KEY.");
        }
        Some(key)
    } else {
        std::env::var("ACI_API_KEY").ok()
    };
    let verification = verify_service(
        &args.base_url,
        None,
        &args.accepted_composes,
        require_production_os,
        false,
    )
    .await?;
    if !args.json {
        println!("== service verification: {} ==", verification.base_url);
        print!("{}", verification.transcript.render_human(false));
        println!();
    }
    if !verification.transcript.verified() {
        if args.json {
            print_json(&verification.transcript.to_json(false))?;
        }
        return Err("service verification failed; not sending the prompt (fail closed)".into());
    }

    let pins = verification.attested_spkis();
    let ServiceVerification {
        mut transcript,
        report,
        identity,
        client,
        base_url,
        host,
        ..
    } = verification;

    // Enforce the just-verified TLS keys on every further connection to this host.
    client.pin(&host, &pins)?;

    let model = match &args.model {
        Some(model) => model.clone(),
        None => first_model(&client, &base_url, bearer.as_deref()).await?,
    };
    let prompt = args
        .prompt
        .clone()
        .unwrap_or_else(|| DEFAULT_PROMPT.to_string());
    let stream = !args.no_stream;
    let mut body = json!({
        "model": model,
        "messages": [{ "role": "user", "content": prompt }],
        "stream": stream,
    });
    // §5.3 serving constraints ride in the body; the service consumes and
    // strips the member before forwarding. Verified serving is the default —
    // `--allow-unverified` drops the demand.
    if !args.allow_unverified || !args.sessions.is_empty() {
        let mut provider = serde_json::Map::new();
        if !args.allow_unverified {
            provider.insert(PROVIDER_ACI_VERIFIED.to_string(), json!(true));
        }
        if !args.sessions.is_empty() {
            provider.insert(PROVIDER_ACI_SESSION_IDS.to_string(), json!(args.sessions));
        }
        body["provider"] = Value::Object(provider);
    }
    let request_body =
        serde_json::to_vec(&body).map_err(|e| format!("failed to serialize request body: {e}"))?;

    if !args.json {
        println!("model:  {model}");
        println!("prompt: {prompt}");
        print!("reply:  ");
        let _ = std::io::stdout().flush();
    }
    let mut collector = SseTextCollector::new(stream && !args.json);
    let response = client
        .post_chat_captured(
            &base_url,
            bearer.as_deref(),
            request_body.clone(),
            |chunk| {
                if stream {
                    collector.feed(chunk);
                }
            },
        )
        .await?;
    collector.finish();
    if let Err(e) = response.error_for_status("chat completion") {
        // The `reply:  ` prefix was already printed (no newline); close the line
        // so the error does not glue onto it as if it were the reply.
        if !args.json {
            println!();
        }
        return Err(e);
    }

    let response_text = if stream {
        collector.text
    } else {
        buffered_response_text(&response)
    };
    if !args.json {
        if !stream {
            print!("{response_text}");
        }
        println!();
        println!();
    }

    // A streaming response the aggregator committed before selecting an
    // upstream (pre-first-byte keep-alive, §5.2) carries no X-Receipt-Id
    // header; the receipt still exists once the stream finalized and is
    // retrievable by the response's own id. That is only legitimate for an
    // unverified stream — a constrained request (the default) is never
    // committed early, so a missing header there is a §5.2 violation this
    // command must keep surfacing, not paper over.
    let receipt_id = match response
        .headers
        .get("x-receipt-id")
        .and_then(|value| value.to_str().ok())
        .map(str::to_string)
    {
        Some(id) => id,
        None if args.allow_unverified && stream => response_chat_id(&response.body)
            .ok_or("response carried no X-Receipt-Id header and no readable response id")?,
        None => return Err("response carried no X-Receipt-Id header".into()),
    };
    let receipt_resp = client
        .fetch_receipt(&base_url, &receipt_id, bearer.as_deref())
        .await?;
    receipt_resp.error_for_status("receipt fetch")?;
    let receipt = parse_receipt_document(receipt_resp.json()?)?;

    // id-2 already established the identity; a verified run always carries it.
    let identity = identity.ok_or("verified run carried no established identity")?;
    // Audit the cited session (§9.2): its id is the hash of the parsed
    // document's JCS form, so any served encoding verifies.
    let (session_resp, no_session_reason) = fetch_live_session(&client, &base_url, &receipt).await;
    run_response_checks(
        &mut transcript,
        &receipt,
        &identity,
        Some(&BodyDigest::of(&request_body)),
        Some(&BodyDigest::of(&response.body)),
        UpstreamContext {
            session_bytes: session_resp.as_ref().map(|resp| resp.body.as_slice()),
            no_session_reason: &no_session_reason,
            pinned: (!args.sessions.is_empty()).then_some(args.sessions.as_slice()),
            // §5.3: pinning sessions implies verified serving.
            requires_verified: !args.allow_unverified || !args.sessions.is_empty(),
            serving: &report.service_capabilities.serving,
            required_claims: &args.require_claims,
        },
    );

    if args.json {
        let mut output = transcript.to_json(false);
        output["model"] = json!(model);
        output["receipt_id"] = json!(receipt_id);
        output["request_body"] = json!(String::from_utf8_lossy(&request_body));
        output["response_text"] = json!(response_text);
        print_json(&output)?;
    } else {
        println!("== receipt verification: {receipt_id} ==");
        print!("{}", transcript.render_human(false));
    }
    Ok(if transcript.verified() { 0 } else { 1 })
}

fn print_json(value: &Value) -> Result<(), String> {
    println!(
        "{}",
        serde_json::to_string_pretty(value).map_err(|e| format!("failed to serialize: {e}"))?
    );
    Ok(())
}

async fn first_model(
    client: &crate::client::AciClient,
    base_url: &str,
    bearer: Option<&str>,
) -> Result<String, String> {
    let resp = client.fetch_models(base_url, bearer).await?;
    resp.error_for_status("GET /v1/models")?;
    resp.json()?["data"][0]["id"]
        .as_str()
        .map(str::to_string)
        .ok_or_else(|| "GET /v1/models returned no models; pass --model".to_string())
}

/// The response's own `id`, from a buffered JSON body or the first SSE data
/// event that carries one. The receipt endpoint accepts it as a lookup key.
fn response_chat_id(body: &[u8]) -> Option<String> {
    let id = |data: &[u8]| {
        serde_json::from_slice::<Value>(data)
            .ok()
            .and_then(|value| value["id"].as_str().map(str::to_string))
    };
    if let Some(id) = id(body) {
        return Some(id);
    }
    let mut found = None;
    let mut lines = DataLines::new();
    let mut first = |data: &str| {
        if found.is_none() {
            found = id(data.as_bytes());
        }
    };
    lines.push(body, &mut first);
    lines.finish(&mut first);
    found
}

fn buffered_response_text(response: &HttpResult) -> String {
    serde_json::from_slice::<Value>(&response.body)
        .ok()
        .and_then(|value| {
            value["choices"][0]["message"]["content"]
                .as_str()
                .map(str::to_string)
        })
        .unwrap_or_else(|| String::from_utf8_lossy(&response.body).into_owned())
}

/// Collects the streamed `choices[0].delta.content` text across chunk
/// boundaries, and optionally echoes it as it arrives. The wire bytes
/// themselves are captured separately, untouched.
struct SseTextCollector {
    lines: DataLines,
    text: String,
    echo: bool,
}

impl SseTextCollector {
    fn new(echo: bool) -> Self {
        Self {
            lines: DataLines::new(),
            text: String::new(),
            echo,
        }
    }

    fn feed(&mut self, chunk: &[u8]) {
        let (text, echo) = (&mut self.text, self.echo);
        self.lines
            .push(chunk, |data| append_delta(text, echo, data));
    }

    fn finish(&mut self) {
        let (text, echo) = (&mut self.text, self.echo);
        self.lines.finish(|data| append_delta(text, echo, data));
    }
}

/// Appends an event's content delta; `[DONE]` and other events carry none.
fn append_delta(text: &mut String, echo: bool, data: &str) {
    let Ok(value) = serde_json::from_str::<Value>(data) else {
        return;
    };
    if let Some(delta) = value["choices"][0]["delta"]["content"].as_str() {
        text.push_str(delta);
        if echo {
            print!("{delta}");
            let _ = std::io::stdout().flush();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{response_chat_id, SseTextCollector};

    #[test]
    fn reads_the_response_id_from_json_or_the_first_event_carrying_one() {
        assert_eq!(
            response_chat_id(br#"{"id":"chat-1"}"#).as_deref(),
            Some("chat-1")
        );
        assert_eq!(
            response_chat_id(
                b": keep-alive\n\ndata: {\"id\":\"chat-2\"}\r\n\ndata: {\"id\":\"chat-3\"}\n"
            )
            .as_deref(),
            Some("chat-2")
        );
        assert_eq!(response_chat_id(b"data: [DONE]\n"), None);
    }

    #[test]
    fn collects_deltas_across_chunk_boundaries() {
        let mut c = SseTextCollector::new(false);
        c.feed(b"data: {\"choices\":[{\"delta\":{\"content\":\"Hel\"}}]}\n\ndata: {\"choi");
        c.feed(b"ces\":[{\"delta\":{\"content\":\"lo\"}}]}\n\ndata: [DONE]\n\n");
        c.finish();
        assert_eq!(c.text, "Hello");
    }

    #[test]
    fn handles_no_space_data_prefix_and_crlf() {
        let mut c = SseTextCollector::new(false);
        c.feed(b"data:{\"choices\":[{\"delta\":{\"content\":\"hi\"}}]}\r\n");
        c.finish();
        assert_eq!(c.text, "hi");
    }
}
