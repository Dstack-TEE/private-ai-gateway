use std::{
    fs::File,
    io::{Read, Write},
    path::PathBuf,
    time::Duration,
};

use desktop_gateway::catalog::EndpointInventory;
use reqwest::{
    header::{ETAG, IF_NONE_MATCH},
    Client, StatusCode,
};
use serde::Serialize;
use tokio::sync::Mutex;

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
}

impl InventoryUpdater {
    pub fn new(cache_path: PathBuf) -> Result<Self, String> {
        let bundled = EndpointInventory::bundled()?;
        let cached = read_cache(&cache_path).unwrap_or(CachedInventory {
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
        })
    }

    pub async fn refresh(&self) -> EndpointInventory {
        self.refresh_from(SOURCE).await
    }

    async fn refresh_from(&self, source: &str) -> EndpointInventory {
        let mut cache = self.cache.lock().await;
        if let Some(client) = &self.client {
            if let Ok(Some(updated)) = download(client, source, cache.etag.as_deref()).await {
                // Persist before replacing the in-memory copy. A full disk must
                // not discard validated data already available to this session.
                if let Err(error) = persist(&self.cache_path, &updated) {
                    eprintln!("Cannot cache model endpoint inventory: {error}");
                }
                *cache = updated;
            }
        }
        cache.inventory.clone()
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
    async fn conditional_updates_preserve_valid_cache_on_bad_download_and_restart() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("inventory.json");
        let updater = InventoryUpdater::new(path.clone()).unwrap();
        let mut updated = serde_json::to_value(EndpointInventory::bundled().unwrap()).unwrap();
        updated["checkedAt"] = "2026-09-12T00:00:00.000Z".into();
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
