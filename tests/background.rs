//! Background images: `POST /start/background` (magic bytes, size limit, CSRF,
//! replacement), `GET /start/background/{id}` (own image only, by cookie or
//! API token), `DELETE /start/background`, the layout's check of the image id
//! and the removal of the file with its user.

mod common;

use std::net::SocketAddr;
use std::path::PathBuf;

use axum::body::{Body, to_bytes};
use axum::extract::ConnectInfo;
use axum::http::{HeaderMap, HeaderValue, Request, StatusCode, header};
use common::{App, HOST, Res, Web, add_user, bearer_get, make_token, user_id};
use serde_json::{Value, json};
use tower::ServiceExt as _;
use vole_kit::auth::TokenScope;

const MIB: usize = 1024 * 1024;
const BOUNDARY: &str = "mm-test-boundary-7f3a";

/// PNG bytes: the signature, then `tail` (only the signature is looked at).
fn png(tail: &[u8]) -> Vec<u8> {
    let mut v = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
    v.extend_from_slice(tail);
    v
}

fn jpeg(tail: &[u8]) -> Vec<u8> {
    let mut v = vec![0xff, 0xd8, 0xff, 0xe0];
    v.extend_from_slice(tail);
    v
}

fn webp(tail: &[u8]) -> Vec<u8> {
    let mut v = b"RIFF".to_vec();
    v.extend_from_slice(&(tail.len() as u32 + 4).to_le_bytes());
    v.extend_from_slice(b"WEBP");
    v.extend_from_slice(tail);
    v
}

/// A `multipart/form-data` body with one field.
fn multipart(field: &str, filename: &str, content_type: &str, data: &[u8]) -> Vec<u8> {
    let mut body = format!(
        "--{BOUNDARY}\r\nContent-Disposition: form-data; name=\"{field}\"; filename=\"{filename}\"\r\n\
         Content-Type: {content_type}\r\n\r\n"
    )
    .into_bytes();
    body.extend_from_slice(data);
    body.extend_from_slice(format!("\r\n--{BOUNDARY}--\r\n").as_bytes());
    body
}

/// What the page's own script sends: same-origin and `HX-Request`.
fn upload_request(body: Vec<u8>) -> Request<Body> {
    Request::post("/start/background")
        .header(header::ORIGIN, format!("http://{HOST}"))
        .header("hx-request", "true")
        .header(
            header::CONTENT_TYPE,
            format!("multipart/form-data; boundary={BOUNDARY}"),
        )
        .body(Body::from(body))
        .unwrap()
}

async fn upload_as(web: &mut Web<'_>, filename: &str, content_type: &str, data: &[u8]) -> Res {
    let body = multipart("image", filename, content_type, data);
    web.send(upload_request(body)).await
}

async fn upload(web: &mut Web<'_>, data: &[u8]) -> Res {
    upload_as(web, "background.png", "image/png", data).await
}

/// Uploads `data`, expects success, returns the id.
async fn upload_ok(web: &mut Web<'_>, data: &[u8]) -> String {
    let res = upload(web, data).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
    res.json()["id"].as_str().unwrap().to_string()
}

/// A response with the body as bytes (`Res` holds text only).
struct Raw {
    status: StatusCode,
    headers: HeaderMap,
    body: Vec<u8>,
}

impl Raw {
    fn header(&self, name: &str) -> &str {
        self.headers
            .get(name)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
    }
}

/// `GET path` with an optional cookie header and an optional bearer token.
async fn fetch(app: &App, path: &str, cookie: Option<String>, token: Option<&str>) -> Raw {
    let mut req = Request::get(path)
        .header(header::HOST, HOST)
        .body(Body::empty())
        .unwrap();
    if let Some(cookie) = cookie {
        req.headers_mut()
            .insert(header::COOKIE, HeaderValue::from_str(&cookie).unwrap());
    }
    if let Some(token) = token {
        req.headers_mut().insert(
            header::AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {token}")).unwrap(),
        );
    }
    let peer: SocketAddr = "127.0.0.1:40000".parse().unwrap();
    req.extensions_mut().insert(ConnectInfo(peer));
    let res = app.router.clone().oneshot(req).await.unwrap();
    let (status, headers) = (res.status(), res.headers().clone());
    let body = to_bytes(res.into_body(), usize::MAX)
        .await
        .unwrap()
        .to_vec();
    Raw {
        status,
        headers,
        body,
    }
}

async fn image_as(app: &App, web: &Web<'_>, id: &str) -> Raw {
    fetch(
        app,
        &format!("/start/background/{id}"),
        Some(web.cookie_header()),
        None,
    )
    .await
}

fn dir_of(app: &App, uid: i64) -> PathBuf {
    app.dir.join("backgrounds").join(uid.to_string())
}

/// The names of the files in the user's image directory.
fn files_of(app: &App, uid: i64) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir_of(app, uid))
        .map(|d| {
            d.map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    names
}

fn layout_with(version: u64, background: Value) -> Value {
    json!({
        "version": version,
        "background": background,
        "groups": [],
        "bookmarks": []
    })
}

async fn save_layout(web: &mut Web<'_>, layout: &Value) -> Res {
    let req = Request::post("/start/layout")
        .header(header::ORIGIN, format!("http://{HOST}"))
        .header("hx-request", "true")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(serde_json::to_vec(layout).unwrap()))
        .unwrap();
    web.send(req).await
}

async fn delete_background(web: &mut Web<'_>) -> Res {
    web.delete("/start/background").await
}

async fn stored_layout(app: &App, uid: i64) -> Value {
    let doc: String = app
        .magpie
        .core
        .with_conn(move |c| {
            c.query_row("SELECT doc FROM layouts WHERE user_id = ?1", [uid], |r| {
                r.get(0)
            })
        })
        .await
        .unwrap();
    serde_json::from_str(&doc).unwrap()
}

#[tokio::test]
async fn an_uploaded_png_is_stored_and_served_back() {
    let app = App::new().await;
    let mut web = Web::new(&app, "ada", "admin").await;
    let uid = user_id(&app, "ada").await;
    let data = png(b"pretend pixels");

    let res = upload(&mut web, &data).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
    let id = res.json()["id"].as_str().unwrap().to_string();
    assert_eq!(id.len(), 32, "{id}");
    assert!(id.bytes().all(|b| b.is_ascii_hexdigit()), "{id}");
    assert_eq!(res.json().as_object().unwrap().len(), 1);

    // On disk under the data directory, named after the id.
    assert_eq!(files_of(&app, uid), vec![format!("{id}.png")]);
    assert_eq!(
        std::fs::read(dir_of(&app, uid).join(format!("{id}.png"))).unwrap(),
        data
    );
    // In the table.
    let row: (String, String, i64) = app
        .magpie
        .core
        .with_conn(move |c| {
            c.query_row(
                "SELECT id, mime, bytes FROM backgrounds WHERE user_id = ?1",
                [uid],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
        })
        .await
        .unwrap();
    assert_eq!(row, (id.clone(), "image/png".into(), data.len() as i64));

    // Served back to its owner.
    let img = image_as(&app, &web, &id).await;
    assert_eq!(img.status, StatusCode::OK);
    assert_eq!(img.header("content-type"), "image/png");
    assert_eq!(
        img.header("cache-control"),
        "private, max-age=31536000, immutable"
    );
    assert_eq!(img.header("x-content-type-options"), "nosniff");
    assert_eq!(img.body, data);
}

#[tokio::test]
async fn the_id_follows_the_content() {
    let app = App::new().await;
    let mut web = Web::new(&app, "ada", "admin").await;
    let a = upload_ok(&mut web, &png(b"one")).await;
    let again = upload_ok(&mut web, &png(b"one")).await;
    let b = upload_ok(&mut web, &png(b"two")).await;
    assert_eq!(a, again);
    assert_ne!(a, b);
    // Uploading the same picture again keeps its file.
    let uid = user_id(&app, "ada").await;
    assert_eq!(files_of(&app, uid), vec![format!("{b}.png")]);
}

#[tokio::test]
async fn jpeg_and_webp_are_served_with_their_own_types() {
    let app = App::new().await;
    let mut web = Web::new(&app, "ada", "admin").await;
    let uid = user_id(&app, "ada").await;

    let data = jpeg(b"jpeg data");
    let id = upload_ok(&mut web, &data).await;
    assert_eq!(files_of(&app, uid), vec![format!("{id}.jpg")]);
    let img = image_as(&app, &web, &id).await;
    assert_eq!(img.header("content-type"), "image/jpeg");
    assert_eq!(img.body, data);

    let data = webp(b"VP8 webp data");
    let id = upload_ok(&mut web, &data).await;
    assert_eq!(files_of(&app, uid), vec![format!("{id}.webp")]);
    let img = image_as(&app, &web, &id).await;
    assert_eq!(img.header("content-type"), "image/webp");
    assert_eq!(img.body, data);
}

#[tokio::test]
async fn only_the_bytes_decide_what_a_file_is() {
    let app = App::new().await;
    let mut web = Web::new(&app, "ada", "admin").await;
    let uid = user_id(&app, "ada").await;

    let html: &[u8] = b"<!doctype html><script>alert(1)</script>";
    let gif: &[u8] = b"GIF89a\x01\x00\x01\x00";
    let wav = {
        let mut v = b"RIFF".to_vec();
        v.extend_from_slice(&[4, 0, 0, 0]);
        v.extend_from_slice(b"WAVEfmt ");
        v
    };
    let svg: &[u8] = b"<svg xmlns=\"http://www.w3.org/2000/svg\"></svg>";
    for data in [html, gif, &wav, svg, b"", b"\x89PNG", b"\xff\xd8"] {
        let res = upload_as(&mut web, "background.png", "image/png", data).await;
        assert_eq!(res.status, StatusCode::UNPROCESSABLE_ENTITY, "{}", res.body);
        assert_eq!(res.json(), json!({ "error": "bad_image" }));
    }
    assert_eq!(app.count("SELECT COUNT(*) FROM backgrounds").await, 0);
    assert!(files_of(&app, uid).is_empty());

    // A real PNG under another name and type is still a PNG.
    let id = upload_as(&mut web, "x.txt", "text/plain", &png(b"ok")).await;
    assert_eq!(id.status, StatusCode::OK, "{}", id.body);
    let id = id.json()["id"].as_str().unwrap().to_string();
    assert_eq!(
        image_as(&app, &web, &id).await.header("content-type"),
        "image/png"
    );
}

#[tokio::test]
async fn a_bad_upload_keeps_the_image_there_was() {
    let app = App::new().await;
    let mut web = Web::new(&app, "ada", "admin").await;
    let uid = user_id(&app, "ada").await;
    let id = upload_ok(&mut web, &png(b"keep me")).await;
    let res = upload(&mut web, b"<html>").await;
    assert_eq!(res.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(files_of(&app, uid), vec![format!("{id}.png")]);
    assert_eq!(image_as(&app, &web, &id).await.status, StatusCode::OK);
}

#[tokio::test]
async fn the_size_limit_is_five_megabytes() {
    let app = App::new().await;
    let mut web = Web::new(&app, "ada", "admin").await;
    let uid = user_id(&app, "ada").await;

    // Exactly 5 MiB goes in.
    let mut big = png(&[]);
    big.resize(5 * MIB, 7);
    let res = upload(&mut web, &big).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);

    // One byte more does not, and neither does 6 MiB.
    for size in [5 * MIB + 1, 6 * MIB] {
        let mut big = png(&[]);
        big.resize(size, 9);
        let res = upload(&mut web, &big).await;
        assert_eq!(res.status, StatusCode::PAYLOAD_TOO_LARGE, "{size}");
    }
    // The 5 MiB image is still the one stored.
    assert_eq!(files_of(&app, uid).len(), 1);
    assert_eq!(app.count("SELECT COUNT(*) FROM backgrounds").await, 1);
}

#[tokio::test]
async fn a_request_without_the_image_field_is_a_bad_request() {
    let app = App::new().await;
    let mut web = Web::new(&app, "ada", "admin").await;
    let body = multipart("other", "a.png", "image/png", &png(b"x"));
    let res = web.send(upload_request(body)).await;
    assert_eq!(res.status, StatusCode::BAD_REQUEST, "{}", res.body);
    assert_eq!(res.json(), json!({ "error": "bad_request" }));

    let req = Request::post("/start/background")
        .header(header::ORIGIN, format!("http://{HOST}"))
        .header("hx-request", "true")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from("{}"))
        .unwrap();
    let res = web.send(req).await;
    assert_eq!(res.status, StatusCode::BAD_REQUEST, "{}", res.body);
}

#[tokio::test]
async fn uploads_need_a_signed_in_user_and_a_same_origin_request() {
    let app = App::new().await;
    let mut web = Web::new(&app, "ada", "admin").await;
    let body = multipart("image", "a.png", "image/png", &png(b"x"));

    // Signed in, but with neither HX-Request nor a same-origin Origin.
    let req = Request::post("/start/background")
        .header(
            header::CONTENT_TYPE,
            format!("multipart/form-data; boundary={BOUNDARY}"),
        )
        .body(Body::from(body.clone()))
        .unwrap();
    assert_eq!(web.send(req).await.status, StatusCode::FORBIDDEN);

    // A foreign origin.
    let req = Request::post("/start/background")
        .header(header::ORIGIN, "https://evil.example")
        .header(
            header::CONTENT_TYPE,
            format!("multipart/form-data; boundary={BOUNDARY}"),
        )
        .body(Body::from(body.clone()))
        .unwrap();
    assert_eq!(web.send(req).await.status, StatusCode::FORBIDDEN);

    // DELETE too.
    let req = Request::delete("/start/background")
        .body(Body::empty())
        .unwrap();
    assert_eq!(web.send(req).await.status, StatusCode::FORBIDDEN);

    // Same origin but not signed in.
    let mut anon = Web::anonymous(&app);
    let res = anon.send(upload_request(body)).await;
    assert_eq!(res.status, StatusCode::UNAUTHORIZED, "{}", res.body);
    let res = anon.delete("/start/background").await;
    assert_eq!(res.status, StatusCode::UNAUTHORIZED, "{}", res.body);

    assert_eq!(app.count("SELECT COUNT(*) FROM backgrounds").await, 0);
}

#[tokio::test]
async fn nobody_else_can_fetch_an_image() {
    let app = App::new().await;
    let mut ada = Web::new(&app, "ada", "admin").await;
    let bob = Web::new(&app, "bob", "editor").await;
    let id = upload_ok(&mut ada, &png(b"ada's")).await;

    // Bob has the id and gets nothing; an id that exists nowhere gives the same.
    let theirs = image_as(&app, &bob, &id).await;
    let unknown = image_as(&app, &bob, &"0".repeat(32)).await;
    assert_eq!(theirs.status, StatusCode::NOT_FOUND);
    assert_eq!(unknown.status, StatusCode::NOT_FOUND);
    assert_eq!(theirs.body, unknown.body);
    assert_eq!(theirs.header("cache-control"), "no-store");
    assert!(!theirs.header("content-type").starts_with("image/"));

    // Odd ids are not found either, and never reach the file system.
    for odd in ["..", "%2e%2e%2f%2e%2e%2fmagpie.sqlite", "a.png", "x/y"] {
        let res = image_as(&app, &ada, odd).await;
        assert_eq!(res.status, StatusCode::NOT_FOUND, "{odd}");
    }

    // Without any sign-in: a plain 401, no redirect to the sign-in page.
    let res = fetch(&app, &format!("/start/background/{id}"), None, None).await;
    assert_eq!(res.status, StatusCode::UNAUTHORIZED);
    assert!(res.headers.get(header::LOCATION).is_none());

    // The owner still has it.
    assert_eq!(image_as(&app, &ada, &id).await.status, StatusCode::OK);
}

#[tokio::test]
async fn a_read_token_fetches_its_owners_image_without_a_redirect() {
    let app = App::new().await;
    let mut ada = Web::new(&app, "ada", "admin").await;
    let data = png(b"for the extension");
    let id = upload_ok(&mut ada, &data).await;
    let token = make_token(&app, user_id(&app, "ada").await, TokenScope::Read, "ext").await;
    let path = format!("/start/background/{id}");

    let res = fetch(&app, &path, None, Some(&token)).await;
    assert_eq!(res.status, StatusCode::OK);
    assert!(res.headers.get(header::LOCATION).is_none());
    assert_eq!(res.header("content-type"), "image/png");
    assert_eq!(
        res.header("cache-control"),
        "private, max-age=31536000, immutable"
    );
    assert_eq!(res.header("x-content-type-options"), "nosniff");
    assert_eq!(res.body, data);

    // The same route through the common helper (a program, no cookies).
    let res = bearer_get(&app, &path, &token).await;
    assert_eq!(res.status, StatusCode::OK);
}

#[tokio::test]
async fn a_token_never_opens_another_users_image() {
    let app = App::new().await;
    let mut ada = Web::new(&app, "ada", "admin").await;
    let id = upload_ok(&mut ada, &png(b"ada's")).await;
    add_user(&app, "bob", "editor").await;
    let bob = make_token(&app, user_id(&app, "bob").await, TokenScope::Read, "ext").await;
    let path = format!("/start/background/{id}");

    let res = fetch(&app, &path, None, Some(&bob)).await;
    assert_eq!(res.status, StatusCode::NOT_FOUND);
    assert!(res.headers.get(header::LOCATION).is_none());
    // Not different from an id that does not exist.
    let none = fetch(
        &app,
        &format!("/start/background/{}", "f".repeat(32)),
        None,
        Some(&bob),
    )
    .await;
    assert_eq!(none.status, StatusCode::NOT_FOUND);
    assert_eq!(none.body, res.body);

    // A token wins over a cookie: ada's cookie with bob's token is bob.
    let res = fetch(&app, &path, Some(ada.cookie_header()), Some(&bob)).await;
    assert_eq!(res.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn a_bad_or_revoked_token_is_401_and_never_a_redirect() {
    let app = App::new().await;
    let mut ada = Web::new(&app, "ada", "admin").await;
    let id = upload_ok(&mut ada, &png(b"x")).await;
    let path = format!("/start/background/{id}");

    let junk = format!("mm_{}", "x".repeat(43));
    for bad in [junk.as_str(), "mm_short", "not-a-token"] {
        let res = fetch(&app, &path, None, Some(bad)).await;
        assert_eq!(res.status, StatusCode::UNAUTHORIZED, "{bad}");
        assert!(res.headers.get(header::LOCATION).is_none(), "{bad}");
    }
    // A bad token is no reason to fall back to the cookie.
    let res = fetch(&app, &path, Some(ada.cookie_header()), Some(&junk)).await;
    assert_eq!(res.status, StatusCode::UNAUTHORIZED);

    let token = make_token(&app, user_id(&app, "ada").await, TokenScope::Read, "ext").await;
    assert_eq!(
        fetch(&app, &path, None, Some(&token)).await.status,
        StatusCode::OK
    );
    app.exec("DELETE FROM api_tokens".into()).await;
    let res = fetch(&app, &path, None, Some(&token)).await;
    assert_eq!(res.status, StatusCode::UNAUTHORIZED);
    assert!(res.headers.get(header::LOCATION).is_none());
}

#[tokio::test]
async fn a_second_upload_replaces_the_first() {
    let app = App::new().await;
    let mut web = Web::new(&app, "ada", "admin").await;
    let uid = user_id(&app, "ada").await;

    let first = upload_ok(&mut web, &png(b"first")).await;
    assert_eq!(files_of(&app, uid), vec![format!("{first}.png")]);
    let second = upload_ok(&mut web, &jpeg(b"second")).await;
    assert_ne!(first, second);

    // Only the new file is left, whatever its type, and nothing temporary.
    assert_eq!(files_of(&app, uid), vec![format!("{second}.jpg")]);
    assert_eq!(app.count("SELECT COUNT(*) FROM backgrounds").await, 1);
    // The old id is gone, the new one serves.
    assert_eq!(
        image_as(&app, &web, &first).await.status,
        StatusCode::NOT_FOUND
    );
    let img = image_as(&app, &web, &second).await;
    assert_eq!(img.status, StatusCode::OK);
    assert_eq!(img.header("content-type"), "image/jpeg");
}

#[tokio::test]
async fn users_have_their_own_directories() {
    let app = App::new().await;
    let mut ada = Web::new(&app, "ada", "admin").await;
    let mut bob = Web::new(&app, "bob", "editor").await;
    let a = upload_ok(&mut ada, &png(b"same")).await;
    let b = upload_ok(&mut bob, &png(b"same")).await;
    // The same picture has the same id for both, and each has a file of its own.
    assert_eq!(a, b);
    assert_eq!(files_of(&app, user_id(&app, "ada").await).len(), 1);
    assert_eq!(files_of(&app, user_id(&app, "bob").await).len(), 1);
    assert_eq!(app.count("SELECT COUNT(*) FROM backgrounds").await, 2);
}

#[tokio::test]
async fn deleting_a_user_removes_the_file() {
    let app = App::new().await;
    let mut ada = Web::new(&app, "ada", "admin").await;
    let mut bob = Web::new(&app, "bob", "editor").await;
    let a = upload_ok(&mut ada, &png(b"ada")).await;
    upload_ok(&mut bob, &png(b"bob")).await;
    let (ada_id, bob_id) = (user_id(&app, "ada").await, user_id(&app, "bob").await);
    assert!(dir_of(&app, bob_id).exists());

    let res = ada
        .post_with(&format!("/settings/users/{bob_id}/delete"), &[], true)
        .await;
    assert!(res.status.is_redirection(), "{} {}", res.status, res.body);
    assert_eq!(
        app.count("SELECT COUNT(*) FROM users WHERE id > 1").await,
        0
    );
    assert_eq!(app.count("SELECT COUNT(*) FROM backgrounds").await, 1);
    assert!(!dir_of(&app, bob_id).exists());
    // The other user's image stays.
    assert_eq!(files_of(&app, ada_id), vec![format!("{a}.png")]);
}

#[tokio::test]
async fn a_restart_sweeps_files_of_users_that_are_gone() {
    let app = App::new().await;
    let mut ada = Web::new(&app, "ada", "admin").await;
    let mut bob = Web::new(&app, "bob", "editor").await;
    let a = upload_ok(&mut ada, &png(b"ada")).await;
    upload_ok(&mut bob, &png(b"bob")).await;
    let (ada_id, bob_id) = (user_id(&app, "ada").await, user_id(&app, "bob").await);
    // The user goes without the app noticing (a delete outside the page).
    app.exec(format!("DELETE FROM users WHERE id = {bob_id}"))
        .await;
    assert!(dir_of(&app, bob_id).exists());
    // And an upload that died halfway: a temporary file next to the image.
    std::fs::write(dir_of(&app, ada_id).join(".leftover.tmp"), b"half").unwrap();
    // A directory that is not a user's is left alone.
    let other = app.dir.join("backgrounds").join("not-a-user");
    std::fs::create_dir_all(&other).unwrap();
    drop((ada, bob));

    let app = app.restart().await;
    assert!(!dir_of(&app, bob_id).exists());
    assert_eq!(files_of(&app, ada_id), vec![format!("{a}.png")]);
    assert!(other.exists());
}

#[tokio::test]
async fn delete_removes_the_image_and_resets_the_background() {
    let app = App::new().await;
    let mut web = Web::new(&app, "ada", "admin").await;
    let uid = user_id(&app, "ada").await;
    let id = upload_ok(&mut web, &png(b"bye")).await;
    let res = save_layout(
        &mut web,
        &layout_with(0, json!({ "kind": "image", "id": id })),
    )
    .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);

    let res = delete_background(&mut web).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
    // The layout moved on a version, so another tab's old copy is a conflict.
    assert_eq!(res.json(), json!({ "version": 2 }));
    let layout = stored_layout(&app, uid).await;
    assert_eq!(
        layout["background"],
        json!({ "kind": "preset", "name": "hedgerow" })
    );
    assert_eq!(app.count("SELECT COUNT(*) FROM backgrounds").await, 0);
    assert!(files_of(&app, uid).is_empty());
    assert_eq!(
        image_as(&app, &web, &id).await.status,
        StatusCode::NOT_FOUND
    );
    // The old copy of the layout (version 1, with the image) is refused.
    let res = save_layout(
        &mut web,
        &layout_with(1, json!({ "kind": "image", "id": id })),
    )
    .await;
    assert_eq!(res.status, StatusCode::UNPROCESSABLE_ENTITY, "{}", res.body);
}

#[tokio::test]
async fn delete_leaves_a_chosen_preset_and_works_without_an_image() {
    let app = App::new().await;
    let mut web = Web::new(&app, "ada", "admin").await;
    let uid = user_id(&app, "ada").await;

    // Nothing stored at all: fine, and nothing is made up.
    let res = delete_background(&mut web).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
    assert_eq!(res.json(), json!({ "version": 0 }));
    assert_eq!(app.count("SELECT COUNT(*) FROM layouts").await, 0);

    // An image that is stored but not in use: the chosen preset stays.
    upload_ok(&mut web, &png(b"unused")).await;
    let res = save_layout(
        &mut web,
        &layout_with(0, json!({ "kind": "preset", "name": "hav" })),
    )
    .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
    let res = delete_background(&mut web).await;
    assert_eq!(res.json(), json!({ "version": 1 }));
    assert_eq!(
        stored_layout(&app, uid).await["background"],
        json!({ "kind": "preset", "name": "hav" })
    );
    assert_eq!(app.count("SELECT COUNT(*) FROM backgrounds").await, 0);
    assert!(files_of(&app, uid).is_empty());
}

#[tokio::test]
async fn a_layout_may_only_use_the_users_own_image() {
    let app = App::new().await;
    let mut ada = Web::new(&app, "ada", "admin").await;
    let mut bob = Web::new(&app, "bob", "editor").await;
    let a = upload_ok(&mut ada, &png(b"ada's")).await;
    let b = upload_ok(&mut bob, &png(b"bob's")).await;
    assert_ne!(a, b);
    let uid = user_id(&app, "bob").await;

    // Ada's id in Bob's layout.
    let res = save_layout(
        &mut bob,
        &layout_with(0, json!({ "kind": "image", "id": a })),
    )
    .await;
    assert_eq!(res.status, StatusCode::UNPROCESSABLE_ENTITY, "{}", res.body);
    assert_eq!(
        res.json(),
        json!({
            "error": "invalid",
            "fields": [{ "path": "background.id", "code": "bad_image" }]
        })
    );
    assert_eq!(app.count("SELECT COUNT(*) FROM layouts").await, 0);

    // An id nobody has.
    for id in ["0".repeat(32), "not an id".to_string(), String::new()] {
        let res = save_layout(
            &mut bob,
            &layout_with(0, json!({ "kind": "image", "id": id })),
        )
        .await;
        assert_eq!(res.status, StatusCode::UNPROCESSABLE_ENTITY, "{id}");
    }

    // Bob's own.
    let res = save_layout(
        &mut bob,
        &layout_with(0, json!({ "kind": "image", "id": b })),
    )
    .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
    assert_eq!(stored_layout(&app, uid).await["background"]["id"], json!(b));

    // After a new upload the old id is no longer his.
    let b2 = upload_ok(&mut bob, &png(b"bob's other")).await;
    let res = save_layout(
        &mut bob,
        &layout_with(1, json!({ "kind": "image", "id": b })),
    )
    .await;
    assert_eq!(res.status, StatusCode::UNPROCESSABLE_ENTITY, "{}", res.body);
    let res = save_layout(
        &mut bob,
        &layout_with(1, json!({ "kind": "image", "id": b2 })),
    )
    .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
}

#[tokio::test]
async fn a_bad_image_id_is_reported_with_the_other_problems() {
    let app = App::new().await;
    let mut web = Web::new(&app, "ada", "admin").await;
    let mut layout = layout_with(0, json!({ "kind": "image", "id": "x".repeat(65) }));
    layout["bookmarks"] = json!([{ "id": "b1", "title": "", "url": "https://a.example/" }]);
    let res = save_layout(&mut web, &layout).await;
    assert_eq!(res.status, StatusCode::UNPROCESSABLE_ENTITY, "{}", res.body);
    let codes: Vec<(String, String)> = res.json()["fields"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| {
            (
                f["path"].as_str().unwrap().into(),
                f["code"].as_str().unwrap().into(),
            )
        })
        .collect();
    assert!(codes.contains(&("background.id".into(), "bad_image".into())));
    assert!(codes.contains(&("bookmarks[0].title".into(), "required".into())));
}
