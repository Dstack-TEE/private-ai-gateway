//! Client-pinned dstack governance policy, independent of gateway evidence.
//! Normal CA-validated HTTPS (never the gateway's attested-SPKI transport).

use std::{future::Future, time::Duration};

use futures_util::StreamExt;
use serde_json::{json, Value};

// DstackKms.allowedOsImages(bytes32), Ethereum mainnet (chain id 1).
// https://github.com/Dstack-TEE/dstack/blob/next/dstack/kms/auth-eth/contracts/DstackKms.sol
pub const CONTRACT: &str = "0xd343a3f5593b93D8056aB5D60c433622d7D65a80";
const SELECTOR: &str = "9a4e1d18";
const DEFAULT_RPC_URLS: [&str; 2] = [
    "https://ethereum-rpc.publicnode.com",
    "https://eth.drpc.org",
];
const RPC_URLS_ENV: &str = "PRIVATE_AI_PROXY_OS_REGISTRY_RPC_URLS";
const RPC_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_RESPONSE_BYTES: usize = 4096;
const REQUEST_FAILED: &str =
    "OS registry provider request failed or timed out; approval could not be established";

pub async fn check(hash: &[u8; 32]) -> Result<(), &'static str> {
    // Resolve on every verification: neither positive nor negative approvals
    // are cached, because the registry owner can revoke them.
    let urls = match std::env::var(RPC_URLS_ENV) {
        Ok(raw) => serde_json::from_str::<Vec<String>>(&raw).map_err(|_| {
            "OS registry RPC environment override must be a JSON array of HTTPS URLs"
        })?,
        Err(std::env::VarError::NotPresent) => desktop_core::config::load()
            .map_err(|_| "Cannot read OS registry RPC settings")?
            .os_registry_rpc_urls
            .unwrap_or_else(|| DEFAULT_RPC_URLS.iter().map(|url| (*url).into()).collect()),
        Err(_) => return Err("Cannot read OS registry RPC environment override"),
    };
    let client = reqwest::Client::builder()
        .https_only(true)
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(RPC_TIMEOUT)
        .timeout(RPC_TIMEOUT)
        .build()
        .map_err(|_| "Cannot initialize OS registry HTTPS client")?;
    check_with_rpc(hash, &urls, |url, body| rpc(&client, url, body)).await
}

fn call_data(hash: &[u8; 32]) -> String {
    format!("0x{SELECTOR}{}", hex::encode(hash))
}

/// The injected request executor keeps policy tests offline, including a
/// stalled transport. The deadline covers the executor and response body.
async fn check_with_rpc<F, Fut>(
    hash: &[u8; 32],
    urls: &[String],
    mut request: F,
) -> Result<(), &'static str>
where
    F: FnMut(String, Value) -> Fut,
    Fut: Future<Output = Result<Value, &'static str>>,
{
    desktop_core::config::validate_os_registry_rpc_urls(urls)?;
    for url in urls {
        let chain = json!({"jsonrpc":"2.0", "id":1, "method":"eth_chainId", "params":[]});
        let chain = tokio::time::timeout(RPC_TIMEOUT, request(url.clone(), chain))
            .await
            .map_err(|_| REQUEST_FAILED)??;
        if rpc_result(&chain, 1)? != "0x1" {
            return Err("OS registry provider is not on Ethereum mainnet (chain 1)");
        }
        let call = json!({"jsonrpc":"2.0", "id":2, "method":"eth_call", "params":[
            {"to":CONTRACT, "data":call_data(hash)}, "finalized"
        ]});
        let result = tokio::time::timeout(RPC_TIMEOUT, request(url.clone(), call))
            .await
            .map_err(|_| REQUEST_FAILED)??;
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
        if word[31] == 0 {
            return Err("os-image-hash is not approved by every provider in the dstack KMS registry (chain 1)");
        }
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
        let mut calls = 0;
        for _ in 0..2 {
            check_with_rpc(&[42; 32], &urls(), |url, body| {
                calls += 1;
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
                std::future::ready(Ok(result))
            })
            .await
            .unwrap();
        }
        assert_eq!(calls, 8);
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
        assert!(outcome.is_err());
    }

    #[tokio::test(start_paused = true)]
    async fn network_errors_and_stalled_requests_fail_closed() {
        assert!(
            check_with_rpc(&[0; 32], &urls(), |_, _| std::future::ready(Err(
                REQUEST_FAILED
            )))
            .await
            .is_err()
        );
        assert!(
            check_with_rpc(&[0; 32], &urls(), |_, _| std::future::pending())
                .await
                .is_err()
        );
        assert!(check_with_rpc(&[0; 32], &urls(), |_, body| async move {
            if body["id"] == 1 {
                Ok(response(1, "0x1"))
            } else {
                std::future::pending().await
            }
        })
        .await
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
