//! Certificate pinning: a service with a self-signed certificate is reached by
//! naming the certificate's SHA-256 fingerprint, never by switching the
//! verification off. The tests run a TLS server with a certificate made in the
//! test and drive the `http` kind, «Test», the store and the poller against it.

mod common;

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, header};
use axum::routing::get;
use common::{App, Res, Web};
use mindful_magpie::integrations::poller::{Options, Poller};
use mindful_magpie::integrations::{
    self, FetchCtx, FetchError, Integration, Snapshot, Status, pin,
};
use mindful_magpie::services::{SecretChange, ServiceInput, Visibility, store};
use serde_json::{Value as Json, json};
use sha2::{Digest as _, Sha256};
use tokio::net::{TcpListener, TcpStream};
use tokio_rustls::TlsAcceptor;
use tokio_rustls::rustls::ServerConfig;
use tokio_rustls::rustls::pki_types::PrivatePkcs8KeyDer;
use tokio_rustls::rustls::server::{ClientHello, ResolvesServerCert};
use tokio_rustls::rustls::sign::CertifiedKey;

const SECRET: &str = "pin-S3cret-Tok3n-1a2b3c";

// ---------------------------------------------------------------------------
// A TLS server with a self-signed certificate

/// What the stub saw: the `Authorization` header of every request that got as
/// far as a handler.
type Seen = Arc<Mutex<Vec<Option<String>>>>;

struct TlsStub {
    port: u16,
    /// The leaf certificate's SHA-256, as 64 lowercase hex characters.
    fingerprint: String,
    seen: Seen,
}

impl TlsStub {
    fn url(&self, path: &str) -> String {
        format!("https://127.0.0.1:{}{path}", self.port)
    }

    fn requests(&self) -> Vec<Option<String>> {
        self.seen.lock().unwrap().clone()
    }
}

struct TlsListener {
    tcp: TcpListener,
    acceptor: TlsAcceptor,
}

impl axum::serve::Listener for TlsListener {
    type Io = tokio_rustls::server::TlsStream<TcpStream>;
    type Addr = SocketAddr;

    async fn accept(&mut self) -> (Self::Io, Self::Addr) {
        loop {
            let Ok((stream, addr)) = self.tcp.accept().await else {
                continue;
            };
            // A client that refuses the certificate ends here; the next one
            // is waited for.
            if let Ok(io) = self.acceptor.accept(stream).await {
                return (io, addr);
            }
        }
    }

    fn local_addr(&self) -> std::io::Result<Self::Addr> {
        self.tcp.local_addr()
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

async fn record(State(seen): State<Seen>, headers: HeaderMap) -> &'static str {
    seen.lock().unwrap().push(
        headers
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .map(str::to_string),
    );
    "hello"
}

/// A TLS 1.3 server on a free loopback port.
async fn tls_stub() -> TlsStub {
    serve_tls(&[&tokio_rustls::rustls::version::TLS13]).await
}

async fn serve_tls(
    versions: &[&'static tokio_rustls::rustls::SupportedProtocolVersion],
) -> TlsStub {
    serve(versions, false).await
}

/// A server whose certificate is signed by a key that is not the certificate's
/// own: someone who has the certificate (it is public) but not its private key.
async fn impostor() -> TlsStub {
    serve(&[&tokio_rustls::rustls::version::TLS13], true).await
}

/// Always shows the same certificate and signs with the same key.
#[derive(Debug)]
struct Fixed(Arc<CertifiedKey>);

impl ResolvesServerCert for Fixed {
    fn resolve(&self, _hello: ClientHello<'_>) -> Option<Arc<CertifiedKey>> {
        Some(self.0.clone())
    }
}

async fn serve(
    versions: &[&'static tokio_rustls::rustls::SupportedProtocolVersion],
    wrong_key: bool,
) -> TlsStub {
    let made = rcgen::generate_simple_self_signed(vec!["localhost".to_string()]).unwrap();
    let der = made.cert.der().clone();
    let fingerprint = hex(&Sha256::digest(der.as_ref()));
    let other = rcgen::generate_simple_self_signed(vec!["localhost".to_string()]).unwrap();
    let key_pair = if wrong_key {
        &other.signing_key
    } else {
        &made.signing_key
    };
    let key = PrivatePkcs8KeyDer::from(key_pair.serialize_der());
    let provider = Arc::new(tokio_rustls::rustls::crypto::aws_lc_rs::default_provider());
    let signing = provider.key_provider.load_private_key(key.into()).unwrap();
    let certified = Arc::new(CertifiedKey::new(vec![der], signing));
    let config = ServerConfig::builder_with_provider(provider)
        .with_protocol_versions(versions)
        .unwrap()
        .with_no_client_auth()
        .with_cert_resolver(Arc::new(Fixed(certified)));
    let seen = Seen::default();
    let app = Router::new()
        .route("/ok", get(record))
        .route(
            "/fail",
            get(|| async { (StatusCode::INTERNAL_SERVER_ERROR, "boom") }),
        )
        .with_state(seen.clone());
    let tcp = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = tcp.local_addr().unwrap().port();
    let listener = TlsListener {
        tcp,
        acceptor: TlsAcceptor::from(Arc::new(config)),
    };
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    TlsStub {
        port,
        fingerprint,
        seen,
    }
}

// ---------------------------------------------------------------------------
// Helpers

fn http_kind() -> &'static dyn Integration {
    integrations::find("http").expect("the http kind is registered")
}

/// A fetch context the way the store builds one: the client follows the
/// settings.
fn ctx(url: &str, pin: Option<&str>, secret: Option<&str>) -> FetchCtx {
    let mut settings = json!({ "url": url });
    if let Some(pin) = pin {
        settings[pin::SETTING] = Json::String(pin.to_string());
    }
    FetchCtx {
        http: integrations::client_for(&settings).expect("a client for the settings"),
        settings,
        secret: secret.map(str::to_string),
    }
}

async fn fetch(url: &str, pin: Option<&str>, secret: Option<&str>) -> Snapshot {
    http_kind()
        .fetch(&ctx(url, pin, secret))
        .await
        .expect("the http kind answers with a snapshot")
}

/// The form of a new `http` service as the page posts it.
fn form<'a>(url: &'a str, pin: &'a str, secret: &'a str) -> Vec<(&'a str, &'a str)> {
    vec![
        ("kind", "http"),
        ("name", "Router"),
        ("url", url),
        ("f_expected_status", ""),
        ("cert_sha256", pin),
        ("interval", ""),
        ("visibility", "admin"),
        ("secret", secret),
    ]
}

fn input(url: &str, pin: Option<&str>, secret: SecretChange) -> ServiceInput {
    let mut settings = json!({});
    if let Some(pin) = pin {
        settings[pin::SETTING] = Json::String(pin.to_string());
    }
    ServiceInput {
        kind: "http".into(),
        name: "Router".into(),
        url: Some(url.into()),
        settings,
        secret,
        interval_s: 60,
        visibility: Visibility::Admin,
        users: Vec::new(),
    }
}

/// `aa:bb:…` in capitals, the way a browser or `openssl` shows it.
fn colons(fingerprint: &str) -> String {
    fingerprint
        .as_bytes()
        .chunks(2)
        .map(|c| std::str::from_utf8(c).unwrap().to_uppercase())
        .collect::<Vec<_>>()
        .join(":")
}

fn assert_no_secret(res: &Res, what: &str) {
    assert!(!res.body.contains(SECRET), "{what} leaks the secret");
    assert!(!res.body.contains("Bearer"), "{what} mentions the header");
}

// ---------------------------------------------------------------------------
// The fetch

#[tokio::test]
async fn without_a_pin_a_self_signed_certificate_fails() {
    let stub = tls_stub().await;
    let snapshot = fetch(&stub.url("/ok"), None, None).await;
    assert_eq!(snapshot.status, Status::Bad);
    let text = snapshot.error.expect("a reason");
    assert_eq!(text, FetchError::Cert.to_string());
    assert_eq!(integrations::error_text_key(&text), Some("fetch_err.cert"));
    assert!(stub.requests().is_empty(), "no request got through");
}

#[tokio::test]
async fn with_the_right_pin_the_fetch_works() {
    let stub = tls_stub().await;
    let snapshot = fetch(&stub.url("/ok"), Some(&stub.fingerprint), Some(SECRET)).await;
    assert_eq!(snapshot.status, Status::Ok, "{:?}", snapshot.error);
    assert_eq!(stub.requests(), [Some(format!("Bearer {SECRET}"))]);
}

#[tokio::test]
async fn a_pin_may_have_colons_spaces_and_capitals() {
    let stub = tls_stub().await;
    for text in [
        colons(&stub.fingerprint),
        format!("  {}  ", stub.fingerprint.to_uppercase()),
        colons(&stub.fingerprint).replace(':', " "),
    ] {
        let snapshot = fetch(&stub.url("/ok"), Some(&text), None).await;
        assert_eq!(
            snapshot.status,
            Status::Ok,
            "{text:?}: {:?}",
            snapshot.error
        );
    }
}

#[tokio::test]
async fn a_tls_1_2_server_works_with_the_pin_too() {
    let stub = serve_tls(&[&tokio_rustls::rustls::version::TLS12]).await;
    let snapshot = fetch(&stub.url("/ok"), Some(&stub.fingerprint), None).await;
    assert_eq!(snapshot.status, Status::Ok, "{:?}", snapshot.error);
}

#[tokio::test]
async fn a_wrong_pin_fails_with_a_certificate_error_and_sends_nothing() {
    let stub = tls_stub().await;
    let wrong = "ab".repeat(32);
    let snapshot = fetch(&stub.url("/ok"), Some(&wrong), Some(SECRET)).await;
    assert_eq!(snapshot.status, Status::Bad);
    let text = snapshot.error.expect("a reason");
    assert_eq!(text, FetchError::Cert.to_string());
    assert_eq!(integrations::error_text_key(&text), Some("fetch_err.cert"));
    assert!(!text.contains(SECRET));
    assert!(stub.requests().is_empty(), "the key never left");
}

#[tokio::test]
async fn the_certificate_alone_is_not_enough_the_server_must_hold_its_key() {
    let stub = impostor().await;
    let snapshot = fetch(&stub.url("/ok"), Some(&stub.fingerprint), Some(SECRET)).await;
    assert_eq!(snapshot.status, Status::Bad);
    assert_eq!(snapshot.error.as_deref(), Some("certificate not accepted"));
    assert!(stub.requests().is_empty(), "the key never left");
}

#[tokio::test]
async fn the_pin_is_for_that_server_only() {
    // Another server's certificate does not pass because the first one's
    // fingerprint is known.
    let (a, b) = (tls_stub().await, tls_stub().await);
    assert_ne!(a.fingerprint, b.fingerprint);
    let snapshot = fetch(&b.url("/ok"), Some(&a.fingerprint), None).await;
    assert_eq!(snapshot.error.as_deref(), Some("certificate not accepted"));
}

#[tokio::test]
async fn a_pinned_client_still_refuses_the_metadata_address() {
    let settings = json!({ "url": "https://169.254.169.254/", pin::SETTING: "ab".repeat(32) });
    let ctx = FetchCtx {
        http: integrations::client_for(&settings).unwrap(),
        settings,
        secret: None,
    };
    let err = http_kind().fetch(&ctx).await;
    assert!(
        matches!(err, Err(FetchError::Forbidden))
            || err.is_ok_and(|s| s.error.as_deref() == Some("address not allowed")),
        "the address check comes first"
    );
}

#[tokio::test]
async fn pinned_clients_are_cached_per_fingerprint_and_bounded() {
    let (a, b) = ("11".repeat(32), "22".repeat(32));
    pin::pinned_client(&a).unwrap();
    pin::pinned_client(&b).unwrap();
    let before = pin::cached_clients();
    assert!(before >= 2);
    pin::pinned_client(&a).unwrap();
    assert_eq!(
        pin::cached_clients(),
        before,
        "the same pin, the same client"
    );
    // Colons and capitals are the same pin.
    pin::pinned_client(&colons(&a)).unwrap();
    assert_eq!(pin::cached_clients(), before);
    for i in 0..(pin::CACHE_MAX + 20) {
        pin::pinned_client(&format!("{i:064x}")).unwrap();
    }
    assert!(pin::cached_clients() <= pin::CACHE_MAX);
}

#[test]
fn bad_pins_are_config_errors() {
    for bad in [
        "",
        "xyz",
        &"ab".repeat(31),
        &"ab".repeat(33),
        &"zz".repeat(32),
    ] {
        assert_eq!(
            pin::pinned_client(bad).unwrap_err(),
            FetchError::Config,
            "{bad:?}"
        );
    }
    // A pin on a plain-http address is refused even if the stored settings got there.
    let settings = json!({ "url": "http://10.0.0.5/", pin::SETTING: "ab".repeat(32) });
    assert_eq!(
        integrations::client_for(&settings).unwrap_err(),
        FetchError::Config
    );
    // A bad stored pin does not fall back to the shared client.
    let settings = json!({ "url": "https://10.0.0.5/", pin::SETTING: "nonsense" });
    assert_eq!(
        integrations::client_for(&settings).unwrap_err(),
        FetchError::Config
    );
    // No pin (or an empty one): the shared client.
    for settings in [
        json!({ "url": "https://10.0.0.5/" }),
        json!({ "url": "https://10.0.0.5/", pin::SETTING: "" }),
        json!({ "url": "http://10.0.0.5/" }),
    ] {
        assert!(integrations::client_for(&settings).is_ok(), "{settings}");
    }
}

// ---------------------------------------------------------------------------
// «Test»

#[tokio::test]
async fn test_shows_the_presented_fingerprint_and_the_probe_sends_nothing() {
    let stub = tls_stub().await;
    let app = App::new().await;
    let mut admin = Web::new(&app, "root", "admin").await;
    let res = admin
        .post(
            "/settings/services/test",
            &form(&stub.url("/ok"), "", SECRET),
        )
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
    assert!(
        res.body.contains("data-test-status=\"bad\""),
        "{}",
        res.body
    );
    assert!(res.body.contains("certificate"), "the reason, in words");
    assert!(
        res.body.contains(&colons(&stub.fingerprint)),
        "the fingerprint is shown: {}",
        res.body
    );
    assert!(
        res.body
            .contains(&format!("data-use-fingerprint=\"{}\"", stub.fingerprint)),
        "one click fills the field: {}",
        res.body
    );
    assert_no_secret(&res, "the test result");
    assert!(
        stub.requests().is_empty(),
        "neither the fetch nor the probe got a request through, so no secret went out"
    );
    assert_eq!(app.count("SELECT COUNT(*) FROM services").await, 0);
}

#[tokio::test]
async fn test_with_the_right_pin_works_and_shows_no_fingerprint_prompt() {
    let stub = tls_stub().await;
    let app = App::new().await;
    let mut admin = Web::new(&app, "root", "admin").await;
    let res = admin
        .post(
            "/settings/services/test",
            &form(&stub.url("/ok"), &colons(&stub.fingerprint), SECRET),
        )
        .await;
    assert!(res.body.contains("data-test-status=\"ok\""), "{}", res.body);
    assert!(!res.body.contains("data-use-fingerprint"));
    assert_eq!(stub.requests(), [Some(format!("Bearer {SECRET}"))]);
}

#[tokio::test]
async fn test_with_a_wrong_pin_shows_what_the_server_presents() {
    let stub = tls_stub().await;
    let app = App::new().await;
    let mut admin = Web::new(&app, "root", "admin").await;
    let wrong = "cd".repeat(32);
    let res = admin
        .post(
            "/settings/services/test",
            &form(&stub.url("/ok"), &wrong, SECRET),
        )
        .await;
    assert!(
        res.body.contains("data-test-status=\"bad\""),
        "{}",
        res.body
    );
    assert!(
        res.body
            .contains(&format!("data-use-fingerprint=\"{}\"", stub.fingerprint)),
        "{}",
        res.body
    );
    assert_no_secret(&res, "the test result");
    assert!(stub.requests().is_empty());
}

#[tokio::test]
async fn test_does_not_probe_a_server_that_is_not_there_or_not_https() {
    let app = App::new().await;
    let mut admin = Web::new(&app, "root", "admin").await;
    // Nobody listens: no certificate to show.
    let dead = {
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        l.local_addr().unwrap().port()
    };
    let res = admin
        .post(
            "/settings/services/test",
            &form(&format!("https://127.0.0.1:{dead}/"), "", SECRET),
        )
        .await;
    assert!(res.body.contains("data-test-status=\"bad\""));
    assert!(!res.body.contains("data-use-fingerprint"));
    // A pin on a plain-http address is refused before anything is tried.
    let res = admin
        .post(
            "/settings/services/test",
            &form("http://127.0.0.1:9/", &"ab".repeat(32), SECRET),
        )
        .await;
    assert!(
        res.body.contains("data-test-status=\"unknown\""),
        "{}",
        res.body
    );
    assert!(!res.body.contains("data-use-fingerprint"));
}

#[tokio::test]
async fn test_never_probes_the_metadata_address() {
    let app = App::new().await;
    let mut admin = Web::new(&app, "root", "admin").await;
    let res = admin
        .post(
            "/settings/services/test",
            &form("https://169.254.169.254/", "", SECRET),
        )
        .await;
    assert!(
        res.body.contains("data-test-status=\"bad\""),
        "{}",
        res.body
    );
    assert!(!res.body.contains("data-use-fingerprint"));
    assert_no_secret(&res, "the refusal");
}

// ---------------------------------------------------------------------------
// Saving and showing the pin

#[tokio::test]
async fn the_pin_is_saved_normalised_and_shown_to_admins() {
    let stub = tls_stub().await;
    let app = App::new().await;
    let mut admin = Web::new(&app, "root", "admin").await;
    let res = admin
        .post(
            "/settings/services/new",
            &form(&stub.url("/ok"), &colons(&stub.fingerprint), ""),
        )
        .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    let service = store::list(&app.magpie.core).await.unwrap().remove(0);
    assert_eq!(
        service.settings.get(pin::SETTING).and_then(Json::as_str),
        Some(stub.fingerprint.as_str()),
        "stored as 64 lowercase hex characters"
    );
    let page = admin
        .get(&format!("/settings/services/{}", service.id))
        .await;
    assert_eq!(page.status, StatusCode::OK);
    assert!(
        page.body
            .contains(&format!("value=\"{}\"", stub.fingerprint)),
        "the edit form shows it: {}",
        page.body
    );
    assert!(page.body.contains("Certificate fingerprint (SHA-256)"));
    // The new form has the field too, empty.
    let blank = admin.get("/settings/services/new?kind=http").await;
    assert!(blank.body.contains("name=\"cert_sha256\""));
    assert!(blank.body.contains("self-signed"), "the help text");
}

#[test]
fn the_field_has_both_languages() {
    let en = include_str!("../locales/en.toml");
    let nb = include_str!("../locales/nb.toml");
    assert!(en.contains("Certificate fingerprint (SHA-256)"));
    assert!(nb.contains("Sertifikat-fingeravtrykk (SHA-256)"));
    for key in ["pin_help", "pin_presented", "pin_use"] {
        assert!(en.contains(&format!("{key} = ")), "en {key}");
        assert!(nb.contains(&format!("{key} = ")), "nb {key}");
    }
    for key in ["fingerprint", "fingerprint_https"] {
        assert!(en.contains(&format!("{key} = ")), "en {key}");
        assert!(nb.contains(&format!("{key} = ")), "nb {key}");
    }
}

#[tokio::test]
async fn a_bad_pin_is_refused_with_a_message_and_nothing_is_stored() {
    let app = App::new().await;
    let mut admin = Web::new(&app, "root", "admin").await;
    for bad in ["not-a-fingerprint", &"ab".repeat(31)] {
        let res = admin
            .post(
                "/settings/services/new",
                &form("https://10.0.0.5/", bad, ""),
            )
            .await;
        assert_eq!(res.status, StatusCode::UNPROCESSABLE_ENTITY, "{bad}");
        assert!(
            res.body.contains("64 hexadecimal characters"),
            "{}",
            res.body
        );
    }
    assert_eq!(app.count("SELECT COUNT(*) FROM services").await, 0);
}

#[tokio::test]
async fn a_pin_on_an_http_address_is_refused() {
    let app = App::new().await;
    let mut admin = Web::new(&app, "root", "admin").await;
    let res = admin
        .post(
            "/settings/services/new",
            &form("http://10.0.0.5/", &"ab".repeat(32), ""),
        )
        .await;
    assert_eq!(res.status, StatusCode::UNPROCESSABLE_ENTITY, "{}", res.body);
    assert!(
        res.body.contains("only works with an https address"),
        "{}",
        res.body
    );
    assert_eq!(app.count("SELECT COUNT(*) FROM services").await, 0);
    // The store refuses it too, whoever calls it.
    let err = store::create(
        &app.magpie.core,
        &input(
            "http://10.0.0.5/",
            Some(&"ab".repeat(32)),
            SecretChange::Keep,
        ),
    )
    .await
    .unwrap_err();
    match err {
        store::StoreError::Invalid(errors) => {
            assert!(
                errors.iter().any(|e| e.field == "cert_sha256"),
                "{errors:?}"
            );
        }
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn an_empty_field_means_no_pin_and_the_http_form_stays_valid() {
    let app = App::new().await;
    let mut admin = Web::new(&app, "root", "admin").await;
    let res = admin
        .post(
            "/settings/services/new",
            &form("http://10.0.0.5/", "  ", ""),
        )
        .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    let service = store::list(&app.magpie.core).await.unwrap().remove(0);
    assert!(service.settings.get(pin::SETTING).is_none());
}

#[tokio::test]
async fn a_changed_pin_keeps_the_secret_but_a_changed_address_does_not() {
    let app = App::new().await;
    let id = store::create(
        &app.magpie.core,
        &input(
            "https://10.0.0.5:8006/",
            Some(&"aa".repeat(32)),
            SecretChange::Replace(SECRET.into()),
        ),
    )
    .await
    .unwrap();
    // A new pin, the same origin, the key left alone.
    store::update(
        &app.magpie.core,
        id,
        &input(
            "https://10.0.0.5:8006/",
            Some(&"bb".repeat(32)),
            SecretChange::Keep,
        ),
    )
    .await
    .unwrap();
    assert_eq!(
        store::open_secret(&app.magpie.core, id)
            .await
            .unwrap()
            .as_deref(),
        Some(SECRET)
    );
    let service = store::get(&app.magpie.core, id).await.unwrap().unwrap();
    assert_eq!(
        service.settings.get(pin::SETTING).and_then(Json::as_str),
        Some("bb".repeat(32).as_str())
    );
    // Removing the pin is allowed too.
    store::update(
        &app.magpie.core,
        id,
        &input("https://10.0.0.5:8006/", None, SecretChange::Keep),
    )
    .await
    .unwrap();
    // Another address with the key kept is still refused, pin or not.
    let err = store::update(
        &app.magpie.core,
        id,
        &input(
            "https://10.0.0.9:8006/",
            Some(&"bb".repeat(32)),
            SecretChange::Keep,
        ),
    )
    .await
    .unwrap_err();
    match err {
        store::StoreError::Invalid(errors) => {
            assert!(errors.iter().any(|e| e.field == "secret"), "{errors:?}");
        }
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn the_store_builds_the_pinned_client_into_the_fetch_context() {
    let stub = tls_stub().await;
    let app = App::new().await;
    let pinned = store::create(
        &app.magpie.core,
        &input(
            &stub.url("/ok"),
            Some(&stub.fingerprint),
            SecretChange::Keep,
        ),
    )
    .await
    .unwrap();
    let plain = store::create(
        &app.magpie.core,
        &input(&stub.url("/ok"), None, SecretChange::Keep),
    )
    .await
    .unwrap();
    for (id, works) in [(pinned, true), (plain, false)] {
        let service = store::get(&app.magpie.core, id).await.unwrap().unwrap();
        let ctx = store::fetch_ctx(&app.magpie.core, &service).await.unwrap();
        let snapshot = http_kind().fetch(&ctx).await.unwrap();
        assert_eq!(snapshot.status == Status::Ok, works, "{:?}", snapshot.error);
    }
}

#[tokio::test]
async fn the_poller_fetches_a_pinned_service_with_its_pin() {
    let stub = tls_stub().await;
    let app = App::new().await;
    let id = store::create(
        &app.magpie.core,
        &input(
            &stub.url("/ok"),
            Some(&stub.fingerprint),
            SecretChange::Keep,
        ),
    )
    .await
    .unwrap();
    // The poller is handed the shared client, as the server does; the pin
    // still wins for this service.
    let poller = Poller::start_with(
        app.magpie.core.clone(),
        integrations::client().clone(),
        &app.magpie.service_events,
        Options {
            kinds: integrations::find,
            spread: Duration::ZERO,
        },
    );
    let mut got = None;
    for _ in 0..200 {
        if let Some(snapshot) = poller.snapshot(id) {
            got = Some(snapshot);
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let snapshot = got.expect("the poller made a reading");
    assert_eq!(snapshot.status, Status::Ok, "{:?}", snapshot.error);
}
