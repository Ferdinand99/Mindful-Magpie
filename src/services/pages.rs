//! The admin's Settings page for services.
//!
//! - `GET  /settings/services`: the list
//! - `GET  /settings/services/new[?kind=…]`, `POST /settings/services/new`
//! - `GET  /settings/services/{id}`, `POST /settings/services/{id}`
//! - `POST /settings/services/{id}/delete`
//! - `POST /settings/services/test`: one fetch with the form's values, shown
//!   as a fragment; it never saves.
//!
//! Every route is for admins. The forms never show a secret: the edit form
//! has an empty password input whose placeholder says one is saved, and an
//! empty input on save keeps the stored secret. A refused form is shown again
//! with the secret field empty.

use std::collections::BTreeMap;

use askama::Template;
use axum::Router;
use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, Extension, Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum_extra::extract::cookie::SignedCookieJar;
use serde::Deserialize;
use serde_json::{Map, Value as Json};
use vole_kit::auth::CurrentUser;
use vole_kit::http::error::ApiError;
use vole_kit::http::{ApiResult, HtmlTemplate};
use vole_kit::i18n::{self, Tr as _};
use vole_kit::page::{PageCtx, flash};
use vole_kit::state::Core;

use super::store::{self, StoreError};
use super::{
    FieldError, SecretChange, Service, ServiceEvent, ServiceInput, Visibility, same_origin,
    without_userinfo,
};
use crate::Magpie;
use crate::integrations::{
    self, FetchCtx, FetchError, FieldKind, Format, Integration, Snapshot, Status, Value,
};

/// The most a service form is allowed to send.
const MAX_FORM: usize = 64 * 1024;

/// The routes (the caller adds the sign-in guard).
pub fn router() -> Router<Core> {
    Router::new()
        .route("/settings/services", get(list))
        .route("/settings/services/new", get(new_form).post(create))
        .route("/settings/services/test", post(test))
        .route("/settings/services/{id}", get(edit_form).post(update))
        .route("/settings/services/{id}/delete", post(delete))
        .layer(DefaultBodyLimit::max(MAX_FORM))
}

// ---------------------------------------------------------------------------
// The posted form

/// A form body: the names and values in the order they came.
struct Raw(Vec<(String, String)>);

impl Raw {
    fn parse(body: &[u8]) -> Raw {
        Raw(url::form_urlencoded::parse(body).into_owned().collect())
    }

    /// The first value called `name`, or "".
    fn get(&self, name: &str) -> &str {
        self.0
            .iter()
            .find(|(n, _)| n == name)
            .map_or("", |(_, v)| v.as_str())
    }

    fn all<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a str> {
        self.0
            .iter()
            .filter(move |(n, _)| n == name)
            .map(|(_, v)| v.as_str())
    }
}

/// A service form read into an input, with what was typed (to show again) and
/// what was wrong with it.
struct Parsed {
    input: ServiceInput,
    interval: String,
    settings: BTreeMap<&'static str, String>,
    errors: Vec<FieldError>,
}

fn parse(form: &Raw, kind: &'static dyn Integration) -> Parsed {
    let mut errors = Vec::new();
    let mut settings = Map::new();
    let mut typed = BTreeMap::new();
    for field in kind.fields().iter().filter(|f| !f.secret) {
        let name = format!("f_{}", field.key);
        let text = form.get(&name).trim().to_string();
        typed.insert(field.key, text.clone());
        if text.is_empty() {
            continue;
        }
        match field.kind {
            FieldKind::Text => {
                settings.insert(field.key.to_string(), Json::String(text));
            }
            FieldKind::Number => match text.parse::<i64>() {
                Ok(n) => {
                    settings.insert(field.key.to_string(), Json::from(n));
                }
                Err(_) => errors.push(FieldError {
                    field: name,
                    key: "err.number",
                }),
            },
        }
    }

    let secret = if !kind.fields().iter().any(|f| f.secret) {
        SecretChange::Keep
    } else if form.get("clear_secret") == "true" {
        SecretChange::Clear
    } else {
        match form.get("secret").trim() {
            "" => SecretChange::Keep,
            secret => SecretChange::Replace(secret.to_string()),
        }
    };

    let default_s = i64::try_from(kind.default_interval().as_secs()).unwrap_or(60);
    let interval = form.get("interval").trim().to_string();
    let interval_s = if interval.is_empty() {
        default_s
    } else {
        interval.parse::<i64>().unwrap_or_else(|_| {
            errors.push(FieldError {
                field: "interval".into(),
                key: "err.interval",
            });
            default_s
        })
    };

    let visibility = match form.get("visibility") {
        "" => Visibility::Admin,
        text => Visibility::parse(text).unwrap_or_else(|| {
            errors.push(FieldError {
                field: "visibility".into(),
                key: "err.visibility",
            });
            Visibility::Admin
        }),
    };

    let url = Some(form.get("url").trim().to_string()).filter(|u| !u.is_empty());
    Parsed {
        input: ServiceInput {
            kind: kind.kind().to_string(),
            name: form.get("name").trim().to_string(),
            url,
            settings: Json::Object(settings),
            secret,
            interval_s,
            visibility,
            users: form.all("users").filter_map(|u| u.parse().ok()).collect(),
        },
        interval,
        settings: typed,
        errors,
    }
}

// ---------------------------------------------------------------------------
// The list

struct Row {
    id: i64,
    name: String,
    kind: String,
    /// The kind's label key, when this build has the kind.
    kind_label: Option<&'static str>,
    url: String,
    visibility: Visibility,
    viewers: usize,
    has_secret: bool,
}

impl Row {
    fn new(s: Service) -> Row {
        Row {
            id: s.id,
            kind_label: s.integration().map(Integration::label_key),
            name: s.name,
            kind: s.kind,
            url: s.url.unwrap_or_default(),
            visibility: s.visibility,
            viewers: s.users.len(),
            has_secret: s.has_secret,
        }
    }
}

#[derive(Template)]
#[template(path = "services.html")]
struct ListPage {
    ctx: PageCtx,
    rows: Vec<Row>,
}

async fn list(
    State(core): State<Core>,
    user: CurrentUser,
    jar: SignedCookieJar,
) -> ApiResult<Response> {
    user.require_admin()?;
    let rows = store::list(&core)
        .await?
        .into_iter()
        .map(Row::new)
        .collect();
    let (ctx, jar) = PageCtx::build(
        &core,
        Some(&user),
        &jar,
        i18n::t("nav.services"),
        "settings",
        "services",
    )
    .await;
    Ok((jar, HtmlTemplate(ListPage { ctx, rows })).into_response())
}

// ---------------------------------------------------------------------------
// The form

struct SettingView {
    name: String,
    label_key: &'static str,
    help: &'static str,
    placeholder: &'static str,
    required: bool,
    value: String,
    error: String,
}

struct SecretView {
    label_key: &'static str,
    help: &'static str,
}

struct UserChoice {
    id: i64,
    name: String,
    checked: bool,
}

struct KindChoice {
    kind: &'static str,
    label_key: &'static str,
}

struct FormView {
    /// Where the form posts.
    action: String,
    id: Option<i64>,
    kind: &'static str,
    kind_label: &'static str,
    /// The kinds to choose from, instead of a form.
    choose: Vec<KindChoice>,
    name: String,
    url: String,
    url_required: bool,
    interval: String,
    interval_default: u64,
    interval_min: u64,
    visibility: Visibility,
    settings: Vec<SettingView>,
    secret: Option<SecretView>,
    has_secret: bool,
    /// The stored secret can't be opened any more.
    secret_unreadable: bool,
    users: Vec<UserChoice>,
    name_error: String,
    url_error: String,
    interval_error: String,
    visibility_error: String,
    secret_error: String,
}

#[derive(Template)]
#[template(path = "service_form.html")]
struct FormPage {
    ctx: PageCtx,
    form: FormView,
}

/// What a form shows: the typed values and what was wrong.
struct Shown {
    name: String,
    url: String,
    interval: String,
    settings: BTreeMap<&'static str, String>,
    visibility: Visibility,
    users: Vec<i64>,
    errors: Vec<FieldError>,
}

impl Shown {
    fn empty() -> Shown {
        Shown {
            name: String::new(),
            url: String::new(),
            interval: String::new(),
            settings: BTreeMap::new(),
            visibility: Visibility::Admin,
            users: Vec::new(),
            errors: Vec::new(),
        }
    }

    fn of(service: &Service) -> Shown {
        let settings = service.settings.as_object();
        Shown {
            name: service.name.clone(),
            url: service.url.clone().unwrap_or_default(),
            interval: service.interval_s.to_string(),
            settings: service
                .integration()
                .map(|kind| {
                    kind.fields()
                        .iter()
                        .filter(|f| !f.secret)
                        .map(|f| {
                            let value = match settings.and_then(|s| s.get(f.key)) {
                                Some(Json::String(text)) => text.clone(),
                                Some(Json::Number(n)) => n.to_string(),
                                _ => String::new(),
                            };
                            (f.key, value)
                        })
                        .collect()
                })
                .unwrap_or_default(),
            visibility: service.visibility,
            users: service.users.clone(),
            errors: Vec::new(),
        }
    }

    fn typed(parsed: Parsed, form: &Raw, errors: Vec<FieldError>) -> Shown {
        Shown {
            name: parsed.input.name,
            // The address is shown again without a user name and password.
            url: without_userinfo(form.get("url").trim()),
            interval: parsed.interval,
            settings: parsed.settings,
            visibility: parsed.input.visibility,
            users: parsed.input.users,
            errors,
        }
    }
}

/// The people who can be chosen as viewers: everyone but the admins, who see
/// every service anyway.
async fn user_choices(core: &Core, chosen: &[i64]) -> ApiResult<Vec<UserChoice>> {
    let users = core
        .with_conn(|c| vole_kit::db::users::list_users(c))
        .await?;
    Ok(users
        .into_iter()
        .filter(|u| !u.role.is_admin())
        .map(|u| UserChoice {
            checked: chosen.contains(&u.id),
            name: u.name().to_string(),
            id: u.id,
        })
        .collect())
}

async fn form_view(
    core: &Core,
    kind: &'static dyn Integration,
    id: Option<i64>,
    has_secret: bool,
    shown: Shown,
) -> ApiResult<FormView> {
    let error = |field: &str| {
        shown
            .errors
            .iter()
            .find(|e| e.field == field)
            .map(|e| i18n::t(e.key).to_string())
            .unwrap_or_default()
    };
    let settings = kind
        .fields()
        .iter()
        .filter(|f| !f.secret)
        .map(|f| SettingView {
            name: format!("f_{}", f.key),
            label_key: f.label_key,
            help: f.help_key,
            placeholder: f.placeholder,
            required: f.required,
            value: shown.settings.get(f.key).cloned().unwrap_or_default(),
            error: error(&format!("f_{}", f.key)),
        })
        .collect();
    let secret = kind.fields().iter().find(|f| f.secret).map(|f| SecretView {
        label_key: f.label_key,
        help: f.help_key,
    });
    Ok(FormView {
        action: match id {
            Some(id) => format!("/settings/services/{id}"),
            None => "/settings/services/new".to_string(),
        },
        id,
        kind: kind.kind(),
        kind_label: kind.label_key(),
        choose: Vec::new(),
        url_required: kind.url_required(),
        interval_default: kind.default_interval().as_secs(),
        interval_min: kind.min_interval().as_secs(),
        users: user_choices(core, &shown.users).await?,
        name_error: error("name"),
        url_error: error("url"),
        interval_error: error("interval"),
        visibility_error: error("visibility"),
        secret_error: error("secret"),
        name: shown.name.clone(),
        url: shown.url.clone(),
        interval: shown.interval.clone(),
        visibility: shown.visibility,
        settings,
        secret,
        has_secret,
        secret_unreadable: false,
    })
}

/// The kind chooser, as a form view that has no form.
fn chooser() -> FormView {
    FormView {
        action: String::new(),
        id: None,
        kind: "",
        kind_label: "",
        choose: integrations::registry()
            .iter()
            .map(|k| KindChoice {
                kind: k.kind(),
                label_key: k.label_key(),
            })
            .collect(),
        name: String::new(),
        url: String::new(),
        url_required: false,
        interval: String::new(),
        interval_default: 0,
        interval_min: 0,
        visibility: Visibility::Admin,
        settings: Vec::new(),
        secret: None,
        has_secret: false,
        secret_unreadable: false,
        users: Vec::new(),
        name_error: String::new(),
        url_error: String::new(),
        interval_error: String::new(),
        visibility_error: String::new(),
        secret_error: String::new(),
    }
}

async fn render_form(
    core: &Core,
    user: &CurrentUser,
    jar: SignedCookieJar,
    status: StatusCode,
    form: FormView,
    title: &'static str,
) -> ApiResult<Response> {
    let (ctx, jar) = PageCtx::build(core, Some(user), &jar, title, "settings", "services").await;
    Ok((status, jar, HtmlTemplate(FormPage { ctx, form })).into_response())
}

#[derive(Deserialize)]
struct NewQuery {
    kind: Option<String>,
}

async fn new_form(
    State(core): State<Core>,
    user: CurrentUser,
    jar: SignedCookieJar,
    Query(query): Query<NewQuery>,
) -> ApiResult<Response> {
    user.require_admin()?;
    let kind = match query.kind.as_deref() {
        Some(kind) => integrations::find(kind),
        // One kind: no need to choose.
        None if integrations::registry().len() == 1 => integrations::registry().first().copied(),
        None => None,
    };
    let form = match kind {
        Some(kind) => form_view(&core, kind, None, false, Shown::empty()).await?,
        None => chooser(),
    };
    render_form(
        &core,
        &user,
        jar,
        StatusCode::OK,
        form,
        i18n::t("service.new"),
    )
    .await
}

/// Whether the service has a secret that can't be opened any more (the instance
/// key changed): the form says so instead of "saved".
async fn unreadable(core: &Core, service: &Service) -> bool {
    service.has_secret
        && matches!(
            store::open_secret(core, service.id).await,
            Err(store::CtxError::SecretUnreadable(_))
        )
}

async fn edit_form(
    State(core): State<Core>,
    user: CurrentUser,
    jar: SignedCookieJar,
    Path(id): Path<i64>,
) -> ApiResult<Response> {
    user.require_admin()?;
    let service = store::get(&core, id).await?.ok_or(ApiError::NotFound)?;
    let kind = service.integration().ok_or(ApiError::NotFound)?;
    let has_secret = service.has_secret;
    let mut form = form_view(&core, kind, Some(id), has_secret, Shown::of(&service)).await?;
    form.secret_unreadable = unreadable(&core, &service).await;
    render_form(
        &core,
        &user,
        jar,
        StatusCode::OK,
        form,
        i18n::t("service.edit"),
    )
    .await
}

fn announce(magpie: &Magpie, event: ServiceEvent) {
    // Nobody listening is fine.
    let _ = magpie.service_events.send(event);
}

fn done(jar: SignedCookieJar, message: &'static str) -> Response {
    (
        flash(jar, "success", i18n::t(message)),
        Redirect::to("/settings/services"),
    )
        .into_response()
}

async fn create(
    State(core): State<Core>,
    Extension(magpie): Extension<Magpie>,
    user: CurrentUser,
    jar: SignedCookieJar,
    body: Bytes,
) -> ApiResult<Response> {
    user.require_admin()?;
    let raw = Raw::parse(&body);
    let Some(kind) = integrations::find(raw.get("kind")) else {
        return render_form(
            &core,
            &user,
            jar,
            StatusCode::UNPROCESSABLE_ENTITY,
            chooser(),
            i18n::t("service.new"),
        )
        .await;
    };
    let parsed = parse(&raw, kind);
    let mut errors = parsed.errors.clone();
    errors.extend(parsed.input.check());
    let result = if errors.is_empty() {
        store::create(&core, &parsed.input).await
    } else {
        Err(StoreError::Invalid(errors.clone()))
    };
    match result {
        Ok(id) => {
            tracing::info!(admin = %user.username, service_id = id, kind = kind.kind(), "service created");
            announce(&magpie, ServiceEvent::Created(id));
            Ok(done(jar, "service.created"))
        }
        Err(StoreError::Invalid(errors)) => {
            let form =
                form_view(&core, kind, None, false, Shown::typed(parsed, &raw, errors)).await?;
            render_form(
                &core,
                &user,
                jar,
                StatusCode::UNPROCESSABLE_ENTITY,
                form,
                i18n::t("service.new"),
            )
            .await
        }
        Err(StoreError::NotFound) => Err(ApiError::NotFound),
        Err(StoreError::Db(err)) => Err(ApiError::internal(err)),
    }
}

async fn update(
    State(core): State<Core>,
    Extension(magpie): Extension<Magpie>,
    user: CurrentUser,
    jar: SignedCookieJar,
    Path(id): Path<i64>,
    body: Bytes,
) -> ApiResult<Response> {
    user.require_admin()?;
    let service = store::get(&core, id).await?.ok_or(ApiError::NotFound)?;
    let kind = service.integration().ok_or(ApiError::NotFound)?;
    let raw = Raw::parse(&body);
    let parsed = parse(&raw, kind);
    let mut errors = parsed.errors.clone();
    errors.extend(parsed.input.check());
    let result = if errors.is_empty() {
        store::update(&core, id, &parsed.input).await
    } else {
        Err(StoreError::Invalid(errors.clone()))
    };
    match result {
        Ok(()) => {
            tracing::info!(admin = %user.username, service_id = id, kind = kind.kind(), "service updated");
            announce(&magpie, ServiceEvent::Updated(id));
            Ok(done(jar, "service.saved"))
        }
        Err(StoreError::Invalid(errors)) => {
            let mut form = form_view(
                &core,
                kind,
                Some(id),
                service.has_secret,
                Shown::typed(parsed, &raw, errors),
            )
            .await?;
            form.secret_unreadable = unreadable(&core, &service).await;
            render_form(
                &core,
                &user,
                jar,
                StatusCode::UNPROCESSABLE_ENTITY,
                form,
                i18n::t("service.edit"),
            )
            .await
        }
        Err(StoreError::NotFound) => Err(ApiError::NotFound),
        Err(StoreError::Db(err)) => Err(ApiError::internal(err)),
    }
}

async fn delete(
    State(core): State<Core>,
    Extension(magpie): Extension<Magpie>,
    user: CurrentUser,
    jar: SignedCookieJar,
    Path(id): Path<i64>,
) -> ApiResult<Response> {
    user.require_admin()?;
    if !store::delete(&core, id).await? {
        return Err(ApiError::NotFound);
    }
    tracing::info!(admin = %user.username, service_id = id, "service deleted");
    announce(&magpie, ServiceEvent::Deleted(id));
    Ok(done(jar, "service.deleted"))
}

// ---------------------------------------------------------------------------
// «Test»

struct TestRow {
    label: &'static str,
    value: String,
}

#[derive(Template)]
#[template(path = "partials/service_test.html")]
struct TestResult {
    /// `ok`, `warn`, `bad` or `unknown` (also `data-test-status`).
    status: &'static str,
    headline: &'static str,
    /// Why, in words that hold no secret, header or address.
    reason: Option<String>,
    rows: Vec<TestRow>,
}

fn headline(status: &str) -> &'static str {
    match status {
        "ok" => i18n::t("service.test_ok"),
        "warn" => i18n::t("service.test_warn"),
        "bad" => i18n::t("service.test_bad"),
        _ => i18n::t("service.test_unknown"),
    }
}

fn status_name(status: Status) -> &'static str {
    match status {
        Status::Ok => "ok",
        Status::Warn => "warn",
        Status::Bad => "bad",
        Status::Unknown => "unknown",
    }
}

/// A reason as words: the fixed texts translated, the rest ("HTTP 500") as is.
fn reason_text(text: &str) -> String {
    integrations::error_text_key(text)
        .map_or_else(|| text.to_string(), |key| i18n::t(key).to_string())
}

fn trim_number(n: f64) -> String {
    if (n - n.round()).abs() < 1e-9 {
        format!("{n:.0}")
    } else {
        format!("{n:.1}")
    }
}

/// A value for the result list. (The start page draws its own from the
/// snapshot; this is the plain version «Test» needs.)
fn format_value(format: Format, value: &Value) -> String {
    let Some(n) = value.num else {
        return value.text.clone().unwrap_or_default();
    };
    match format {
        Format::Number => trim_number(n),
        Format::Percent => format!("{} %", trim_number(n)),
        Format::Bytes | Format::BytesPerSec => {
            let units = ["B", "KB", "MB", "GB", "TB"];
            let (mut size, mut unit) = (n, 0);
            while size >= 1024.0 && unit < units.len() - 1 {
                size /= 1024.0;
                unit += 1;
            }
            let per = if format == Format::BytesPerSec {
                "/s"
            } else {
                ""
            };
            format!("{} {}{per}", trim_number(size), units[unit])
        }
        Format::Duration => {
            let s = n.max(0.0).round() as u64;
            match s {
                0..=59 => format!("{s} s"),
                60..=3599 => format!("{} min", s / 60),
                3600..=86_399 => format!("{} h {} min", s / 3600, (s % 3600) / 60),
                _ => format!("{} d {} h", s / 86_400, (s % 86_400) / 3600),
            }
        }
        Format::Since => {
            let age = (jiff::Timestamp::now().as_second() as f64 - n).max(0.0);
            format_value(Format::Duration, &Value::num(age))
        }
        Format::Text => value.text.clone().unwrap_or_else(|| trim_number(n)),
    }
}

fn unknown(reason: String) -> TestResult {
    TestResult {
        status: "unknown",
        headline: headline("unknown"),
        reason: Some(reason),
        rows: Vec::new(),
    }
}

fn describe(kind: &'static dyn Integration, outcome: Result<Snapshot, FetchError>) -> TestResult {
    match outcome {
        Ok(snapshot) => {
            let status = status_name(snapshot.status);
            let rows = kind
                .metrics()
                .iter()
                .filter_map(|metric| {
                    snapshot.values.get(metric.id).map(|value| TestRow {
                        label: i18n::t(metric.label_key),
                        value: format_value(metric.format, value),
                    })
                })
                .collect();
            TestResult {
                status,
                headline: headline(status),
                reason: snapshot.error.as_deref().map(reason_text),
                rows,
            }
        }
        // The settings can't be used: nothing was tried.
        Err(FetchError::Config) => unknown(reason_text(&FetchError::Config.to_string())),
        Err(err) => TestResult {
            status: "bad",
            headline: headline("bad"),
            reason: Some(reason_text(&err.to_string())),
            rows: Vec::new(),
        },
    }
}

async fn test(State(core): State<Core>, user: CurrentUser, body: Bytes) -> ApiResult<Response> {
    user.require_admin()?;
    let raw = Raw::parse(&body);
    let Some(kind) = integrations::find(raw.get("kind")) else {
        return Ok(HtmlTemplate(unknown(i18n::t("err.kind").to_string())).into_response());
    };
    let parsed = parse(&raw, kind);
    let mut errors = parsed.errors.clone();
    errors.extend(parsed.input.check_connection());
    if let Some(first) = errors.first() {
        return Ok(HtmlTemplate(unknown(i18n::t(first.key).to_string())).into_response());
    }
    let input = parsed.input;
    // The typed secret, or else the one stored for this service (when the
    // form is an edit form for a service of the same kind).
    let secret = match &input.secret {
        SecretChange::Replace(secret) => Some(secret.clone()),
        SecretChange::Clear => None,
        SecretChange::Keep => match raw.get("id").parse::<i64>() {
            Ok(id) => match store::get(&core, id).await? {
                Some(service) if service.kind == input.kind && service.has_secret => {
                    // The stored secret only goes to the address it was saved for.
                    if !same_origin(service.url.as_deref(), input.url.as_deref()) {
                        return Ok(
                            HtmlTemplate(unknown(i18n::t("err.secret_origin").to_string()))
                                .into_response(),
                        );
                    }
                    match store::open_secret(&core, id).await {
                        Ok(secret) => secret,
                        Err(store::CtxError::SecretUnreadable(_)) => {
                            return Ok(HtmlTemplate(unknown(reason_text(
                                &FetchError::SecretUnreadable.to_string(),
                            )))
                            .into_response());
                        }
                        Err(store::CtxError::Db(err)) => return Err(ApiError::internal(err)),
                    }
                }
                _ => None,
            },
            Err(_) => None,
        },
    };
    let mut settings = input.settings.as_object().cloned().unwrap_or_default();
    if let Some(url) = &input.url {
        settings.insert("url".into(), Json::String(url.clone()));
    }
    let ctx = FetchCtx {
        http: integrations::client().clone(),
        settings: Json::Object(settings),
        secret,
    };
    let outcome = kind.fetch(&ctx).await;
    let result = describe(kind, outcome);
    // Only what happened, never the address, the settings or the secret.
    tracing::info!(admin = %user.username, kind = kind.kind(), status = result.status, "service tested");
    Ok(HtmlTemplate(result).into_response())
}
