//! Claude Code settings from the verified model list.

use super::*;

pub(super) const CLAUDE_PROVIDER_SWITCHES: &[&str] = &[
    "CLAUDE_CODE_USE_BEDROCK",
    "CLAUDE_CODE_USE_VERTEX",
    "CLAUDE_CODE_USE_FOUNDRY",
    "CLAUDE_CODE_USE_MANTLE",
];

/// Claude Code settings for the connected default model, which Claude Code
/// cannot know: an ID it does not recognize gets a 1M window, 32000 output
/// tokens and Anthropic model IDs for its Opus, Sonnet, Haiku and Fable aliases,
/// which the proxy does not serve.
pub(super) fn claude_model_env(model: &CatalogModel) -> Vec<Field> {
    let mut fields = Vec::new();
    for [key, name] in CLAUDE_MODEL_ALIASES {
        fields.push(set(&["env", key], model.id()));
        fields.push(set(&["env", name], model.display_name()));
    }
    // Written absent without a limit, so a model that has none never keeps
    // the previous model's.
    let context = model.remote.context_length.filter(|value| *value > 0);
    let output = model
        .remote
        .max_output_length
        .filter(|value| *value > 0)
        .map(|output| context.map_or(output, |context| output.min(context)));
    for (key, limit) in [
        ("CLAUDE_CODE_MAX_CONTEXT_TOKENS", context),
        ("CLAUDE_CODE_MAX_OUTPUT_TOKENS", output),
    ] {
        fields.push(match limit {
            Some(limit) => set(&["env", key], limit.to_string()),
            None => absent(&["env", key]),
        });
    }
    fields
}

/// Claude Code's Opus, Sonnet, Haiku and Fable aliases and their picker names.
const CLAUDE_MODEL_ALIASES: [[&str; 2]; 4] = [
    [
        "ANTHROPIC_DEFAULT_OPUS_MODEL",
        "ANTHROPIC_DEFAULT_OPUS_MODEL_NAME",
    ],
    [
        "ANTHROPIC_DEFAULT_SONNET_MODEL",
        "ANTHROPIC_DEFAULT_SONNET_MODEL_NAME",
    ],
    [
        "ANTHROPIC_DEFAULT_HAIKU_MODEL",
        "ANTHROPIC_DEFAULT_HAIKU_MODEL_NAME",
    ],
    [
        "ANTHROPIC_DEFAULT_FABLE_MODEL",
        "ANTHROPIC_DEFAULT_FABLE_MODEL_NAME",
    ],
];

/// Whether people may change the `env` setting in Claude Code without ending
/// the connection: the model aliases and the settings for the model's limits.
pub(super) fn claude_env_is_tunable(key: &str) -> bool {
    CLAUDE_MODEL_ALIASES
        .iter()
        .flatten()
        .any(|alias| *alias == key)
        || matches!(
            key,
            "CLAUDE_CODE_MAX_CONTEXT_TOKENS"
                | "CLAUDE_CODE_MAX_OUTPUT_TOKENS"
                | "CLAUDE_CODE_AUTO_MODE_SERVER"
        )
}
