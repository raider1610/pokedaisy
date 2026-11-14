//! The server certificate check: the end-entity certificate's SHA-256 must be the pinned one (fernbedienung trusts the
//! server's own `server.crt` the same way), and the handshake must be signed by that certificate's key, checked as rustls
//! checks any other (so a copied certificate without its key is no use). No chain, no names, no expiry: the pin is it.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use pokedaisy_coop_proto as proto;
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{verify_tls12_signature, verify_tls13_signature, WebPkiSupportedAlgorithms};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{CertificateError, DigitallySignedStruct, Error, SignatureScheme};

#[derive(Clone, Debug)]
pub struct PinnedServer {
    pin: [u8; 32],
    algorithms: WebPkiSupportedAlgorithms,
    mismatch: Arc<AtomicBool>,
}

impl PinnedServer {
    pub fn new(pin: [u8; 32]) -> Self {
        Self {
            pin,
            algorithms: rustls::crypto::ring::default_provider().signature_verification_algorithms,
            mismatch: Arc::new(AtomicBool::new(false)),
        }
    }

    /// A certificate was offered that isn't the pinned one (shared by every clone: both transports report here).
    pub fn mismatched(&self) -> bool { self.mismatch.load(Ordering::SeqCst) }
}

impl ServerCertVerifier for PinnedServer {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, Error> {
        let got = proto::fingerprint(end_entity.as_ref());
        // Constant time is moot for a public certificate's hash, but cheap.
        let same = got.iter().zip(&self.pin).fold(0u8, |d, (a, b)| d | (a ^ b)) == 0;
        if same {
            Ok(ServerCertVerified::assertion())
        } else {
            self.mismatch.store(true, Ordering::SeqCst);
            Err(Error::InvalidCertificate(CertificateError::ApplicationVerificationFailure))
        }
    }

    fn verify_tls12_signature(&self, message: &[u8], cert: &CertificateDer<'_>, dss: &DigitallySignedStruct) -> Result<HandshakeSignatureValid, Error> {
        verify_tls12_signature(message, cert, dss, &self.algorithms)
    }

    fn verify_tls13_signature(&self, message: &[u8], cert: &CertificateDer<'_>, dss: &DigitallySignedStruct) -> Result<HandshakeSignatureValid, Error> {
        verify_tls13_signature(message, cert, dss, &self.algorithms)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> { self.algorithms.supported_schemes() }
}
