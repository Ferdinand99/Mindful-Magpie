//! The "Koble til Magpie" flow of the browser extension: the consent page
//! (`GET`/`POST /connect/extension`), the code-for-token exchange
//! (`POST /api/v1/extension/token`, PKCE) and the API reference.

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use common::{App, HOST, PASSWORD, Res, Web, bearer_get, post_json};
use serde_json::json;
use sha2::{Digest as _, Sha256};

const HASH: &str = "0123456789abcdef0123456789abcdef01234567";
const REDIRECT: &str = "https://0123456789abcdef0123456789abcdef01234567.extensions.allizom.org/";
/// The test vector of RFC 7636, appendix B.
const VERIFIER: &str = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
const CHALLENGE: &str = "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM";
const STATE: &str = "c3RhdGUtZnJvbS10aGUtZXh0ZW5zaW9u";
const DEVICE: &str = "Firefox på Windows";

fn enc(value: &str) -> String {
    url::form_urlencoded::byte_serialize(value.as_bytes()).collect()
}

fn query(redirect: &str, state: &str, challenge: &str, method: &str, device: &str) -> String {
    format!(
        "redirect_uri={}&state={}&code_challenge={}&code_challenge_method={}&device={}",
        enc(redirect),
        enc(state),
        enc(challenge),
        enc(method),
        enc(device)
    )
}

fn good_query() -> String {
    query(REDIRECT, STATE, CHALLENGE, "S256", DEVICE)
}

fn good_path() -> String {
    format!("/connect/extension?{}", good_query())
}

fn good_form(decision: &'static str) -> Vec<(&'static str, &'static str)> {
    vec![
        ("redirect_uri", REDIRECT),
        ("state", STATE),
        ("code_challenge", CHALLENGE),
        ("code_challenge_method", "S256"),
        ("device", DEVICE),
        ("decision", decision),
    ]
}

/// The consent page as a browser with `Accept-Language: lang`.
async fn consent(web: &mut Web<'_>, path: &str, lang: &str) -> Res {
    let req = Request::get(path)
        .header("accept-language", lang)
        .body(Body::empty())
        .unwrap();
    web.send(req).await
}

fn assert_refused(res: &Res, what: &str) {
    assert_eq!(res.status, StatusCode::BAD_REQUEST, "{what}: {}", res.body);
    assert_eq!(res.header("location"), "", "{what}: redirected");
}

/// The one-time code from the redirect of an approval.
fn code_of(res: &Res) -> String {
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    let location = res.header("location");
    let url = url::Url::parse(location).unwrap();
    assert!(location.starts_with(REDIRECT), "{location}");
    url.query_pairs()
        .find(|(k, _)| k == "code")
        .unwrap_or_else(|| panic!("no code in {location}"))
        .1
        .into_owned()
}

/// Approves as the signed-in user and returns the code.
async fn approve(web: &mut Web<'_>) -> String {
    let res = web.post("/connect/extension", &good_form("approve")).await;
    code_of(&res)
}

async fn exchange(app: &App, code: &str, verifier: &str) -> Res {
    post_json(
        app,
        "/api/v1/extension/token",
        &json!({ "code": code, "code_verifier": verifier }).to_string(),
    )
    .await
}

fn invalid_grant(res: &Res) {
    assert_eq!(res.status, StatusCode::BAD_REQUEST, "{}", res.body);
    assert_eq!(res.json(), json!({ "error": "invalid_grant" }));
}

fn sha256_hex(text: &str) -> String {
    Sha256::digest(text.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

// ---- connect_page ----------------------------------------------------------

#[tokio::test]
async fn connect_page_needs_sign_in_and_comes_back_after_it() {
    let app = App::new().await;
    common::add_user(&app, "ada", "admin").await;

    let res = app.get(&good_path()).await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    let login = res.header("location").to_string();
    assert!(login.starts_with("/login?next="), "{login}");
    let url = url::Url::parse(&format!("http://{HOST}{login}")).unwrap();
    let next = url
        .query_pairs()
        .find(|(k, _)| k == "next")
        .unwrap()
        .1
        .into_owned();
    assert_eq!(next, good_path());

    // Signing in with that `next` lands on the consent page again.
    let mut web = Web::anonymous(&app);
    let res = web
        .post(
            "/login",
            &[("username", "ada"), ("password", PASSWORD), ("next", &next)],
        )
        .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    assert_eq!(res.header("location"), good_path());
    let res = web.get(&good_path()).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
}

#[tokio::test]
async fn consent_page_asks_in_the_users_language() {
    let app = App::new().await;
    let mut web = Web::new(&app, "ada", "admin").await;

    let res = consent(&mut web, &good_path(), "nb").await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
    assert!(
        res.body
            .contains("Gi Firefox på Windows tilgang til å lese startsiden din?"),
        "{}",
        res.body
    );
    assert!(res.body.contains("Godkjenn"), "{}", res.body);
    assert!(res.body.contains("Avbryt"), "{}", res.body);
    assert_eq!(res.header("cache-control"), "no-store");

    let res = consent(&mut web, &good_path(), "en").await;
    assert!(
        res.body
            .contains("Give Firefox på Windows access to read your start page?"),
        "{}",
        res.body
    );
    assert!(res.body.contains("Approve"), "{}", res.body);
    assert!(res.body.contains("Cancel"), "{}", res.body);

    // The request goes back in hidden fields, and the redirect is a plain
    // (not boosted) form post.
    for (name, value) in [
        ("redirect_uri", REDIRECT),
        ("state", STATE),
        ("code_challenge", CHALLENGE),
        ("code_challenge_method", "S256"),
    ] {
        let field = format!(r#"<input type="hidden" name="{name}" value="{value}">"#);
        assert!(res.body.contains(&field), "{field} in {}", res.body);
    }
    assert!(res.body.contains(r#"hx-boost="false""#), "{}", res.body);
    // The form may send the browser on to the extension's redirect address.
    let csp = res.header("content-security-policy");
    let origin = REDIRECT.trim_end_matches('/');
    assert!(
        csp.contains(&format!("form-action 'self' {origin}")),
        "{csp}"
    );
}

#[tokio::test]
async fn consent_page_refuses_a_foreign_redirect_without_redirecting() {
    let app = App::new().await;
    let mut web = Web::new(&app, "ada", "admin").await;
    let ext = format!("{HASH}.extensions.allizom.org");
    let upper = format!("https://{}.extensions.allizom.org/", HASH.to_uppercase());
    let short = format!("https://{}.extensions.allizom.org/", &HASH[1..]);
    let long = format!("https://a{HASH}.extensions.allizom.org/");
    let variants = [
        "https://evil.com/".to_string(),
        "https://x.extensions.allizom.org.evil.com/".to_string(),
        format!("https://{ext}.evil.com/"),
        format!("http://{ext}/"),
        format!("https://{ext}/callback"),
        format!("https://{ext}"),
        format!("https://{ext}//"),
        format!("https://{ext}/?next=https://evil.com/"),
        format!("https://{ext}/#x"),
        format!("https://user@{ext}/"),
        format!("https://user:pass@{ext}/"),
        format!("https://evil.com@{ext}/"),
        format!("https://{ext}:8443/"),
        format!("https://{ext}:443/"),
        format!("https://{ext}@evil.com/"),
        format!("https://sub.{ext}/"),
        format!("https://{HASH}.extensions.allizom.org./"),
        format!("https://{HASH}.allizom.org/"),
        format!("https://{HASH}.extensions.mozilla.org/"),
        format!("ftp://{ext}/"),
        format!("//{ext}/"),
        format!(" https://{ext}/"),
        format!("https://{ext}/ "),
        "javascript:alert(1)".to_string(),
        "https://xyz.extensions.allizom.org/".to_string(),
        upper,
        short,
        long,
        String::new(),
    ];
    for redirect in &variants {
        let q = query(redirect, STATE, CHALLENGE, "S256", DEVICE);
        let res = web.get(&format!("/connect/extension?{q}")).await;
        assert_refused(&res, redirect);
        assert!(!res.body.contains("evil.com"), "{redirect}: echoed");
    }
    // No query at all, and no redirect_uri.
    let res = web.get("/connect/extension").await;
    assert_refused(&res, "no query");
    let q = format!(
        "state={STATE}&code_challenge={CHALLENGE}&code_challenge_method=S256&device=Firefox"
    );
    let res = web.get(&format!("/connect/extension?{q}")).await;
    assert_refused(&res, "no redirect_uri");
    // A repeated redirect_uri does not smuggle a second one in.
    let res = web
        .get(&format!(
            "/connect/extension?{}&redirect_uri={}",
            good_query(),
            enc("https://evil.com/")
        ))
        .await;
    assert_eq!(res.header("location"), "");
    assert!(!res.body.contains("evil.com"), "{}", res.body);
}

#[tokio::test]
async fn consent_page_refuses_bad_parameters() {
    let app = App::new().await;
    let mut web = Web::new(&app, "ada", "admin").await;
    let long_state = "a".repeat(129);
    let long_device = "d".repeat(61);
    let plus = CHALLENGE.replace('-', "+");
    let longer = format!("{CHALLENGE}A");
    let padded = format!("{}=", &CHALLENGE[..42]);
    let cases: Vec<(&str, String)> = vec![
        (
            "challenge too short",
            query(REDIRECT, STATE, &CHALLENGE[..42], "S256", DEVICE),
        ),
        (
            "challenge too long",
            query(REDIRECT, STATE, &longer, "S256", DEVICE),
        ),
        (
            "challenge with +",
            query(REDIRECT, STATE, &plus, "S256", DEVICE),
        ),
        (
            "challenge with padding",
            query(REDIRECT, STATE, &padded, "S256", DEVICE),
        ),
        (
            "empty challenge",
            query(REDIRECT, STATE, "", "S256", DEVICE),
        ),
        (
            "method plain",
            query(REDIRECT, STATE, CHALLENGE, "plain", DEVICE),
        ),
        (
            "method lowercase",
            query(REDIRECT, STATE, CHALLENGE, "s256", DEVICE),
        ),
        ("no method", query(REDIRECT, STATE, CHALLENGE, "", DEVICE)),
        (
            "empty state",
            query(REDIRECT, "", CHALLENGE, "S256", DEVICE),
        ),
        (
            "state too long",
            query(REDIRECT, &long_state, CHALLENGE, "S256", DEVICE),
        ),
        (
            "state with <",
            query(REDIRECT, "a<b", CHALLENGE, "S256", DEVICE),
        ),
        (
            "state with space",
            query(REDIRECT, "a b", CHALLENGE, "S256", DEVICE),
        ),
        (
            "state with &",
            query(REDIRECT, "a&b=c", CHALLENGE, "S256", DEVICE),
        ),
        (
            "empty device",
            query(REDIRECT, STATE, CHALLENGE, "S256", ""),
        ),
        (
            "blank device",
            query(REDIRECT, STATE, CHALLENGE, "S256", "   "),
        ),
        (
            "device too long",
            query(REDIRECT, STATE, CHALLENGE, "S256", &long_device),
        ),
        (
            "device with newline",
            query(REDIRECT, STATE, CHALLENGE, "S256", "a\nb"),
        ),
    ];
    for (what, q) in cases {
        let res = web.get(&format!("/connect/extension?{q}")).await;
        assert_refused(&res, what);
    }
    // The limits themselves are fine.
    let ok = query(
        REDIRECT,
        &"a".repeat(128),
        CHALLENGE,
        "S256",
        &"d".repeat(60),
    );
    let res = web.get(&format!("/connect/extension?{ok}")).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
}

#[tokio::test]
async fn consent_page_escapes_the_device() {
    let app = App::new().await;
    let mut web = Web::new(&app, "ada", "admin").await;
    let device = "<script>alert(1)</script>";
    let q = query(REDIRECT, STATE, CHALLENGE, "S256", device);
    let res = web.get(&format!("/connect/extension?{q}")).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
    assert!(!res.body.contains("<script>alert(1)"), "{}", res.body);
    assert!(!res.body.contains("</script>alert"), "{}", res.body);
    assert!(res.body.contains("alert(1)"), "the device is shown as text");
}

// ---- connect_post ----------------------------------------------------------

#[tokio::test]
async fn approving_redirects_with_a_code_stored_hashed() {
    let app = App::new().await;
    let mut web = Web::new(&app, "ada", "admin").await;
    let uid = common::user_id(&app, "ada").await;

    let res = web.post("/connect/extension", &good_form("approve")).await;
    let code = code_of(&res);
    assert_eq!(
        res.header("location"),
        format!("{REDIRECT}?code={code}&state={STATE}")
    );
    assert_eq!(code.len(), 43, "32 bytes, base64url without padding");
    assert!(
        code.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'),
        "{code}"
    );

    assert_eq!(app.count("SELECT COUNT(*) FROM extension_codes").await, 1);
    type Row = (String, i64, String, String, String, i64);
    let (stored, user, challenge, device, redirect, ttl): Row = app
        .magpie
        .core
        .with_conn(|c| {
            c.query_row(
                "SELECT code_hash, user_id, code_challenge, device, redirect_uri,
                        expires_at - CAST(strftime('%s','now') AS INTEGER)
                 FROM extension_codes",
                [],
                |r| {
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        r.get(2)?,
                        r.get(3)?,
                        r.get(4)?,
                        r.get(5)?,
                    ))
                },
            )
        })
        .await
        .unwrap();
    assert_ne!(stored, code, "the code is never stored as it is");
    assert_eq!(stored, sha256_hex(&code));
    assert_eq!(user, uid);
    assert_eq!(challenge, CHALLENGE);
    assert_eq!(device, DEVICE);
    assert_eq!(redirect, REDIRECT);
    assert!(
        (118..=120).contains(&ttl),
        "valid for two minutes, got {ttl} s"
    );

    // A second approval is a second code.
    let second = approve(&mut web).await;
    assert_ne!(second, code);
}

#[tokio::test]
async fn cancelling_redirects_with_access_denied() {
    let app = App::new().await;
    let mut web = Web::new(&app, "ada", "admin").await;
    let res = web.post("/connect/extension", &good_form("deny")).await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    assert_eq!(
        res.header("location"),
        format!("{REDIRECT}?error=access_denied&state={STATE}")
    );
    assert_eq!(app.count("SELECT COUNT(*) FROM extension_codes").await, 0);
}

#[tokio::test]
async fn posting_without_csrf_proof_is_refused() {
    let app = App::new().await;
    let mut web = Web::new(&app, "ada", "admin").await;
    // A form post with no Origin, Referer or HX-Request, as a cross-site
    // page could send it with the user's cookie.
    let body = url::form_urlencoded::Serializer::new(String::new())
        .extend_pairs(good_form("approve"))
        .finish();
    let req = Request::post("/connect/extension")
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .body(Body::from(body.clone()))
        .unwrap();
    let res = web.send(req).await;
    assert_eq!(res.status, StatusCode::FORBIDDEN, "{}", res.body);
    // And from another origin.
    let req = Request::post("/connect/extension")
        .header(header::ORIGIN, "https://evil.com")
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .body(Body::from(body))
        .unwrap();
    let res = web.send(req).await;
    assert_eq!(res.status, StatusCode::FORBIDDEN, "{}", res.body);
    assert_eq!(app.count("SELECT COUNT(*) FROM extension_codes").await, 0);
}

#[tokio::test]
async fn posting_signed_out_is_refused() {
    let app = App::new().await;
    common::add_user(&app, "ada", "admin").await;
    let mut web = Web::anonymous(&app);
    let res = web.post("/connect/extension", &good_form("approve")).await;
    // Sent to sign in, or refused: never on to the extension.
    assert!(!res.header("location").contains("allizom"), "{}", res.body);
    assert!(!res.header("location").contains("code="), "{}", res.body);
    assert_eq!(app.count("SELECT COUNT(*) FROM extension_codes").await, 0);
}

#[tokio::test]
async fn posting_revalidates_everything() {
    let app = App::new().await;
    let mut web = Web::new(&app, "ada", "admin").await;
    for (field, bad) in [
        ("redirect_uri", "https://evil.com/"),
        ("redirect_uri", "https://x.extensions.allizom.org.evil.com/"),
        ("state", "a b"),
        ("code_challenge", "short"),
        ("code_challenge_method", "plain"),
        ("device", ""),
        ("decision", "maybe"),
        ("decision", ""),
    ] {
        let form: Vec<_> = good_form("approve")
            .into_iter()
            .map(|(k, v)| if k == field { (k, bad) } else { (k, v) })
            .collect();
        let res = web.post("/connect/extension", &form).await;
        assert_refused(&res, &format!("{field}={bad}"));
        // Cancelling a bad request does not redirect either.
        let form: Vec<_> = form
            .into_iter()
            .map(|(k, v)| {
                if k == "decision" && field != "decision" {
                    (k, "deny")
                } else {
                    (k, v)
                }
            })
            .collect();
        let res = web.post("/connect/extension", &form).await;
        assert_refused(&res, &format!("deny with {field}={bad}"));
    }
    assert_eq!(app.count("SELECT COUNT(*) FROM extension_codes").await, 0);
}

// ---- token_exchange --------------------------------------------------------

#[tokio::test]
async fn the_whole_flow_gives_a_read_token_that_works() {
    let app = App::new().await;
    let mut web = Web::new(&app, "ada", "admin").await;
    let code = approve(&mut web).await;

    // No cookies, no auth: the extension calls this on its own.
    let res = exchange(&app, &code, VERIFIER).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
    assert!(res.header("cache-control").contains("no-store"));
    let token = res.json()["token"].as_str().unwrap().to_string();
    assert!(token.starts_with("mm_"), "{token}");
    assert!(vole_kit::auth::tokens::is_well_formed(&token));

    // The code is spent.
    assert_eq!(app.count("SELECT COUNT(*) FROM extension_codes").await, 0);

    // One token, read scope, named after the device, ada's.
    assert_eq!(app.count("SELECT COUNT(*) FROM api_tokens").await, 1);
    let (name, scope, owner): (String, String, i64) = app
        .magpie
        .core
        .with_conn(|c| {
            c.query_row("SELECT name, scope, user_id FROM api_tokens", [], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?))
            })
        })
        .await
        .unwrap();
    assert_eq!(name, format!("Utvidelse: {DEVICE}"));
    assert_eq!(scope, "read");
    assert_eq!(owner, common::user_id(&app, "ada").await);

    // It reads the layout...
    let res = bearer_get(&app, "/api/v1/me/start", &token).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
    // ...and is listed, by name, where it can be revoked.
    let page = web.get("/settings/tokens").await;
    assert_eq!(page.status, StatusCode::OK, "{}", page.body);
    assert!(
        page.body.contains(&format!("Utvidelse: {DEVICE}")),
        "{}",
        page.body
    );
    let id: i64 = app
        .magpie
        .core
        .with_conn(|c| c.query_row("SELECT id FROM api_tokens", [], |r| r.get(0)))
        .await
        .unwrap();
    let res = web.delete(&format!("/settings/tokens/{id}")).await;
    assert!(res.status.is_success(), "{} {}", res.status, res.body);
    let res = bearer_get(&app, "/api/v1/me/start", &token).await;
    assert_eq!(res.status, StatusCode::UNAUTHORIZED, "{}", res.body);
}

#[tokio::test]
async fn a_wrong_verifier_is_refused_and_burns_the_code() {
    let app = App::new().await;
    let mut web = Web::new(&app, "ada", "admin").await;
    let code = approve(&mut web).await;

    let wrong = "A".repeat(43);
    invalid_grant(&exchange(&app, &code, &wrong).await);
    assert_eq!(app.count("SELECT COUNT(*) FROM api_tokens").await, 0);
    // The right verifier no longer helps: whoever has the code gets one try.
    invalid_grant(&exchange(&app, &code, VERIFIER).await);
    assert_eq!(app.count("SELECT COUNT(*) FROM api_tokens").await, 0);
}

#[tokio::test]
async fn a_code_works_once() {
    let app = App::new().await;
    let mut web = Web::new(&app, "ada", "admin").await;
    let code = approve(&mut web).await;
    assert_eq!(exchange(&app, &code, VERIFIER).await.status, StatusCode::OK);
    invalid_grant(&exchange(&app, &code, VERIFIER).await);
    assert_eq!(app.count("SELECT COUNT(*) FROM api_tokens").await, 1);
}

#[tokio::test]
async fn an_expired_code_is_refused() {
    let app = App::new().await;
    let mut web = Web::new(&app, "ada", "admin").await;
    let code = approve(&mut web).await;
    app.exec(
        "UPDATE extension_codes SET expires_at = CAST(strftime('%s','now') AS INTEGER) - 1".into(),
    )
    .await;
    invalid_grant(&exchange(&app, &code, VERIFIER).await);
    assert_eq!(app.count("SELECT COUNT(*) FROM api_tokens").await, 0);
}

#[tokio::test]
async fn expired_codes_are_cleaned_up_when_a_new_one_is_made() {
    let app = App::new().await;
    let mut web = Web::new(&app, "ada", "admin").await;
    approve(&mut web).await;
    app.exec("UPDATE extension_codes SET expires_at = 1".into())
        .await;
    approve(&mut web).await;
    assert_eq!(app.count("SELECT COUNT(*) FROM extension_codes").await, 1);
}

#[tokio::test]
async fn unknown_and_malformed_requests_are_the_same_refusal() {
    let app = App::new().await;
    let mut web = Web::new(&app, "ada", "admin").await;
    let code = approve(&mut web).await;

    let unknown = "B".repeat(43);
    let ok = |c: &str, v: &str| json!({ "code": c, "code_verifier": v }).to_string();
    let bodies = vec![
        ok(&unknown, VERIFIER),
        ok("", VERIFIER),
        ok(&code, ""),
        ok(&code, "short"),
        ok(&code, &"v".repeat(129)),
        ok(&"c".repeat(10_000), VERIFIER),
        json!({ "code": code }).to_string(),
        json!({ "code_verifier": VERIFIER }).to_string(),
        json!({ "code": 1, "code_verifier": 2 }).to_string(),
        json!({ "code": null, "code_verifier": null }).to_string(),
        json!([]).to_string(),
        "not json".to_string(),
        String::new(),
    ];
    let mut seen = Vec::new();
    let peer: std::net::IpAddr = "127.0.0.1".parse().unwrap();
    for body in bodies {
        // Every body is a fresh request: don't run into the rate limit.
        app.magpie.token_budget.reset(&peer);
        let res = post_json(&app, "/api/v1/extension/token", &body).await;
        invalid_grant(&res);
        seen.push(res.body);
    }
    assert!(seen.windows(2).all(|w| w[0] == w[1]), "{seen:?}");
    assert_eq!(app.count("SELECT COUNT(*) FROM api_tokens").await, 0);
}

#[tokio::test]
async fn a_form_post_does_not_reach_the_exchange() {
    let app = App::new().await;
    let mut web = Web::new(&app, "ada", "admin").await;
    let code = approve(&mut web).await;
    // The exchange wants JSON; the same fields as a form are refused.
    let body = format!("code={code}&code_verifier={VERIFIER}");
    let req = Request::post("/api/v1/extension/token")
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .body(Body::from(body))
        .unwrap();
    let res = app.send(req).await;
    invalid_grant(&res);
    assert_eq!(app.count("SELECT COUNT(*) FROM api_tokens").await, 0);
}

#[tokio::test]
async fn the_exchange_is_rate_limited() {
    let app = App::new().await;
    common::add_user(&app, "ada", "admin").await;
    let unknown = "B".repeat(43);
    let mut limited = None;
    for i in 0..40 {
        let res = exchange(&app, &unknown, VERIFIER).await;
        if res.status == StatusCode::TOO_MANY_REQUESTS {
            limited = Some(i);
            assert!(!res.header("retry-after").is_empty());
            break;
        }
        invalid_grant(&res);
    }
    let at = limited.expect("never rate limited");
    assert!(at >= 5, "limited too early: after {at} requests");
}

// ---- docs ------------------------------------------------------------------

#[tokio::test]
async fn the_api_reference_lists_both_routes() {
    let app = App::new().await;
    let mut web = Web::new(&app, "ada", "admin").await;
    for path in ["/api-docs", "/api-docs.md", "/api/v1/openapi.json"] {
        let res = web.get(path).await;
        assert_eq!(res.status, StatusCode::OK, "{path}: {}", res.body);
        assert!(res.body.contains("/api/v1/me/start"), "{path}");
        assert!(res.body.contains("/api/v1/extension/token"), "{path}");
    }
}
