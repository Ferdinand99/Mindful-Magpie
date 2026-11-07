//! Services: the admin's instances of an integration kind (a website, later a
//! monitor, a code host …), with their address, settings, sealed secret,
//! fetch interval and who may see them. [`store`] keeps them in the database,
//! [`pages`] is the admin's Settings page for them.
//!
//! A service's secret is sealed (`vole_kit::crypto`, purpose
//! [`SECRET_PURPOSE`]) and leaves the store only inside a
//! [`FetchCtx`](crate::integrations::FetchCtx) built by
//! [`store::fetch_ctx`]. Nothing here renders, serialises or logs it.

pub mod pages;
pub mod store;
pub mod view;

pub use view::{MetricView, ServiceView, services_for, view_of};

use std::time::Duration;

use serde_json::{Map, Value as Json};
use url::Url;
use vole_kit::auth::{CurrentUser, Role};

use crate::integrations::{self, Integration};

/// The purpose a service's secret is sealed for.
pub const SECRET_PURPOSE: &str = "magpie.service.secret";

/// The form field (and error field) of the pinned certificate's fingerprint,
/// the same as its settings key.
pub const CERT_FIELD: &str = integrations::pin::SETTING;

/// The longest interval an admin may choose (a day).
pub const MAX_INTERVAL_S: i64 = 86_400;

/// A change to a service's secret.
#[derive(Clone, PartialEq, Eq)]
pub enum SecretChange {
    /// Leave the stored secret as it is (a new service has none).
    Keep,
    /// Seal this value and store it.
    Replace(String),
    /// Remove the stored secret.
    Clear,
}

impl std::fmt::Debug for SecretChange {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            SecretChange::Keep => "Keep",
            SecretChange::Replace(_) => "Replace(<redacted>)",
            SecretChange::Clear => "Clear",
        })
    }
}

/// Who sees a service.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Visibility {
    /// Every signed-in user.
    All,
    /// Admins only.
    Admin,
    /// Admins and the selected users.
    Users,
}

impl Visibility {
    pub fn as_str(self) -> &'static str {
        match self {
            Visibility::All => "all",
            Visibility::Admin => "admin",
            Visibility::Users => "users",
        }
    }

    pub fn parse(text: &str) -> Option<Visibility> {
        match text {
            "all" => Some(Visibility::All),
            "admin" => Some(Visibility::Admin),
            "users" => Some(Visibility::Users),
            _ => None,
        }
    }

    /// Whether this is `text` (for templates).
    pub fn is(self, text: &str) -> bool {
        self.as_str() == text
    }
}

/// Who is asking, for [`store::visible_to`] and [`store::can_see`]. Admins see
/// every service.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Viewer {
    pub id: i64,
    pub admin: bool,
}

impl Viewer {
    pub fn new(id: i64, admin: bool) -> Self {
        Viewer { id, admin }
    }
}

impl From<&CurrentUser> for Viewer {
    /// Admin by the request's role: a read-only API token is capped at viewer,
    /// as everywhere in the kit. Token routes that show the owner's own page
    /// use [`Viewer::for_token`].
    fn from(user: &CurrentUser) -> Self {
        Viewer::new(user.id, user.is_admin())
    }
}

impl Viewer {
    /// Who a token request shows the page of: admin when the request is an
    /// admin's own (a session), or when the token carries the
    /// [`OWNER_SCOPE`](crate::product::OWNER_SCOPE) scope (the browser
    /// extension's token, given by the connect flow) and its owner is an
    /// admin. A plain read-only token of an admin stays a viewer's, so a
    /// script token that leaks does not show admin-only services.
    pub fn for_token(user: &CurrentUser) -> Self {
        let owner = user.has_scope(crate::product::OWNER_SCOPE) && user.may_act_as(Role::Admin);
        Viewer::new(user.id, user.is_admin() || owner)
    }
}

/// A stored service. It has no secret: [`Service::has_secret`] says whether
/// one is stored.
#[derive(Debug, Clone)]
pub struct Service {
    pub id: i64,
    pub kind: String,
    pub name: String,
    pub url: Option<String>,
    /// A JSON object, never holding a secret.
    pub settings: Json,
    pub has_secret: bool,
    pub interval_s: i64,
    pub visibility: Visibility,
    /// The users who see it when `visibility` is [`Visibility::Users`].
    pub users: Vec<i64>,
    pub created_at: String,
    pub updated_at: String,
}

impl Service {
    /// The service's kind, if this build still has it.
    pub fn integration(&self) -> Option<&'static dyn Integration> {
        integrations::find(&self.kind)
    }
}

/// What anyone who may see a service learns about it: no address, no
/// settings, only the link a card may point at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceSummary {
    pub id: i64,
    pub kind: String,
    pub name: String,
    /// Seconds between fetches; a page needs it to tell a stale value.
    pub interval_s: i64,
    /// Where a card of the service links when it has no link of its own: the
    /// service's own «Lenke på kort», else the origin of its address (see
    /// [`card_link`]). Empty when the service has neither.
    pub link: String,
}

/// The setting that holds a service's «Lenke på kort». It is not a field of
/// any kind: it is kept in the settings object next to them, reserved for
/// this, and nothing fetches it.
pub const LINK_KEY: &str = "link";

/// The link a card of a service points at when it has none of its own: the
/// service's `link` setting if it has one, else the origin of its address
/// (scheme, host and port: no user name, path or query), else empty.
pub fn card_link(settings: &Json, url: Option<&str>) -> String {
    if let Some(link) = settings
        .get(LINK_KEY)
        .and_then(Json::as_str)
        .filter(|l| !l.is_empty())
    {
        return link.to_string();
    }
    url.and_then(|u| Url::parse(u.trim()).ok())
        .filter(|u| matches!(u.scheme(), "http" | "https") && u.host_str().is_some())
        .map(|u| u.origin().ascii_serialization())
        .unwrap_or_default()
}

/// A change to the set of services, announced after it is stored (the
/// poller listens).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceEvent {
    Created(i64),
    Updated(i64),
    Deleted(i64),
}

/// What to store for a service: the admin's input, checked by
/// [`ServiceInput::check`] before anything is written.
#[derive(Debug, Clone)]
pub struct ServiceInput {
    pub kind: String,
    pub name: String,
    pub url: Option<String>,
    /// A JSON object made from the kind's fields, never holding a secret.
    pub settings: Json,
    pub secret: SecretChange,
    pub interval_s: i64,
    pub visibility: Visibility,
    pub users: Vec<i64>,
}

/// A problem with one input: the field (a form field name) and the
/// translation key of the text that says what is wrong.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldError {
    pub field: String,
    pub key: &'static str,
}

impl FieldError {
    pub fn new(field: impl Into<String>, key: &'static str) -> Self {
        FieldError {
            field: field.into(),
            key,
        }
    }
}

/// `url` as an address a service may have: `http` or `https`, a host, no user
/// name or password (those belong in the secret).
pub fn check_url(url: &str) -> Result<(), &'static str> {
    if url.chars().count() > 2048 {
        return Err("err.url");
    }
    let parsed = Url::parse(url).map_err(|_| "err.url")?;
    if !matches!(parsed.scheme(), "http" | "https") || parsed.host_str().is_none() {
        return Err("err.url");
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err("err.url_userinfo");
    }
    Ok(())
}

/// Whether two addresses have the same origin (scheme, host and port, the
/// default port counted as given). Both missing counts as the same; an address
/// that does not parse never matches. A stored secret may only be used for an
/// address of the origin it was saved for.
pub fn same_origin(a: Option<&str>, b: Option<&str>) -> bool {
    match (a, b) {
        (None, None) => true,
        (Some(a), Some(b)) => match (Url::parse(a.trim()), Url::parse(b.trim())) {
            (Ok(a), Ok(b)) => a.origin().is_tuple() && a.origin() == b.origin(),
            _ => false,
        },
        _ => false,
    }
}

/// `url` without a user name and password, for showing a refused form again.
pub fn without_userinfo(url: &str) -> String {
    match Url::parse(url) {
        Ok(mut parsed) if !parsed.username().is_empty() || parsed.password().is_some() => {
            let _ = parsed.set_username("");
            let _ = parsed.set_password(None);
            parsed.to_string()
        }
        _ => url.to_string(),
    }
}

/// `settings` as they are stored: a pin in any usual spelling as 64 lowercase
/// hex characters, an empty one left out.
pub fn canonical_settings(settings: &Json) -> Json {
    let mut settings = settings.clone();
    if let Some(map) = settings.as_object_mut() {
        let pin = map.get(integrations::pin::SETTING).and_then(Json::as_str);
        match pin.map(str::trim) {
            Some("") => {
                map.remove(integrations::pin::SETTING);
            }
            Some(text) => {
                if let Some(hex) = integrations::pin::normalize(text) {
                    map.insert(integrations::pin::SETTING.to_string(), Json::String(hex));
                }
            }
            None => {}
        }
    }
    settings
}

impl ServiceInput {
    /// The kind, if this build has it.
    pub fn integration(&self) -> Option<&'static dyn Integration> {
        integrations::find(&self.kind)
    }

    /// Everything wrong with the input; empty means it can be stored.
    pub fn check(&self) -> Vec<FieldError> {
        let mut errors = Vec::new();
        let name = self.name.trim();
        if name.is_empty() || name.chars().count() > 80 {
            errors.push(FieldError::new("name", "err.name"));
        }
        let Some(kind) = self.integration() else {
            errors.push(FieldError::new("kind", "err.kind"));
            return errors;
        };
        let min = i64::try_from(kind.min_interval().as_secs()).unwrap_or(i64::MAX);
        if self.interval_s < min || self.interval_s > MAX_INTERVAL_S {
            errors.push(FieldError::new("interval", "err.interval"));
        }
        errors.extend(self.check_connection());
        errors
    }

    /// What «Test» needs to be right: the address, the kind's settings and
    /// the secret.
    pub fn check_connection(&self) -> Vec<FieldError> {
        let mut errors = Vec::new();
        let Some(kind) = self.integration() else {
            errors.push(FieldError::new("kind", "err.kind"));
            return errors;
        };
        match self.url.as_deref().map(str::trim).filter(|u| !u.is_empty()) {
            Some(url) => {
                if let Err(key) = check_url(url) {
                    errors.push(FieldError::new("url", key));
                }
            }
            None if kind.url_required() => errors.push(FieldError::new("url", "err.url_required")),
            None => {}
        }
        let empty = Map::new();
        let settings = self.settings.as_object().unwrap_or(&empty);
        // «Lenke på kort»: a link for the browser, checked like a card's. It is
        // never fetched, so what it points at is not checked. Empty is none.
        if let Some(link) = settings.get(LINK_KEY) {
            let fine = link
                .as_str()
                .is_some_and(|l| l.is_empty() || crate::layout::model::link_problem(l).is_none());
            if !fine {
                errors.push(FieldError::new("link", "err.link"));
            }
        }
        for field in kind.fields().iter().filter(|f| !f.secret && f.required) {
            if !settings.contains_key(field.key) {
                errors.push(FieldError::new(format!("f_{}", field.key), "err.required"));
            }
        }
        for (key, value) in settings {
            // The pin has its own message.
            if key != integrations::pin::SETTING
                && value
                    .as_str()
                    .is_some_and(|text| text.chars().count() > 1024)
            {
                errors.push(FieldError::new(format!("f_{key}"), "err.too_long"));
            }
        }
        if let Err(problem) = kind.check_settings(settings) {
            errors.push(FieldError::new(
                format!("f_{}", problem.field),
                problem.message_key,
            ));
        }
        match integrations::pin::from_settings(&self.settings) {
            Ok(None) => {}
            Ok(Some(_)) => {
                // A pin is for TLS: on a plain-http address it would pin nothing.
                let https = self
                    .url
                    .as_deref()
                    .and_then(|url| Url::parse(url.trim()).ok())
                    .is_some_and(|url| url.scheme() == "https");
                if !https {
                    errors.push(FieldError::new(CERT_FIELD, "err.fingerprint_https"));
                }
            }
            Err(_) => errors.push(FieldError::new(CERT_FIELD, "err.fingerprint")),
        }
        if let SecretChange::Replace(secret) = &self.secret {
            let has_field = kind.fields().iter().any(|f| f.secret);
            // Visible ASCII only (no spaces, line breaks or other control characters):
            // the secret goes into a header.
            if !has_field
                || secret.chars().count() > 2048
                || !secret.chars().all(|c| ('!'..='~').contains(&c))
            {
                errors.push(FieldError::new("secret", "err.secret"));
            }
        }
        errors
    }

    /// The interval as a duration.
    pub fn interval(&self) -> Duration {
        Duration::from_secs(u64::try_from(self.interval_s).unwrap_or_default())
    }
}
