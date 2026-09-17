//! Usage normalization and cost computation.
//!
//! Pricing: rates are per-token and may arrive as a string, a number, or
//! null/empty (unset). Input and output rates fall back to zero when unset;
//! cache rates fall back to the input rate. Arithmetic is exact (`rust_decimal`);
//! only the final conversion to `f64` is lossy.

use rust_decimal::prelude::*;
use rust_decimal::Decimal;
use serde_json::Value;

struct ResolvedUsage {
    prompt: i64,
    completion: i64,
    cache_read: i64,
    cache_creation: i64,
}

// Read a token count, accepting integer- or float-encoded numbers (a provider
// may send `10.0`). Returns None only for missing/null/non-numeric values, so the
// `??`-style chain: a numeric `0` stops the chain (not treated as missing).
fn token_value(value: &Value, key: &str) -> Option<i64> {
    let field = value.get(key)?;
    field.as_i64().or_else(|| field.as_f64().map(|f| f as i64))
}

fn token_field(usage: &Value, keys: &[&str]) -> i64 {
    for key in keys {
        if let Some(value) = token_value(usage, key) {
            return value;
        }
    }
    0
}

// The cached portion of an OpenAI Responses usage, or `None` when `usage` is not
// that shape.
//
// Responses spells the total `input_tokens`, the same name Anthropic uses for
// its cache-EXCLUDED count, so the name alone cannot say whether the cache
// buckets must be added. The shape is therefore recognized only when it carries
// no other cache signal, and a usage that resolves a cache bucket some other way
// resolves exactly as it would without this function. A cached count outside
// `0..=input_tokens` is a reporting error and is treated as absent. The control
// plane resolves usage by the same rules, so the cost shown is the cost billed.
fn responses_cached_tokens(usage: &Value) -> Option<i64> {
    let carries_another_cache_signal = token_value(usage, "prompt_tokens").is_some()
        || token_value(usage, "cache_read_input_tokens").is_some()
        || token_value(usage, "cache_creation_input_tokens").is_some()
        || usage
            .get("prompt_tokens_details")
            .is_some_and(Value::is_object);
    if carries_another_cache_signal {
        return None;
    }
    let input = token_value(usage, "input_tokens")?;
    let cached = usage
        .get("input_tokens_details")
        .and_then(|details| token_value(details, "cached_tokens"))?;
    (0..=input).contains(&cached).then_some(cached)
}

fn resolve_usage(usage: &Value) -> ResolvedUsage {
    let completion = token_field(usage, &["completion_tokens", "output_tokens"]);
    let responses_cached = responses_cached_tokens(usage);
    let cache_read = token_value(usage, "cache_read_input_tokens")
        .or_else(|| {
            usage
                .get("prompt_tokens_details")
                .and_then(|d| token_value(d, "cached_tokens"))
        })
        .or(responses_cached)
        .unwrap_or(0);
    let cache_creation = token_value(usage, "cache_creation_input_tokens").unwrap_or(0);
    // OpenAI's `prompt_tokens` already includes cached tokens; Anthropic's
    // `input_tokens` excludes them (cache_read/cache_creation are separate,
    // additive buckets). Normalize to the OpenAI convention (prompt includes
    // cache) so the uncached-input subtraction in `compute_cost` is correct for
    // either family — otherwise native Anthropic usage under-counts input. A
    // Responses `input_tokens` is already that total: adding its cached portion
    // again would count those tokens twice.
    let input = token_value(usage, "input_tokens").unwrap_or(0);
    let prompt = match token_value(usage, "prompt_tokens") {
        Some(prompt_tokens) => prompt_tokens,
        None if responses_cached.is_some() => input,
        None => input + cache_read + cache_creation,
    };
    ResolvedUsage {
        prompt,
        completion,
        cache_read,
        cache_creation,
    }
}

// Parse a per-token rate. Numbers are parsed from their shortest string form for
// exact Decimal parsing. null/empty → unset.
fn rate(pricing: &Value, key: &str) -> Option<Decimal> {
    match pricing.get(key) {
        Some(Value::String(s)) if !s.is_empty() => Decimal::from_str(s).ok(),
        Some(Value::Number(n)) => Decimal::from_str(&n.to_string()).ok(),
        _ => None,
    }
}

/// Compute the request cost from usage and pricing, as an `f64`.
pub fn compute_cost(usage: &Value, pricing: &Value) -> f64 {
    let resolved = resolve_usage(usage);
    let input_rate = rate(pricing, "inputCostPerToken").unwrap_or(Decimal::ZERO);
    let cache_read_rate = rate(pricing, "cacheReadCostPerToken").unwrap_or(input_rate);
    let cache_creation_rate = rate(pricing, "cacheCreationCostPerToken").unwrap_or(input_rate);
    let output_rate = rate(pricing, "outputCostPerToken").unwrap_or(Decimal::ZERO);

    let uncached_input = (resolved.prompt - resolved.cache_read - resolved.cache_creation).max(0);

    let cost = Decimal::from(uncached_input) * input_rate
        + Decimal::from(resolved.cache_read) * cache_read_rate
        + Decimal::from(resolved.cache_creation) * cache_creation_rate
        + Decimal::from(resolved.completion) * output_rate;
    cost.to_f64().unwrap_or(0.0)
}

/// Serialize an `f64` cost the way JavaScript's `JSON.stringify` would: an
/// integer-valued cost has no decimal point, otherwise the shortest round-trip
/// form. The numeric value is identical either way.
pub fn cost_to_json(cost: f64) -> Value {
    if cost.is_finite() && cost.fract() == 0.0 && cost.abs() < 9.007_199_254_740_992e15 {
        Value::from(cost as i64)
    } else {
        serde_json::Number::from_f64(cost)
            .map(Value::Number)
            .unwrap_or(Value::from(0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn resolve_handles_openai_and_anthropic_shapes() {
        let openai = resolve_usage(&json!({
            "prompt_tokens": 100, "completion_tokens": 20,
            "prompt_tokens_details": { "cached_tokens": 10 }
        }));
        assert_eq!(
            (openai.prompt, openai.completion, openai.cache_read),
            (100, 20, 10)
        );

        // Anthropic `input_tokens` excludes cache, so `prompt` normalizes to
        // input + cache_read + cache_creation = 100 + 10 + 5 = 115 (OpenAI
        // convention: prompt includes cache).
        let anthropic = resolve_usage(&json!({
            "input_tokens": 100, "output_tokens": 20,
            "cache_read_input_tokens": 10, "cache_creation_input_tokens": 5
        }));
        assert_eq!(
            (
                anthropic.prompt,
                anthropic.completion,
                anthropic.cache_read,
                anthropic.cache_creation
            ),
            (115, 20, 10, 5)
        );
    }

    #[test]
    fn cost_uncached_input_plus_output() {
        // 90 uncached input @ 1e-6 + 20 output @ 2e-6 = 9e-5 + 4e-5 = 1.3e-4
        let usage = json!({ "prompt_tokens": 100, "completion_tokens": 20, "prompt_tokens_details": { "cached_tokens": 10 } });
        let pricing = json!({
            "inputCostPerToken": "0.000001",
            "outputCostPerToken": "0.000002",
            "cacheReadCostPerToken": "0.0000005"
        });
        // uncached = 100 - 10 - 0 = 90; cost = 90*1e-6 + 10*5e-7 + 20*2e-6
        // = 0.00009 + 0.000005 + 0.00004 = 0.000135
        let cost = compute_cost(&usage, &pricing);
        assert!((cost - 0.000135).abs() < 1e-15, "got {cost}");
    }

    #[test]
    fn cache_rates_fall_back_to_input_rate() {
        let usage =
            json!({ "input_tokens": 100, "output_tokens": 0, "cache_read_input_tokens": 40 });
        let pricing = json!({ "inputCostPerToken": "0.00001", "outputCostPerToken": "0" });
        // Anthropic input_tokens (100) excludes cache and is all uncached;
        // cache_read (40) falls back to the input rate. cost = (100 + 40) * 1e-5.
        let cost = compute_cost(&usage, &pricing);
        assert!((cost - 0.0014).abs() < 1e-15, "got {cost}");
    }

    #[test]
    fn float_encoded_token_counts_are_not_zeroed() {
        // A provider that sends `10.0` must bill the same as `10`.
        let usage = json!({ "prompt_tokens": 10.0, "completion_tokens": 20.0 });
        let pricing = json!({ "inputCostPerToken": "0.000001", "outputCostPerToken": "0.000002" });
        // 10*1e-6 + 20*2e-6 = 5e-5
        let cost = compute_cost(&usage, &pricing);
        assert!((cost - 0.00005).abs() < 1e-15, "got {cost}");
    }

    // Distinct rates per bucket, so a token in the wrong bucket changes the cost.
    // The rows and costs are the control plane's `resolveUsage` test: the cost
    // the gateway shows must be the cost the control plane bills.
    #[test]
    fn usage_shapes_cost_what_the_control_plane_bills() {
        let pricing = json!({
            "inputCostPerToken": "1", "outputCostPerToken": "2",
            "cacheReadCostPerToken": "0.1", "cacheCreationCostPerToken": "1.25"
        });
        for (name, usage, cost) in [
            (
                "OpenAI chat with a cached portion",
                json!({ "prompt_tokens": 100, "completion_tokens": 5,
                        "prompt_tokens_details": { "cached_tokens": 40 } }),
                74.0,
            ),
            (
                "Anthropic native, cache buckets added to the total",
                json!({ "input_tokens": 60, "output_tokens": 5,
                        "cache_read_input_tokens": 30, "cache_creation_input_tokens": 10 }),
                85.5,
            ),
            (
                "gateway Anthropic-to-chat transform",
                json!({ "prompt_tokens": 100, "completion_tokens": 5,
                        "cache_read_input_tokens": 30, "cache_creation_input_tokens": 20 }),
                88.0,
            ),
            (
                "Anthropic native without cache",
                json!({ "input_tokens": 100, "output_tokens": 5 }),
                110.0,
            ),
            // `input_tokens` already includes the cached portion here.
            (
                "OpenAI Responses cached portion, counted once",
                json!({ "input_tokens": 100, "output_tokens": 5,
                        "input_tokens_details": { "cached_tokens": 40 } }),
                74.0,
            ),
            // An unreadable cached count is absent: billed at the input rate.
            (
                "Responses cached count above its total",
                json!({ "input_tokens": 100, "output_tokens": 5,
                        "input_tokens_details": { "cached_tokens": 999 } }),
                110.0,
            ),
            (
                "Responses cached count negative",
                json!({ "input_tokens": 100, "output_tokens": 5,
                        "input_tokens_details": { "cached_tokens": -5 } }),
                110.0,
            ),
            (
                "Responses null details",
                json!({ "input_tokens": 100, "output_tokens": 5, "input_tokens_details": null }),
                110.0,
            ),
            // `input_tokens_details` never overrides another cache signal.
            (
                "input_tokens_details beside an Anthropic cache bucket",
                json!({ "input_tokens": 60, "output_tokens": 5, "cache_read_input_tokens": 30,
                        "input_tokens_details": { "cached_tokens": 30 } }),
                73.0,
            ),
            (
                "input_tokens_details beside prompt_tokens",
                json!({ "prompt_tokens": 100, "input_tokens": 100, "output_tokens": 5,
                        "input_tokens_details": { "cached_tokens": 40 } }),
                110.0,
            ),
            (
                "input_tokens_details beside prompt_tokens_details",
                json!({ "input_tokens": 100, "output_tokens": 5,
                        "prompt_tokens_details": { "cached_tokens": 40 },
                        "input_tokens_details": { "cached_tokens": 40 } }),
                114.0,
            ),
        ] {
            assert_eq!(compute_cost(&usage, &pricing), cost, "{name}");
        }
    }

    #[test]
    fn cost_to_json_matches_js_integer_formatting() {
        assert_eq!(cost_to_json(0.0), json!(0));
        assert_eq!(cost_to_json(0.000135), json!(0.000135));
    }
}
