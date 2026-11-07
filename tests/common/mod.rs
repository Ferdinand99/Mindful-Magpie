//! Shared by the integration tests: a Magpie in a temporary directory, driven
//! in-process, with people and a signed-in browser.

#![allow(dead_code)]

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::extract::ConnectInfo;
use axum::http::{HeaderMap, HeaderValue, Request, StatusCode, header};
use mindful_magpie::Magpie;
use serde_json::Value;
use tower::ServiceExt as _;

/// The password of every user the tests add.
pub const PASSWORD: &str = "Magpie-Owner-Pass-2026!";
pub const HOST: &str = "magpie.test:3010";

pub struct Res {
    pub status: StatusCode,
    pub headers: HeaderMap,
    pub body: String,
}

impl Res {
    pub fn json(&self) -> Value {
        serde_json::from_str(&self.body).unwrap_or_else(|e| panic!("{e}: {}", self.body))
    }

    pub fn header(&self, name: &str) -> &str {
        self.headers
            .get(name)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
    }
}

static N: AtomicUsize = AtomicUsize::new(0);

/// A Magpie in a temporary directory.
pub struct App {
    pub magpie: Magpie,
    pub router: Router,
    pub dir: PathBuf,
    /// Leave the data directory in place on drop (a restart reuses it).
    keep_dir: bool,
}

impl Drop for App {
    fn drop(&mut self) {
        if !self.keep_dir {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }
}

impl App {
    pub async fn new() -> App {
        let dir = std::env::temp_dir().join(format!(
            "mindful-magpie-test-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        App::open(dir).await
    }

    /// Starts a Magpie on `dir` (new or existing data).
    async fn open(dir: PathBuf) -> App {
        let magpie = mindful_magpie::start(&dir, None).await.unwrap();
        App {
            router: mindful_magpie::router(magpie.clone()),
            magpie,
            dir,
            keep_dir: false,
        }
    }

    /// Stops this Magpie and starts a new one on the same data directory,
    /// as a process restart does.
    pub async fn restart(mut self) -> App {
        self.keep_dir = true;
        let dir = self.dir.clone();
        drop(self);
        App::open(dir).await
    }

    /// Sends `req` as a program would (no cookies).
    pub async fn send(&self, mut req: Request<Body>) -> Res {
        let h = req.headers_mut();
        if !h.contains_key(header::HOST) {
            h.insert(header::HOST, HeaderValue::from_static(HOST));
        }
        let peer: SocketAddr = "127.0.0.1:40000".parse().unwrap();
        req.extensions_mut().insert(ConnectInfo(peer));
        let res = self.router.clone().oneshot(req).await.unwrap();
        let (status, headers) = (res.status(), res.headers().clone());
        let body = to_bytes(res.into_body(), usize::MAX).await.unwrap();
        Res {
            status,
            headers,
            body: String::from_utf8_lossy(&body).into_owned(),
        }
    }

    /// `GET path` without cookies.
    pub async fn get(&self, path: &str) -> Res {
        self.send(Request::get(path).body(Body::empty()).unwrap())
            .await
    }

    pub async fn count(&self, sql: &'static str) -> i64 {
        self.magpie
            .core
            .with_conn(move |c| c.query_row(sql, [], |r| r.get(0)))
            .await
            .unwrap()
    }

    /// Runs SQL on the database.
    pub async fn exec(&self, sql: String) {
        self.magpie
            .core
            .with_conn(move |c| c.execute_batch(&sql))
            .await
            .unwrap();
    }
}

/// Adds a user with `role` (`viewer`, `editor`, `admin`); its id.
pub async fn add_user(app: &App, name: &'static str, role: &'static str) -> i64 {
    let hash = vole_kit::auth::hash_password(PASSWORD);
    app.magpie
        .core
        .with_conn(move |c| {
            c.execute(
                "INSERT INTO users (username, password_hash, role) VALUES (?1, ?2, ?3)",
                rusqlite::params![name, hash, role],
            )?;
            Ok(c.last_insert_rowid())
        })
        .await
        .unwrap()
}

/// A browser with its cookies.
pub struct Web<'a> {
    pub app: &'a App,
    pub cookies: BTreeMap<String, String>,
}

impl<'a> Web<'a> {
    /// A browser that has not signed in.
    pub fn anonymous(app: &'a App) -> Web<'a> {
        Web {
            app,
            cookies: BTreeMap::new(),
        }
    }

    /// Adds a user with `role` and signs in (needs the kit auth routes, which
    /// are mounted from the auth task on).
    pub async fn new(app: &'a App, name: &'static str, role: &'static str) -> Web<'a> {
        add_user(app, name, role).await;
        let mut b = Web::anonymous(app);
        let res = b
            .post("/login", &[("username", name), ("password", PASSWORD)])
            .await;
        assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
        b
    }

    pub fn cookie_header(&self) -> String {
        self.cookies
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect::<Vec<_>>()
            .join("; ")
    }

    pub async fn send(&mut self, mut req: Request<Body>) -> Res {
        let h = req.headers_mut();
        h.insert(header::HOST, HeaderValue::from_static(HOST));
        if !self.cookies.is_empty() {
            h.insert(
                header::COOKIE,
                HeaderValue::from_str(&self.cookie_header()).unwrap(),
            );
        }
        let peer: SocketAddr = "127.0.0.1:40000".parse().unwrap();
        req.extensions_mut().insert(ConnectInfo(peer));
        let res = self.app.router.clone().oneshot(req).await.unwrap();
        for v in res.headers().get_all(header::SET_COOKIE) {
            let v = v.to_str().unwrap();
            if let Some((k, val)) = v.split(';').next().unwrap().split_once('=') {
                if val.is_empty() || v.contains("Max-Age=0") {
                    self.cookies.remove(k);
                } else {
                    self.cookies.insert(k.into(), val.into());
                }
            }
        }
        let (status, headers) = (res.status(), res.headers().clone());
        let body = to_bytes(res.into_body(), usize::MAX).await.unwrap();
        Res {
            status,
            headers,
            body: String::from_utf8_lossy(&body).into_owned(),
        }
    }

    pub async fn get(&mut self, path: &str) -> Res {
        self.send(Request::get(path).body(Body::empty()).unwrap())
            .await
    }

    /// A form post from the app's own pages (with `HX-Request` when `htmx`).
    pub async fn post_with(&mut self, path: &str, form: &[(&str, &str)], htmx: bool) -> Res {
        let body = url::form_urlencoded::Serializer::new(String::new())
            .extend_pairs(form)
            .finish();
        let mut req = Request::post(path)
            .header(header::ORIGIN, format!("http://{HOST}"))
            .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded");
        if htmx {
            req = req.header("hx-request", "true");
        }
        self.send(req.body(Body::from(body)).unwrap()).await
    }

    pub async fn post(&mut self, path: &str, form: &[(&str, &str)]) -> Res {
        self.post_with(path, form, false).await
    }

    pub async fn delete(&mut self, path: &str) -> Res {
        let req = Request::delete(path)
            .header(header::ORIGIN, format!("http://{HOST}"))
            .header("hx-request", "true")
            .body(Body::empty())
            .unwrap();
        self.send(req).await
    }
}
