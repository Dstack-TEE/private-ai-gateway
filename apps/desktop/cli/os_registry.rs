//! Client-pinned dstack governance policy, independent of gateway evidence.
//! Normal CA-validated HTTPS (never the gateway's attested-SPKI transport).

use std::{future::Future, time::Duration};

use futures_util::{future::join_all, StreamExt};
use serde_json::{json, Value};

// DstackKms.allowedOsImages(bytes32), Ethereum mainnet (chain id 1).
// https://github.com/Dstack-TEE/dstack/blob/next/dstack/kms/auth-eth/contracts/DstackKms.sol
pub const CONTRACT: &str = "0xd343a3f5593b93D8056aB5D60c433622d7D65a80";
const SELECTOR: &str = "9a4e1d18";
const DEFAULT_RPC_URLS: [&str; 2] = [
    "https://ethereum-rpc.publicnode.com",
    "https://eth.drpc.org",
];
const RPC_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_RESPONSE_BYTES: usize = 4096;
const REQUEST_FAILED: &str =
    "OS registry provider request failed or timed out; approval could not be established";

fn rpc_urls(configured: Option<&[String]>) -> Result<Vec<String>, &'static str> {
    let urls = configured.map_or_else(
        || DEFAULT_RPC_URLS.iter().map(|url| (*url).into()).collect(),
        <[String]>::to_vec,
    );
    desktop_core::config::validate_os_registry_rpc_urls(&urls)?;
    Ok(urls)
}

pub async fn check(hash: &[u8; 32], configured: Option<&[String]>) -> Result<(), String> {
    // Configuration is passed by the caller, never read from disk here.
    // Every verification reads approvals anew because they can be revoked.
    let urls = rpc_urls(configured)?;
    let client = reqwest::Client::builder()
        .https_only(true)
        .redirect(reqwest::redirect::Policy::none())
        .timeout(RPC_TIMEOUT)
        .build()
        .map_err(|_| "Cannot initialize OS registry HTTPS client")?;
    check_with_rpc(hash, &urls, |url, body| rpc(&client, url, body)).await
}

fn call_data(hash: &[u8; 32]) -> String {
    format!("0x{SELECTOR}{}", hex::encode(hash))
}

/// The injected executor keeps policy tests offline; reqwest owns the deadline.
async fn check_with_rpc<F, Fut>(hash: &[u8; 32], urls: &[String], request: F) -> Result<(), String>
where
    F: Fn(String, Value) -> Fut,
    Fut: Future<Output = Result<Value, &'static str>>,
{
    let approvals = join_all(urls.iter().map(|url| async {
        let chain = json!({"jsonrpc":"2.0", "id":1, "method":"eth_chainId", "params":[]});
        let chain = request(url.clone(), chain).await?;
        if rpc_result(&chain, 1)? != "0x1" {
            return Err("OS registry provider is not on Ethereum mainnet (chain 1)");
        }
        let call = json!({"jsonrpc":"2.0", "id":2, "method":"eth_call", "params":[
            {"to":CONTRACT, "data":call_data(hash)}, "finalized"
        ]});
        let result = request(url.clone(), call).await?;
        let result = rpc_result(&result, 2)?;
        let mut word = [0u8; 32];
        let encoded = result
            .strip_prefix("0x")
            .ok_or("OS registry returned a malformed ABI boolean")?;
        hex::decode_to_slice(encoded, &mut word)
            .map_err(|_| "OS registry returned a malformed ABI boolean")?;
        if word[..31] != [0u8; 31] || word[31] > 1 {
            return Err("OS registry returned a malformed ABI boolean");
        }
        Ok(word[31] == 1)
    }))
    .await
    .into_iter()
    .collect::<Result<Vec<_>, _>>()?;
    if approvals.iter().any(|approved| *approved) && approvals.iter().any(|approved| !*approved) {
        return Err(format!(
            "OS registry providers disagree about os-image-hash {} (chain 1)",
            hex::encode(hash)
        ));
    }
    if !approvals.iter().all(|approved| *approved) {
        return Err(format!(
            "os-image-hash {} is not approved in the dstack KMS registry (chain 1)",
            hex::encode(hash)
        ));
    }
    Ok(())
}

fn rpc_result(value: &Value, id: u64) -> Result<&str, &'static str> {
    if value["jsonrpc"] != "2.0" || value["id"] != id || value.get("error").is_some() {
        return Err("OS registry provider returned an invalid JSON-RPC response or an error");
    }
    value["result"]
        .as_str()
        .ok_or("OS registry provider returned no valid result")
}

async fn rpc(client: &reqwest::Client, url: String, body: Value) -> Result<Value, &'static str> {
    let response = client
        .post(url)
        .json(&body)
        .send()
        .await
        .map_err(|_| REQUEST_FAILED)?;
    if !response.status().is_success() {
        return Err(REQUEST_FAILED);
    }
    let mut stream = response.bytes_stream();
    let mut bytes = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| REQUEST_FAILED)?;
        if bytes.len().saturating_add(chunk.len()) > MAX_RESPONSE_BYTES {
            return Err("OS registry provider response exceeded the size limit");
        }
        bytes.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&bytes).map_err(|_| "OS registry provider returned invalid JSON")
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha3::{Digest, Keccak256};

    fn urls() -> Vec<String> {
        DEFAULT_RPC_URLS.iter().map(|url| (*url).into()).collect()
    }

    fn response(id: u64, result: &str) -> Value {
        json!({"jsonrpc":"2.0", "id":id, "result":result})
    }

    fn boolean(approved: bool) -> String {
        format!(
            "0x{}{}",
            "00".repeat(31),
            if approved { "01" } else { "00" }
        )
    }

    #[test]
    fn selector_and_bytes32_encoding() {
        let selector = Keccak256::digest(b"allowedOsImages(bytes32)");
        assert_eq!(hex::encode(&selector[..4]), SELECTOR);
        let hash = std::array::from_fn(|i| i as u8);
        assert_eq!(
            call_data(&hash),
            "0x9a4e1d18000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f"
        );
    }

    #[tokio::test]
    async fn every_provider_must_approve_at_finalized_and_each_run_rechecks() {
        let calls = std::cell::Cell::new(0);
        let providers = urls();
        for run in 0..2 {
            let verification = check_with_rpc(&[42; 32], &providers, |url, body| {
                calls.set(calls.get() + 1);
                assert!(urls().contains(&url));
                let result = if body["method"] == "eth_chainId" {
                    assert_eq!(body["params"], json!([]));
                    response(1, "0x1")
                } else {
                    assert_eq!(body["method"], "eth_call");
                    assert_eq!(
                        body["params"],
                        json!([{"to":CONTRACT, "data":call_data(&[42; 32])}, "finalized"])
                    );
                    response(2, &boolean(true))
                };
                async move {
                    tokio::task::yield_now().await;
                    Ok(result)
                }
            });
            futures_util::pin_mut!(verification);
            assert!(futures_util::poll!(verification.as_mut()).is_pending());
            // Both chain requests start before the first provider responds.
            assert_eq!(calls.get(), run * 4 + 2);
            verification.await.unwrap();
        }
        assert_eq!(calls.get(), 8);
    }

    #[tokio::test]
    async fn false_disagreement_wrong_chain_and_malformed_fail_closed() {
        for (chain, result) in [
            ("0x1", boolean(false)),
            ("0x2", boolean(true)),
            ("0x01", boolean(true)),
            ("0x1", "0x1".into()),
            ("0x1", format!("0x{}02", "00".repeat(31))),
            ("0x1", format!("0x01{}", "00".repeat(31))),
            ("0x1", format!("0x{}", "00".repeat(33))),
            ("0x1", format!("0x{}gg", "00".repeat(31))),
        ] {
            let outcome = check_with_rpc(&[0; 32], &urls(), |_, body| {
                std::future::ready(Ok(if body["id"] == 1 {
                    response(1, chain)
                } else {
                    response(2, &result)
                }))
            })
            .await;
            assert!(outcome.is_err(), "chain={chain}, result={result}");
        }
        let outcome = check_with_rpc(&[0; 32], &urls(), |url, body| {
            std::future::ready(Ok(if body["id"] == 1 {
                response(1, "0x1")
            } else {
                response(2, &boolean(url == DEFAULT_RPC_URLS[0]))
            }))
        })
        .await;
        assert!(outcome.unwrap_err().contains("providers disagree"));
        let denied = check_with_rpc(&[0xab; 32], &urls(), |_, body| {
            std::future::ready(Ok(if body["id"] == 1 {
                response(1, "0x1")
            } else {
                response(2, &boolean(false))
            }))
        })
        .await
        .unwrap_err();
        assert_eq!(
            denied,
            format!(
                "os-image-hash {} is not approved in the dstack KMS registry (chain 1)",
                "ab".repeat(32)
            )
        );
    }

    #[tokio::test]
    async fn transport_failures_including_timeouts_fail_closed() {
        assert!(
            check_with_rpc(&[0; 32], &urls(), |_, _| std::future::ready(Err(
                REQUEST_FAILED
            )))
            .await
            .is_err()
        );
    }

    #[test]
    fn rpc_configuration_uses_defaults_or_the_explicit_setting() {
        assert_eq!(rpc_urls(None).unwrap(), urls());
        let configured = vec![
            "https://a.example/rpc".into(),
            "https://b.example/rpc".into(),
        ];
        assert_eq!(rpc_urls(Some(&configured)).unwrap(), configured);
        // An explicitly empty or invalid setting must not silently use defaults.
        assert!(rpc_urls(Some(&[])).is_err());
        assert!(rpc_urls(Some(&[
            "https://a.example".into(),
            "http://b.example".into()
        ]))
        .is_err());
    }

    #[test]
    fn json_rpc_errors_wrong_ids_and_non_strings_fail_closed() {
        for value in [
            json!({"jsonrpc":"2.0", "id":2, "error":{"message":"private details"}}),
            json!({"jsonrpc":"2.0", "id":2, "result":true}),
            json!({"jsonrpc":"1.0", "id":2, "result":boolean(true)}),
            response(3, &boolean(true)),
        ] {
            assert!(rpc_result(&value, 2).is_err());
        }
    }
}
