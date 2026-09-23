//! Console credential helper bundled next to Private AI Proxy. Codex
//! (`auth.command`) and Claude Code (`apiKeyHelper`) run it as
//! `private-ai-proxy-helper --agent-token <agent>`; it prints that agent's
//! machine-local token and exits. It reads only the private token files the
//! app issues and never the RedPill key. Being a separate console binary keeps
//! stdout usable on Windows, where the GUI app has no console.

use std::process::ExitCode;

use agent_bridge::{agents, tokens::TokenFiles};

fn usage() -> String {
    let ids = agents::Agent::ALL.map(agents::Agent::id).join("|");
    format!("usage: private-ai-proxy-helper --agent-token <{ids}>")
}

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let agent = match (args.next().as_deref(), args.next(), args.next()) {
        (Some("--help" | "-h"), None, None) => {
            println!("{}", usage());
            return ExitCode::SUCCESS;
        }
        (Some("--agent-token"), Some(agent), None) => agent,
        _ => {
            eprintln!("{}", usage());
            return ExitCode::from(2);
        }
    };
    let token = agents::Agent::from_id(&agent)
        .and_then(|agent| agents::app_data_dir().map(|dir| (agent, dir)))
        .and_then(|(agent, dir)| TokenFiles::new(&dir).read(agent.id()))
        .and_then(|token| {
            token.ok_or_else(|| {
                format!(
                    "No active gateway credential. Open {}, enable protection, and reconnect this agent if it needs attention. After disconnecting, restart the agent to reload its restored configuration.",
                    agent_bridge::brand::PRODUCT_NAME
                )
            })
        });
    match token {
        Ok(token) => {
            print!("{token}");
            ExitCode::SUCCESS
        }
        Err(message) => {
            eprintln!("{message}");
            ExitCode::FAILURE
        }
    }
}
