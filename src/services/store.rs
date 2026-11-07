//! The `services` and `service_users` tables.
//!
//! The secret is sealed here and opened only by [`fetch_ctx`]; every other
//! read leaves the column alone.

use rusqlite::{Connection, OptionalExtension as _, params};
use serde_json::{Map, Value as Json};
use vole_kit::crypto::{OpenError, Sealer};
use vole_kit::db::{self, DbError};
use vole_kit::state::Core;

use super::{
    FieldError, SECRET_PURPOSE, SecretChange, Service, ServiceInput, ServiceSummary, Viewer,
    Visibility, card_link, same_origin,
};
use crate::integrations::{FetchCtx, FetchError};

/// Why a service was not stored.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    /// The input breaks a rule of [`ServiceInput::check`] (or changes the
    /// kind of a stored service); nothing was written.
    #[error("the service is not valid")]
    Invalid(Vec<FieldError>),
    #[error("no such service")]
    NotFound,
    #[error(transparent)]
    Db(#[from] DbError),
}

/// Why a fetch context could not be made.
#[derive(Debug, thiserror::Error)]
pub enum CtxError {
    /// The sealed secret does not open (the instance key changed): it has to
    /// be entered again.
    #[error("the stored secret can't be opened ({0})")]
    SecretUnreadable(OpenError),
    /// The service was changed (or deleted) after the caller read it: what
    /// it holds is no longer what the caller meant to fetch.
    #[error("the service changed")]
    Changed,
    /// The service's settings can't be used (a pin that is not a fingerprint,
    /// or one on an address that is not https).
    #[error("the service's settings can't be used")]
    Config,
    #[error(transparent)]
    Db(#[from] DbError),
}

const COLUMNS: &str = "id, kind, name, url, settings, secret IS NOT NULL, interval_s, visibility, \
                       created_at, updated_at";

fn sealer(core: &Core) -> Sealer {
    Sealer::new(&core.session_key, SECRET_PURPOSE)
}

fn row(r: &rusqlite::Row<'_>) -> rusqlite::Result<Service> {
    let settings: String = r.get(4)?;
    // A settings column that is not a JSON object (changed outside Magpie)
    // reads as no settings.
    let settings = serde_json::from_str::<Json>(&settings)
        .ok()
        .filter(Json::is_object)
        .unwrap_or_else(|| Json::Object(Map::new()));
    let visibility: String = r.get(7)?;
    Ok(Service {
        id: r.get(0)?,
        kind: r.get(1)?,
        name: r.get(2)?,
        url: r.get(3)?,
        settings,
        has_secret: r.get(5)?,
        interval_s: r.get(6)?,
        visibility: Visibility::parse(&visibility).unwrap_or(Visibility::Admin),
        users: Vec::new(),
        created_at: r.get(8)?,
        updated_at: r.get(9)?,
    })
}

fn user_ids(c: &Connection, service_id: i64) -> rusqlite::Result<Vec<i64>> {
    let mut stmt =
        c.prepare("SELECT user_id FROM service_users WHERE service_id = ?1 ORDER BY user_id")?;
    stmt.query_map(params![service_id], |r| r.get(0))?.collect()
}

fn select_all(c: &Connection) -> rusqlite::Result<Vec<Service>> {
    let mut stmt = c.prepare(&format!(
        "SELECT {COLUMNS} FROM services ORDER BY name COLLATE NOCASE, id"
    ))?;
    let mut services: Vec<Service> = stmt.query_map([], row)?.collect::<Result<_, _>>()?;
    for service in &mut services {
        service.users = user_ids(c, service.id)?;
    }
    Ok(services)
}

fn select_one(c: &Connection, id: i64) -> rusqlite::Result<Option<Service>> {
    let found = c
        .query_row(
            &format!("SELECT {COLUMNS} FROM services WHERE id = ?1"),
            params![id],
            row,
        )
        .optional()?;
    let Some(mut service) = found else {
        return Ok(None);
    };
    service.users = user_ids(c, id)?;
    Ok(Some(service))
}

/// Every service, by name.
pub async fn list(core: &Core) -> Result<Vec<Service>, DbError> {
    db::interact(&core.db, |c| select_all(c)).await
}

/// One service.
pub async fn get(core: &Core, id: i64) -> Result<Option<Service>, DbError> {
    db::interact(&core.db, move |c| select_one(c, id)).await
}

/// The services `viewer` may see, by name. Admins see all of them; anyone
/// else sees the ones for everybody and the ones that name them.
pub async fn visible_to(
    core: &Core,
    viewer: impl Into<Viewer>,
) -> Result<Vec<ServiceSummary>, DbError> {
    let viewer = viewer.into();
    db::interact(&core.db, move |c| {
        let mut stmt = c.prepare(
            "SELECT id, kind, name, interval_s, url, settings FROM services s
             WHERE ?1 = 1 OR s.visibility = 'all'
                OR (s.visibility = 'users' AND EXISTS (
                      SELECT 1 FROM service_users su
                      WHERE su.service_id = s.id AND su.user_id = ?2))
             ORDER BY name COLLATE NOCASE, id",
        )?;
        stmt.query_map(params![viewer.admin, viewer.id], |r| {
            let url: Option<String> = r.get(4)?;
            let settings: String = r.get(5)?;
            let settings = serde_json::from_str::<Json>(&settings).unwrap_or(Json::Null);
            Ok(ServiceSummary {
                id: r.get(0)?,
                kind: r.get(1)?,
                name: r.get(2)?,
                interval_s: r.get(3)?,
                link: card_link(&settings, url.as_deref()),
            })
        })?
        .collect()
    })
    .await
}

/// The kind of the service `id` when `viewer` may see it, on a connection
/// (the layout save asks inside its transaction). `None` for a service that
/// does not exist and one that is hidden from `viewer`, so a caller cannot
/// tell the two apart.
pub fn visible_kind(
    c: &Connection,
    viewer: impl Into<Viewer>,
    id: i64,
) -> rusqlite::Result<Option<String>> {
    let viewer = viewer.into();
    c.query_row(
        "SELECT s.kind FROM services s
         WHERE s.id = ?3 AND (?1 = 1 OR s.visibility = 'all'
            OR (s.visibility = 'users' AND EXISTS (
                  SELECT 1 FROM service_users su
                  WHERE su.service_id = s.id AND su.user_id = ?2)))",
        params![viewer.admin, viewer.id, id],
        |r| r.get(0),
    )
    .optional()
}

/// Whether `viewer` may see the service `id` (false when there is none).
pub async fn can_see(core: &Core, viewer: impl Into<Viewer>, id: i64) -> Result<bool, DbError> {
    let viewer = viewer.into();
    db::interact(&core.db, move |c| {
        visible_kind(c, viewer, id).map(|kind| kind.is_some())
    })
    .await
}

/// The selected users of a service, for the rows of the `service_users`
/// table (only those who exist; only when the service is for selected users).
fn write_users(
    c: &Connection,
    service_id: i64,
    visibility: Visibility,
    users: &[i64],
) -> rusqlite::Result<()> {
    c.execute(
        "DELETE FROM service_users WHERE service_id = ?1",
        params![service_id],
    )?;
    if visibility == Visibility::Users {
        for user in users {
            c.execute(
                "INSERT OR IGNORE INTO service_users (service_id, user_id)
                 SELECT ?1, id FROM users WHERE id = ?2",
                params![service_id, user],
            )?;
        }
    }
    Ok(())
}

fn encode(input: &ServiceInput) -> (String, Option<String>, String) {
    let url = input
        .url
        .as_deref()
        .map(str::trim)
        .filter(|u| !u.is_empty())
        .map(str::to_string);
    let settings = serde_json::to_string(&super::canonical_settings(&input.settings))
        .unwrap_or_else(|_| "{}".into());
    (input.name.trim().to_string(), url, settings)
}

/// Stores a new service; its id.
pub async fn create(core: &Core, input: &ServiceInput) -> Result<i64, StoreError> {
    let errors = input.check();
    if !errors.is_empty() {
        return Err(StoreError::Invalid(errors));
    }
    let sealed = match &input.secret {
        SecretChange::Replace(secret) => Some(sealer(core).seal(secret)),
        SecretChange::Keep | SecretChange::Clear => None,
    };
    let (name, url, settings) = encode(input);
    let input = input.clone();
    Ok(db::interact(&core.db, move |c| {
        let tx = c.transaction()?;
        tx.execute(
            "INSERT INTO services
               (kind, name, url, settings, secret, interval_s, visibility, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7,
                     strftime('%Y-%m-%dT%H:%M:%SZ', 'now'), strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))",
            params![
                input.kind,
                name,
                url,
                settings,
                sealed,
                input.interval_s,
                input.visibility.as_str()
            ],
        )?;
        let id = tx.last_insert_rowid();
        write_users(&tx, id, input.visibility, &input.users)?;
        tx.commit()?;
        Ok(id)
    })
    .await?)
}

/// Replaces a service's name, address, settings, interval and viewers; its
/// secret follows `input.secret`. The kind cannot change.
pub async fn update(core: &Core, id: i64, input: &ServiceInput) -> Result<(), StoreError> {
    let errors = input.check();
    if !errors.is_empty() {
        return Err(StoreError::Invalid(errors));
    }
    let sealed = match &input.secret {
        SecretChange::Replace(secret) => Some(sealer(core).seal(secret)),
        SecretChange::Keep | SecretChange::Clear => None,
    };
    let (name, url, settings) = encode(input);
    let input = input.clone();
    db::interact(&core.db, move |c| {
        let tx = c.transaction()?;
        let stored: Option<(String, Option<String>, bool)> = tx
            .query_row(
                "SELECT kind, url, secret IS NOT NULL FROM services WHERE id = ?1",
                params![id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?;
        let Some((kind, old_url, had_secret)) = stored else {
            return Ok(Err(StoreError::NotFound));
        };
        if kind != input.kind {
            return Ok(Err(StoreError::Invalid(vec![FieldError::new(
                "kind", "err.kind",
            )])));
        }
        // A stored secret never follows a changed address: keeping it needs the
        // same origin, otherwise the key has to be typed again (or removed).
        if input.secret == SecretChange::Keep
            && had_secret
            && !same_origin(old_url.as_deref(), url.as_deref())
        {
            return Ok(Err(StoreError::Invalid(vec![FieldError::new(
                "secret",
                "err.secret_origin",
            )])));
        }
        tx.execute(
            "UPDATE services SET name = ?2, url = ?3, settings = ?4, interval_s = ?5,
                    visibility = ?6, updated_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now')
             WHERE id = ?1",
            params![
                id,
                name,
                url,
                settings,
                input.interval_s,
                input.visibility.as_str()
            ],
        )?;
        match &input.secret {
            SecretChange::Keep => {}
            SecretChange::Replace(_) => {
                tx.execute(
                    "UPDATE services SET secret = ?2 WHERE id = ?1",
                    params![id, sealed],
                )?;
            }
            SecretChange::Clear => {
                tx.execute(
                    "UPDATE services SET secret = NULL WHERE id = ?1",
                    params![id],
                )?;
            }
        }
        write_users(&tx, id, input.visibility, &input.users)?;
        tx.commit()?;
        Ok(Ok(()))
    })
    .await?
}

/// Deletes a service and its viewers; whether there was one.
pub async fn delete(core: &Core, id: i64) -> Result<bool, DbError> {
    db::interact(&core.db, move |c| {
        Ok(c.execute("DELETE FROM services WHERE id = ?1", params![id])? > 0)
    })
    .await
}

/// What the row of a service holds about its secret, read in one query so the
/// address, the secret and the time of the last change belong together: a
/// change that lands between two reads can't pair a new key with an old
/// address.
#[derive(Debug)]
pub struct StoredSecret {
    pub kind: String,
    pub url: Option<String>,
    pub updated_at: String,
    sealed: Option<String>,
}

impl StoredSecret {
    /// Whether a secret is stored.
    pub fn has_secret(&self) -> bool {
        self.sealed.is_some()
    }

    /// The secret, opened. `None` when there is none.
    pub fn open(&self, core: &Core) -> Result<Option<String>, CtxError> {
        self.sealed
            .as_deref()
            .map(|sealed| {
                sealer(core)
                    .open(sealed)
                    .map_err(CtxError::SecretUnreadable)
            })
            .transpose()
    }
}

/// The kind, address, sealed secret and `updated_at` of service `id`, from one
/// row. `None` when there is no such service.
pub async fn stored_secret(core: &Core, id: i64) -> Result<Option<StoredSecret>, DbError> {
    db::interact(&core.db, move |c| {
        c.query_row(
            "SELECT kind, url, updated_at, secret FROM services WHERE id = ?1",
            params![id],
            |r| {
                Ok(StoredSecret {
                    kind: r.get(0)?,
                    url: r.get(1)?,
                    updated_at: r.get(2)?,
                    sealed: r.get(3)?,
                })
            },
        )
        .optional()
    })
    .await
}

/// The stored secret of service `id`, opened. `None` when there is none.
pub async fn open_secret(core: &Core, id: i64) -> Result<Option<String>, CtxError> {
    match stored_secret(core, id).await? {
        Some(stored) => stored.open(core),
        None => Ok(None),
    }
}

impl CtxError {
    /// The fetch error to record when no context could be made.
    pub fn fetch_error(&self) -> FetchError {
        match self {
            CtxError::SecretUnreadable(_) => FetchError::SecretUnreadable,
            CtxError::Config => FetchError::Config,
            CtxError::Changed | CtxError::Db(_) => FetchError::Other,
        }
    }
}

/// What a fetch of `service` needs: its client (the shared one, or the one
/// that accepts only its pinned certificate), its settings with the address
/// added as `"url"`, and its secret, opened. The address and the secret come
/// from one row, and [`CtxError::Changed`] answers when that row is not the
/// `service` the caller holds (changed or deleted since it was read), so a new
/// key is never sent to an old address or the other way round.
pub async fn fetch_ctx(core: &Core, service: &Service) -> Result<FetchCtx, CtxError> {
    let stored = stored_secret(core, service.id)
        .await?
        .filter(|stored| stored.updated_at == service.updated_at)
        .ok_or(CtxError::Changed)?;
    let secret = if service.has_secret {
        stored.open(core)?
    } else {
        None
    };
    let mut settings = service.settings.as_object().cloned().unwrap_or_default();
    if let Some(url) = &stored.url {
        settings.insert("url".into(), Json::String(url.clone()));
    }
    let settings = Json::Object(settings);
    let http = crate::integrations::client_for(&settings).map_err(|_| CtxError::Config)?;
    Ok(FetchCtx {
        http,
        settings,
        secret,
    })
}
