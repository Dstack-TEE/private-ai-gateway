//! Console credential helper bundled next to the desktop app. Codex
//! (`auth.command`) and Claude Code (`apiKeyHelper`) run it as
//! `private-ai-gateway-helper --agent-token <agent>`; it prints that agent's
//! machine-local token and exits. It reads only the private token files the
//! app issues and never the RedPill key. Being a separate console binary keeps
//! stdout usable on Windows, where the GUI app has no console.

use std::process::ExitCode;

use desktop_gateway::{agents, tokens::TokenFiles};

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let agent = match (args.next().as_deref(), args.next()) {
        (Some("--agent-token"), Some(agent)) => agent,
        _ => {
            eprintln!(
                "usage: private-ai-gateway-helper --agent-token <codex|claude-code|opencode>"
            );
            return ExitCode::from(2);
        }
    };
    let token = agents::Agent::from_id(&agent)
        .and_then(|agent| agents::app_data_dir().map(|dir| (agent, dir)))
        .and_then(|(agent, dir)| TokenFiles::new(&dir).read(agent.id()))
        .and_then(|token| {
            token.ok_or_else(|| "This agent is not connected in Private AI Gateway".to_string())
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
