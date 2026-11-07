//! The "Koble til Magpie" flow of the browser extension: the consent page
//! (`GET`/`POST /connect/extension`), the code-for-token exchange
//! (`POST /api/v1/extension/token`, PKCE) and the API reference.

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use common::{App, HOST, PASSWORD, Res, Web, bearer_get, post_json};
use serde_json::json;
use sha2::{Digest as _, Sha256};

/// SHA-1 of the official extension's gecko id, mindful-magpie@hedgerow.tools
/// (what `identity.getRedirectURL()` uses as the host).
const HASH: &str = "9bd6a096502a3a8555d679b3f64704219dffb501";
const REDIRECT: &str = "https://9bd6a096502a3a8555d679b3f64704219dffb501.extensions.allizom.org/";
/// Another extension's redirect host (not the official one).
const OTHER_HASH: &str = "0123456789abcdef0123456789abcdef01234567";
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
        ("current_password", PASSWORD),
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
    type Row = (String, i64, String, String, i64);
    let (stored, user, challenge, device, ttl): Row = app
        .magpie
        .core
        .with_conn(|c| {
            c.query_row(
                "SELECT code_hash, user_id, code_challenge, device,
                        expires_at - CAST(strftime('%s','now') AS INTEGER)
                 FROM extension_codes",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )
        })
        .await
        .unwrap();
    assert_ne!(stored, code, "the code is never stored as it is");
    assert_eq!(stored, sha256_hex(&code));
    assert_eq!(user, uid);
    assert_eq!(challenge, CHALLENGE);
    assert_eq!(device, DEVICE);
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

// ---- parallel use, the consent form, the token cap and who may connect ---------

fn sha1_hex(text: &str) -> String {
    use sha1::{Digest as _, Sha1};
    Sha1::digest(text.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn form_with(
    decision: &'static str,
    redirect: &'static str,
    device: &'static str,
) -> Vec<(&'static str, &'static str)> {
    good_form(decision)
        .into_iter()
        .map(|(k, v)| match k {
            "redirect_uri" => (k, redirect),
            "device" => (k, device),
            _ => (k, v),
        })
        .collect()
}

/// Approves for `device` and exchanges the code; the answer.
async fn connect_device(app: &App, web: &mut Web<'_>, device: &'static str) -> Res {
    let res = web
        .post(
            "/connect/extension",
            &form_with("approve", REDIRECT, device),
        )
        .await;
    let code = code_of(&res);
    app.magpie
        .token_budget
        .reset(&"127.0.0.1".parse::<std::net::IpAddr>().unwrap());
    exchange(app, &code, VERIFIER).await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_code_cannot_be_spent_twice_in_parallel() {
    let app = std::sync::Arc::new(App::new().await);
    let mut web = Web::new(&app, "ada", "admin").await;
    let code = approve(&mut web).await;
    app.magpie
        .token_budget
        .reset(&"127.0.0.1".parse::<std::net::IpAddr>().unwrap());

    let mut tasks = tokio::task::JoinSet::new();
    for _ in 0..8 {
        let (app, code) = (app.clone(), code.clone());
        tasks.spawn(async move { exchange(&app, &code, VERIFIER).await });
    }
    let (mut ok, mut refused) = (0, 0);
    while let Some(res) = tasks.join_next().await {
        let res = res.unwrap();
        if res.status == StatusCode::OK {
            ok += 1;
        } else {
            invalid_grant(&res);
            refused += 1;
        }
    }
    assert_eq!((ok, refused), (1, 7));
    assert_eq!(app.count("SELECT COUNT(*) FROM api_tokens").await, 1);
    assert_eq!(app.count("SELECT COUNT(*) FROM extension_codes").await, 0);
}

#[tokio::test]
async fn approving_asks_for_the_password_first() {
    let app = App::new().await;
    let mut web = Web::new(&app, "ada", "admin").await;

    // No password: the consent page again, with the reason, nothing issued.
    let mut form = good_form("approve");
    form.retain(|(k, _)| *k != "current_password");
    let res = web.post("/connect/extension", &form).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
    assert_eq!(res.header("location"), "");
    assert!(
        res.body.contains(r#"name="current_password""#),
        "{}",
        res.body
    );
    assert!(res.body.contains("callout"), "the reason is shown");
    assert!(res.body.contains(r#"name="state" value="#), "request kept");
    assert_eq!(app.count("SELECT COUNT(*) FROM extension_codes").await, 0);

    // A wrong password: the same, with the kit's wording for it.
    let mut form = good_form("approve");
    for (k, v) in &mut form {
        if *k == "current_password" {
            *v = "not-the-password";
        }
    }
    let res = web.post("/connect/extension", &form).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
    assert_eq!(res.header("location"), "");
    assert_eq!(app.count("SELECT COUNT(*) FROM extension_codes").await, 0);

    // The right one goes on to the extension.
    let res = web.post("/connect/extension", &good_form("approve")).await;
    code_of(&res);
    assert_eq!(app.count("SELECT COUNT(*) FROM extension_codes").await, 1);

    // Cancelling needs no password.
    let mut form = good_form("deny");
    form.retain(|(k, _)| *k != "current_password");
    let res = web.post("/connect/extension", &form).await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
}

#[tokio::test]
async fn password_guesses_share_the_kits_throttle() {
    let app = App::new().await;
    let mut web = Web::new(&app, "ada", "admin").await;
    let mut form = good_form("approve");
    for (k, v) in &mut form {
        if *k == "current_password" {
            *v = "wrong";
        }
    }
    let mut stopped = false;
    for _ in 0..30 {
        let res = web.post("/connect/extension", &form).await;
        assert_eq!(res.status, StatusCode::OK, "{}", res.body);
        if res.body.to_lowercase().contains("too many") {
            stopped = true;
            break;
        }
    }
    assert!(stopped, "guessing was never throttled");
    // The throttled attempts issued nothing.
    assert_eq!(app.count("SELECT COUNT(*) FROM extension_codes").await, 0);
}

#[tokio::test]
async fn the_kits_token_cap_holds() {
    let app = App::new().await;
    let mut web = Web::new(&app, "ada", "admin").await;
    let uid = common::user_id(&app, "ada").await;
    for _ in 0..50 {
        common::make_token(&app, uid, vole_kit::auth::TokenScope::Read, "script").await;
    }
    let res = connect_device(&app, &mut web, "Firefox på Windows").await;
    invalid_grant(&res);
    assert_eq!(app.count("SELECT COUNT(*) FROM api_tokens").await, 50);
    // The code is spent all the same.
    assert_eq!(app.count("SELECT COUNT(*) FROM extension_codes").await, 0);

    // One fewer, and it goes through.
    app.exec("DELETE FROM api_tokens WHERE id = (SELECT MIN(id) FROM api_tokens)".into())
        .await;
    let res = connect_device(&app, &mut web, "Firefox på Windows").await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
    assert_eq!(app.count("SELECT COUNT(*) FROM api_tokens").await, 50);
}

#[tokio::test]
async fn reconnecting_at_the_cap_replaces_instead_of_failing() {
    let app = App::new().await;
    let mut web = Web::new(&app, "ada", "admin").await;
    let uid = common::user_id(&app, "ada").await;
    let first = connect_device(&app, &mut web, "Firefox på Windows").await;
    assert_eq!(first.status, StatusCode::OK, "{}", first.body);
    for _ in 0..49 {
        common::make_token(&app, uid, vole_kit::auth::TokenScope::Read, "script").await;
    }
    assert_eq!(app.count("SELECT COUNT(*) FROM api_tokens").await, 50);
    let second = connect_device(&app, &mut web, "Firefox på Windows").await;
    assert_eq!(second.status, StatusCode::OK, "{}", second.body);
    assert_eq!(app.count("SELECT COUNT(*) FROM api_tokens").await, 50);
}

#[tokio::test]
async fn reconnecting_the_same_device_leaves_one_token() {
    let app = App::new().await;
    let mut web = Web::new(&app, "ada", "admin").await;
    let mut bob = Web::new(&app, "bob", "viewer").await;
    let name = "Utvidelse: Firefox på Windows";

    let first = connect_device(&app, &mut web, "Firefox på Windows").await;
    let first = first.json()["token"].as_str().unwrap().to_string();
    // Another device, and another user's device of the same name, stay.
    let mac = connect_device(&app, &mut web, "Firefox på macOS").await;
    let mac = mac.json()["token"].as_str().unwrap().to_string();
    let bobs = connect_device(&app, &mut bob, "Firefox på Windows").await;
    let bobs = bobs.json()["token"].as_str().unwrap().to_string();
    assert_eq!(app.count("SELECT COUNT(*) FROM api_tokens").await, 3);

    let second = connect_device(&app, &mut web, "Firefox på Windows").await;
    let second = second.json()["token"].as_str().unwrap().to_string();
    assert_ne!(first, second);
    assert_eq!(app.count("SELECT COUNT(*) FROM api_tokens").await, 3);
    let ada = common::user_id(&app, "ada").await;
    let named: i64 = app
        .magpie
        .core
        .with_conn(move |c| {
            c.query_row(
                "SELECT COUNT(*) FROM api_tokens WHERE name = ?1 AND user_id = ?2",
                rusqlite::params![name, ada],
                |r| r.get(0),
            )
        })
        .await
        .unwrap();
    assert_eq!(named, 1, "one token of that name for ada");

    let status = |token: String| {
        let app = &app;
        async move { bearer_get(app, "/api/v1/me/start", &token).await.status }
    };
    // The replacement carries the extension's `start:owner` scope, and only
    // that: one scope row for the one token of the device.
    let scopes: Vec<String> = app
        .magpie
        .core
        .with_conn(move |c| {
            let mut stmt = c.prepare(
                "SELECT s.scope FROM api_tokens t
                 JOIN api_token_scopes s ON s.token_id = t.id
                 WHERE t.name = ?1 AND t.user_id = ?2",
            )?;
            stmt.query_map(rusqlite::params![name, ada], |r| r.get(0))?
                .collect()
        })
        .await
        .unwrap();
    assert_eq!(scopes, ["start:owner"]);

    assert_eq!(status(first).await, StatusCode::UNAUTHORIZED, "replaced");
    assert_eq!(status(second).await, StatusCode::OK);
    assert_eq!(status(mac).await, StatusCode::OK);
    assert_eq!(status(bobs).await, StatusCode::OK);
}

#[tokio::test]
async fn only_the_official_extension_is_accepted_by_default() {
    let app = App::new().await;
    let mut web = Web::new(&app, "ada", "admin").await;
    let other = format!("https://{OTHER_HASH}.extensions.allizom.org/");
    let q = query(&other, STATE, CHALLENGE, "S256", DEVICE);
    let res = web.get(&format!("/connect/extension?{q}")).await;
    assert_refused(&res, "another extension");
    let form: Vec<(&str, &str)> = good_form("approve")
        .into_iter()
        .map(|(k, v)| {
            if k == "redirect_uri" {
                (k, other.as_str())
            } else {
                (k, v)
            }
        })
        .collect();
    let res = web.post("/connect/extension", &form).await;
    assert_refused(&res, "another extension, posted");
    assert_eq!(app.count("SELECT COUNT(*) FROM extension_codes").await, 0);
}

#[tokio::test]
async fn configured_extension_ids_are_accepted_too() {
    let app = App::with_extension_ids(&["dev-build@example.com", "  "]).await;
    let mut web = Web::new(&app, "ada", "admin").await;
    let dev = format!(
        "https://{}.extensions.allizom.org/",
        sha1_hex("dev-build@example.com")
    );
    let q = query(&dev, STATE, CHALLENGE, "S256", DEVICE);
    let res = web.get(&format!("/connect/extension?{q}")).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
    // The official one still works, and an unlisted one still does not.
    let res = web.get(&good_path()).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
    let other = format!("https://{OTHER_HASH}.extensions.allizom.org/");
    let q = query(&other, STATE, CHALLENGE, "S256", DEVICE);
    let res = web.get(&format!("/connect/extension?{q}")).await;
    assert_refused(&res, "unlisted");
    // The whole flow back to the configured address.
    let leaked: &'static str = Box::leak(dev.clone().into_boxed_str());
    let res = web
        .post("/connect/extension", &form_with("approve", leaked, DEVICE))
        .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    assert!(res.header("location").starts_with(&dev));
}

#[tokio::test]
async fn the_device_name_has_no_invisible_or_reversing_characters() {
    let app = App::new().await;
    let mut web = Web::new(&app, "ada", "admin").await;
    for device in [
        "Fire\u{202e}fox",
        "Fire\u{200b}fox",
        "\u{feff}Firefox",
        "Fire\u{2066}fox",
        "Fire\u{ad}fox",
        "Fire\u{200d}fox",
    ] {
        let q = query(REDIRECT, STATE, CHALLENGE, "S256", device);
        let res = web.get(&format!("/connect/extension?{q}")).await;
        assert_refused(&res, &format!("{device:?}"));
    }
}

#[tokio::test]
async fn the_consent_page_cannot_be_framed() {
    let app = App::new().await;
    let mut web = Web::new(&app, "ada", "admin").await;
    let res = web.get(&good_path()).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
    assert!(
        res.header("content-security-policy")
            .contains("frame-ancestors 'none'"),
        "{}",
        res.header("content-security-policy")
    );
    assert_eq!(res.header("x-frame-options"), "DENY");
    // Also with the password error showing.
    let mut form = good_form("approve");
    form.retain(|(k, _)| *k != "current_password");
    let res = web.post("/connect/extension", &form).await;
    assert!(
        res.header("content-security-policy")
            .contains("frame-ancestors 'none'")
    );
    assert_eq!(res.header("x-frame-options"), "DENY");
}

#[tokio::test]
async fn a_user_has_at_most_ten_codes_waiting() {
    let app = App::new().await;
    let mut web = Web::new(&app, "ada", "admin").await;
    let mut bob = Web::new(&app, "bob", "viewer").await;
    approve(&mut bob).await;
    let mut codes = Vec::new();
    for _ in 0..13 {
        codes.push(approve(&mut web).await);
    }
    let ada = common::user_id(&app, "ada").await;
    let sql = format!("SELECT COUNT(*) FROM extension_codes WHERE user_id = {ada}");
    let sql: &'static str = Box::leak(sql.into_boxed_str());
    assert_eq!(app.count(sql).await, 10);
    assert_eq!(
        app.count("SELECT COUNT(*) FROM extension_codes").await,
        11,
        "bob's code stays"
    );
    // The newest ten work, the oldest three do not.
    app.magpie
        .token_budget
        .reset(&"127.0.0.1".parse::<std::net::IpAddr>().unwrap());
    invalid_grant(&exchange(&app, &codes[0], VERIFIER).await);
    assert_eq!(
        exchange(&app, codes.last().unwrap(), VERIFIER).await.status,
        StatusCode::OK
    );
}

#[tokio::test]
async fn an_inactive_users_code_is_refused() {
    let app = App::new().await;
    let mut web = Web::new(&app, "ada", "admin").await;
    let code = approve(&mut web).await;
    app.exec("UPDATE users SET active = 0".into()).await;
    invalid_grant(&exchange(&app, &code, VERIFIER).await);
    assert_eq!(app.count("SELECT COUNT(*) FROM api_tokens").await, 0);
}

#[tokio::test]
async fn a_quote_in_the_device_name_stays_inside_its_field() {
    let app = App::new().await;
    let mut web = Web::new(&app, "ada", "admin").await;
    let device = r#"x" onfocus="alert(1)"#;
    let q = query(REDIRECT, STATE, CHALLENGE, "S256", device);
    let res = web.get(&format!("/connect/extension?{q}")).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
    assert!(!res.body.contains(r#"value="x" onfocus"#), "{}", res.body);
    assert!(
        res.body
            .contains(r#"name="device" value="x&#34; onfocus=&#34;alert(1)">"#),
        "{}",
        res.body
    );
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
