//! The server's certificate (self-signed, made on first start like fernbedienung's `openssl req -x509`) and the TLS
//! settings both transports share: TLS 1.3 only, the protocol's ALPN, no client certificates (players authenticate with
//! a key after the handshake). The app pins the certificate's SHA-256, so its names and expiry are informative.

use std::path::Path;
use std::sync::Arc;

use pokedaisy_coop_proto as proto;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};

pub struct Identity {
    pub cert: CertificateDer<'static>,
    pub key: PrivateKeyDer<'static>,
}

impl Identity {
    pub fn fingerprint(&self) -> [u8; 32] { proto::fingerprint(self.cert.as_ref()) }
}

/// Loads the certificate and key; makes both (and writes them) when both are missing. One without the other is an error,
/// so a lost key never silently becomes a new certificate the players would have to pin again.
pub fn load_or_make(cert_path: &Path, key_path: &Path, names: &[String]) -> Result<Identity, String> {
    match (cert_path.exists(), key_path.exists()) {
        (true, true) => load(cert_path, key_path),
        (false, false) => {
            let id = make(names)?;
            write_pem(cert_path, &pem("CERTIFICATE", id.cert.as_ref()), false)?;
            let PrivateKeyDer::Pkcs8(k) = &id.key else { unreachable!() };
            write_pem(key_path, &pem("PRIVATE KEY", k.secret_pkcs8_der()), true)?;
            log::info!("made a new self-signed certificate: {} and {}", cert_path.display(), key_path.display());
            Ok(id)
        }
        (true, false) => Err(format!("{} exists but {} doesn't", cert_path.display(), key_path.display())),
        (false, true) => Err(format!("{} exists but {} doesn't", key_path.display(), cert_path.display())),
    }
}

/// A new self-signed ECDSA P-256 certificate for [names].
pub fn make(names: &[String]) -> Result<Identity, String> {
    let ck = rcgen::generate_simple_self_signed(names.to_vec()).map_err(|e| format!("making a certificate: {e}"))?;
    Ok(Identity {
        cert: ck.cert.der().clone(),
        key: PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(ck.key_pair.serialize_der())),
    })
}

fn load(cert_path: &Path, key_path: &Path) -> Result<Identity, String> {
    let cert_pem = std::fs::read(cert_path).map_err(|e| format!("{}: {e}", cert_path.display()))?;
    let key_pem = std::fs::read(key_path).map_err(|e| format!("{}: {e}", key_path.display()))?;
    let cert = rustls_pemfile::certs(&mut cert_pem.as_slice())
        .next()
        .ok_or_else(|| format!("{}: no certificate", cert_path.display()))?
        .map_err(|e| format!("{}: {e}", cert_path.display()))?;
    let key = rustls_pemfile::private_key(&mut key_pem.as_slice())
        .map_err(|e| format!("{}: {e}", key_path.display()))?
        .ok_or_else(|| format!("{}: no private key", key_path.display()))?;
    Ok(Identity { cert, key })
}

fn pem(label: &str, der: &[u8]) -> String {
    const B64: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut b = String::new();
    for c in der.chunks(3) {
        let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
        for i in 0..4 {
            if i <= c.len() { b.push(B64[(n >> (18 - 6 * i) & 63) as usize] as char) } else { b.push('=') }
        }
    }
    let mut out = format!("-----BEGIN {label}-----\n");
    for line in b.as_bytes().chunks(64) { out.push_str(std::str::from_utf8(line).unwrap()); out.push('\n'); }
    out.push_str(&format!("-----END {label}-----\n"));
    out
}

fn write_pem(p: &Path, text: &str, secret: bool) -> Result<(), String> {
    if let Some(dir) = p.parent() { if !dir.as_os_str().is_empty() { std::fs::create_dir_all(dir).map_err(|e| e.to_string())?; } }
    std::fs::write(p, text).map_err(|e| format!("{}: {e}", p.display()))?;
    #[cfg(unix)]
    if secret {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o600)).map_err(|e| format!("{}: {e}", p.display()))?;
    }
    let _ = secret;
    Ok(())
}

/// TLS 1.3 with the protocol's ALPN, for both QUIC and TCP.
pub fn server_config(id: &Identity) -> Result<Arc<rustls::ServerConfig>, String> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let mut cfg = rustls::ServerConfig::builder_with_provider(provider)
        .with_protocol_versions(&[&rustls::version::TLS13])
        .map_err(|e| e.to_string())?
        .with_no_client_auth()
        .with_single_cert(vec![id.cert.clone()], id.key.clone_key())
        .map_err(|e| format!("certificate and key: {e}"))?;
    cfg.alpn_protocols = vec![proto::ALPN.to_vec()];
    cfg.max_early_data_size = 0;
    Ok(Arc::new(cfg))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn makes_once_then_loads_the_same_certificate() {
        let dir = tempfile::tempdir().unwrap();
        let (c, k) = (dir.path().join("server.crt"), dir.path().join("server.key"));
        let made = load_or_make(&c, &k, &["pokedaisy-coop".into()]).unwrap();
        let again = load_or_make(&c, &k, &["other".into()]).unwrap();
        assert_eq!(made.fingerprint(), again.fingerprint());
        assert!(server_config(&again).is_ok());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&k).unwrap().permissions().mode() & 0o777, 0o600);
        }
    }

    #[test]
    fn a_lone_certificate_or_key_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let (c, k) = (dir.path().join("server.crt"), dir.path().join("server.key"));
        load_or_make(&c, &k, &["x".into()]).unwrap();
        std::fs::remove_file(&k).unwrap();
        assert!(load_or_make(&c, &k, &["x".into()]).is_err());
    }

    #[test]
    fn pem_matches_rcgens() {
        let ck = rcgen::generate_simple_self_signed(vec!["x".into()]).unwrap();
        assert_eq!(pem("CERTIFICATE", ck.cert.der()), ck.cert.pem().replace("\r\n", "\n"));
    }
}
