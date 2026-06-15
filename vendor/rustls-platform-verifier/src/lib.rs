//! Drop-in `rustls-platform-verifier` replacement — see this crate's `Cargo.toml`
//! for the full rationale. In short: the real crate's Android verifier panics
//! unless a JVM/Kotlin init has been run; reqwest wires that verifier into every
//! client, and our app never inits it, so the first HTTPS call aborted the
//! process on Android. This shim provides the same `Verifier` API reqwest uses
//! (`new`, `new_with_extra_roots`, `impl ServerCertVerifier`) backed by a
//! pure-Rust WebPKI verifier over the bundled Mozilla roots — no JNI.

use std::sync::Arc;

use rustls::client::WebPkiServerVerifier;
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::CryptoProvider;
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{
    DigitallySignedStruct, Error as TlsError, OtherError, RootCertStore, SignatureScheme,
};

pub use Verifier as PlatformVerifier;

/// A WebPKI-backed server certificate verifier. API-compatible with the subset
/// of `rustls_platform_verifier::Verifier` that reqwest 0.13 depends on.
#[derive(Debug)]
pub struct Verifier {
    inner: Arc<WebPkiServerVerifier>,
}

impl Verifier {
    /// Verifier over the bundled Mozilla roots (and, on desktop, the OS store).
    pub fn new(crypto_provider: Arc<CryptoProvider>) -> Result<Self, TlsError> {
        Self::new_inner(std::iter::empty(), crypto_provider)
    }

    /// As [`Verifier::new`], plus the caller-supplied extra root certificates.
    pub fn new_with_extra_roots(
        extra_roots: impl IntoIterator<Item = CertificateDer<'static>>,
        crypto_provider: Arc<CryptoProvider>,
    ) -> Result<Self, TlsError> {
        Self::new_inner(extra_roots, crypto_provider)
    }

    fn new_inner(
        extra_roots: impl IntoIterator<Item = CertificateDer<'static>>,
        crypto_provider: Arc<CryptoProvider>,
    ) -> Result<Self, TlsError> {
        let mut roots = RootCertStore::empty();

        // Bundled Mozilla roots — present on every target, including Android,
        // with no platform/JNI dependency. This is what fixes the crash.
        roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());

        // Caller-provided extras (reqwest's `new_with_extra_roots`).
        for cert in extra_roots {
            roots.add(cert)?;
        }

        // Desktop: also trust the OS store so private/enterprise CAs (self-hosted
        // media servers etc.) validate just like they did with the real
        // platform verifier. Best-effort — the bundled roots already cover all
        // public CAs, so failures here are non-fatal.
        #[cfg(all(not(target_os = "android"), not(target_arch = "wasm32")))]
        {
            let result = rustls_native_certs::load_native_certs();
            let _ = roots.add_parsable_certificates(result.certs);
        }

        let inner = WebPkiServerVerifier::builder_with_provider(roots.into(), crypto_provider)
            .build()
            .map_err(|e| TlsError::Other(OtherError(Arc::new(e))))?;

        Ok(Self { inner })
    }
}

impl ServerCertVerifier for Verifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        server_name: &ServerName<'_>,
        ocsp_response: &[u8],
        now: UnixTime,
    ) -> Result<ServerCertVerified, TlsError> {
        self.inner
            .verify_server_cert(end_entity, intermediates, server_name, ocsp_response, now)
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, TlsError> {
        self.inner.verify_tls12_signature(message, cert, dss)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, TlsError> {
        self.inner.verify_tls13_signature(message, cert, dss)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.inner.supported_verify_schemes()
    }
}
