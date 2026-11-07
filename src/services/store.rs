//! The `services` and `service_users` tables.
//!
//! The secret is sealed here and opened only by [`fetch_ctx`] and
//! [`open_secret_in`]; every other read leaves the column alone.
//!
//! Every function a page calls takes a [`Scope`] (or has a shared-only name):
//! the admin's pages work on the shared services, a user's own page on that
//! user's personal ones, and neither can reach the other's rows. The poller
//! alone reads every service ([`list_all`], [`get_any`]).

use rusqlite::{Connection, OptionalExtension as _, params};
use serde_json::{Map, Value as Json};
use vole_kit::crypto::{OpenError, Sealer};
use vole_kit::db::{self, DbError};
use vole_kit::state::Core;

use super::{
    FieldError, MAX_PERSONAL, SECRET_PURPOSE, Scope, SecretChange, Service, ServiceInput,
    ServiceSummary, Viewer, Visibility, card_link_for, same_origin,
};
use crate::integrations::{FetchCtx, FetchError, SERVICE_ID_KEY};

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
                       created_at, updated_at, owner_user_id";

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
        owner_user_id: r.get(10)?,
    })
}

fn user_ids(c: &Connection, service_id: i64) -> rusqlite::Result<Vec<i64>> {
    let mut stmt =
        c.prepare("SELECT user_id FROM service_users WHERE service_id = ?1 ORDER BY user_id")?;
    stmt.query_map(params![service_id], |r| r.get(0))?.collect()
}

/// The services of `scope`, or all of them (`None`), by name.
fn select_all(c: &Connection, scope: Option<Scope>) -> rusqlite::Result<Vec<Service>> {
    let mut stmt = c.prepare(&format!(
        "SELECT {COLUMNS} FROM services
         WHERE ?1 = 0 OR owner_user_id IS ?2
         ORDER BY name COLLATE NOCASE, id"
    ))?;
    let owner = scope.and_then(Scope::owner);
    let mut services: Vec<Service> = stmt
        .query_map(params![scope.is_some(), owner], row)?
        .collect::<Result<_, _>>()?;
    for service in &mut services {
        service.users = user_ids(c, service.id)?;
    }
    Ok(services)
}

/// The service `id` when it is of `scope` (or of any, `None`).
fn select_one(c: &Connection, scope: Option<Scope>, id: i64) -> rusqlite::Result<Option<Service>> {
    let owner = scope.and_then(Scope::owner);
    let found = c
        .query_row(
            &format!(
                "SELECT {COLUMNS} FROM services
                 WHERE id = ?1 AND (?2 = 0 OR owner_user_id IS ?3)"
            ),
            params![id, scope.is_some(), owner],
            row,
        )
        .optional()?;
    let Some(mut service) = found else {
        return Ok(None);
    };
    service.users = user_ids(c, id)?;
    Ok(Some(service))
}

/// Every shared service, by name (the admin's list).
pub async fn list(core: &Core) -> Result<Vec<Service>, DbError> {
    list_in(core, Scope::Shared).await
}

/// The services of `scope`, by name.
pub async fn list_in(core: &Core, scope: Scope) -> Result<Vec<Service>, DbError> {
    db::interact(&core.db, move |c| select_all(c, Some(scope))).await
}

/// Every service, shared and personal: for the poller, which fetches them all.
/// No page may use it.
pub async fn list_all(core: &Core) -> Result<Vec<Service>, DbError> {
    db::interact(&core.db, |c| select_all(c, None)).await
}

/// One shared service.
pub async fn get(core: &Core, id: i64) -> Result<Option<Service>, DbError> {
    get_in(core, Scope::Shared, id).await
}

/// One service of `scope`; `None` for one that is of the other kind, like one
/// that does not exist.
pub async fn get_in(core: &Core, scope: Scope, id: i64) -> Result<Option<Service>, DbError> {
    db::interact(&core.db, move |c| select_one(c, Some(scope), id)).await
}

/// One service, shared or personal: for the poller. No page may use it.
pub async fn get_any(core: &Core, id: i64) -> Result<Option<Service>, DbError> {
    db::interact(&core.db, move |c| select_one(c, None, id)).await
}

/// The services `viewer` may see, by name. Admins see all the shared ones;
/// anyone else sees the shared ones for everybody and the ones that name
/// them. A personal service is seen by its user and by nobody else, whatever
/// their role.
pub async fn visible_to(
    core: &Core,
    viewer: impl Into<Viewer>,
) -> Result<Vec<ServiceSummary>, DbError> {
    let viewer = viewer.into();
    db::interact(&core.db, move |c| {
        let mut stmt = c.prepare(
            "SELECT id, kind, name, interval_s, url, settings, owner_user_id IS NOT NULL
             FROM services s
             WHERE (s.owner_user_id IS NULL AND (?1 = 1 OR s.visibility = 'all'
                OR (s.visibility = 'users' AND EXISTS (
                      SELECT 1 FROM service_users su
                      WHERE su.service_id = s.id AND su.user_id = ?2))))
                OR s.owner_user_id = ?2
             ORDER BY name COLLATE NOCASE, id",
        )?;
        stmt.query_map(params![viewer.admin, viewer.id], |r| {
            let kind: String = r.get(1)?;
            let url: Option<String> = r.get(4)?;
            let settings: String = r.get(5)?;
            let settings = serde_json::from_str::<Json>(&settings).unwrap_or(Json::Null);
            Ok(ServiceSummary {
                id: r.get(0)?,
                kind: kind.clone(),
                name: r.get(2)?,
                interval_s: r.get(3)?,
                link: card_link_for(Some(&kind), &settings, url.as_deref()),
                personal: r.get(6)?,
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
         WHERE s.id = ?3 AND ((s.owner_user_id IS NULL AND (?1 = 1 OR s.visibility = 'all'
            OR (s.visibility = 'users' AND EXISTS (
                  SELECT 1 FROM service_users su
                  WHERE su.service_id = s.id AND su.user_id = ?2))))
            OR s.owner_user_id = ?2)",
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

/// What is wrong with `input` for `scope` besides [`ServiceInput::check`]: a
/// personal kind (one with a fixed address and a key of the user's own) is
/// only for personal services, and a shared kind only for shared ones.
fn check_scope(scope: Scope, input: &ServiceInput) -> Vec<FieldError> {
    match input.integration() {
        Some(kind) if kind.personal() != scope.is_personal() => {
            vec![FieldError::new("kind", "err.kind")]
        }
        _ => Vec::new(),
    }
}

/// Stores a new shared service; its id.
pub async fn create(core: &Core, input: &ServiceInput) -> Result<i64, StoreError> {
    create_in(core, Scope::Shared, input).await
}

/// Stores a new service of `scope`; its id. A user may have at most
/// [`MAX_PERSONAL`] personal services (a form error on the name). A personal
/// service is for its user only: whatever visibility and users the input names, nobody else
/// sees it.
pub async fn create_in(core: &Core, scope: Scope, input: &ServiceInput) -> Result<i64, StoreError> {
    let mut errors = input.check();
    errors.extend(check_scope(scope, input));
    if !errors.is_empty() {
        return Err(StoreError::Invalid(errors));
    }
    let sealed = match &input.secret {
        SecretChange::Replace(secret) => Some(sealer(core).seal(secret)),
        SecretChange::Keep | SecretChange::Clear => None,
    };
    let (name, url, settings) = encode(input);
    let mut input = input.clone();
    if scope.is_personal() {
        input.visibility = Visibility::Users;
        input.users.clear();
    }
    let owner = scope.owner();
    db::interact(&core.db, move |c| {
        // Immediate: the count and the insert are one step, so two requests
        // at once cannot both pass the cap.
        let tx = c.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        if let Some(user) = owner {
            let have: i64 = tx.query_row(
                "SELECT COUNT(*) FROM services WHERE owner_user_id = ?1",
                params![user],
                |r| r.get(0),
            )?;
            if have >= MAX_PERSONAL {
                return Ok(Err(StoreError::Invalid(vec![FieldError::new(
                    "name",
                    "err.personal_limit",
                )])));
            }
        }
        tx.execute(
            "INSERT INTO services
               (kind, name, url, settings, secret, interval_s, visibility, owner_user_id,
                created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8,
                     strftime('%Y-%m-%dT%H:%M:%SZ', 'now'), strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))",
            params![
                input.kind,
                name,
                url,
                settings,
                sealed,
                input.interval_s,
                input.visibility.as_str(),
                owner
            ],
        )?;
        let id = tx.last_insert_rowid();
        write_users(&tx, id, input.visibility, &input.users)?;
        tx.commit()?;
        Ok(Ok(id))
    })
    .await?
}

/// Replaces a shared service's name, address, settings, interval and viewers;
/// its secret follows `input.secret`. The kind cannot change.
pub async fn update(core: &Core, id: i64, input: &ServiceInput) -> Result<(), StoreError> {
    update_in(core, Scope::Shared, id, input).await
}

/// [`update`] for a service of `scope`; a service of the other kind is
/// [`StoreError::NotFound`]. A personal service stays its user's own and
/// visible to nobody else.
pub async fn update_in(
    core: &Core,
    scope: Scope,
    id: i64,
    input: &ServiceInput,
) -> Result<(), StoreError> {
    let mut errors = input.check();
    errors.extend(check_scope(scope, input));
    if !errors.is_empty() {
        return Err(StoreError::Invalid(errors));
    }
    let sealed = match &input.secret {
        SecretChange::Replace(secret) => Some(sealer(core).seal(secret)),
        SecretChange::Keep | SecretChange::Clear => None,
    };
    let (name, url, settings) = encode(input);
    let mut input = input.clone();
    if scope.is_personal() {
        input.visibility = Visibility::Users;
        input.users.clear();
    }
    let owner = scope.owner();
    db::interact(&core.db, move |c| {
        let tx = c.transaction()?;
        let stored: Option<(String, Option<String>, bool)> = tx
            .query_row(
                "SELECT kind, url, secret IS NOT NULL FROM services
                 WHERE id = ?1 AND owner_user_id IS ?2",
                params![id, owner],
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

/// Deletes a shared service and its viewers; whether there was one.
pub async fn delete(core: &Core, id: i64) -> Result<bool, DbError> {
    delete_in(core, Scope::Shared, id).await
}

/// Deletes a service of `scope` and what goes with it (its viewers, snapshot
/// and acknowledgements); whether there was one.
pub async fn delete_in(core: &Core, scope: Scope, id: i64) -> Result<bool, DbError> {
    let owner = scope.owner();
    db::interact(&core.db, move |c| {
        Ok(c.execute(
            "DELETE FROM services WHERE id = ?1 AND owner_user_id IS ?2",
            params![id, owner],
        )? > 0)
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

/// The kind, address, sealed secret and `updated_at` of service `id` when it
/// is of `scope` (`None`: of any, for the fetch), from one row. `None` when
/// there is no such service.
pub async fn stored_secret_in(
    core: &Core,
    scope: Option<Scope>,
    id: i64,
) -> Result<Option<StoredSecret>, DbError> {
    let owner = scope.and_then(Scope::owner);
    let scoped = scope.is_some();
    db::interact(&core.db, move |c| {
        c.query_row(
            "SELECT kind, url, updated_at, secret FROM services
             WHERE id = ?1 AND (?2 = 0 OR owner_user_id IS ?3)",
            params![id, scoped, owner],
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

/// [`stored_secret_in`] for a service of any scope.
pub async fn stored_secret(core: &Core, id: i64) -> Result<Option<StoredSecret>, DbError> {
    stored_secret_in(core, None, id).await
}

/// The stored secret of the shared service `id`, opened. `None` when there is
/// none.
pub async fn open_secret(core: &Core, id: i64) -> Result<Option<String>, CtxError> {
    open_secret_in(core, Some(Scope::Shared), id).await
}

/// The stored secret of service `id` when it is of `scope` (`None`: of any),
/// opened. `None` when there is none.
pub async fn open_secret_in(
    core: &Core,
    scope: Option<Scope>,
    id: i64,
) -> Result<Option<String>, CtxError> {
    match stored_secret_in(core, scope, id).await? {
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
/// added as `"url"` and the service's number as `"service_id"` (what a kind
/// keeps its memory of the service under), and its secret, opened. Both names
/// are the store's alone: a stored setting of that name is dropped. The address
/// and the secret come from one row, and [`CtxError::Changed`] answers when that
/// row is not the `service` the caller holds (changed or deleted since it was
/// read), so a new key is never sent to an old address or the other way round.
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
    settings.remove("url");
    settings.remove(SERVICE_ID_KEY);
    if let Some(url) = &stored.url {
        settings.insert("url".into(), Json::String(url.clone()));
    }
    settings.insert(SERVICE_ID_KEY.into(), Json::from(service.id));
    let settings = Json::Object(settings);
    let http = crate::integrations::client_for(&settings).map_err(|_| CtxError::Config)?;
    Ok(FetchCtx {
        http,
        settings,
        secret,
    })
}
