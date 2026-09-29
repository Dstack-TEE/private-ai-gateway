//! The client side of Private AI Proxy, shared by the desktop shell, the CLI
//! and the backend: renderer contracts, the management API and its client,
//! the local endpoint, backend launch, the settings file, app paths and
//! owner-only file primitives. It links no database; its HTTP clients are the
//! management API client and the release-channel update check, and the
//! `server` feature adds the serving loop the backend's listeners share.

pub mod account;
pub mod agent_access;
pub mod agents;
pub mod brand;
pub mod client;
pub mod config;
pub mod contracts;
pub mod launch;
pub mod listen;
pub mod lock;
pub mod logging;
pub mod maintenance;
pub mod paths;
pub mod private_fs;
pub mod protection;
pub mod protocol;
pub mod relocation;
#[cfg(feature = "server")]
pub mod serve;
pub mod sse;
pub mod transport;
pub mod ui_api;
mod ui_methods;
pub mod updates;
pub mod usage;
#[cfg(windows)]
pub mod windows_acl;

/// Seconds since the Unix epoch; 0 if the system clock is set before it.
pub fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

/// `base` with `segments` appended to its path, each percent-encoded as one
/// segment. `url` drops `.` and `..` segments, so they are refused.
pub fn endpoint(base: &str, segments: &[&str]) -> Result<url::Url, String> {
    let mut url =
        url::Url::parse(base).map_err(|error| format!("invalid URL {base:?}: {error}"))?;
    if let Some(segment) = segments
        .iter()
        .find(|segment| matches!(**segment, "" | "." | ".."))
    {
        return Err(format!("{segment:?} is not a URL path segment"));
    }
    url.path_segments_mut()
        .map_err(|()| format!("URL {base:?} cannot have a path"))?
        .pop_if_empty()
        .extend(segments);
    Ok(url)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoint_segments_cannot_leave_their_path() {
        assert_eq!(
            endpoint("https://gateway.example/base/", &["v1", "a/b?c"])
                .unwrap()
                .as_str(),
            "https://gateway.example/base/v1/a%2Fb%3Fc"
        );
        assert_eq!(
            endpoint("http://[::1]:4190", &["v1"]).unwrap().as_str(),
            "http://[::1]:4190/v1"
        );
        for segment in ["", ".", ".."] {
            assert!(endpoint("https://gateway.example", &["v1", segment]).is_err());
        }
        assert!(endpoint("not a url", &["v1"]).is_err());
        assert!(endpoint("mailto:someone@example.com", &["v1"]).is_err());
    }
}
