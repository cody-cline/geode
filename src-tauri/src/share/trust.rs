//! The client side of trusting a sharing machine.
//!
//! Three ways to trust a server, chosen by how its address arrived:
//!
//! - **Pinned** — from a link. Only the certificate with that fingerprint is
//!   accepted.
//! - **First use** — an address somebody typed. Any certificate is accepted
//!   the first time and its fingerprint is recorded, to be pinned from then
//!   on. The weaker case, and the reason links are the thing admins copy.
//! - **Domain** — an `https://` address. Ordinary certificate-authority
//!   validation, because a domain in front of a tunnel presents the tunnel's
//!   certificate, not the admin's.
//!
//! In every case the TLS handshake signature is still verified against the
//! certificate presented. A certificate is public — anyone who connects is
//! shown it — so a verifier that only compares fingerprints would accept
//! anybody holding a copy. Checking the signature is what proves the server
//! also holds the key.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{verify_tls12_signature, verify_tls13_signature, WebPkiSupportedAlgorithms};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{CertificateError, DigitallySignedStruct, SignatureScheme};
use sha2::{Digest, Sha256};

use crate::error::{Error, Result};
use crate::net::USER_AGENT;

use super::link::Pin;

/// An admin's machine either answers within this or is not reachable at that
/// address — the player should hear so while they are still looking at the
/// dialog.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Per read, not per request: a pack coming up an admin's home connection can
/// take minutes, and that is not a failure.
///
/// Long, because the wait for the first byte counts as a read too, and the
/// admin's machine builds the pack before it sends one — hashing and reading
/// every jar of a few-hundred-mod server. Half a minute would fail exactly the
/// big servers. A request that must answer quickly sets its own deadline.
const READ_TIMEOUT: Duration = Duration::from_secs(5 * 60);

/// How a connection decides whether to trust the server.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Trust {
    Pinned(Pin),
    FirstUse,
    Domain,
}

/// A client that trusts exactly one way, and remembers what it saw.
pub struct TrustedClient {
    pub client: reqwest::Client,
    /// The fingerprint of the certificate the last handshake presented.
    /// Always filled for `Pinned` and `FirstUse`; `None` for `Domain`.
    seen: Arc<Mutex<Option<Pin>>>,
}

impl TrustedClient {
    /// Build a client for one trust mode.
    ///
    /// `Pinned` and `FirstUse` use a custom rustls verifier on the `ring`
    /// provider and set `https_only(true)`, so a link edited to `http://` is
    /// refused instead of silently skipping the pin. `Domain` uses reqwest's
    /// ordinary rustls configuration with the bundled web PKI roots.
    ///
    /// The rustls `ClientConfig` must be built with
    /// `builder_with_provider(ring)`, never `builder()`, and passed through
    /// `use_preconfigured_tls` — see the note in Cargo.toml about providers.
    pub fn new(trust: Trust) -> Result<Self> {
        let seen = Arc::new(Mutex::new(None));
        // `Domain` is https-only too: the address is already `https://`, and
        // this is what stops a redirect from walking it back to plain HTTP,
        // where no certificate is checked at all.
        //
        // And nothing is decompressed, in any mode. A sharing machine never
        // compresses what it sends, and with these off no request offers to
        // accept it, so nothing in front of the machine has reason to either.
        // A compressed reply is a megabyte meant to inflate to gigabytes, and
        // every limit on what a reply may be is counted in the bytes that
        // actually arrived. Each of these exists whether or not its feature is
        // on, so this stays true if another dependency turns one on later.
        let builder = reqwest::Client::builder()
            .user_agent(USER_AGENT)
            .connect_timeout(CONNECT_TIMEOUT)
            .read_timeout(READ_TIMEOUT)
            .https_only(true)
            .no_gzip()
            .no_brotli()
            .no_deflate()
            .no_zstd();
        let builder = match trust {
            Trust::Pinned(pin) => builder.use_preconfigured_tls(machine_tls(Some(pin), &seen)?),
            Trust::FirstUse => builder.use_preconfigured_tls(machine_tls(None, &seen)?),
            Trust::Domain => builder,
        };
        Ok(Self {
            client: builder.build()?,
            seen,
        })
    }

    /// The certificate fingerprint the server presented, once a request has
    /// completed a handshake.
    pub fn seen_pin(&self) -> Option<Pin> {
        *self.seen.lock().unwrap_or_else(|p| p.into_inner())
    }
}

/// TLS for talking straight to an admin's machine: its certificate is judged
/// by [`MachineCert`] alone.
fn machine_tls(
    expect: Option<Pin>,
    seen: &Arc<Mutex<Option<Pin>>>,
) -> Result<rustls::ClientConfig> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let verifier = MachineCert {
        expect,
        seen: seen.clone(),
        algs: provider.signature_verification_algorithms,
    };
    Ok(rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|e| Error::msg(format!("could not set up TLS: {e}")))?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(verifier))
        .with_no_client_auth())
}

/// Trusts a sharing machine's certificate by its fingerprint — the one a link
/// named, or on first use whichever is shown — and checks nothing a
/// certificate authority would: not the name, not the dates, not the issuer.
/// None of those mean anything for a self-signed certificate reached by IP.
#[derive(Debug)]
struct MachineCert {
    /// `None` on first use.
    expect: Option<Pin>,
    seen: Arc<Mutex<Option<Pin>>>,
    algs: WebPkiSupportedAlgorithms,
}

impl MachineCert {
    /// Record a certificate once the server has proven it holds the key.
    ///
    /// Recorded here rather than when the certificate first arrives, so a
    /// first-use handshake that an impostor with a copied certificate fails
    /// leaves no fingerprint behind to be pinned.
    fn proven(
        &self,
        cert: &CertificateDer<'_>,
        valid: HandshakeSignatureValid,
    ) -> HandshakeSignatureValid {
        *self.seen.lock().unwrap_or_else(|p| p.into_inner()) = Some(fingerprint(cert));
        valid
    }
}

fn fingerprint(cert: &CertificateDer<'_>) -> Pin {
    Pin(Sha256::digest(cert.as_ref()).into())
}

impl ServerCertVerifier for MachineCert {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        match self.expect {
            Some(pin) if pin != fingerprint(end_entity) => Err(rustls::Error::InvalidCertificate(
                CertificateError::ApplicationVerificationFailure,
            )),
            _ => Ok(ServerCertVerified::assertion()),
        }
    }

    // rustls hands these the same certificate it gave `verify_server_cert`,
    // so the key checked here is the key of the certificate judged above.

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls12_signature(message, cert, dss, &self.algs).map(|valid| self.proven(cert, valid))
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls13_signature(message, cert, dss, &self.algs).map(|valid| self.proven(cert, valid))
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.algs.supported_schemes()
    }
}

#[cfg(test)]
mod tests {
    use std::convert::Infallible;
    use std::net::SocketAddr;

    use http_body_util::Full;
    use hyper::body::Bytes;
    use hyper::service::service_fn;
    use hyper::Response;
    use hyper_util::rt::TokioIo;
    use rustls::pki_types::{PrivateKeyDer, PrivatePkcs8KeyDer};
    use rustls::sign::{CertifiedKey, SingleCertAndKey};
    use rustls::version::{TLS12, TLS13};
    use rustls::SupportedProtocolVersion;
    use tokio::net::TcpListener;
    use tokio_rustls::TlsAcceptor;

    use super::super::identity::Identity;
    use super::*;

    const BODY: &str = "the pack";

    fn identity() -> Identity {
        let rcgen::CertifiedKey { cert, signing_key } =
            rcgen::generate_simple_self_signed(vec!["geode-share.invalid".to_string()]).unwrap();
        Identity {
            cert_der: cert.der().to_vec(),
            key_der: signing_key.serialize_der(),
        }
    }

    /// A server that shows `shown`'s certificate but signs its handshakes with
    /// `signer`'s key, speaking only `version`. Given the same identity twice
    /// it is the genuine machine; given two, it is somebody who copied the
    /// certificate — which anyone who has ever connected can do.
    fn presenting(
        shown: &Identity,
        signer: &Identity,
        version: &'static SupportedProtocolVersion,
    ) -> Arc<rustls::ServerConfig> {
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let key = provider
            .key_provider
            .load_private_key(PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(
                signer.key_der.clone(),
            )))
            .unwrap();
        let certified = CertifiedKey::new(vec![CertificateDer::from(shown.cert_der.clone())], key);
        assert_eq!(
            certified.keys_match().is_ok(),
            shown.cert_der == signer.cert_der,
            "the test server is not the server it claims to be"
        );
        Arc::new(
            rustls::ServerConfig::builder_with_provider(provider)
                .with_protocol_versions(&[version])
                .unwrap()
                .with_no_client_auth()
                .with_cert_resolver(Arc::new(SingleCertAndKey::from(certified))),
        )
    }

    /// Serve [`BODY`] on loopback, over TLS with `tls` or plain HTTP without.
    async fn serve(tls: Option<Arc<rustls::ServerConfig>>) -> SocketAddr {
        serve_reply(tls, || {
            Response::new(Full::new(Bytes::from_static(BODY.as_bytes())))
        })
        .await
    }

    /// Serve whatever `reply` makes, for every request.
    async fn serve_reply(
        tls: Option<Arc<rustls::ServerConfig>>,
        reply: fn() -> Response<Full<Bytes>>,
    ) -> SocketAddr {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            loop {
                let Ok((tcp, _)) = listener.accept().await else {
                    continue;
                };
                let tls = tls.clone();
                tokio::spawn(async move {
                    let service =
                        service_fn(move |_request| async move { Ok::<_, Infallible>(reply()) });
                    let http = hyper::server::conn::http1::Builder::new();
                    match tls {
                        Some(config) => {
                            if let Ok(stream) = TlsAcceptor::from(config).accept(tcp).await {
                                let _ = http.serve_connection(TokioIo::new(stream), service).await;
                            }
                        }
                        None => {
                            let _ = http.serve_connection(TokioIo::new(tcp), service).await;
                        }
                    }
                });
            }
        });
        addr
    }

    async fn fetch(client: &TrustedClient, url: String) -> reqwest::Result<String> {
        client
            .client
            .get(url)
            .send()
            .await?
            .error_for_status()?
            .text()
            .await
    }

    /// The rustls error that ended the handshake, dug out of reqwest's chain,
    /// so a test can tell a refused certificate from a refused connection.
    ///
    /// It arrives wrapped in an `io::Error` inside another, and
    /// `io::Error::source` skips past what it wraps, so those are unwrapped
    /// by hand.
    fn tls_error(err: &reqwest::Error) -> Option<rustls::Error> {
        let mut next: Option<&(dyn std::error::Error + 'static)> = Some(err);
        while let Some(e) = next {
            if let Some(tls) = e.downcast_ref::<rustls::Error>() {
                return Some(tls.clone());
            }
            next = match e.downcast_ref::<std::io::Error>() {
                Some(io) => io.get_ref().map(|inner| inner as _),
                None => e.source(),
            };
        }
        None
    }

    fn refused(cert: CertificateError) -> Option<rustls::Error> {
        Some(rustls::Error::InvalidCertificate(cert))
    }

    #[tokio::test]
    async fn a_pinned_client_accepts_the_machine_the_link_names() {
        let machine = identity();
        let servers = [
            ("default", machine.server_config().unwrap()),
            ("TLS 1.2", presenting(&machine, &machine, &TLS12)),
        ];
        for (what, config) in servers {
            let addr = serve(Some(config)).await;
            let client = TrustedClient::new(Trust::Pinned(machine.pin())).unwrap();
            assert_eq!(client.seen_pin(), None, "nothing seen before connecting");

            let body = fetch(&client, format!("https://{addr}/")).await;

            assert_eq!(body.unwrap(), BODY, "{what}");
            assert_eq!(client.seen_pin(), Some(machine.pin()), "{what}");
        }
    }

    #[tokio::test]
    async fn a_pinned_client_refuses_any_other_machine() {
        let machine = identity();
        let addr = serve(Some(machine.server_config().unwrap())).await;
        let client = TrustedClient::new(Trust::Pinned(identity().pin())).unwrap();

        let err = fetch(&client, format!("https://{addr}/"))
            .await
            .unwrap_err();

        assert_eq!(
            tls_error(&err),
            refused(CertificateError::ApplicationVerificationFailure),
            "{err:?}"
        );
        assert_eq!(client.seen_pin(), None);
    }

    /// A typed address has nothing to pin yet, so whatever the machine shows
    /// is accepted — and remembered, which is what gets pinned next time.
    #[tokio::test]
    async fn first_use_accepts_the_machine_and_records_its_fingerprint() {
        let machine = identity();
        for version in [&TLS13, &TLS12] {
            let addr = serve(Some(presenting(&machine, &machine, version))).await;
            let client = TrustedClient::new(Trust::FirstUse).unwrap();

            let body = fetch(&client, format!("https://{addr}/")).await;

            assert_eq!(body.unwrap(), BODY, "{version:?}");
            assert_eq!(client.seen_pin(), Some(machine.pin()), "{version:?}");
        }
    }

    /// The attack a fingerprint check alone does not stop: the certificate is
    /// public, so an impostor can show the real one and simply sign the
    /// handshake with a key of its own. Only checking that signature against
    /// the certificate catches it, on either TLS version, pinned or not — and
    /// a first-use client must not come away having recorded the fingerprint
    /// the impostor showed.
    #[tokio::test]
    async fn a_copied_certificate_without_its_key_is_refused() {
        let machine = identity();
        let impostor = identity();
        for version in [&TLS13, &TLS12] {
            let addr = serve(Some(presenting(&machine, &impostor, version))).await;
            for trust in [Trust::Pinned(machine.pin()), Trust::FirstUse] {
                let client = TrustedClient::new(trust).unwrap();

                let err = fetch(&client, format!("https://{addr}/"))
                    .await
                    .expect_err(&format!("{trust:?} over {version:?} let the impostor in"));

                assert_eq!(
                    tls_error(&err),
                    refused(CertificateError::BadSignature),
                    "{trust:?} over {version:?}: {err:?}"
                );
                assert_eq!(client.seen_pin(), None, "{trust:?} over {version:?}");
            }
        }
    }

    /// A domain is checked by its certificate authority, which has never heard
    /// of a sharing machine's self-signed certificate.
    #[tokio::test]
    async fn a_domain_client_refuses_a_self_signed_machine() {
        let machine = identity();
        let addr = serve(Some(machine.server_config().unwrap())).await;
        let client = TrustedClient::new(Trust::Domain).unwrap();

        let err = fetch(&client, format!("https://{addr}/"))
            .await
            .unwrap_err();

        assert_eq!(
            tls_error(&err),
            refused(CertificateError::UnknownIssuer),
            "{err:?}"
        );
        assert_eq!(client.seen_pin(), None);
    }

    /// A link edited to `http://` would otherwise fetch in the clear with no
    /// certificate to check. The server here answers plain HTTP happily, so
    /// only the client's refusal keeps the request from succeeding.
    #[tokio::test]
    async fn plain_http_is_refused_before_connecting() {
        let addr = serve(None).await;
        for trust in [
            Trust::Pinned(identity().pin()),
            Trust::FirstUse,
            Trust::Domain,
        ] {
            let client = TrustedClient::new(trust).unwrap();

            let err = fetch(&client, format!("http://{addr}/")).await.unwrap_err();

            assert!(
                err.is_builder(),
                "{trust:?}: refused as a bad scheme, not a failed request: {err:?}"
            );
            assert_eq!(client.seen_pin(), None, "{trust:?}");
        }
    }

    /// A megabyte of zeros, gzipped: about a kilobyte on the wire.
    fn gzipped_megabyte() -> Vec<u8> {
        use std::io::Write;
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        encoder.write_all(&[0u8; 1024 * 1024]).unwrap();
        encoder.finish().unwrap()
    }

    /// A reply marked as compressed arrives as exactly the bytes that were
    /// sent. A sharing machine never compresses, so a client that inflated
    /// replies would only be letting a server turn a megabyte on the wire into
    /// gigabytes in the player's memory, past every limit counted in bytes
    /// received.
    #[tokio::test]
    async fn a_compressed_reply_is_never_inflated() {
        let machine = identity();
        let addr = serve_reply(Some(machine.server_config().unwrap()), || {
            let mut response = Response::new(Full::new(Bytes::from(gzipped_megabyte())));
            response
                .headers_mut()
                .insert(hyper::header::CONTENT_ENCODING, "gzip".parse().unwrap());
            response
        })
        .await;
        let sent = gzipped_megabyte();
        assert!(
            sent.len() < 64 * 1024,
            "the test needs a reply that inflates a long way"
        );

        for trust in [Trust::Pinned(machine.pin()), Trust::FirstUse] {
            let client = TrustedClient::new(trust).unwrap();

            let body = client
                .client
                .get(format!("https://{addr}/"))
                .send()
                .await
                .unwrap()
                .bytes()
                .await
                .unwrap();

            assert_eq!(body.len(), sent.len(), "{trust:?} inflated the reply");
            assert_eq!(body.as_ref(), sent.as_slice(), "{trust:?}");
        }
    }
}
