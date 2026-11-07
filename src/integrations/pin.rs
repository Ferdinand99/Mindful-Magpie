//! Certificate pinning: how a service with a self-signed certificate is
//! reached without ever switching verification off.
//!
//! A service may carry the SHA-256 fingerprint of its leaf certificate (the
//! DER bytes) in its settings under [`SETTING`]. Its fetches then go through a
//! client whose TLS accepts exactly that certificate and nothing else: the
//! hostname and chain checks are replaced by the pin, and the certificate's
//! dates are not looked at (a self-signed certificate that has expired is the
//! one the admin pinned). The signature of the handshake is still checked in
//! full against the certificate's key, with the same algorithms as the shared
//! client, so holding the certificate is not enough: the server has to hold its
//! private key.
//!
//! Everything else about the client is the shared one (see
//! [`super::guarded`]): the guarded resolver, no proxy, the timeouts, the
//! redirect rules and, through [`super::read_capped`], the body cap. A pin
//! only applies to `https` addresses.
//!
//! [`presented`] is for «Test»: it reads the leaf certificate a server shows,
//! without trusting it and without sending anything, so the admin can see the
//! fingerprint to pin. The fingerprint is not a secret.

use std::collections::VecDeque;
use std::error::Error as StdError;
use std::fmt;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{WebPkiSupportedAlgorithms, aws_lc_rs};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{CertificateError, DigitallySignedStruct, Error, OtherError, SignatureScheme};
use serde_json::Value as Json;
use sha2::{Digest as _, Sha256};
use url::Url;
use vole_kit::net::check_host;

use super::{FetchError, TIMEOUT};

/// The settings key a service's pin is stored under (64 lowercase hex
/// characters, or absent).
pub const SETTING: &str = "cert_sha256";

/// How many pinned clients are kept. A client holds a connection pool, so the
/// number is small; past it the one used longest ago goes.
pub const CACHE_MAX: usize = 32;

/// A fingerprint as 64 lowercase hex characters. Colons, spaces and capitals
/// in `text` are fine (`openssl` and browsers show `AA:BB:…`); anything else
/// is `None`.
pub fn normalize(text: &str) -> Option<String> {
    let hex: String = text
        .chars()
        .filter(|c| *c != ':' && !c.is_whitespace())
        .map(|c| c.to_ascii_lowercase())
        .collect();
    (hex.len() == 64 && hex.bytes().all(|b| b.is_ascii_hexdigit())).then_some(hex)
}

/// The fingerprint of a certificate: SHA-256 of its DER, as 64 lowercase hex
/// characters.
pub fn fingerprint(der: &[u8]) -> String {
    hex(&Sha256::digest(der))
}

/// `fingerprint` (as [`normalize`] gives it) the way `openssl` writes it:
/// capitals, pairs separated by colons.
pub fn display(fingerprint: &str) -> String {
    fingerprint
        .as_bytes()
        .chunks(2)
        .map(|pair| String::from_utf8_lossy(pair).to_uppercase())
        .collect::<Vec<_>>()
        .join(":")
}

fn hex(bytes: &[u8]) -> String {
    use fmt::Write as _;
    bytes.iter().fold(String::with_capacity(64), |mut out, b| {
        let _ = write!(out, "{b:02x}");
        out
    })
}

fn unhex(text: &str) -> Option<[u8; 32]> {
    let mut out = [0u8; 32];
    let bytes = text.as_bytes();
    if bytes.len() != 64 {
        return None;
    }
    for (i, pair) in bytes.chunks(2).enumerate() {
        let digit = |b: u8| char::from(b).to_digit(16);
        out[i] = u8::try_from(digit(pair[0])? * 16 + digit(pair[1])?).ok()?;
    }
    Some(out)
}

/// The pin in a service's settings: `None` when there is none (absent, null or
/// empty), the normalised fingerprint, or [`FetchError::Config`] for a value
/// that is not a fingerprint.
pub fn from_settings(settings: &Json) -> Result<Option<String>, FetchError> {
    match settings.get(SETTING) {
        None | Some(Json::Null) => Ok(None),
        Some(Json::String(text)) if text.trim().is_empty() => Ok(None),
        Some(Json::String(text)) => normalize(text).map(Some).ok_or(FetchError::Config),
        Some(_) => Err(FetchError::Config),
    }
}

// ---------------------------------------------------------------------------
// The verifiers

/// Why a certificate was not accepted. Its text names nothing.
#[derive(Debug)]
struct CertRefused;

impl fmt::Display for CertRefused {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("the server's certificate is not the pinned one")
    }
}

impl StdError for CertRefused {}

fn refused() -> Error {
    Error::InvalidCertificate(CertificateError::Other(OtherError(Arc::new(CertRefused))))
}

/// Whether `err`, or something it wraps, is a certificate that TLS did not
/// accept: a pin that does not match, or a certificate the ordinary checks
/// refused (unknown issuer, expired, wrong name …).
pub(crate) fn is_certificate_error(err: &(dyn StdError + 'static)) -> bool {
    if err.downcast_ref::<CertRefused>().is_some()
        || matches!(
            err.downcast_ref::<Error>(),
            Some(Error::InvalidCertificate(_))
        )
    {
        return true;
    }
    // An `io::Error` that wraps an error hides the wrapped one from `source()`
    // (it passes on the wrapped error's source); TLS errors travel that way.
    err.downcast_ref::<std::io::Error>()
        .and_then(std::io::Error::get_ref)
        .is_some_and(|inner| is_certificate_error(inner))
}

/// The signature algorithms of the default crypto provider: the same the
/// shared client's handshakes are checked with.
fn algorithms() -> WebPkiSupportedAlgorithms {
    aws_lc_rs::default_provider().signature_verification_algorithms
}

/// Accepts the leaf certificate whose SHA-256 is `want`, and nothing else. The
/// trust decision is the only thing replaced: the handshake signature is
/// checked against the certificate's key by rustls' own functions.
#[derive(Debug)]
struct PinVerifier {
    want: [u8; 32],
    algorithms: WebPkiSupportedAlgorithms,
}

impl ServerCertVerifier for PinVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, Error> {
        // Public data compared with public data: no timing concern.
        if Sha256::digest(end_entity.as_ref()).as_slice() == self.want {
            Ok(ServerCertVerified::assertion())
        } else {
            Err(refused())
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, Error> {
        rustls::crypto::verify_tls12_signature(message, cert, dss, &self.algorithms)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, Error> {
        rustls::crypto::verify_tls13_signature(message, cert, dss, &self.algorithms)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.algorithms.supported_schemes()
    }
}

/// Reads the leaf certificate and then refuses it, so the connection ends
/// during the handshake and nothing is ever sent over it.
#[derive(Debug)]
struct Capture {
    seen: Arc<Mutex<Option<String>>>,
    algorithms: WebPkiSupportedAlgorithms,
}

impl ServerCertVerifier for Capture {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, Error> {
        *self.seen.lock().unwrap_or_else(|e| e.into_inner()) = Some(fingerprint(end_entity));
        Err(refused())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, Error> {
        rustls::crypto::verify_tls12_signature(message, cert, dss, &self.algorithms)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, Error> {
        rustls::crypto::verify_tls13_signature(message, cert, dss, &self.algorithms)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.algorithms.supported_schemes()
    }
}

/// A TLS client configuration that trusts by `verifier`. (rustls files
/// replacing the verifier under `dangerous()`; both verifiers here are strict:
/// one accepts a single certificate, the other accepts none.)
fn tls_config(verifier: Arc<dyn ServerCertVerifier>) -> rustls::ClientConfig {
    let provider = Arc::new(aws_lc_rs::default_provider());
    let mut config = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .expect("the default protocol versions are supported")
        .dangerous()
        .with_custom_certificate_verifier(verifier)
        .with_no_client_auth();
    // Plain HTTP/1.1, as the shared client speaks.
    config.alpn_protocols = vec![b"http/1.1".to_vec()];
    config
}

// ---------------------------------------------------------------------------
// The clients

/// A client for `https` that accepts only the certificate with `fingerprint`
/// (see [`normalize`] for what is read), and is otherwise the shared client
/// with `timeout`. [`FetchError::Config`] for a value that is not a
/// fingerprint.
pub fn build_pinned_client(
    fingerprint: &str,
    timeout: Duration,
) -> Result<reqwest::Client, FetchError> {
    let want = normalize(fingerprint)
        .and_then(|hex| unhex(&hex))
        .ok_or(FetchError::Config)?;
    let verifier = Arc::new(PinVerifier {
        want,
        algorithms: algorithms(),
    });
    super::guarded(
        reqwest::Client::builder().tls_backend_preconfigured(tls_config(verifier)),
        timeout,
    )
    // A pinned service is reached over TLS, and a redirect to plain http is
    // refused already.
    .https_only(true)
    .build()
    .map_err(|_| FetchError::Other)
}

type Cache = Mutex<VecDeque<(String, reqwest::Client)>>;

fn cache() -> std::sync::MutexGuard<'static, VecDeque<(String, reqwest::Client)>> {
    static CACHE: Cache = Mutex::new(VecDeque::new());
    CACHE.lock().unwrap_or_else(|e| e.into_inner())
}

/// The pinned client for `fingerprint`, with the shared [`TIMEOUT`]. One client
/// per fingerprint is kept (at most [`CACHE_MAX`]), so the poller does not
/// build one for every fetch.
pub fn pinned_client(fingerprint: &str) -> Result<reqwest::Client, FetchError> {
    let key = normalize(fingerprint).ok_or(FetchError::Config)?;
    {
        let mut cache = cache();
        if let Some(at) = cache.iter().position(|(k, _)| *k == key)
            && let Some(entry) = cache.remove(at)
        {
            let client = entry.1.clone();
            cache.push_back(entry);
            return Ok(client);
        }
    }
    let client = build_pinned_client(&key, TIMEOUT)?;
    let mut cache = cache();
    // Another fetch may have made it meanwhile; either is fine.
    cache.retain(|(k, _)| *k != key);
    cache.push_back((key, client.clone()));
    while cache.len() > CACHE_MAX {
        cache.pop_front();
    }
    Ok(client)
}

/// How many pinned clients are kept.
pub fn cached_clients() -> usize {
    cache().len()
}

// ---------------------------------------------------------------------------
// «Test»

/// The fingerprint of the leaf certificate that the server at `url` presents,
/// as 64 lowercase hex characters; `None` when `url` is not `https`, is not an
/// allowed address, or nothing answers the handshake.
///
/// The certificate is read, not trusted: the handshake is refused as soon as it
/// is seen, so no request, header or secret is ever sent. The same address
/// rules and timeouts as every fetch apply. The server has not proved it holds
/// the certificate's key at that point, so the admin has to know the
/// fingerprint is right (the pin check itself, on every later fetch, does
/// verify the signature).
pub async fn presented(url: &Url) -> Option<String> {
    if url.scheme() != "https" {
        return None;
    }
    // An address in the URL never reaches the guarded resolver.
    check_host(url.host_str()?).ok()?;
    let seen = Arc::new(Mutex::new(None));
    let verifier = Arc::new(Capture {
        seen: seen.clone(),
        algorithms: algorithms(),
    });
    let client = super::guarded(
        reqwest::Client::builder().tls_backend_preconfigured(tls_config(verifier)),
        TIMEOUT,
    )
    .redirect(reqwest::redirect::Policy::none())
    .https_only(true)
    .build()
    .ok()?;
    let mut target = url.clone();
    target.set_query(None);
    target.set_fragment(None);
    // The request is never sent: the handshake is refused first.
    let _ = client.head(target).send().await;
    seen.lock().unwrap_or_else(|e| e.into_inner()).take()
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn a_fingerprint_is_read_in_any_usual_spelling() {
        let plain = "0123456789abcdef".repeat(4);
        assert_eq!(normalize(&plain).as_deref(), Some(plain.as_str()));
        assert_eq!(
            normalize(&plain.to_uppercase()).as_deref(),
            Some(plain.as_str())
        );
        assert_eq!(normalize(&display(&plain)).as_deref(), Some(plain.as_str()));
        assert_eq!(
            normalize(&format!("  {}\n", display(&plain).replace(':', " "))).as_deref(),
            Some(plain.as_str())
        );
        assert_eq!(
            normalize(&format!("sha256 fingerprint={}", display(&plain))),
            None,
            "a label is not part of it"
        );
        for bad in [
            "",
            "abc",
            &plain[..63],
            &format!("{plain}0"),
            &"g".repeat(64),
            &format!("{}é", &plain[..62]),
        ] {
            assert_eq!(normalize(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn the_fingerprint_is_the_sha256_of_the_der() {
        // SHA-256 of the empty input.
        assert_eq!(
            fingerprint(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            display(&fingerprint(b"")),
            "E3:B0:C4:42:98:FC:1C:14:9A:FB:F4:C8:99:6F:B9:24:27:AE:41:E4:64:9B:93:4C:A4:95:99:1B:78:52:B8:55"
        );
        assert_eq!(hex(&unhex(&fingerprint(b"x")).unwrap()), fingerprint(b"x"));
    }

    #[test]
    fn the_pin_is_read_from_the_settings() {
        let plain = "ab".repeat(32);
        let at = |v: Json| from_settings(&json!({ SETTING: v }));
        assert_eq!(at(json!(plain)).unwrap().as_deref(), Some(plain.as_str()));
        assert_eq!(
            at(json!(display(&plain))).unwrap().as_deref(),
            Some(plain.as_str())
        );
        assert_eq!(at(json!("")).unwrap(), None);
        assert_eq!(at(json!("  ")).unwrap(), None);
        assert_eq!(at(json!(null)).unwrap(), None);
        assert_eq!(from_settings(&json!({})).unwrap(), None);
        for bad in [json!("nope"), json!(5), json!(true), json!([plain])] {
            assert_eq!(at(bad).unwrap_err(), FetchError::Config);
        }
    }

    #[test]
    fn only_the_pinned_certificate_passes() {
        let der = CertificateDer::from(vec![1u8, 2, 3]);
        let verifier = PinVerifier {
            want: unhex(&fingerprint(&[1, 2, 3])).unwrap(),
            algorithms: algorithms(),
        };
        let name = ServerName::try_from("anything.example").unwrap();
        let check = |cert: &CertificateDer<'_>| {
            verifier.verify_server_cert(cert, &[], &name, &[], UnixTime::now())
        };
        assert!(
            check(&der).is_ok(),
            "the name and the dates are not looked at"
        );
        let err = check(&CertificateDer::from(vec![1u8, 2, 4])).unwrap_err();
        assert!(is_certificate_error(&err));
        assert!(!verifier.supported_verify_schemes().is_empty());
    }

    #[test]
    fn certificate_errors_are_found_through_io_errors() {
        let tls = Error::InvalidCertificate(CertificateError::UnknownIssuer);
        assert!(is_certificate_error(&tls));
        let wrapped = std::io::Error::new(std::io::ErrorKind::InvalidData, tls);
        assert!(is_certificate_error(&wrapped));
        let other = std::io::Error::new(std::io::ErrorKind::InvalidData, Error::DecryptError);
        assert!(!is_certificate_error(&other));
        assert!(!is_certificate_error(&std::io::Error::other("x")));
    }
}
