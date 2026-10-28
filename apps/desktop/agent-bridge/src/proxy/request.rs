//! Shared validation and parsing of buffered request carriers.

use axum::{body::Bytes, http::HeaderMap};

/// Keep body semantics and serving constraints intact across HTTP forwarding.
/// Returns the sole Content-Type value, or an empty string when absent.
pub fn check_carrier_headers(headers: &HeaderMap) -> Result<&str, &'static str> {
    let dropped = super::hop_by_hop_names(headers);
    if dropped.contains("x-aci-provider") || dropped.contains("content-type") {
        return Err("serving constraint carriers cannot be hop-by-hop headers");
    }
    let mut content_types = headers.get_all("content-type").iter();
    let content_type = content_types.next();
    if content_types.next().is_some() {
        return Err("duplicate Content-Type headers");
    }
    content_type
        .map(|value| value.to_str().map_err(|_| "invalid Content-Type header"))
        .transpose()
        .map(|value| value.unwrap_or(""))
}

/// Parse buffered multipart bytes under caller-supplied field and size limits.
pub fn multipart(
    bytes: Bytes,
    content_type: &str,
    constraints: multer::Constraints,
) -> Result<multer::Multipart<'static>, &'static str> {
    let boundary =
        multer::parse_boundary(content_type).map_err(|_| "invalid multipart boundary")?;
    if boundary.is_empty() || boundary.len() > 70 {
        return Err("invalid multipart boundary");
    }
    let stream =
        futures_util::stream::once(async move { Ok::<_, std::convert::Infallible>(bytes) });
    Ok(multer::Multipart::with_constraints(
        stream,
        boundary,
        constraints,
    ))
}
