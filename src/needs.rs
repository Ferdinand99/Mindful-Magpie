//! «Trenger deg»: the needs of the services a user's start page shows, and
//! the user's acknowledgements of them.
//!
//! **Which needs.** Those of the services that are bound on the user's own
//! layout and visible to them (the list [`crate::values`] builds the values
//! document from), and for the `config` need (a service whose settings or
//! secret can't be used) only an admin's: it asks for something only an admin
//! can do. A need shows nothing but the translated label of its fixed text
//! key (see [`label_of`]) with the service's name filled in; no other text of
//! a snapshot reaches a page.
//!
//! **The rule.** An acknowledgement hides a need for that user and nobody
//! else. The need shows again when
//!
//! - its level has risen (warn to bad),
//! - its score has risen above the score it had when it was acknowledged,
//! - it began after the acknowledgement (see [`Need::since`]), or
//! - it went and came back: the poller deletes the acknowledgements of a need
//!   that is gone from a service's latest snapshot ([`clear_gone`]), so a need
//!   that returns has none.
//!
//! The level and the score of an acknowledgement are read from the server's
//! latest snapshot when it is made, never from the request. A need that is
//! acknowledged is still in the list, with `acked: true`, for the «N
//! kvittert» toggle.
//!
//! **Routes.** `POST /start/needs/ack` and `POST /start/needs/unack` take the
//! JSON body `{ "service_id": n, "key": "…" }` and answer `{ "needs": [...] }`,
//! the user's list as it is now (the page applies it at once; the user's open
//! streams are woken too, so the other tabs follow). A service that is hidden
//! from the user, not bound on their layout or without that need (and the
//! `config` need for a non-admin) is a plain 404 that says nothing else: it is
//! not told apart from a service that does not exist. The routes are the page's:
//! a session and the kit's same-origin proof, like every other post; a token
//! is turned away.

use std::collections::HashMap;
use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, Extension, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use jiff::Timestamp;
use rusqlite::params;
use serde::{Deserialize, Serialize};
use serde_json::json;
use vole_kit::auth::{AuthMethod, CurrentUser};
use vole_kit::db::{self, DbError};
use vole_kit::http::ApiError;
use vole_kit::i18n;
use vole_kit::state::Core;

use crate::Magpie;
use crate::integrations::poller::{CONFIG_KEY, PollerHandle};
use crate::integrations::{Level, Need, Snapshot};
use crate::layout;
use crate::services::{ServiceSummary, Viewer};
use crate::values::bound_visible;

/// The largest body of an ack route (a number and a short key).
const MAX_BODY: usize = 4096;

/// One need as the page shows it: a line of the «Trenger deg» row.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct NeedView {
    pub service_id: i64,
    pub service_name: String,
    /// The need's key within the service (what an acknowledgement refers to).
    pub key: String,
    pub level: Level,
    /// The need's text in the request's language, the service's name in it.
    pub label: String,
    /// Unix seconds: when the need began.
    pub since: i64,
    /// The user has acknowledged it and it has not got worse since.
    pub acked: bool,
}

/// What a user acknowledged: the need as it was then.
#[derive(Debug, Clone, PartialEq)]
pub struct Ack {
    pub level: Level,
    pub score: f64,
    pub acked_at: Timestamp,
}

/// A user's acknowledgements by service id and need key.
pub type Acks = HashMap<(i64, String), Ack>;

fn level_text(level: Level) -> &'static str {
    match level {
        Level::Warn => "warn",
        Level::Bad => "bad",
    }
}

fn parse_level(text: &str) -> Option<Level> {
    match text {
        "warn" => Some(Level::Warn),
        "bad" => Some(Level::Bad),
        _ => None,
    }
}

/// The need's text: its label key's translation with the service's name. The
/// key is read from stored data, so only the fixed `need.*` texts are used;
/// anything else reads as the general text of a need.
fn label_of(need: &Need, service_name: &str) -> String {
    let lang = i18n::current();
    let text = need
        .label_key
        .strip_prefix("need.")
        .and_then(|_| i18n::lookup(lang, &need.label_key))
        .or_else(|| i18n::lookup(lang, "need.other"))
        .unwrap_or("{name}");
    i18n::interpolate(text, &[("name", service_name)])
}

/// Whether `ack` still hides `need` (see the module for the rule).
fn hides(need: &Need, ack: &Ack) -> bool {
    need.level <= ack.level
        && need.score <= ack.score
        && need.since.is_none_or(|since| since <= ack.acked_at)
}

/// The needs of `services`, each with its latest snapshot, for a user who is
/// an admin or not, and the user's `acks`: bad before warn, then by service
/// name and key. Pure: the caller reads the snapshots and the acknowledgements.
pub fn needs_for(
    admin: bool,
    services: &[(&ServiceSummary, Option<Arc<Snapshot>>)],
    acks: &Acks,
) -> Vec<NeedView> {
    let mut out = Vec::new();
    for (service, snapshot) in services {
        let Some(snapshot) = snapshot else { continue };
        for need in &snapshot.needs {
            if need.key == CONFIG_KEY && !admin {
                continue;
            }
            let acked = acks
                .get(&(service.id, need.key.clone()))
                .is_some_and(|ack| hides(need, ack));
            out.push(NeedView {
                service_id: service.id,
                service_name: service.name.clone(),
                key: need.key.clone(),
                level: need.level,
                label: label_of(need, &service.name),
                since: need.since.unwrap_or(snapshot.fetched_at).as_second(),
                acked,
            });
        }
    }
    out.sort_by(|a, b| {
        b.level
            .cmp(&a.level)
            .then_with(|| {
                a.service_name
                    .to_lowercase()
                    .cmp(&b.service_name.to_lowercase())
            })
            .then_with(|| a.service_id.cmp(&b.service_id))
            .then_with(|| a.key.cmp(&b.key))
    });
    out
}

/// The needs of `viewer` for `services` (the bound, visible ones), with the
/// acknowledgements read from the database.
pub async fn needs_of(
    core: &Core,
    poller: &PollerHandle,
    viewer: Viewer,
    services: &[ServiceSummary],
) -> Result<Vec<NeedView>, DbError> {
    if services.is_empty() {
        return Ok(Vec::new());
    }
    let acks = acks_of(core, viewer.id).await?;
    let with_snapshots: Vec<(&ServiceSummary, Option<Arc<Snapshot>>)> = services
        .iter()
        .map(|s| (s, poller.snapshot(s.id)))
        .collect();
    Ok(needs_for(viewer.admin, &with_snapshots, &acks))
}

// ---------------------------------------------------------------------------
// The table

/// Everything `user_id` has acknowledged.
pub async fn acks_of(core: &Core, user_id: i64) -> Result<Acks, DbError> {
    db::interact(&core.db, move |c| {
        let mut stmt = c.prepare(
            "SELECT service_id, need_key, level, score, acked_at FROM acks WHERE user_id = ?1",
        )?;
        let rows = stmt.query_map([user_id], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, f64>(3)?,
                r.get::<_, String>(4)?,
            ))
        })?;
        let mut acks = Acks::new();
        for row in rows {
            let (service, key, level, score, at) = row?;
            // A row that does not read (it was written by something else) hides nothing.
            if let (Some(level), Ok(at)) = (parse_level(&level), at.parse::<Timestamp>()) {
                acks.insert(
                    (service, key),
                    Ack {
                        level,
                        score,
                        acked_at: at,
                    },
                );
            }
        }
        Ok(acks)
    })
    .await
}

/// Records that `user_id` acknowledged the need `key` of the service, as it is
/// now (`level` and `score`), replacing an earlier acknowledgement.
pub async fn ack(
    core: &Core,
    user_id: i64,
    service_id: i64,
    key: &str,
    level: Level,
    score: f64,
) -> Result<(), DbError> {
    let (key, at) = (key.to_string(), Timestamp::now().to_string());
    db::interact(&core.db, move |c| {
        c.execute(
            "INSERT INTO acks (user_id, service_id, need_key, level, score, acked_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT (user_id, service_id, need_key)
             DO UPDATE SET level = excluded.level, score = excluded.score,
                           acked_at = excluded.acked_at",
            params![user_id, service_id, key, level_text(level), score, at],
        )
        .map(|_| ())
    })
    .await
}

/// Takes `user_id`'s acknowledgement of the need back.
pub async fn unack(core: &Core, user_id: i64, service_id: i64, key: &str) -> Result<(), DbError> {
    let key = key.to_string();
    db::interact(&core.db, move |c| {
        c.execute(
            "DELETE FROM acks WHERE user_id = ?1 AND service_id = ?2 AND need_key = ?3",
            params![user_id, service_id, key],
        )
        .map(|_| ())
    })
    .await
}

/// Deletes everybody's acknowledgements of the service's needs that are not
/// among `keys`, the need keys of its latest snapshot: the poller calls it when
/// a need has gone, so that one that comes back is shown again.
pub async fn clear_gone(core: &Core, service_id: i64, keys: Vec<String>) -> Result<(), DbError> {
    db::interact(&core.db, move |c| {
        let marks = vec!["?"; keys.len()].join(", ");
        let sql = format!("DELETE FROM acks WHERE service_id = ?1 AND need_key NOT IN ({marks})");
        let mut args: Vec<&dyn rusqlite::ToSql> = vec![&service_id];
        args.extend(keys.iter().map(|k| k as &dyn rusqlite::ToSql));
        c.execute(&sql, args.as_slice()).map(|_| ())
    })
    .await
}

// ---------------------------------------------------------------------------
// The routes

pub fn router() -> Router<Core> {
    Router::new()
        .route(
            "/start/needs/ack",
            post(ack_route).layer(DefaultBodyLimit::max(MAX_BODY)),
        )
        .route(
            "/start/needs/unack",
            post(unack_route).layer(DefaultBodyLimit::max(MAX_BODY)),
        )
}

#[derive(Deserialize)]
struct Target {
    service_id: i64,
    key: String,
}

/// The answer to everything that is not the user's to acknowledge.
fn not_found() -> Response {
    (StatusCode::NOT_FOUND, Json(json!({ "error": "not_found" }))).into_response()
}

async fn ack_route(
    State(core): State<Core>,
    Extension(magpie): Extension<Magpie>,
    user: CurrentUser,
    method: Option<Extension<AuthMethod>>,
    body: Bytes,
) -> Result<Response, ApiError> {
    change(&core, &magpie, &user, method, &body, true).await
}

async fn unack_route(
    State(core): State<Core>,
    Extension(magpie): Extension<Magpie>,
    user: CurrentUser,
    method: Option<Extension<AuthMethod>>,
    body: Bytes,
) -> Result<Response, ApiError> {
    change(&core, &magpie, &user, method, &body, false).await
}

/// Makes (`acknowledge`) or takes back an acknowledgement for the signed-in
/// user, and answers with their list.
async fn change(
    core: &Core,
    magpie: &Magpie,
    user: &CurrentUser,
    method: Option<Extension<AuthMethod>>,
    body: &[u8],
    acknowledge: bool,
) -> Result<Response, ApiError> {
    if !matches!(method, Some(Extension(AuthMethod::Session))) {
        return Err(ApiError::Unauthorized);
    }
    let Ok(target) = serde_json::from_slice::<Target>(body) else {
        let body = Json(json!({ "error": "bad_request" }));
        return Ok((StatusCode::BAD_REQUEST, body).into_response());
    };
    let viewer = Viewer::from(user);
    let (layout, _) = layout::load_or_default(core, user.id).await?;
    let services = bound_visible(core, viewer, &layout).await?;
    let Some(service) = services.iter().find(|s| s.id == target.service_id) else {
        return Ok(not_found());
    };
    let Some(snapshot) = magpie.poller.snapshot(service.id) else {
        return Ok(not_found());
    };
    let current = snapshot
        .needs
        .iter()
        .find(|n| n.key == target.key && (viewer.admin || n.key != CONFIG_KEY));
    let Some(need) = current else {
        return Ok(not_found());
    };
    if acknowledge {
        // What the need is now, from the server's snapshot, not from the request.
        ack(core, user.id, service.id, &need.key, need.level, need.score).await?;
    } else {
        unack(core, user.id, service.id, &need.key).await?;
    }
    // The user's other tabs follow.
    let _ = magpie.ack_events.send(user.id);
    let needs = needs_of(core, &magpie.poller, viewer, &services).await?;
    Ok((
        [(header::CACHE_CONTROL, "no-store")],
        Json(json!({ "needs": needs })),
    )
        .into_response())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn service(id: i64, name: &str) -> ServiceSummary {
        ServiceSummary {
            id,
            kind: "http".into(),
            name: name.into(),
            interval_s: 60,
        }
    }

    fn need(key: &str, level: Level, score: f64) -> Need {
        Need {
            key: key.into(),
            level,
            label_key: "need.http_down".into(),
            params: BTreeMap::new(),
            score,
            since: None,
        }
    }

    fn ack_of(level: Level, score: f64) -> Ack {
        Ack {
            level,
            score,
            acked_at: Timestamp::now(),
        }
    }

    #[test]
    fn the_rule_in_one_place() {
        let acked = ack_of(Level::Warn, 88.0);
        assert!(hides(&need("d", Level::Warn, 88.0), &acked));
        assert!(hides(&need("d", Level::Warn, 70.0), &acked));
        assert!(!hides(&need("d", Level::Warn, 88.5), &acked));
        assert!(!hides(&need("d", Level::Bad, 88.0), &acked));
        let mut later = need("d", Level::Warn, 88.0);
        later.since = Some(Timestamp::now() + jiff::SignedDuration::from_secs(5));
        assert!(!hides(&later, &acked));
    }

    #[test]
    fn a_service_without_a_snapshot_has_no_needs_and_config_is_for_admins() {
        crate::install();
        let blog = service(1, "Blog");
        let wiki = service(2, "Wiki");
        let mut snapshot = Snapshot::new(crate::integrations::Status::Bad);
        snapshot.needs = vec![
            need(CONFIG_KEY, Level::Bad, 1.0),
            need("disk", Level::Warn, 90.0),
        ];
        let services = [(&blog, Some(Arc::new(snapshot))), (&wiki, None)];
        let acks = Acks::new();
        assert_eq!(needs_for(false, &services, &acks).len(), 1);
        assert_eq!(needs_for(true, &services, &acks).len(), 2);
    }
}
