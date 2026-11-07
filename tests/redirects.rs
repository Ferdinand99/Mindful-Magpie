//! A request that carries a secret never follows a redirect to another
//! origin, whatever the header is called. The HTTP client drops
//! `Authorization` itself on such a redirect, but not `x-api-key` or
//! anything else, so every request the API helpers send with a
//! [`Credential`] refuses it, and so does the `http` kind when it has a
//! secret. A request without a secret keeps the ordinary rules.

mod common;

use common::stub::{self, Answer, Stub};
use mindful_magpie::integrations::api::{self, Credential, Reply};
use mindful_magpie::integrations::{self, FetchCtx, FetchError, Integration, Status};
use serde_json::{Value as Json, json};
use url::Url;

const SECRET: &str = "redirect-S3cret-4f2a9c";

/// A target that answers `{"ok": true}` at `/target`, a server that
/// redirects every request to it (in each of the three ways), and the
/// address of that server.
async fn redirecting(
    redirect: fn(&str) -> axum::response::Redirect,
) -> (String, Stub) {
    let (target, stub) = stub::serve_one("/target", Answer::Json(json!({ "ok": true }))).await;
    let origin = stub::redirector(redirect, format!("{target}/target")).await;
    (origin, stub)
}

fn url(origin: &str) -> Url {
    Url::parse(&format!("{origin}/start")).unwrap()
}

fn client() -> &'static reqwest::Client {
    integrations::client()
}

/// Asserts that the other origin saw nothing.
fn assert_untouched(stub: &Stub, what: &str) {
    assert!(stub.seen().is_empty(), "{what}: {:?}", stub.seen());
}

#[tokio::test]
async fn a_bearer_token_stays_with_its_origin() {
    for redirect in stub::redirects() {
        let (origin, target) = redirecting(redirect).await;
        let credential = Credential::bearer(Some(SECRET)).unwrap();
        let err = api::get_json::<Json>(client(), url(&origin), &credential)
            .await
            .unwrap_err();
        assert_eq!(err, FetchError::RedirectScheme);
        assert_untouched(&target, "bearer");
    }
}

#[tokio::test]
async fn a_proxmox_token_stays_with_its_origin() {
    for redirect in stub::redirects() {
        let (origin, target) = redirecting(redirect).await;
        let credential =
            Credential::raw("authorization", &format!("PVEAPIToken=u@pam!t={SECRET}")).unwrap();
        let err = api::get_json::<Json>(client(), url(&origin), &credential)
            .await
            .unwrap_err();
        assert_eq!(err, FetchError::RedirectScheme);
        assert_untouched(&target, "PVEAPIToken");
    }
}

#[tokio::test]
async fn an_api_key_header_stays_with_its_origin() {
    // `x-api-key` is the header the client would carry along, GET or POST.
    for redirect in stub::redirects() {
        let (origin, target) = redirecting(redirect).await;
        let credential = Credential::header("x-api-key", Some(SECRET)).unwrap();
        let err = api::get_json::<Json>(client(), url(&origin), &credential)
            .await
            .unwrap_err();
        assert_eq!(err, FetchError::RedirectScheme);
        assert_untouched(&target, "x-api-key GET");

        let (origin, target) = redirecting(redirect).await;
        let err = api::post_json::<Json>(client(), url(&origin), &credential, &json!({}))
            .await
            .unwrap_err();
        assert_eq!(err, FetchError::RedirectScheme);
        assert_untouched(&target, "x-api-key POST");
    }
}

#[tokio::test]
async fn the_admin_get_refuses_it_too() {
    let (origin, target) = redirecting(axum::response::Redirect::temporary).await;
    let credential = Credential::bearer(Some(SECRET)).unwrap();
    let err = api::get_json_admin::<Json>(client(), url(&origin), &credential)
        .await
        .unwrap_err();
    assert_eq!(err, FetchError::RedirectScheme);
    assert_untouched(&target, "admin get");
}

#[tokio::test]
async fn a_request_without_a_secret_follows_the_redirect() {
    for redirect in stub::redirects() {
        let (origin, target) = redirecting(redirect).await;
        let reply = api::get_json_public::<Json>(client(), url(&origin))
            .await
            .unwrap();
        assert_eq!(reply, Reply::Found(json!({ "ok": true })));
        let seen = target.seen();
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].authorization, None);
        assert_eq!(seen[0].api_key, None);
    }
}

#[tokio::test]
async fn a_redirect_within_the_origin_keeps_the_credential() {
    let (base, stub) = stub::serve().await;
    stub.route("/start", Answer::Moved("/target"));
    stub.route("/target", Answer::Json(json!({ "ok": true })));
    let url = Url::parse(&format!("{base}/start")).unwrap();
    let credential = Credential::header("x-api-key", Some(SECRET)).unwrap();
    let reply = api::get_json::<Json>(client(), url, &credential)
        .await
        .unwrap();
    assert_eq!(reply, Reply::Found(json!({ "ok": true })));
    assert_eq!(
        stub.hits_on("/target")[0].api_key.as_deref(),
        Some(SECRET),
        "the same origin may have it"
    );
}

fn http_kind() -> &'static dyn Integration {
    integrations::find("http").expect("the http kind is registered")
}

fn http_ctx(origin: &str, secret: Option<&str>) -> FetchCtx {
    FetchCtx {
        http: client().clone(),
        settings: json!({ "url": format!("{origin}/start") }),
        secret: secret.map(str::to_string),
    }
}

#[tokio::test]
async fn the_http_kind_with_a_secret_stays_with_its_origin() {
    for redirect in stub::redirects() {
        let (origin, target) = redirecting(redirect).await;
        let snapshot = http_kind()
            .fetch(&http_ctx(&origin, Some(SECRET)))
            .await
            .unwrap();
        assert_eq!(snapshot.status, Status::Bad);
        assert_eq!(snapshot.error.as_deref(), Some("redirect not allowed"));
        assert_untouched(&target, "http kind");
    }
}

#[tokio::test]
async fn the_http_kind_without_a_secret_still_follows_a_redirect() {
    for redirect in stub::redirects() {
        let (origin, target) = redirecting(redirect).await;
        for secret in [None, Some("")] {
            target.forget_seen();
            let snapshot = http_kind()
                .fetch(&http_ctx(&origin, secret))
                .await
                .unwrap();
            assert_eq!(snapshot.status, Status::Ok, "{:?}", snapshot.error);
            let seen = target.seen();
            assert_eq!(seen.len(), 1);
            assert_eq!(seen[0].authorization, None);
        }
    }
}
