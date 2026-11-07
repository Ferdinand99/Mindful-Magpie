//! Magpie's REST routes under `/api/v1`, for the browser extension:
//!
//! - `GET /api/v1/me/start`: the token owner's start page layout. Only an API
//!   token opens it. The path is exempt from the CSRF guard, so a session
//!   cookie, which a foreign page can make the browser send, is never enough.
//! - `GET /api/v1/me/values`: the latest readings of the services bound to the
//!   cards on that layout (same rules; see [`crate::values`]).
//! - `POST /api/v1/extension/token`: the last step of the connect flow (see
//!   [`crate::connect`]): a one-time code and the PKCE verifier for a
//!   read-only API token. It takes no credentials; the code is the credential.

use std::net::{IpAddr, Ipv4Addr};
use std::time::Instant;

use axum::body::{Body, to_bytes};
use axum::extract::{Extension, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use rusqlite::{OptionalExtension as _, TransactionBehavior, params};
use serde::Deserialize;
use serde_json::json;
use sha2::{Digest as _, Sha256};
use vole_kit::auth::guard::PeerAddr;
use vole_kit::auth::totp::ct_eq;
use vole_kit::auth::{AuthMethod, CurrentUser, TokenScope, session, tokens};
use vole_kit::db::{self, api_tokens};
use vole_kit::docs::{ErrorCode, RouteDoc, r};
use vole_kit::http::ApiError;
use vole_kit::http::ratelimit::{Rate, too_many_requests};
use vole_kit::state::Core;

use crate::layout::Layout;
use crate::product::OWNER_SCOPE;
use crate::services::{ServiceView, Viewer, services_for};
use crate::values::values_for;
use crate::{Magpie, connect, layout};

/// Token requests one address may make: a person connects once in a while, a
/// guesser tries many codes.
pub const TOKEN_RATE: Rate = Rate::per_minute(10);
/// The largest body the token endpoint reads (two short strings).
const MAX_TOKEN_BODY: usize = 4096;
/// The name a connected extension's token gets: the user's token list shows
/// it, and revoking the token disconnects the extension.
const TOKEN_NAME_PREFIX: &str = "Utvidelse: ";
/// The most API tokens one user may have: the kit's cap on its Tokens page
/// (`MAX_TOKENS_PER_USER` there), so the flow cannot get around it.
const MAX_TOKENS_PER_USER: i64 = 50;

const ME_START_EXAMPLE: &str = r##"{"version": 3, "background": {"kind": "preset", "name": "hav"}, "groups": [{"id": "g1", "title": "Services", "cards": [{"id": "c1", "title": "Vigilant Vole", "description": "Uptime", "url": "https://vole.example.com/", "icon": {"emoji": null, "colour": "teal"}, "service": {"id": 1, "metrics": ["status", "latency_ms"]}}]}], "bookmarks": [{"id": "b1", "title": "Docs", "url": "https://docs.example.com/"}], "services": [{"id": 1, "name": "Blog", "kind": "http", "interval_s": 60, "link": "https://blog.example.com", "metrics": [{"id": "status", "label": "HTTP status", "format": "number"}, {"id": "latency_ms", "label": "Response time (ms)", "format": "number"}]}]}"##;

const ME_VALUES_EXAMPLE: &str = r##"{"now": 1760000090, "services": {"1": {"name": "Blog", "kind": "http", "interval_s": 60, "status": "ok", "values": {"status": {"num": 200.0, "text": null}, "latency_ms": {"num": 84.0, "text": null}}, "fetched_at": 1760000060}, "2": {"name": "Wiki", "kind": "http", "interval_s": 60, "status": "bad", "values": {}, "fetched_at": 1760000070, "error_label": "Timeout"}}, "needs": [{"service_id": 2, "service_name": "Wiki", "key": "down", "level": "bad", "label": "down", "since": 1760000010, "acked": false}]}"##;

/// Magpie's documented endpoints (the API reference at `/api-docs`).
pub const ROUTES: &[RouteDoc] = &[
    r(
        "GET",
        "/api/v1/me/start",
        "viewer",
        "Start page",
        "The start page layout of the token's owner, as the page shows it: `version`, `background`, `groups` of cards and `bookmarks`, and `services`: the services the owner may see, each `{id, name, kind, interval_s, link, metrics: [{id, label, format}]}` (`link` is where a card without a link of its own opens: the service's own link, else its address up to the port, or empty; `label` in the request's language, `format` one of `number`, `percent`, `bytes`, `bytes_per_sec`, `duration`, `since`, `text`), never an address, a setting or a key. A card bound to a service has `service: {id, metrics: [ids]}`; the id may not be in `services` (the service was removed or is no longer visible). Any API token opens it (read access is enough); a session cookie never does. If the stored layout is unreadable, the answer is the empty layout at the stored version, as the page shows it. A picture background that is no longer the user's picture is shown as the `hedgerow` preset, with the stored version. Never cached (`Cache-Control: no-store`).",
    )
    .res(ME_START_EXAMPLE),
    r(
        "GET",
        "/api/v1/me/values",
        "viewer",
        "Service values",
        "The latest reading of every service that is bound to a card on the start page of the token's user and that the user may see: `{now, services, needs}`, `now` being the server's clock in Unix seconds and `services` an object by service id with `{name, kind, interval_s, status, values, fetched_at, error_label}` for each. `status` is `ok`, `warn`, `bad` or `unknown`; `values` maps a metric id to `{num, text}` (either may be `null`); `fetched_at` is the time of the reading in Unix seconds, or `null` while the service has not been read yet (then `status` is `unknown` and `values` is empty); `error_label` is present when the last reading failed and says why in the request's language (a fixed set of texts, never the raw error). `needs` (beside `services`) lists what needs the user's attention, worst first: `{service_id, service_name, key, level, label, since, acked}` for each need of those services, `level` being `warn` or `bad`, `label` the text in the request's language (without the service's name, which is `service_name`: show them side by side), `since` the time the need began in Unix seconds and `acked` whether the user has acknowledged it on the page and it has not got worse since (an acknowledgement is made on the start page only; a token cannot make one). A need that only an admin can act on (a service whose settings must be set up again) is listed only when the user is an admin and the request is their session or the browser extension's token; any other token never gets it. A service the user may not see is left out entirely, with its needs. Pages and the extension never wait for the service itself: this is the stored reading, and its age is `now - fetched_at`. Any API token opens it (read access is enough); a session cookie never does. Never cached (`Cache-Control: no-store`).",
    )
    .res(ME_VALUES_EXAMPLE),
    r(
        "POST",
        "/api/v1/extension/token",
        "public",
        "Extension",
        "Swaps the one-time code of the browser extension's connect flow (`/connect/extension`, PKCE with S256) for an API token with read access, named `Utvidelse: <device>`, owned by the user who approved. The code lives two minutes, works once and is spent by any attempt. `code_verifier` is the secret whose SHA-256 (base64url) was sent as `code_challenge`. Connecting the same device again replaces the user's earlier token of that name, and a user who already has 50 tokens gets none. Every failure is the same `400 invalid_grant`; more than 10 requests a minute from one address get `429` (behind a reverse proxy, set `MM_TRUST_PROXY=1`, or every client shares the proxy's address). The token shows in the user's API tokens, where it can be revoked.",
    )
    .req(r#"{"code": "Zm9yLWV4YW1wbGUtb25seS1ub3QtYS1yZWFsLWNvZGU", "code_verifier": "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"}"#)
    .res(r#"{"token": "mm_…"}"#),
];

/// The error codes of the routes above, besides the kit's.
pub const ERROR_CODES: &[ErrorCode] = &[ErrorCode {
    code: "invalid_grant",
    status: 400,
    when: "`POST /api/v1/extension/token` with a code that is unknown, used, expired or not for this `code_verifier`, or with a body that is not `{code, code_verifier}` (the answer is `{\"error\": \"invalid_grant\"}`, without the usual envelope)",
    on_page: false,
}];

/// The routes that take an API token (the caller adds the sign-in guard,
/// which resolves the token).
pub fn router() -> Router<Core> {
    Router::new()
        .route("/api/v1/me/start", get(me_start))
        .route("/api/v1/me/values", get(me_values))
}

/// The routes without credentials.
pub fn public_router() -> Router<Core> {
    Router::new().route("/api/v1/extension/token", post(extension_token))
}

/// The answer of `GET /api/v1/me/start`: the layout document, as it always
/// was, and `services` next to its fields. A client that reads `version`,
/// `background`, `groups` and `bookmarks` (the extension before 0.3.0) does not
/// notice the extra field; a newer one draws the numbers of bound cards with it.
#[derive(serde::Serialize)]
struct StartDoc {
    #[serde(flatten)]
    layout: Layout,
    services: Vec<ServiceView>,
}

/// `GET /api/v1/me/start`.
async fn me_start(
    State(core): State<Core>,
    user: CurrentUser,
    method: Option<Extension<AuthMethod>>,
) -> Result<Response, ApiError> {
    // The guard lets a signed-in cookie through on an `/api/` path that sends
    // no `Authorization` header; this route wants the token itself.
    if !matches!(method, Some(Extension(AuthMethod::Token { .. }))) {
        return Err(ApiError::Unauthorized);
    }
    if !user.can_read() {
        return Err(ApiError::Forbidden);
    }
    let (layout, _) = layout::load_or_default(&core, user.id).await?;
    let services = services_for(&core, Viewer::for_token(&user)).await?;
    Ok((
        [(header::CACHE_CONTROL, "no-store")],
        Json(StartDoc { layout, services }),
    )
        .into_response())
}

/// `GET /api/v1/me/values`: the readings of the services bound on the token
/// owner's layout and visible to them, whole (see [`crate::values`]). Only a
/// token opens it, as with `/api/v1/me/start`.
async fn me_values(
    State(core): State<Core>,
    Extension(magpie): Extension<Magpie>,
    user: CurrentUser,
    method: Option<Extension<AuthMethod>>,
) -> Result<Response, ApiError> {
    if !matches!(method, Some(Extension(AuthMethod::Token { .. }))) {
        return Err(ApiError::Unauthorized);
    }
    if !user.can_read() {
        return Err(ApiError::Forbidden);
    }
    let (layout, _) = layout::load_or_default(&core, user.id).await?;
    let doc = values_for(&core, &magpie.poller, Viewer::for_token(&user), &layout).await?;
    Ok(([(header::CACHE_CONTROL, "no-store")], Json(doc)).into_response())
}

#[derive(Deserialize)]
struct TokenRequest {
    code: String,
    code_verifier: String,
}

/// The one answer to every failed exchange.
fn invalid_grant() -> Response {
    (
        StatusCode::BAD_REQUEST,
        [(header::CACHE_CONTROL, "no-store")],
        Json(json!({ "error": "invalid_grant" })),
    )
        .into_response()
}

/// A code is 32 bytes as base64url: 43 characters.
fn code_shape(code: &str) -> bool {
    code.len() == 43
        && code
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
}

/// A PKCE verifier (RFC 7636): 43 to 128 characters of `A-Z a-z 0-9 - . _ ~`.
fn verifier_shape(verifier: &str) -> bool {
    (43..=128).contains(&verifier.len())
        && verifier
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~'))
}

/// `BASE64URL(SHA256(verifier))` is the stored challenge, compared in
/// constant time.
fn pkce_matches(verifier: &str, challenge: &str) -> bool {
    let computed = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    ct_eq(computed.as_bytes(), challenge.as_bytes())
}

/// `POST /api/v1/extension/token`.
async fn extension_token(
    State(core): State<Core>,
    Extension(magpie): Extension<Magpie>,
    PeerAddr(peer): PeerAddr,
    headers: HeaderMap,
    body: Body,
) -> Response {
    let ip = session::client_ip(&headers, peer, session::trust_proxy())
        .unwrap_or(IpAddr::V4(Ipv4Addr::UNSPECIFIED));
    if let Err(wait) = magpie.token_budget.take(&ip, TOKEN_RATE, Instant::now()) {
        tracing::warn!(%ip, "extension token requests over the budget");
        return too_many_requests(wait);
    }
    let Ok(bytes) = to_bytes(body, MAX_TOKEN_BODY).await else {
        return invalid_grant();
    };
    let Ok(request) = serde_json::from_slice::<TokenRequest>(&bytes) else {
        return invalid_grant();
    };
    // Wrong shapes can't be a real code or verifier: refuse them without
    // touching the stored codes.
    if !code_shape(&request.code) || !verifier_shape(&request.code_verifier) {
        return invalid_grant();
    }
    let token = tokens::generate();
    let (code_hash, token_hash, prefix) = (
        tokens::hash(&request.code),
        tokens::hash(&token),
        tokens::display_prefix(&token),
    );
    let verifier = request.code_verifier;
    let redeemed = db::interact(&core.db, move |c| {
        let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let row: Option<(i64, String, String, String, bool)> = tx
            .query_row(
                "SELECT c.user_id, u.username, c.code_challenge, c.device,
                        c.expires_at > CAST(strftime('%s','now') AS INTEGER)
                 FROM extension_codes c JOIN users u ON u.id = c.user_id AND u.active = 1
                 WHERE c.code_hash = ?1",
                params![code_hash],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )
            .optional()?;
        // Single use: whoever finds the code spends it, right or wrong.
        tx.execute(
            "DELETE FROM extension_codes WHERE code_hash = ?1",
            params![code_hash],
        )?;
        connect::purge_expired(&tx)?;
        let granted = match row {
            Some((user_id, username, challenge, device, true))
                if pkce_matches(&verifier, &challenge) =>
            {
                let name = format!("{TOKEN_NAME_PREFIX}{device}");
                // The kit's cap, not counting the tokens this one replaces.
                let others: i64 = tx.query_row(
                    "SELECT COUNT(*) FROM api_tokens WHERE user_id = ?1 AND name != ?2",
                    params![user_id, name],
                    |r| r.get(0),
                )?;
                if others >= MAX_TOKENS_PER_USER {
                    tracing::warn!(user = %username, "extension token refused: the user has the most API tokens allowed");
                    None
                } else {
                    // Connecting the same device again replaces its token.
                    let replaced = tx.execute(
                        "DELETE FROM api_tokens WHERE user_id = ?1 AND name = ?2",
                        params![user_id, name],
                    )?;
                    let token_id = api_tokens::insert(
                        &tx,
                        user_id,
                        &name,
                        &prefix,
                        &token_hash,
                        TokenScope::Read,
                        None,
                    )?;
                    // Read-only, plus the scope that shows its owner's page as
                    // the owner sees it (`Viewer::for_token`).
                    api_tokens::add_scope(&tx, token_id, OWNER_SCOPE)?;
                    if replaced > 0 {
                        tracing::info!(user = %username, token = %name, replaced, "older API tokens of the same device revoked");
                    }
                    Some((username, name))
                }
            }
            _ => None,
        };
        tx.commit()?;
        Ok(granted)
    })
    .await;
    match redeemed {
        Ok(Some((username, name))) => {
            tracing::info!(user = %username, token = %name, scope = "read", "API token created");
            (
                [(header::CACHE_CONTROL, "no-store")],
                Json(json!({ "token": token })),
            )
                .into_response()
        }
        Ok(None) => invalid_grant(),
        // A busy database or a pool that makes the request wait too long is
        // not worth a 500 to the extension: it is told what it is told for
        // any other failure, and may try again.
        Err(err) if is_busy(&err) => {
            tracing::warn!("extension token exchange gave up on a busy database: {err}");
            invalid_grant()
        }
        Err(err) => {
            tracing::error!("extension token exchange failed: {err}");
            ApiError::internal(err).into_response()
        }
    }
}

/// A locked database (`SQLITE_BUSY`, `SQLITE_LOCKED`) or a connection pool
/// that timed out or closed.
fn is_busy(err: &db::DbError) -> bool {
    match err {
        db::DbError::Pool(_) => true,
        db::DbError::Sql(rusqlite::Error::SqliteFailure(failure, _)) => matches!(
            failure.code,
            rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked
        ),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_rfc_7636_vector_matches() {
        assert!(pkce_matches(
            "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk",
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        ));
        assert!(!pkce_matches(
            "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXl",
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        ));
    }

    #[test]
    fn busy_and_pool_errors_are_told_apart_from_real_ones() {
        let busy = rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_BUSY),
            None,
        );
        assert!(is_busy(&db::DbError::Sql(busy)));
        let locked = rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_LOCKED),
            None,
        );
        assert!(is_busy(&db::DbError::Sql(locked)));
        assert!(!is_busy(&db::DbError::Sql(
            rusqlite::Error::QueryReturnedNoRows
        )));
        assert!(!is_busy(&db::DbError::Interact("panic".into())));
    }

    #[test]
    fn shapes() {
        assert!(code_shape(&"a".repeat(43)));
        assert!(!code_shape(&"a".repeat(42)));
        assert!(!code_shape(&format!("{}+", "a".repeat(42))));
        assert!(verifier_shape(&"a".repeat(43)));
        assert!(verifier_shape(&"a".repeat(128)));
        assert!(!verifier_shape(&"a".repeat(42)));
        assert!(!verifier_shape(&"a".repeat(129)));
        assert!(!verifier_shape(&format!("{} ", "a".repeat(43))));
    }
}
