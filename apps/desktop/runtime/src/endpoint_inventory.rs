use std::{
    fs::File,
    io::{Read, Write},
    path::PathBuf,
    sync::{Mutex, PoisonError},
    time::Duration,
};

use desktop_gateway::catalog::EndpointInventory;
use reqwest::{
    header::{ETAG, IF_NONE_MATCH},
    Client, StatusCode,
};
use serde::Serialize;

const SOURCE: &str = "https://raw.githubusercontent.com/Dstack-TEE/private-ai-gateway/main/apps/desktop/gateway/src/endpoint-support.json";
const MAX_BYTES: usize = 1024 * 1024;

#[derive(Serialize)]
struct CachedInventory {
    inventory: EndpointInventory,
    etag: Option<String>,
}

/// Public compatibility data never shares the authenticated inference client.
/// The bundled inventory and last validated cache remain usable offline.
pub(crate) struct InventoryUpdater {
    client: Option<Client>,
    cache_path: PathBuf,
    cache: Mutex<CachedInventory>,
    refreshing: tokio::sync::Mutex<()>,
    #[cfg(test)]
    source: Option<String>,
}

impl InventoryUpdater {
    pub fn new(cache_path: PathBuf) -> Result<Self, String> {
        let bundled = EndpointInventory::bundled()?;
        let cached = read_cache(&cache_path)
            .filter(|cache| cache.inventory.is_newer_than(&bundled))
            .unwrap_or(CachedInventory {
                inventory: bundled,
                etag: None,
            });
        Ok(Self {
            client: Client::builder()
                .timeout(Duration::from_secs(4))
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .ok(),
            cache_path,
            cache: Mutex::new(cached),
            refreshing: tokio::sync::Mutex::new(()),
            #[cfg(test)]
            source: None,
        })
    }

    pub async fn refresh(&self) -> EndpointInventory {
        #[cfg(test)]
        if let Some(source) = &self.source {
            return self.refresh_from(source).await;
        }
        self.refresh_from(SOURCE).await
    }

    #[cfg(test)]
    pub(crate) fn with_source(mut self, source: String) -> Self {
        self.source = Some(source);
        self
    }

    pub fn current(&self) -> EndpointInventory {
        self.cache
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .inventory
            .clone()
    }

    async fn refresh_from(&self, source: &str) -> EndpointInventory {
        let _refresh = match self.refreshing.try_lock() {
            Ok(guard) => guard,
            Err(_) => {
                // Background subscribers share completion across profile/session changes.
                let _completed = self.refreshing.lock().await;
                return self.current();
            }
        };
        let etag = self
            .cache
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .etag
            .clone();
        if let Some(client) = &self.client {
            if let Ok(Some(updated)) = download(client, source, etag.as_deref()).await {
                if !updated.inventory.is_newer_than(&self.current()) {
                    return self.current();
                }
                // Persist before replacing the in-memory copy. A full disk must
                // not discard validated data already available to this session.
                if let Err(error) = persist(&self.cache_path, &updated) {
                    eprintln!("Cannot cache model endpoint inventory: {error}");
                }
                *self.cache.lock().unwrap_or_else(PoisonError::into_inner) = updated;
            }
        }
        self.current()
    }
}

async fn download(
    client: &Client,
    source: &str,
    etag: Option<&str>,
) -> Result<Option<CachedInventory>, ()> {
    let mut request = client.get(source);
    if let Some(etag) = etag {
        request = request.header(IF_NONE_MATCH, etag);
    }
    let mut response = request.send().await.map_err(|_| ())?;
    if response.status() == StatusCode::NOT_MODIFIED {
        return Ok(None);
    }
    if !response.status().is_success()
        || response
            .content_length()
            .is_some_and(|size| size > MAX_BYTES as u64)
    {
        return Err(());
    }
    let etag = response
        .headers()
        .get(ETAG)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| ())? {
        if bytes.len() + chunk.len() > MAX_BYTES {
            return Err(());
        }
        bytes.extend_from_slice(&chunk);
    }
    let inventory = EndpointInventory::parse(&bytes).map_err(|_| ())?;
    Ok(Some(CachedInventory { inventory, etag }))
}

fn read_cache(path: &std::path::Path) -> Option<CachedInventory> {
    let mut bytes = Vec::new();
    File::open(path)
        .ok()?
        .take((MAX_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .ok()?;
    if bytes.len() > MAX_BYTES {
        return None;
    }
    let value: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    let inventory =
        EndpointInventory::parse(&serde_json::to_vec(value.get("inventory")?).ok()?).ok()?;
    let etag = value
        .get("etag")
        .and_then(serde_json::Value::as_str)
        .filter(|etag| reqwest::header::HeaderValue::from_str(etag).is_ok())
        .map(str::to_owned);
    Some(CachedInventory { inventory, etag })
}

fn persist(path: &std::path::Path, cache: &CachedInventory) -> std::io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| std::io::Error::other("Missing cache directory"))?;
    let mut file = tempfile::NamedTempFile::new_in(parent)?;
    serde_json::to_writer(&mut file, cache)?;
    file.flush()?;
    file.as_file().sync_all()?;
    file.persist(path).map_err(|error| error.error)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[tokio::test]
    async fn freshness_uses_parsed_dates_and_never_downgrades_local_data() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("inventory.json");
        let bundled = EndpointInventory::bundled().unwrap();
        let dated = |date: &str| {
            let mut value = serde_json::to_value(&bundled).unwrap();
            value["checkedAt"] = date.into();
            EndpointInventory::parse(&serde_json::to_vec(&value).unwrap()).unwrap()
        };
        let cached = |inventory| CachedInventory {
            inventory,
            etag: Some("\"cached\"".into()),
        };
        persist(&path, &cached(dated("2026-01-01T00:00:00Z"))).unwrap();
        let updater = InventoryUpdater::new(path.clone()).unwrap();
        assert_eq!(
            serde_json::to_value(updater.current()).unwrap(),
            serde_json::to_value(&bundled).unwrap()
        );
        assert!(updater.cache.lock().unwrap().etag.is_none());
        let newer = dated("2027-01-01T00:00:00Z");
        assert!(!dated("2027-01-01T01:00:00+01:00").is_newer_than(&newer));
        persist(&path, &cached(newer.clone())).unwrap();
        let updater = InventoryUpdater::new(path.clone()).unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let source = format!("http://{}", listener.local_addr().unwrap());
        let body = serde_json::to_string(&bundled).unwrap();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            let mut bytes = [0; 1024];
            while !request.ends_with(b"\r\n\r\n") {
                let length = socket.read(&mut bytes).await.unwrap();
                assert!(length > 0 && request.len() < 4096);
                request.extend_from_slice(&bytes[..length]);
            }
            socket
                .write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();
        });
        let result = updater.refresh_from(&source).await;
        server.await.unwrap();
        assert_eq!(
            serde_json::to_value(result).unwrap(),
            serde_json::to_value(&newer).unwrap()
        );
        assert_eq!(
            serde_json::to_value(read_cache(&path).unwrap().inventory).unwrap(),
            serde_json::to_value(newer).unwrap()
        );
    }

    #[tokio::test]
    async fn conditional_updates_preserve_valid_cache_on_bad_download_and_restart() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("inventory.json");
        let updater = InventoryUpdater::new(path.clone()).unwrap();
        let mut updated = serde_json::to_value(EndpointInventory::bundled().unwrap()).unwrap();
        updated["checkedAt"] = "2026-09-12T00:00:00Z".into();
        let body = updated.to_string();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let source = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            for (index, (status, body)) in [
                ("200 OK", body.as_str()),
                ("304 Not Modified", ""),
                ("200 OK", "{}"),
            ]
            .into_iter()
            .enumerate()
            {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut request = Vec::new();
                let mut bytes = [0; 1024];
                while !request.ends_with(b"\r\n\r\n") {
                    let length = socket.read(&mut bytes).await.unwrap();
                    assert!(length > 0 && request.len() < 4096);
                    request.extend_from_slice(&bytes[..length]);
                }
                let request = String::from_utf8(request).unwrap().to_ascii_lowercase();
                assert!(!request.contains("authorization:"));
                assert_eq!(request.contains("if-none-match: \"revision-2\""), index > 0);
                socket.write_all(format!("HTTP/1.1 {status}\r\nContent-Length: {}\r\nETag: \"revision-2\"\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
            }
        });
        for _ in 0..3 {
            let result = updater.refresh_from(&source).await;
            assert_eq!(serde_json::to_value(result).unwrap(), updated);
        }
        server.await.unwrap();
        let restored = InventoryUpdater::new(path.clone()).unwrap();
        assert_eq!(
            serde_json::to_value(restored.refresh_from("file:///unavailable").await).unwrap(),
            updated
        );
        std::fs::write(&path, "{}").unwrap();
        let recovered = InventoryUpdater::new(path).unwrap();
        assert_eq!(
            serde_json::to_value(recovered.refresh_from("file:///unavailable").await).unwrap(),
            serde_json::to_value(EndpointInventory::bundled().unwrap()).unwrap()
        );
    }
}
