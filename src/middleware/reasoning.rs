use serde::Deserialize;
use serde_json::Value;

use super::types::{ReasoningConfig, ReasoningEffort};

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct PublicReasoning {
    effort: Option<ReasoningEffort>,
    max_tokens: Option<u64>,
    enabled: Option<bool>,
    exclude: Option<bool>,
}

pub type NormalizedReasoning = (Value, Option<ReasoningConfig>, bool);

fn implied_enabled(effort: Option<ReasoningEffort>, budget: Option<u64>) -> Option<bool> {
    effort
        .map(|effort| effort != ReasoningEffort::None)
        .or(budget.map(|_| true))
}

fn validate(config: &ReasoningConfig) -> Result<(), String> {
    if config.effort.is_none() && config.max_tokens.is_none() && config.enabled.is_none() {
        return Err("reasoning configuration is empty".into());
    }
    if config.effort.is_some() && config.max_tokens.is_some() {
        return Err("reasoning effort and max_tokens are mutually exclusive".into());
    }
    if config.max_tokens == Some(0) {
        return Err("reasoning.max_tokens must be positive".into());
    }
    if let (Some(explicit), Some(implied)) = (
        config.enabled,
        implied_enabled(config.effort, config.max_tokens),
    ) {
        if explicit != implied {
            return Err("reasoning enabled conflicts with effort or max_tokens".into());
        }
    }
    Ok(())
}

pub fn normalize_chat_request(params: &Value) -> Result<NormalizedReasoning, String> {
    let mut params = params.clone();
    let input = params
        .as_object_mut()
        .ok_or("request body must be a JSON object")?;
    let public: PublicReasoning = input
        .remove("reasoning")
        .map(serde_json::from_value)
        .transpose()
        .map_err(|error| format!("invalid reasoning: {error}"))?
        .unwrap_or_default();
    let legacy = input
        .remove("reasoning_effort")
        .map(serde_json::from_value)
        .transpose()
        .map_err(|error| format!("invalid reasoning_effort: {error}"))?;
    if public.effort.is_some() && legacy.is_some() && public.effort != legacy {
        return Err("reasoning.effort and reasoning_effort must not differ".into());
    }
    let effort = public.effort.or(legacy);
    let enabled = public
        .enabled
        .or_else(|| implied_enabled(effort, public.max_tokens));
    let requirements = (effort.is_some()
        || public.max_tokens.is_some()
        || public.enabled.is_some())
    .then_some(ReasoningConfig {
        effort,
        max_tokens: public.max_tokens,
        enabled,
    });
    if let Some(config) = &requirements {
        validate(config)?;
    }

    let include: Option<bool> = input
        .remove("include_reasoning")
        .map(serde_json::from_value)
        .transpose()
        .map_err(|_| "include_reasoning must be boolean")?;
    if public
        .exclude
        .zip(include)
        .is_some_and(|(exclude, include)| exclude == include)
    {
        return Err("reasoning.exclude conflicts with include_reasoning".into());
    }
    let exclude = public
        .exclude
        .or_else(|| include.map(|value| !value))
        .unwrap_or(false);
    Ok((params, requirements, exclude))
}

pub fn validate_effective(config: &ReasoningConfig) -> Result<(), String> {
    validate(config)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn normalization_accepts_aliases_and_rejects_contradictions() {
        for request in [
            json!({"reasoning":{"effort":"high"}}),
            json!({"reasoning_effort":"high"}),
            json!({"reasoning":{"effort":"high"},"reasoning_effort":"high"}),
        ] {
            let normalized = normalize_chat_request(&request).unwrap();
            assert_eq!(normalized.1.unwrap().effort, Some(ReasoningEffort::High));
        }
        for request in [
            json!({"reasoning":{"effort":"high"},"reasoning_effort":"low"}),
            json!({"reasoning":{"effort":"low","max_tokens":1}}),
            json!({"reasoning":{"effort":"none","enabled":true}}),
            json!({"reasoning":{"exclude":true},"include_reasoning":true}),
            json!({"reasoning":{"effort":"unknown"}}),
            json!({"reasoning":{"max_tokens":0}}),
        ] {
            assert!(normalize_chat_request(&request).is_err(), "{request}");
        }
        let normalized = normalize_chat_request(&json!({"include_reasoning":false})).unwrap();
        assert!(normalized.2);
        assert_eq!(normalized.0, json!({}));
    }
}
