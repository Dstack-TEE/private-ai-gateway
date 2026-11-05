//! Buffered reranking: strict public request and response schemas.

use std::collections::HashSet;

use serde_json::{Map, Value};

use super::images::RequestError;

pub fn validate_request(body: &Value) -> Result<(), RequestError> {
    let invalid = |param: &str, message: &str| RequestError {
        code: "invalid_parameter",
        param: param.into(),
        message: message.into(),
    };
    let object = body
        .as_object()
        .ok_or_else(|| invalid("body", "request body must be a JSON object"))?;
    for key in object.keys() {
        if ![
            "model",
            "query",
            "documents",
            "top_n",
            "return_documents",
            "user",
            "provider",
        ]
        .contains(&key.as_str())
        {
            return Err(RequestError {
                code: "unknown_parameter",
                param: key.clone(),
                message: format!("unknown parameter: {key}"),
            });
        }
    }
    for key in ["model", "query"] {
        if !body.get(key).is_some_and(Value::is_string) {
            return Err(invalid(key, "required parameter must be a string"));
        }
    }
    if body.get("user").is_some_and(|v| !v.is_string()) {
        return Err(invalid("user", "parameter must be a string"));
    }
    let documents = body["documents"]
        .as_array()
        .filter(|v| (1..=1000).contains(&v.len()))
        .ok_or_else(|| invalid("documents", "documents must contain 1 to 1000 entries"))?;
    if !documents.iter().all(|v| {
        v.is_string()
            || v.as_object()
                .is_some_and(|v| v.len() == 1 && v.get("text").is_some_and(Value::is_string))
    }) {
        return Err(invalid(
            "documents",
            "each document must be a string or an object containing only text",
        ));
    }
    if body.get("top_n").is_some_and(|v| {
        !v.as_u64()
            .is_some_and(|n| n >= 1 && n <= documents.len() as u64)
    }) {
        return Err(invalid(
            "top_n",
            "top_n must be an integer from 1 to the document count",
        ));
    }
    if body
        .get("return_documents")
        .is_some_and(|v| !v.is_boolean())
    {
        return Err(invalid("return_documents", "parameter must be a boolean"));
    }
    Ok(())
}

pub fn upstream_body(body: &Value) -> Value {
    let mut output = Map::new();
    for key in ["model", "query", "documents", "top_n", "return_documents"] {
        if let Some(value) = body.get(key) {
            output.insert(key.into(), value.clone());
        }
    }
    Value::Object(output)
}

pub fn validate_response(body: &Value, request: &Value) -> Result<(), &'static str> {
    let message = "upstream returned a malformed rerank success body";
    let documents = request["documents"].as_array().ok_or(message)?.len();
    let top_n = request["top_n"].as_u64().unwrap_or(documents as u64);
    let results = body["results"].as_array().ok_or(message)?;
    let mut indices = HashSet::new();
    if results.len() as u64 > top_n
        || results.iter().any(|result| {
            !result["index"]
                .as_u64()
                .is_some_and(|index| index < documents as u64 && indices.insert(index))
                || !result["relevance_score"]
                    .as_f64()
                    .is_some_and(f64::is_finite)
                || result
                    .get("document")
                    .is_some_and(|document| !document.get("text").is_some_and(Value::is_string))
        })
    {
        return Err(message);
    }
    // Meter only validated input tokens; never turn absent usage into a free request.
    let prompt = body["usage"]["prompt_tokens"]
        .as_i64()
        .filter(|n| *n >= 0)
        .ok_or(message)?;
    let total = body["usage"]["total_tokens"].as_i64().ok_or(message)?;
    if total < prompt {
        return Err(message);
    }
    Ok(())
}

pub fn canonicalize(body: &mut Value) {
    let Some(object) = body.as_object_mut() else {
        return;
    };
    object.retain(|key, value| match key.as_str() {
        "id" | "model" => value.is_string(),
        "results" => value.is_array(),
        "usage" => value.is_object(),
        _ => false,
    });
    if let Some(results) = object.get_mut("results").and_then(Value::as_array_mut) {
        for result in results {
            if let Some(result) = result.as_object_mut() {
                result.retain(|key, _| {
                    ["index", "relevance_score", "document"].contains(&key.as_str())
                });
                if let Some(document) = result.get_mut("document").and_then(Value::as_object_mut) {
                    document.retain(|key, _| key == "text");
                }
            }
        }
    }
    if let Some(usage) = object.get_mut("usage").and_then(Value::as_object_mut) {
        usage.retain(|key, value| match key.as_str() {
            "prompt_tokens" | "total_tokens" => value.is_u64(),
            "cost" => value.is_number(),
            _ => false,
        });
    }
}

pub fn response_body(response: &[u8], request: &[u8]) -> Option<Vec<u8>> {
    let request: Value = serde_json::from_slice(request).ok()?;
    let mut body: Value = serde_json::from_slice(response).ok()?;
    validate_response(&body, &request).ok()?;
    canonicalize(&mut body);
    serde_json::to_vec(&body).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn validates_inputs_without_forwarding_routing_metadata() {
        let request = json!({"model":"reranker","query":"q","documents":["a",{"text":"b"}],"top_n":2,"return_documents":true,"provider":{},"user":"alice"});
        validate_request(&request).unwrap();
        let upstream = upstream_body(&request);
        assert!(upstream.get("provider").is_none());
        assert!(upstream.get("user").is_none());
        for patch in [
            json!({"documents":[]}),
            json!({"documents":[{"text":"a","url":"private"}]}),
            json!({"query":1}),
            json!({"top_n":0}),
            json!({"top_n":3}),
            json!({"top_n":1.5}),
            json!({"return_documents":1}),
            json!({"stream":false}),
        ] {
            let mut body = request.clone();
            body.as_object_mut()
                .unwrap()
                .extend(patch.as_object().unwrap().clone());
            assert!(validate_request(&body).is_err(), "{body}");
        }
    }

    #[test]
    fn rejects_bad_indices_scores_and_missing_usage() {
        let request = json!({"documents":["a","b"],"top_n":1});
        let success = json!({"results":[{"index":1,"relevance_score":0.8}],"usage":{"prompt_tokens":5,"total_tokens":5}});
        validate_response(&success, &request).unwrap();
        for patch in [
            json!({"results":[{"index":2,"relevance_score":0.8}]}),
            json!({"results":[{"index":1,"relevance_score":"0.8"}]}),
            json!({"results":[{"index":1,"relevance_score":0.8},{"index":1,"relevance_score":0.7}]}),
            json!({"usage":null}),
            json!({"usage":{"prompt_tokens":-1,"total_tokens":5}}),
            json!({"usage":{"prompt_tokens":5,"total_tokens":4}}),
        ] {
            let mut body = success.clone();
            body.as_object_mut()
                .unwrap()
                .extend(patch.as_object().unwrap().clone());
            assert!(validate_response(&body, &request).is_err(), "{body}");
        }
        let duplicate = json!({"results":[{"index":0,"relevance_score":0.8},{"index":0,"relevance_score":0.7}],"usage":{"prompt_tokens":5,"total_tokens":5}});
        assert!(validate_response(&duplicate, &json!({"documents":["a","b"]})).is_err());
    }
}
