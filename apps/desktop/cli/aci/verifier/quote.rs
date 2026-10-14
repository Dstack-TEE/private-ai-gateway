//! The §9.1(1) quote steps that need this client's DCAP library.
//!
//! The evidence handling and the report-data comparison come from
//! `aci-verify`; parsing and verifying the quote to its vendor root happen
//! here.

use std::collections::BTreeMap;
use std::future::Future;
use std::time::Duration;

use aci_verify::quote::quote_bytes;
pub use aci_verify::quote::QuoteStepError;
use dcap_qvl::collateral::{CollateralClient, INTEL_PCS_URL, PHALA_PCCS_URL};
use dcap_qvl::configs::DefaultConfig;
use dcap_qvl::http::{HttpClient, HttpResponse};
use dcap_qvl::quote::Quote;
use serde_json::Value;

use crate::aci::tls::{error_chain_message, webpki_client};

/// The collateral services tried in order when the caller names none. The
/// collateral is Intel-signed and verified to Intel's root whichever service
/// supplies it, so falling back to Intel's PCS changes availability, not
/// strictness.
const DEFAULT_COLLATERAL_URLS: [&str; 2] = [PHALA_PCCS_URL, INTEL_PCS_URL];
const COLLATERAL_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const COLLATERAL_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

pub(super) struct VerifiedQuote {
    /// The collateral's TCB status, for the caller to appraise (§8.3).
    pub status: String,
    pub tee_type: &'static str,
    /// The service that supplied the collateral, and any tried before it.
    pub collateral_from: String,
}

pub(super) fn parse_quote_evidence(evidence: &Value) -> Result<(Vec<u8>, Quote), QuoteStepError> {
    let raw = quote_bytes(evidence)?;
    let quote = Quote::parse(&raw).map_err(|e| QuoteStepError::UnparsableQuote(e.to_string()))?;
    Ok((raw, quote))
}

/// Verify the quote to its vendor root and confirm the report's claimed
/// `tee_type` is the one the quote actually carries. Collateral comes from
/// `pccs_url` alone when the caller names one, otherwise from the
/// [`DEFAULT_COLLATERAL_URLS`] in turn.
pub(super) async fn verify_quote_to_root(
    raw_quote: &[u8],
    pccs_url: Option<&str>,
    now_secs: u64,
    claimed_tee_type: &str,
) -> Result<VerifiedQuote, QuoteStepError> {
    let urls = collateral_urls(pccs_url);
    let http = webpki_client(COLLATERAL_CONNECT_TIMEOUT, COLLATERAL_REQUEST_TIMEOUT)
        .map(CollateralHttp)
        .map_err(|reason| QuoteStepError::Collateral {
            url: urls.join(", "),
            reason,
        })?;
    let (collateral, collateral_from) = first_available(&urls, |url| {
        let client = CollateralClient::<DefaultConfig, _>::new(http.clone(), url);
        async move { client.fetch(raw_quote).await }
    })
    .await?;
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
        collateral_from,
    })
}

/// An explicit PCCS is the only one used: falling back from a service the
/// user chose would quietly replace it.
fn collateral_urls(pccs_url: Option<&str>) -> Vec<&str> {
    match pccs_url {
        Some(url) => vec![url],
        None => DEFAULT_COLLATERAL_URLS.to_vec(),
    }
}

/// Fetch from the first of `urls` able to serve. Only an [`Unavailable`]
/// service passes the fetch on to the next; any other failure is the answer.
/// Returns the value with the service that supplied it (naming any tried
/// before it, and why they failed), or an error naming every service tried.
async fn first_available<'a, T, Fut>(
    urls: &[&'a str],
    mut fetch: impl FnMut(&'a str) -> Fut,
) -> Result<(T, String), QuoteStepError>
where
    Fut: Future<Output = anyhow::Result<T>>,
{
    let mut failures: Vec<(&str, String)> = Vec::new();
    for &url in urls {
        match fetch(url).await {
            Ok(value) => {
                let source = failures
                    .iter()
                    .fold(url.to_string(), |source, (url, reason)| {
                        format!("{source} after {url} failed: {reason}")
                    });
                return Ok((value, source));
            }
            Err(error) => {
                let unavailable = error.chain().any(|cause| cause.is::<Unavailable>());
                failures.push((url, format!("{error:#}")));
                if !unavailable {
                    break;
                }
            }
        }
    }
    let mut failures = failures.into_iter();
    let (url, reason) = failures.next().unwrap_or_default();
    let reason = failures.fold(reason, |reason, (fallback, next)| {
        format!("{reason}; fallback {fallback} also failed: {next}")
    });
    Err(QuoteStepError::Collateral {
        url: url.to_string(),
        reason,
    })
}

/// A collateral service could not answer a request: the request failed in
/// transport, or the service answered HTTP 5xx.
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
struct Unavailable(String);

/// The collateral services' transport: [`webpki_client`] rather than
/// `dcap_qvl`'s default client, which verifies certificates with the OS
/// (Security.framework on macOS) and failed there to reach the Phala PCCS
/// that curl and this client's attestation fetch reached.
#[derive(Clone)]
struct CollateralHttp(reqwest::Client);

impl HttpClient for CollateralHttp {
    async fn get(&self, url: &str) -> anyhow::Result<HttpResponse> {
        let unavailable = |error: reqwest::Error| Unavailable(error_chain_message(&error));
        let response = self.0.get(url).send().await.map_err(unavailable)?;
        let status = response.status();
        if status.is_server_error() {
            return Err(Unavailable(format!("{url} answered HTTP {}", status.as_u16())).into());
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
        let body = response.bytes().await.map_err(unavailable)?.to_vec();
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

    fn unavailable(reason: &str) -> anyhow::Result<&'static str> {
        Err(anyhow::Error::new(Unavailable(reason.into())).context("Failed to fetch"))
    }

    #[tokio::test]
    async fn an_unavailable_pccs_falls_back_to_intel() {
        let mut tried = Vec::new();
        let (collateral, source) = first_available(&collateral_urls(None), |url| {
            tried.push(url);
            async move {
                match url {
                    PHALA_PCCS_URL => unavailable("connection refused"),
                    _ => Ok("collateral"),
                }
            }
        })
        .await
        .unwrap();
        assert_eq!(tried, [PHALA_PCCS_URL, INTEL_PCS_URL]);
        assert_eq!(collateral, "collateral");
        assert_eq!(
            source,
            format!("{INTEL_PCS_URL} after {PHALA_PCCS_URL} failed: Failed to fetch: connection refused")
        );
    }

    #[tokio::test]
    async fn a_failure_names_every_service_tried() {
        let error = first_available(&collateral_urls(None), |url| async move {
            unavailable(&format!("{url} answered HTTP 503"))
        })
        .await
        .unwrap_err();
        assert_eq!(
            error.to_string(),
            format!(
                "quote collateral fetch from {PHALA_PCCS_URL} failed: Failed to fetch: \
                 {PHALA_PCCS_URL} answered HTTP 503; fallback {INTEL_PCS_URL} also failed: \
                 Failed to fetch: {INTEL_PCS_URL} answered HTTP 503"
            )
        );
    }

    #[tokio::test]
    async fn only_an_unavailable_service_falls_back() {
        // An answer that is not an outage (here a 404) is the answer.
        let mut tried = Vec::new();
        let error = first_available(&collateral_urls(None), |url| {
            tried.push(url);
            async { Err::<(), _>(anyhow::anyhow!("Failed to fetch: HTTP 404")) }
        })
        .await
        .unwrap_err();
        assert_eq!(tried, [PHALA_PCCS_URL]);
        assert!(error
            .to_string()
            .ends_with("failed: Failed to fetch: HTTP 404"));

        // A PCCS the user named is never replaced.
        let mut tried = Vec::new();
        let named = "https://pccs.example";
        first_available(&collateral_urls(Some(named)), |url| {
            tried.push(url);
            async { unavailable("timed out") }
        })
        .await
        .unwrap_err();
        assert_eq!(tried, [named]);
    }

    #[tokio::test]
    async fn transport_failures_and_server_errors_are_unavailable_with_their_cause() {
        let http =
            CollateralHttp(webpki_client(Duration::from_secs(5), Duration::from_secs(5)).unwrap());

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let closed = format!("http://{}/", listener.local_addr().unwrap());
        drop(listener);
        let Err(error) = http.get(&closed).await else {
            panic!("a closed port answered");
        };
        assert!(error.is::<Unavailable>());
        // The OS's reason, which the request error's own text leaves out.
        assert!(error.to_string().contains("os error"), "{error}");

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let failing = format!("http://{}/", listener.local_addr().unwrap());
        let router =
            axum::Router::new().fallback(|| async { axum::http::StatusCode::SERVICE_UNAVAILABLE });
        tokio::spawn(async move { axum::serve(listener, router).await });
        let Err(error) = http.get(&failing).await else {
            panic!("HTTP 503 was accepted");
        };
        assert!(error.is::<Unavailable>());
        assert_eq!(error.to_string(), format!("{failing} answered HTTP 503"));
    }
}
