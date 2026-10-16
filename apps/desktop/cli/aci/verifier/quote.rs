//! The §9.1(1) quote steps that need this client's DCAP library.
//!
//! The evidence handling and the report-data comparison come from
//! `aci-verify`; parsing and verifying the quote to its vendor root happen
//! here.

use std::collections::BTreeMap;
use std::time::Duration;

use aci_verify::quote::quote_bytes;
pub use aci_verify::quote::QuoteStepError;
use dcap_qvl::collateral::{CollateralClient, PHALA_PCCS_URL};
use dcap_qvl::configs::DefaultConfig;
use dcap_qvl::http::{HttpClient, HttpResponse};
use dcap_qvl::quote::Quote;
use serde_json::Value;

use crate::aci::tls::{error_chain_message, webpki_client};

pub const DEFAULT_PCCS_URL: &str = PHALA_PCCS_URL;
const COLLATERAL_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const COLLATERAL_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

pub(super) struct VerifiedQuote {
    /// The collateral's TCB status, for the caller to appraise (§8.3).
    pub status: String,
    pub tee_type: &'static str,
}

pub(super) fn parse_quote_evidence(evidence: &Value) -> Result<(Vec<u8>, Quote), QuoteStepError> {
    let raw = quote_bytes(evidence)?;
    let quote = Quote::parse(&raw).map_err(|e| QuoteStepError::UnparsableQuote(e.to_string()))?;
    Ok((raw, quote))
}

/// Verify the quote to its vendor root and confirm the report's claimed
/// `tee_type` is the one the quote actually carries. Collateral comes only
/// from `pccs_url`.
pub(super) async fn verify_quote_to_root(
    raw_quote: &[u8],
    pccs_url: &str,
    now_secs: u64,
    claimed_tee_type: &str,
) -> Result<VerifiedQuote, QuoteStepError> {
    let http = webpki_client(COLLATERAL_CONNECT_TIMEOUT, COLLATERAL_REQUEST_TIMEOUT)
        .map(CollateralHttp)
        .map_err(|reason| QuoteStepError::Collateral {
            url: pccs_url.to_string(),
            reason,
        })?;
    let collateral = CollateralClient::<DefaultConfig, _>::new(http, pccs_url)
        .fetch(raw_quote)
        .await
        .map_err(|error| QuoteStepError::Collateral {
            url: pccs_url.to_string(),
            reason: format!("{error:#}"),
        })?;
    let verified = dcap_qvl::verify::rustcrypto::verify(raw_quote, &collateral, now_secs)
        .map_err(|e| QuoteStepError::Verification(e.to_string()))?;
    let tee_type = if verified.report.is_sgx() {
        "sgx"
    } else {
        "tdx"
    };
    if claimed_tee_type != tee_type {
        return Err(QuoteStepError::TeeTypeMismatch {
            reported: claimed_tee_type.to_string(),
            verified: tee_type,
        });
    }
    Ok(VerifiedQuote {
        status: verified.status,
        tee_type,
    })
}

/// The collateral services' transport: [`webpki_client`] rather than
/// `dcap_qvl`'s default client, which verifies certificates with the OS
/// (Security.framework on macOS) and failed there to reach the Phala PCCS
/// that curl and this client's attestation fetch reached.
struct CollateralHttp(reqwest::Client);

impl HttpClient for CollateralHttp {
    async fn get(&self, url: &str) -> anyhow::Result<HttpResponse> {
        let transport_error = |error: reqwest::Error| anyhow::anyhow!(error_chain_message(&error));
        let response = self.0.get(url).send().await.map_err(transport_error)?;
        let status = response.status();
        if status.is_server_error() {
            anyhow::bail!("{url} answered HTTP {}", status.as_u16());
        }
        let headers = response
            .headers()
            .iter()
            .map(|(name, value)| {
                let value = value
                    .to_str()
                    .map_err(|_| anyhow::anyhow!("header {name} is not ASCII"))?;
                Ok((name.as_str().to_string(), value.to_string()))
            })
            .collect::<anyhow::Result<BTreeMap<_, _>>>()?;
        let body = response.bytes().await.map_err(transport_error)?.to_vec();
        Ok(HttpResponse {
            status: status.as_u16(),
            headers,
            body,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn transport_failures_and_server_errors_surface_their_cause() {
        let http =
            CollateralHttp(webpki_client(Duration::from_secs(5), Duration::from_secs(5)).unwrap());

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let closed = format!("http://{}/", listener.local_addr().unwrap());
        drop(listener);
        let Err(error) = http.get(&closed).await else {
            panic!("a closed port answered");
        };
        // The OS's reason, which the request error's own text leaves out.
        assert!(error.to_string().contains("os error"), "{error}");

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let failing = format!("http://{}/", listener.local_addr().unwrap());
        let router =
            axum::Router::new().fallback(|| async { axum::http::StatusCode::SERVICE_UNAVAILABLE });
        let server = tokio::spawn(async move { axum::serve(listener, router).await });
        let Err(error) = http.get(&failing).await else {
            panic!("HTTP 503 was accepted");
        };
        server.abort();
        assert!(server.await.unwrap_err().is_cancelled());
        assert_eq!(error.to_string(), format!("{failing} answered HTTP 503"));
    }
}
