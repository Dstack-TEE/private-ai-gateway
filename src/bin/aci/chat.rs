//! `aci chat`: end-to-end demo. Verify the service (fail closed), send one
//! chat completion over an SPKI-pinned connection capturing the exact wire
//! bytes, then fetch the receipt with the same credential and verify it —
//! §10.2 plus the §10.3 shallow and (when a session is recorded) deep audit.

use std::io::Write;

use serde_json::{json, Value};

use crate::args::ChatArgs;
use crate::checks::{
    established_identity, fetch_live_session, parse_receipt_envelope, run_receipt_checks,
    run_upstream_checks, ReceiptContext,
};
use crate::client::HttpResult;
use crate::verify::{verify_service, ServiceVerification};

const DEFAULT_PROMPT: &str = "Say hello and name the model serving this request.";

pub async fn run(args: ChatArgs) -> Result<i32, String> {
    let verification = verify_service(&args.base_url, None, false).await?;
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

    let ServiceVerification {
        mut transcript,
        report,
        client,
        base_url,
        host,
        observed_spki,
    } = verification;

    // Enforce the just-verified SPKI on every further connection to this host.
    if let Some(spki) = &observed_spki {
        client.pin(&host, spki);
    }

    let bearer = args
        .api_key
        .clone()
        .or_else(|| std::env::var("ACI_API_KEY").ok());
    let model = match &args.model {
        Some(model) => model.clone(),
        None => first_model(&client, &base_url, bearer.as_deref()).await?,
    };
    let prompt = args
        .prompt
        .clone()
        .unwrap_or_else(|| DEFAULT_PROMPT.to_string());
    let stream = !args.no_stream;
    let request_body = serde_json::to_vec(&json!({
        "model": model,
        "messages": [{ "role": "user", "content": prompt }],
        "stream": stream,
    }))
    .map_err(|e| format!("failed to serialize request body: {e}"))?;

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

    let receipt_id = response
        .headers
        .get("x-receipt-id")
        .and_then(|value| value.to_str().ok())
        .map(str::to_string)
        .ok_or("response carried no X-Receipt-Id header")?;
    let receipt_resp = client
        .fetch_receipt(&base_url, &receipt_id, bearer.as_deref())
        .await?;
    receipt_resp.error_for_status("receipt fetch")?;
    let receipt = parse_receipt_envelope(receipt_resp.json()?)?;

    let identity = established_identity(&report)?;
    run_receipt_checks(
        &mut transcript,
        ReceiptContext::new(
            &receipt,
            &identity,
            Some(&request_body),
            Some(&response.body),
        ),
    );

    // Deep audit when the receipt commits to an attested session. The audit
    // runs over the exact served bytes (§9): their hash is the session id.
    let (session_resp, no_session_reason) =
        fetch_live_session(&client, &base_url, &receipt.payload).await;
    run_upstream_checks(
        &mut transcript,
        &receipt.payload,
        session_resp.as_ref().map(|resp| resp.body.as_slice()),
        &no_session_reason,
        false,
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

/// Reassembles SSE lines across chunk boundaries, collects the streamed
/// `choices[0].delta.content` text, and optionally echoes it as it arrives.
/// The wire bytes themselves are captured separately, untouched.
struct SseTextCollector {
    buf: Vec<u8>,
    text: String,
    echo: bool,
}

impl SseTextCollector {
    fn new(echo: bool) -> Self {
        Self {
            buf: Vec::new(),
            text: String::new(),
            echo,
        }
    }

    fn feed(&mut self, chunk: &[u8]) {
        self.buf.extend_from_slice(chunk);
        while let Some(pos) = self.buf.iter().position(|b| *b == b'\n') {
            let line: Vec<u8> = self.buf.drain(..=pos).collect();
            self.line(&line);
        }
    }

    fn finish(&mut self) {
        if !self.buf.is_empty() {
            let rest = std::mem::take(&mut self.buf);
            self.line(&rest);
        }
    }

    fn line(&mut self, raw: &[u8]) {
        let Ok(line) = std::str::from_utf8(raw) else {
            return;
        };
        // Handles both `data:{..}` and `data: {..}`, and CRLF framing.
        let Some(data) = line.trim_end_matches(['\r', '\n']).strip_prefix("data:") else {
            return;
        };
        let data = data.trim();
        if data.is_empty() || data == "[DONE]" {
            return;
        }
        let Ok(value) = serde_json::from_str::<Value>(data) else {
            return;
        };
        if let Some(delta) = value["choices"][0]["delta"]["content"].as_str() {
            self.text.push_str(delta);
            if self.echo {
                print!("{delta}");
                let _ = std::io::stdout().flush();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::SseTextCollector;

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
