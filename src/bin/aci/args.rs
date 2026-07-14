//! Hand-rolled argument parsing for the `aci` CLI.

use std::process;

pub const USAGE: &str = "usage:
  aci verify <base-url> [--nonce <string>] [--json] [--explain]
      Fetch /v1/aci/attestation with a fresh nonce, run the spec \u{a7}10.1 checks,
      print a verification transcript; exit 0 only if the verdict is VERIFIED.

  aci audit --report <file> [--receipt <file>] [--nonce <value>]
            [--request-body <file>] [--response-body <file>] [--session <file>]
            [--skip-expiry] [--json]
      Offline verification of saved artifacts using the same transcript engine.

  aci chat <base-url> [--model <id>] [--prompt <text>] [--api-key <key>]
           [--no-stream] [--json]
      Verify the service (fail closed), send one chat completion over an
      SPKI-pinned connection, then fetch and verify its receipt.
      The API key is also read from the ACI_API_KEY environment variable.

  aci serve <base-url> [--listen <addr:port>]
      Local OpenAI-compatible proxy (default 127.0.0.1:4180, plain HTTP on
      localhost). Verifies the service on startup and refuses to start unless
      VERIFIED, pins the attested TLS key on every upstream hop, and verifies
      each response's receipt after the fact.";

#[derive(Debug)]
pub struct VerifyArgs {
    pub base_url: String,
    pub nonce: Option<String>,
    pub json: bool,
    pub explain: bool,
}

#[derive(Debug, Default)]
pub struct AuditArgs {
    pub report: String,
    pub receipt: Option<String>,
    pub nonce: Option<String>,
    pub request_body: Option<String>,
    pub response_body: Option<String>,
    pub session: Option<String>,
    pub skip_expiry: bool,
    pub json: bool,
}

#[derive(Debug)]
pub struct ServeArgs {
    pub base_url: String,
    pub listen: Option<String>,
}

#[derive(Debug)]
pub struct ChatArgs {
    pub base_url: String,
    pub model: Option<String>,
    pub prompt: Option<String>,
    pub api_key: Option<String>,
    pub no_stream: bool,
    pub json: bool,
}

pub enum Command {
    Verify(VerifyArgs),
    Audit(AuditArgs),
    Chat(ChatArgs),
    Serve(ServeArgs),
}

pub fn parse(mut args: impl Iterator<Item = String>) -> Result<Command, String> {
    let subcommand = args.next().unwrap_or_default();
    match subcommand.as_str() {
        "verify" => parse_verify(args),
        "audit" => parse_audit(args),
        "chat" => parse_chat(args),
        "serve" => parse_serve(args),
        "--help" | "-h" | "help" => {
            println!("{USAGE}");
            process::exit(0);
        }
        "" => Err(format!("missing subcommand\n{USAGE}")),
        other => Err(format!("unknown subcommand: {other}\n{USAGE}")),
    }
}

fn parse_verify(mut args: impl Iterator<Item = String>) -> Result<Command, String> {
    let mut base_url = None;
    let mut nonce = None;
    let mut json = false;
    let mut explain = false;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--nonce" => nonce = Some(next_arg(&mut args, "--nonce")?),
            "--json" => json = true,
            "--explain" => explain = true,
            "--help" | "-h" => help(),
            other if !other.starts_with('-') && base_url.is_none() => {
                base_url = Some(other.to_string())
            }
            other => return Err(format!("verify: unexpected argument: {other}")),
        }
    }
    Ok(Command::Verify(VerifyArgs {
        base_url: base_url.ok_or("verify: <base-url> is required")?,
        nonce,
        json,
        explain,
    }))
}

fn parse_audit(mut args: impl Iterator<Item = String>) -> Result<Command, String> {
    let mut parsed = AuditArgs::default();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--report" => parsed.report = next_arg(&mut args, "--report")?,
            "--receipt" => parsed.receipt = Some(next_arg(&mut args, "--receipt")?),
            "--nonce" => parsed.nonce = Some(next_arg(&mut args, "--nonce")?),
            "--request-body" => parsed.request_body = Some(next_arg(&mut args, "--request-body")?),
            "--response-body" => {
                parsed.response_body = Some(next_arg(&mut args, "--response-body")?)
            }
            "--session" => parsed.session = Some(next_arg(&mut args, "--session")?),
            "--skip-expiry" => parsed.skip_expiry = true,
            "--json" => parsed.json = true,
            "--help" | "-h" => help(),
            other => return Err(format!("audit: unexpected argument: {other}")),
        }
    }
    if parsed.report.is_empty() {
        return Err("audit: --report is required".to_string());
    }
    Ok(Command::Audit(parsed))
}

fn parse_chat(mut args: impl Iterator<Item = String>) -> Result<Command, String> {
    let mut base_url = None;
    let mut model = None;
    let mut prompt = None;
    let mut api_key = None;
    let mut no_stream = false;
    let mut json = false;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--model" => model = Some(next_arg(&mut args, "--model")?),
            "--prompt" => prompt = Some(next_arg(&mut args, "--prompt")?),
            "--api-key" => api_key = Some(next_arg(&mut args, "--api-key")?),
            "--no-stream" => no_stream = true,
            "--json" => json = true,
            "--help" | "-h" => help(),
            other if !other.starts_with('-') && base_url.is_none() => {
                base_url = Some(other.to_string())
            }
            other => return Err(format!("chat: unexpected argument: {other}")),
        }
    }
    Ok(Command::Chat(ChatArgs {
        base_url: base_url.ok_or("chat: <base-url> is required")?,
        model,
        prompt,
        api_key,
        no_stream,
        json,
    }))
}

fn parse_serve(mut args: impl Iterator<Item = String>) -> Result<Command, String> {
    let mut base_url = None;
    let mut listen = None;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--listen" => listen = Some(next_arg(&mut args, "--listen")?),
            "--help" | "-h" => help(),
            other if !other.starts_with('-') && base_url.is_none() => {
                base_url = Some(other.to_string())
            }
            other => return Err(format!("serve: unexpected argument: {other}")),
        }
    }
    Ok(Command::Serve(ServeArgs {
        base_url: base_url.ok_or("serve: <base-url> is required")?,
        listen,
    }))
}

fn next_arg(iter: &mut impl Iterator<Item = String>, flag: &str) -> Result<String, String> {
    iter.next()
        .ok_or_else(|| format!("{flag} requires a value"))
}

fn help() -> ! {
    println!("{USAGE}");
    process::exit(0);
}
