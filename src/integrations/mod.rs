//! The integration engine's contract: what a *kind* of service (a website, a
//! monitor, a code host …) declares and does, and what a fetch returns.
//!
//! A kind implements [`Integration`]: it lists the settings its form needs
//! ([`Field`]), the numbers it can show ([`MetricDef`]) and fetches one
//! [`Snapshot`] from a [`FetchCtx`]. Kinds are compiled in and listed by
//! [`registry`]; the admin's services (see [`crate::services`]) are rows that
//! name a kind.
//!
//! Everything an integration sends goes through [`client`]: a client whose
//! resolver is the kit's `GuardedResolver` (link-local and metadata addresses
//! are refused at connect time, private and loopback ones are allowed because
//! an admin configures them), without proxies, with a 10 s timeout and at most
//! [`MAX_REDIRECTS`] redirects, each checked again. Bodies are read with
//! [`read_capped`].
//!
//! A service with a self-signed certificate carries a pin (see [`pin`]) and is
//! fetched with the client [`client_for`] gives for its settings: the same
//! client, except that TLS accepts only the pinned certificate. There is no way
//! to switch certificate checks off.
//!
//! **Secrets** live in [`FetchCtx::secret`] only. Its `Debug` hides it, and
//! nothing here puts a secret, a settings value or a URL into an error text:
//! [`FetchError`]'s texts are a closed set plus an HTTP status.

pub mod api;
pub mod block;
pub mod github;
pub mod http_kind;
pub mod kk;
pub mod pin;
pub mod poller;
pub mod proxmox;
pub mod tofa;
pub mod unraid;
pub mod vv;
pub mod ww_kind;

use std::collections::BTreeMap;
use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use jiff::Timestamp;
use serde::{Deserialize, Serialize};
use serde_json::Value as Json;
use vole_kit::net::{Forbidden, GuardedResolver, check_host};

use crate::layout::Size;

pub use block::Block;

/// A boxed future, so [`Integration`] can be used as a trait object.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// How long a fetch may take in all (connecting, redirects and reading).
pub const TIMEOUT: Duration = Duration::from_secs(10);
/// How much of a response body a fetch reads before it gives up.
pub const MAX_BODY: usize = 1024 * 1024;
/// How many redirects a fetch follows (to `http` and `https` only).
pub const MAX_REDIRECTS: usize = 3;

/// One kind of service. A kind sends its requests with [`FetchCtx::http`],
/// never with [`client`] (see there), and keeps a secret out of everything it
/// returns.
pub trait Integration: Send + Sync + 'static {
    /// The stored name of the kind (`services.kind`), e.g. `"http"`.
    fn kind(&self) -> &'static str;
    /// Translation key of the kind's name, e.g. `"kind.http"`.
    fn label_key(&self) -> &'static str;
    /// The settings form (below the name and the address, which every
    /// service has). At most one field is the secret.
    fn fields(&self) -> &'static [Field];
    /// The numbers the kind can show on a card.
    fn metrics(&self) -> &'static [MetricDef];
    /// How often the poller fetches when the admin does not choose.
    fn default_interval(&self) -> Duration;
    /// The shortest interval an admin may choose.
    fn min_interval(&self) -> Duration;
    /// Where a card of a service of this kind opens when the service has no
    /// link of its own set: for a kind whose address is an API's, the app's
    /// own site (GitHub). `None`: the origin of the service's address.
    fn default_link(&self) -> Option<&'static str> {
        None
    }
    /// The size a card of a service of this kind has when the person has not
    /// chosen one: wide for the kinds that have a lot to show.
    fn default_size(&self) -> Size {
        Size::Normal
    }
    /// Whether a service of this kind needs an address (`services.url`).
    fn url_required(&self) -> bool {
        true
    }
    /// Whether this is a personal kind: its address is fixed in the kind (a
    /// service has none of its own) and its key is one user's own, so a
    /// service of it belongs to that user (see [`crate::services::Scope`]).
    /// Admins set up the other kinds.
    fn personal(&self) -> bool {
        false
    }
    /// Checks the kind's own settings (`settings` is the JSON object built
    /// from [`Integration::fields`]); the first problem, if any.
    fn check_settings(
        &self,
        _settings: &serde_json::Map<String, Json>,
    ) -> Result<(), SettingError> {
        Ok(())
    }
    /// One fetch. `Err` means no reading could be made (the poller records
    /// it as a bad snapshot); a kind whose subject is reachability itself
    /// (`http`) reports a failed request as a bad [`Snapshot`] instead.
    fn fetch<'a>(&'a self, ctx: &'a FetchCtx) -> BoxFuture<'a, Result<Snapshot, FetchError>>;
}

/// The kinds compiled into this build.
pub fn registry() -> &'static [&'static dyn Integration] {
    static KINDS: [&dyn Integration; 8] = [
        &http_kind::HTTP,
        &vv::VV,
        &kk::KK,
        &ww_kind::WW,
        &proxmox::PROXMOX,
        &unraid::UNRAID,
        &tofa::TOFA,
        &github::GITHUB,
    ];
    &KINDS
}

/// The kind called `kind`.
pub fn find(kind: &str) -> Option<&'static dyn Integration> {
    registry().iter().copied().find(|k| k.kind() == kind)
}

/// What a form field takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldKind {
    Text,
    /// A whole number, stored as a JSON number.
    Number,
}

/// One setting of a kind. The form field is called `f_<key>`; a secret field
/// is the form's `secret` input and is stored sealed, not in `settings`.
#[derive(Debug, Clone, Copy)]
pub struct Field {
    pub key: &'static str,
    pub label_key: &'static str,
    /// Translation key of the help text under the field; `""` for none.
    pub help_key: &'static str,
    pub placeholder: &'static str,
    pub kind: FieldKind,
    pub required: bool,
    pub secret: bool,
}

/// A problem with one setting: the field and the translation key of the text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SettingError {
    pub field: &'static str,
    pub message_key: &'static str,
}

/// One number a kind can show.
#[derive(Debug, Clone, Copy)]
pub struct MetricDef {
    pub id: &'static str,
    pub label_key: &'static str,
    pub format: Format,
}

/// How a metric's number is drawn, and what its unit is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Format {
    /// A plain number.
    Number,
    /// 0–100, drawn with a percent sign.
    Percent,
    /// A size in bytes, drawn as KB, MB, GB …
    Bytes,
    /// Bytes per second.
    BytesPerSec,
    /// A length of time in seconds.
    Duration,
    /// A moment as Unix seconds, drawn as an age ("2 min ago").
    Since,
    /// The value's `text`.
    Text,
}

/// The settings key the store puts a service's number under, for a kind that
/// keeps a memory per service (see [`FetchCtx::service_id`]).
pub const SERVICE_ID_KEY: &str = "service_id";

/// What a fetch needs: the client, the service's settings and its secret. The
/// settings carry the service's address as `"url"` (when it has one) and its
/// number as [`SERVICE_ID_KEY`] (a test of a form has none).
pub struct FetchCtx {
    /// The client to send every request with. A kind always uses this one and
    /// never [`client`]: for a service with a pinned certificate it is the
    /// client that accepts only that certificate, so a request made with
    /// another client would skip the pin. (The shared client is for whoever
    /// builds a [`FetchCtx`].)
    pub http: reqwest::Client,
    pub settings: Json,
    pub secret: Option<String>,
}

impl FetchCtx {
    /// The number of the service being fetched, when it is a stored one.
    pub fn service_id(&self) -> Option<i64> {
        self.settings.get(SERVICE_ID_KEY).and_then(Json::as_i64)
    }
}

impl fmt::Debug for FetchCtx {
    /// Hides the secret, and the settings' values (only their names show).
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let keys: Vec<&str> = self
            .settings
            .as_object()
            .map(|o| o.keys().map(String::as_str).collect())
            .unwrap_or_default();
        f.debug_struct("FetchCtx")
            .field("settings", &keys)
            .field("secret", &self.secret.as_ref().map(|_| "<redacted>"))
            .finish_non_exhaustive()
    }
}

/// The latest reading of a service. Serialised into `service_snapshots` (the
/// time as RFC 3339).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Snapshot {
    pub status: Status,
    // The stored shape may be older than this code: whatever a later version
    // adds to these types has to have a default (`#[serde(default)]`), so an
    // old row still loads.
    #[serde(default)]
    pub values: BTreeMap<String, Value>,
    #[serde(default)]
    pub needs: Vec<Need>,
    /// Lists, bars and the like drawn next to the numbers (see [`block`]).
    /// A kind fills them; the poller cuts them down ([`Snapshot::bound_details`])
    /// before the snapshot is stored.
    #[serde(default)]
    pub details: Vec<Block>,
    pub fetched_at: Timestamp,
    /// A short reason when the fetch failed or the service is not well. Never
    /// contains a secret, a header or a URL (see [`FetchError`]).
    #[serde(default)]
    pub error: Option<String>,
}

impl Snapshot {
    /// A reading with no values yet, taken now.
    pub fn new(status: Status) -> Self {
        Snapshot {
            status,
            values: BTreeMap::new(),
            needs: Vec::new(),
            details: Vec::new(),
            fetched_at: Timestamp::now(),
            error: None,
        }
    }

    /// The same reading with `details` (what a kind's fetch sets, and what a
    /// test sets to have blocks without a kind). They are cut down by the
    /// poller, not here.
    #[must_use]
    pub fn with_details(mut self, details: Vec<Block>) -> Self {
        self.details = details;
        self
    }

    /// Cuts the details down to the limits ([`Block::bounded`]; at most
    /// [`block::MAX_BLOCKS`] blocks, a block with nothing left goes).
    pub fn bound_details(&mut self) {
        self.details = std::mem::take(&mut self.details)
            .into_iter()
            .filter_map(Block::bounded)
            .take(block::MAX_BLOCKS)
            .collect();
    }

    /// A bad reading that says why.
    pub fn failed(error: &FetchError) -> Self {
        Snapshot {
            error: Some(error.to_string()),
            ..Snapshot::new(Status::Bad)
        }
    }
}

/// How well a service is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Ok,
    Warn,
    Bad,
    Unknown,
}

/// One metric's value: a number, a text, or both.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Value {
    #[serde(default)]
    pub num: Option<f64>,
    #[serde(default)]
    pub text: Option<String>,
}

impl Value {
    pub fn num(n: f64) -> Self {
        Value {
            num: Some(n),
            text: None,
        }
    }

    pub fn text(t: impl Into<String>) -> Self {
        Value {
            num: None,
            text: Some(t.into()),
        }
    }
}

/// How serious a [`Need`] is. `Warn` is less than `Bad`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Level {
    Warn,
    Bad,
}

/// Something that needs the user's attention («Trenger deg»).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Need {
    /// Stable within the service; what an acknowledgement refers to.
    pub key: String,
    pub level: Level,
    pub label_key: String,
    /// Values for the placeholders of the label's text. Only `count` is read
    /// (a number, for texts such as "{count} down"); the service's name is not
    /// a parameter, the page shows it beside the text. Never external text.
    #[serde(default)]
    pub params: BTreeMap<String, String>,
    /// Higher is worse.
    #[serde(default)]
    pub score: f64,
    /// When the need began. A kind may set it, to a time that stays the same
    /// while the need does (an acknowledgement stops hiding a need whose
    /// `since` is later than the acknowledgement); the poller fills it in for
    /// a need that has none (the time of the reading that first had it) and
    /// carries it over from one reading to the next.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub since: Option<Timestamp>,
}

/// Why a fetch gave no reading. The texts are fixed (they end up on pages and
/// in logs); none holds a secret, a header, an address or a URL.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum FetchError {
    /// The settings can't be used (the text names no value).
    #[error("settings not valid")]
    Config,
    /// The address is link-local, cloud metadata, unspecified or multicast.
    #[error("address not allowed")]
    Forbidden,
    #[error("timeout")]
    Timeout,
    #[error("connection failed")]
    Connect,
    /// TLS did not accept the server's certificate: it is not the pinned one,
    /// or (without a pin) it is not trusted.
    #[error("certificate not accepted")]
    Cert,
    #[error("too many redirects")]
    Redirects,
    #[error("redirect not allowed")]
    RedirectScheme,
    #[error("response too large")]
    TooLarge,
    #[error("unexpected answer")]
    Invalid,
    /// The stored secret can't be opened (the instance key changed).
    #[error("stored secret unreadable")]
    SecretUnreadable,
    /// The service refused the key (HTTP 401 or 403).
    #[error("key refused")]
    Auth,
    /// Keen Kestrel without the summary endpoint (an older release).
    #[error("Keen Kestrel too old")]
    KkTooOld,
    /// The service knows the key but it may not read what the kind needs
    /// (HTTP 403 where the kind's endpoints are for admins).
    #[error("key is not an admin key")]
    NotAdmin,
    /// The key is accepted but the answer is empty: it sees none of what the
    /// kind reads (a Proxmox token without a role).
    #[error("key sees nothing")]
    NothingVisible,
    /// The service's request limit is used up; the kind waits for it to reset.
    #[error("rate limited")]
    RateLimited,
    #[error("HTTP {0}")]
    Status(u16),
    #[error("request failed")]
    Other,
}

impl FetchError {
    /// Every error except [`FetchError::Status`] (which has a number): what a
    /// stored error text can be besides `HTTP n`. The test of the texts walks
    /// it, so a new error needs a text in both languages.
    pub const ALL: [FetchError; 16] = [
        FetchError::Config,
        FetchError::Forbidden,
        FetchError::Timeout,
        FetchError::Connect,
        FetchError::Cert,
        FetchError::Redirects,
        FetchError::RedirectScheme,
        FetchError::TooLarge,
        FetchError::Invalid,
        FetchError::SecretUnreadable,
        FetchError::Auth,
        FetchError::KkTooOld,
        FetchError::NotAdmin,
        FetchError::NothingVisible,
        FetchError::RateLimited,
        FetchError::Other,
    ];

    /// The error a stored text stands for: one of [`FetchError::ALL`], or
    /// `HTTP n`.
    pub fn from_text(text: &str) -> Option<FetchError> {
        if let Some(code) = text.strip_prefix("HTTP ") {
            return code.parse().ok().map(FetchError::Status);
        }
        FetchError::ALL.into_iter().find(|e| e.to_string() == text)
    }

    /// Whether only an admin can put it right, not the service coming back:
    /// settings or an address that can't be used, a redirect that is not
    /// allowed, a secret that can't be opened, a key that was refused or has
    /// the wrong role or sees nothing, a certificate that was not accepted, a
    /// Keen Kestrel that has to be upgraded. The poller makes the «set it up
    /// again» need of these, never «not answering».
    ///
    /// No catch-all arm: a new error has to be placed here and in
    /// [`FetchError::is_unreachable`] before this compiles.
    pub fn is_config(&self) -> bool {
        match self {
            FetchError::Config
            | FetchError::Forbidden
            | FetchError::RedirectScheme
            | FetchError::SecretUnreadable
            | FetchError::Auth
            | FetchError::KkTooOld
            | FetchError::NotAdmin
            | FetchError::NothingVisible
            | FetchError::Cert => true,
            FetchError::Timeout
            | FetchError::Connect
            | FetchError::Redirects
            | FetchError::TooLarge
            | FetchError::Invalid
            | FetchError::RateLimited
            | FetchError::Other
            | FetchError::Status(_) => false,
        }
    }

    /// Whether it says the service did not answer properly (it may come back
    /// by itself): no connection, a timeout, a server error, an answer that
    /// is not what was expected. The poller makes the «not answering» need of
    /// these once they repeat. A 4xx status, a rate limit and every
    /// [`FetchError::is_config`] error are not.
    ///
    /// No catch-all arm either: a new error has to be classified.
    pub fn is_unreachable(&self) -> bool {
        match self {
            FetchError::Connect
            | FetchError::Timeout
            | FetchError::Invalid
            | FetchError::Other
            | FetchError::Redirects
            | FetchError::TooLarge => true,
            FetchError::Status(code) => (500..600).contains(code),
            FetchError::Config
            | FetchError::Forbidden
            | FetchError::RedirectScheme
            | FetchError::SecretUnreadable
            | FetchError::Auth
            | FetchError::KkTooOld
            | FetchError::NotAdmin
            | FetchError::NothingVisible
            | FetchError::Cert
            | FetchError::RateLimited => false,
        }
    }

    /// The translation key of the text, when it has one (`HTTP n` is the
    /// same in every language).
    pub fn text_key(&self) -> Option<&'static str> {
        Some(match self {
            FetchError::Config => "fetch_err.config",
            FetchError::Forbidden => "fetch_err.forbidden",
            FetchError::Timeout => "fetch_err.timeout",
            FetchError::Connect => "fetch_err.connect",
            FetchError::Cert => "fetch_err.cert",
            FetchError::Redirects => "fetch_err.redirects",
            FetchError::RedirectScheme => "fetch_err.redirect_scheme",
            FetchError::TooLarge => "fetch_err.too_large",
            FetchError::Invalid => "fetch_err.invalid",
            FetchError::SecretUnreadable => "fetch_err.secret",
            FetchError::Auth => "fetch_err.auth",
            FetchError::KkTooOld => "fetch_err.kk_too_old",
            FetchError::NotAdmin => "fetch_err.not_admin",
            FetchError::NothingVisible => "fetch_err.nothing_visible",
            FetchError::RateLimited => "fetch_err.rate_limited",
            FetchError::Other => "fetch_err.other",
            FetchError::Status(_) => return None,
        })
    }
}

/// The text of a text-valued metric in the current language. A kind whose
/// metric takes one of a few known words (Unraid's array state) has a text for
/// each in the locale files, under `value.<kind>_<metric>_<word>` with the
/// word in lower case; any other value shows as it is. The key is built from
/// stored data, so only characters that can be in a key are used.
pub fn value_label(kind: &str, metric: &str, raw: &str) -> String {
    let word: String = raw
        .trim()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .collect();
    if word.is_empty() || word.len() > 40 {
        return raw.to_string();
    }
    vole_kit::i18n::lookup(
        vole_kit::i18n::current(),
        &format!("value.{kind}_{metric}_{word}"),
    )
    .map_or_else(|| raw.to_string(), str::to_string)
}

/// Replaces the text of each text-format metric in `values` with its
/// [`value_label`].
pub fn localize_texts(kind: &str, values: &mut BTreeMap<String, Value>) {
    let Some(integration) = find(kind) else {
        return;
    };
    for metric in integration
        .metrics()
        .iter()
        .filter(|m| m.format == Format::Text)
    {
        if let Some(value) = values.get_mut(metric.id)
            && let Some(text) = value.text.as_mut()
        {
            *text = value_label(kind, metric.id, text);
        }
    }
}

/// The translation key for a stored error text (a [`Snapshot::error`]), when
/// it is one of [`FetchError`]'s fixed texts.
pub fn error_text_key(text: &str) -> Option<&'static str> {
    FetchError::from_text(text).and_then(|e| e.text_key())
}

/// The redirect rules that are not about the address.
#[derive(Debug, Clone, Copy, thiserror::Error)]
enum RedirectError {
    #[error("too many redirects")]
    TooMany,
    #[error("redirect to a scheme that is not http or https")]
    Scheme,
    #[error("redirect to another origin while a secret is sent")]
    CrossOrigin,
}

tokio::task_local! {
    /// Set while a request that carries a secret is sent (see [`with_secret`]).
    static SENDS_SECRET: ();
}

/// Runs `f` (a request being sent) as one that carries a secret: the redirect
/// policy then refuses a redirect to an origin other than the one the request
/// began at ([`FetchError::RedirectScheme`]). The HTTP client drops
/// `Authorization` on such a redirect but not other headers, and the
/// sensitive mark on a header value does not change that, so every request
/// with a key goes through here (see [`api::send_credentialed`]). A request
/// without a secret keeps the ordinary rules.
pub(crate) async fn with_secret<F: Future>(f: F) -> F::Output {
    SENDS_SECRET.scope((), f).await
}

/// Where a URL's origin is: scheme, host and port.
fn origin(url: &url::Url) -> (&str, Option<&str>, Option<u16>) {
    (url.scheme(), url.host_str(), url.port_or_known_default())
}

/// What a failed request is, for a short reason. The error's own text (which
/// can hold the URL) is never used.
pub fn classify(err: &reqwest::Error) -> FetchError {
    let mut source: Option<&(dyn std::error::Error + 'static)> = Some(err);
    while let Some(e) = source {
        if e.downcast_ref::<Forbidden>().is_some() {
            return FetchError::Forbidden;
        }
        match e.downcast_ref::<RedirectError>() {
            Some(RedirectError::TooMany) => return FetchError::Redirects,
            Some(RedirectError::Scheme | RedirectError::CrossOrigin) => {
                return FetchError::RedirectScheme;
            }
            None => {}
        }
        if pin::is_certificate_error(e) {
            return FetchError::Cert;
        }
        source = e.source();
    }
    if err.is_timeout() {
        FetchError::Timeout
    } else if err.is_connect() {
        FetchError::Connect
    } else if err.is_redirect() {
        FetchError::Redirects
    } else if err.is_decode() || err.is_body() {
        FetchError::Invalid
    } else {
        FetchError::Other
    }
}

fn redirect_policy() -> reqwest::redirect::Policy {
    reqwest::redirect::Policy::custom(|attempt| {
        if attempt.previous().len() > MAX_REDIRECTS {
            return attempt.error(RedirectError::TooMany);
        }
        if !matches!(attempt.url().scheme(), "http" | "https") {
            return attempt.error(RedirectError::Scheme);
        }
        // A token sent over https must not follow a redirect to plain http.
        if attempt.url().scheme() == "http"
            && attempt
                .previous()
                .last()
                .is_some_and(|p| p.scheme() == "https")
        {
            return attempt.error(RedirectError::Scheme);
        }
        // A secret stays with the origin it was sent to.
        if SENDS_SECRET.try_with(|()| ()).is_ok()
            && attempt
                .previous()
                .first()
                .is_some_and(|first| origin(first) != origin(attempt.url()))
        {
            return attempt.error(RedirectError::CrossOrigin);
        }
        // A name is checked when it is resolved; an address in the URL never
        // reaches the resolver.
        if let Some(host) = attempt.url().host_str()
            && let Err(err) = check_host(host)
        {
            return attempt.error(err);
        }
        attempt.follow()
    })
}

/// What every integration client has in common: who it says it is, no proxy
/// (it would connect for us and skip the guarded resolver), the guarded
/// resolver, the redirect rules and the timeouts. [`build_client`] and the
/// pinned clients (see [`pin`]) differ only in the TLS they start from.
pub(crate) fn guarded(
    builder: reqwest::ClientBuilder,
    timeout: Duration,
) -> reqwest::ClientBuilder {
    builder
        .user_agent(crate::product::USER_AGENT)
        .no_proxy()
        .dns_resolver(Arc::new(GuardedResolver))
        .redirect(redirect_policy())
        .connect_timeout(timeout.min(Duration::from_secs(5)))
        .timeout(timeout)
}

/// A client for integrations that gives up after `timeout` (in all). Tests
/// use short ones; everything else uses [`client`].
pub fn build_client(timeout: Duration) -> reqwest::Client {
    guarded(vole_kit::tls_roots::http_client(), timeout)
        .build()
        .expect("the HTTP client for integrations can be built")
}

/// The shared client: [`TIMEOUT`], guarded addresses, at most
/// [`MAX_REDIRECTS`] redirects.
pub fn client() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(|| build_client(TIMEOUT))
}

/// The client for a service with these settings (which carry its address as
/// `"url"`): the shared one, or, when the settings hold a pin, the one that
/// accepts only that certificate. A pin that is not a fingerprint, or one on
/// an address that is not `https`, is [`FetchError::Config`]: it never falls
/// back to the shared client.
pub fn client_for(settings: &Json) -> Result<reqwest::Client, FetchError> {
    let Some(fingerprint) = pin::from_settings(settings)? else {
        return Ok(client().clone());
    };
    let https = settings
        .get("url")
        .and_then(Json::as_str)
        .and_then(|url| url::Url::parse(url.trim()).ok())
        .is_some_and(|url| url.scheme() == "https");
    if !https {
        return Err(FetchError::Config);
    }
    pin::pinned_client(&fingerprint)
}

/// The body of `response`, at most [`MAX_BODY`] bytes: past that it stops
/// reading and answers [`FetchError::TooLarge`].
pub async fn read_capped(mut response: reqwest::Response) -> Result<Vec<u8>, FetchError> {
    if response
        .content_length()
        .is_some_and(|n| n > MAX_BODY as u64)
    {
        return Err(FetchError::TooLarge);
    }
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|e| classify(&e))? {
        if body.len() + chunk.len() > MAX_BODY {
            return Err(FetchError::TooLarge);
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A number for each error. The match has no catch-all: a new error does
    /// not compile until it is placed here, and the test below then insists
    /// that it is in [`FetchError::ALL`] (or is `Status`).
    fn slot(e: &FetchError) -> usize {
        match e {
            FetchError::Config => 0,
            FetchError::Forbidden => 1,
            FetchError::Timeout => 2,
            FetchError::Connect => 3,
            FetchError::Cert => 4,
            FetchError::Redirects => 5,
            FetchError::RedirectScheme => 6,
            FetchError::TooLarge => 7,
            FetchError::Invalid => 8,
            FetchError::SecretUnreadable => 9,
            FetchError::Auth => 10,
            FetchError::KkTooOld => 11,
            FetchError::NotAdmin => 12,
            FetchError::NothingVisible => 13,
            FetchError::RateLimited => 14,
            FetchError::Other => 15,
            FetchError::Status(_) => 16,
        }
    }

    #[test]
    fn all_lists_every_error_but_status_once() {
        let mut slots: Vec<usize> = FetchError::ALL.iter().map(slot).collect();
        slots.push(slot(&FetchError::Status(500)));
        slots.sort_unstable();
        assert_eq!(slots, (0..=16).collect::<Vec<_>>());
    }

    #[test]
    fn a_stored_text_gives_its_error_back() {
        for e in FetchError::ALL {
            assert_eq!(FetchError::from_text(&e.to_string()), Some(e), "{e}");
            assert!(e.text_key().is_some(), "{e}");
        }
        assert_eq!(
            FetchError::from_text("HTTP 503"),
            Some(FetchError::Status(503))
        );
        for odd in ["", "HTTP", "HTTP x", "something else", "Config"] {
            assert_eq!(FetchError::from_text(odd), None, "{odd:?}");
        }
        assert_eq!(FetchError::Status(503).text_key(), None);
    }

    #[test]
    fn config_and_unreachable_never_overlap() {
        let all = FetchError::ALL.into_iter().chain([
            FetchError::Status(404),
            FetchError::Status(500),
            FetchError::Status(503),
        ]);
        for e in all {
            assert!(!(e.is_config() && e.is_unreachable()), "{e}");
        }
        for e in [FetchError::Connect, FetchError::Timeout, FetchError::Other] {
            assert!(e.is_unreachable() && !e.is_config(), "{e}");
        }
        assert!(FetchError::Status(502).is_unreachable());
        assert!(!FetchError::Status(404).is_unreachable());
        assert!(!FetchError::RateLimited.is_unreachable());
        assert!(!FetchError::RateLimited.is_config());
        for e in [
            FetchError::Cert,
            FetchError::Forbidden,
            FetchError::RedirectScheme,
        ] {
            assert!(e.is_config() && !e.is_unreachable(), "{e}");
        }
    }
}
