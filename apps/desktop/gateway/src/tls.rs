//! Per-installation TLS identity for the local endpoint.
//!
//! The endpoint is served over HTTPS so an agent can tell this app apart from
//! anything else that happens to listen on the same port later. A local CA is
//! generated once per installation; its private key and certificate live in
//! the OS credential store and the certificate is also written owner-only to
//! the app data directory for agents to trust (`NODE_EXTRA_CA_CERTS`). A
//! fresh server certificate is issued from that CA on every launch and its key
//! never leaves memory.

use std::{
    fs, io,
    path::{Path, PathBuf},
    sync::Arc,
};

use rcgen::{
    BasicConstraints, CertificateParams, CertifiedIssuer, DnType, ExtendedKeyUsagePurpose, IsCa,
    Issuer, KeyPair, KeyUsagePurpose,
};
use rustls::ServerConfig;
use rustls_pki_types::{pem::PemObject, CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use time::{Duration, OffsetDateTime};

use crate::{agents::write_atomic, secrets::SecretStore};

/// Credential-store entry holding the CA private key and certificate (PEM).
pub const CA_ENTRY: &str = "local-ca";
pub const CA_FILE: &str = "local-gateway-ca.pem";
/// Names the server certificate is valid for; agents connect by IP so no DNS
/// resolution is involved.
pub const SERVER_NAMES: [&str; 2] = ["127.0.0.1", "localhost"];

pub struct LocalIdentity {
    /// Owner-only PEM file agents point their trust store at.
    pub ca_path: PathBuf,
    pub ca_pem: String,
    pub server_config: Arc<ServerConfig>,
}

/// Load the installation CA (or create one) and issue this launch's server
/// certificate.
pub fn load_or_create(data_dir: &Path, secrets: &dyn SecretStore) -> Result<LocalIdentity, String> {
    install_provider();
    let (ca_key, ca_pem) = match secrets.get(CA_ENTRY)?.and_then(parse_ca_entry) {
        Some(existing) => existing,
        None => {
            let entry = generate_ca()?;
            secrets.set(CA_ENTRY, &entry)?;
            parse_ca_entry(entry).ok_or_else(|| "The generated local CA is unusable".to_string())?
        }
    };
    let ca_path = data_dir.join(CA_FILE);
    let on_disk = fs::read_to_string(&ca_path).ok();
    if on_disk.as_deref() != Some(ca_pem.as_str()) {
        write_atomic(&ca_path, &ca_pem, None)
            .map_err(|error| format!("Cannot write the local CA certificate: {error}"))?;
    }
    let issuer = Issuer::from_ca_cert_pem(&ca_pem, ca_key)
        .map_err(|error| format!("The local CA certificate is unusable: {error}"))?;
    let server_config = issue_server_config(&issuer, &ca_pem)?;
    Ok(LocalIdentity {
        ca_path,
        ca_pem,
        server_config,
    })
}

fn install_provider() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}

/// A new CA as the credential-store entry: key PEM followed by certificate PEM.
fn generate_ca() -> Result<String, String> {
    let key = KeyPair::generate().map_err(|error| error.to_string())?;
    let key_pem = key.serialize_pem();
    let mut params =
        CertificateParams::new(Vec::<String>::new()).map_err(|error| error.to_string())?;
    params
        .distinguished_name
        .push(DnType::CommonName, "Private AI Gateway local CA");
    params.is_ca = IsCa::Ca(BasicConstraints::Constrained(0));
    params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
    params.not_before = OffsetDateTime::now_utc() - Duration::days(1);
    params.not_after = OffsetDateTime::now_utc() + Duration::days(3650);
    let issuer = CertifiedIssuer::self_signed(params, key).map_err(|error| error.to_string())?;
    Ok(format!("{key_pem}{}", issuer.pem()))
}

/// Split the stored entry back into the key pair and the CA certificate PEM.
fn parse_ca_entry(entry: String) -> Option<(KeyPair, String)> {
    let start = entry.find("-----BEGIN CERTIFICATE-----")?;
    let (key_pem, cert_pem) = entry.split_at(start);
    let key = KeyPair::from_pem(key_pem).ok()?;
    Some((key, cert_pem.to_string()))
}

/// A one-launch server certificate for the loopback names, signed by the CA.
fn issue_server_config(
    issuer: &Issuer<'_, KeyPair>,
    ca_pem: &str,
) -> Result<Arc<ServerConfig>, String> {
    let key = KeyPair::generate().map_err(|error| error.to_string())?;
    let mut params = CertificateParams::new(SERVER_NAMES.map(str::to_string).to_vec())
        .map_err(|error| error.to_string())?;
    params
        .distinguished_name
        .push(DnType::CommonName, "Private AI Gateway");
    params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
    params.key_usages = vec![KeyUsagePurpose::DigitalSignature];
    params.not_before = OffsetDateTime::now_utc() - Duration::days(1);
    params.not_after = OffsetDateTime::now_utc() + Duration::days(365);
    let leaf = params
        .signed_by(&key, issuer)
        .map_err(|error| error.to_string())?;
    let ca_der = CertificateDer::from_pem_slice(ca_pem.as_bytes())
        .map_err(|error| format!("The local CA certificate is unusable: {error}"))?;
    let chain = vec![leaf.der().clone(), ca_der];
    let private_key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key.serialize_der()));
    let config =
        ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
            .with_safe_default_protocol_versions()
            .map_err(|error| error.to_string())?
            .with_no_client_auth()
            .with_single_cert(chain, private_key)
            .map_err(|error| error.to_string())?;
    Ok(Arc::new(config))
}

/// Refuse to serve from anything but a regular file we wrote.
pub fn ca_file_is_regular(path: &Path) -> io::Result<bool> {
    Ok(fs::symlink_metadata(path)?.file_type().is_file())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::secrets::MemoryStore;

    #[test]
    fn ca_is_stable_per_installation_and_never_written_with_its_key() {
        let dir = std::env::temp_dir().join(format!("pag-tls-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let secrets = MemoryStore::default();
        let first = load_or_create(&dir, &secrets).unwrap();
        let second = load_or_create(&dir, &secrets).unwrap();
        assert_eq!(first.ca_pem, second.ca_pem, "the CA survives relaunches");
        let on_disk = fs::read_to_string(&first.ca_path).unwrap();
        assert_eq!(on_disk, first.ca_pem);
        assert!(!on_disk.contains("PRIVATE KEY"));
        assert!(secrets
            .get(CA_ENTRY)
            .unwrap()
            .unwrap()
            .contains("PRIVATE KEY"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&first.ca_path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        let _ = fs::remove_dir_all(&dir);
    }
}
