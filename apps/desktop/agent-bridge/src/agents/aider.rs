//! Aider reads `~/.aider.conf.yml` (then the git root's and the working
//! directory's) and sends `openai/<model>` to `openai-api-base` with
//! `openai-api-key`, which it takes only as a value. `AIDER_*` variables beat
//! the file, including ones loaded from `~/.env` and
//! `~/.aider/oauth-keys.env`.
use super::*;

const BASE: &str = "openai-api-base";
const KEY: &str = "openai-api-key";

pub(super) fn config_path(home: &Path) -> PathBuf {
    home.join(".aider.conf.yml")
}

/// Aider creates `~/.aider` on its first run; the config file sits in Home.
pub(super) fn detection_dir(home: &Path) -> PathBuf {
    home.join(".aider")
}

pub(super) fn fields(
    inputs: &Inputs<'_>,
    base: &str,
    default_model: Option<&str>,
) -> Result<Vec<Field>, AgentError> {
    let mut fields = vec![
        set(&[BASE], format!("{base}/v1")),
        set(&[KEY], inputs.token()?),
    ];
    if let Some(model) = default_model {
        fields.push(set(&["model"], format!("openai/{model}")));
    }
    Ok(fields)
}

pub(super) fn selected_model(doc: &ConfigDoc) -> Option<String> {
    doc.get_str(&["model"])
        .and_then(|model| model.strip_prefix("openai/").map(str::to_string))
}

/// Project config files need Aider's working directory and are not inspected.
pub(super) fn validate(home: &Path, tool_env: bool, owns_model: bool) -> Result<(), AgentError> {
    let mut names = vec!["AIDER_OPENAI_API_BASE", "AIDER_OPENAI_API_KEY"];
    if owns_model {
        names.push("AIDER_MODEL");
    }
    let dotenv = [
        home.join(".env"),
        home.join(".aider").join("oauth-keys.env"),
    ];
    let overridden = names.iter().find(|name| {
        (tool_env && env::var_os(name).is_some())
            || dotenv.iter().any(|path| {
                fs::read_to_string(path).is_ok_and(|text| {
                    text.lines().any(|line| {
                        let line = line.trim_start();
                        let line = line.strip_prefix("export ").unwrap_or(line);
                        line.split_once('=')
                            .is_some_and(|(key, _)| key.trim() == **name)
                    })
                })
            })
    });
    match overridden {
        Some(name) => Err(AgentError::ConfigurationConflict(format!(
            "{name} is set in the environment, ~/.env or ~/.aider/oauth-keys.env and overrides ~/.aider.conf.yml; remove it there"
        ))),
        None => Ok(()),
    }
}
