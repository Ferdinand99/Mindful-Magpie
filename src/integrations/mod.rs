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
//! **Secrets** live in [`FetchCtx::secret`] only. Its `Debug` hides it, and
//! nothing here puts a secret, a settings value or a URL into an error text:
//! [`FetchError`]'s texts are a closed set plus an HTTP status.

pub mod http_kind;
pub mod poller;

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

/// A boxed future, so [`Integration`] can be used as a trait object.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// How long a fetch may take in all (connecting, redirects and reading).
pub const TIMEOUT: Duration = Duration::from_secs(10);
/// How much of a response body a fetch reads before it gives up.
pub const MAX_BODY: usize = 1024 * 1024;
/// How many redirects a fetch follows (to `http` and `https` only).
pub const MAX_REDIRECTS: usize = 3;

/// One kind of service.
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
    /// Whether a service of this kind needs an address (`services.url`).
    fn url_required(&self) -> bool {
        true
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
    static KINDS: [&dyn Integration; 1] = [&http_kind::HTTP];
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

/// What a fetch needs: the shared client, the service's settings and its
/// secret. The settings carry the service's address as `"url"`.
pub struct FetchCtx {
    pub http: reqwest::Client,
    pub settings: Json,
    pub secret: Option<String>,
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
    pub values: BTreeMap<String, Value>,
    pub needs: Vec<Need>,
    pub fetched_at: Timestamp,
    /// A short reason when the fetch failed or the service is not well. Never
    /// contains a secret, a header or a URL (see [`FetchError`]).
    pub error: Option<String>,
}

impl Snapshot {
    /// A reading with no values yet, taken now.
    pub fn new(status: Status) -> Self {
        Snapshot {
            status,
            values: BTreeMap::new(),
            needs: Vec::new(),
            fetched_at: Timestamp::now(),
            error: None,
        }
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
    pub num: Option<f64>,
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

/// How serious a [`Need`] is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
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
    pub params: BTreeMap<String, String>,
    /// Higher is worse.
    pub score: f64,
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
    #[error("too many redirects")]
    Redirects,
    #[error("redirect not allowed")]
    RedirectScheme,
    #[error("response too large")]
    TooLarge,
    #[error("unexpected answer")]
    Invalid,
    #[error("HTTP {0}")]
    Status(u16),
    #[error("request failed")]
    Other,
}

impl FetchError {
    /// The translation key of the text, when it has one (`HTTP n` is the
    /// same in every language).
    pub fn text_key(&self) -> Option<&'static str> {
        Some(match self {
            FetchError::Config => "fetch_err.config",
            FetchError::Forbidden => "fetch_err.forbidden",
            FetchError::Timeout => "fetch_err.timeout",
            FetchError::Connect => "fetch_err.connect",
            FetchError::Redirects => "fetch_err.redirects",
            FetchError::RedirectScheme => "fetch_err.redirect_scheme",
            FetchError::TooLarge => "fetch_err.too_large",
            FetchError::Invalid => "fetch_err.invalid",
            FetchError::Other => "fetch_err.other",
            FetchError::Status(_) => return None,
        })
    }
}

/// The translation key for a stored error text (a [`Snapshot::error`]), when
/// it is one of [`FetchError`]'s fixed texts.
pub fn error_text_key(text: &str) -> Option<&'static str> {
    [
        FetchError::Config,
        FetchError::Forbidden,
        FetchError::Timeout,
        FetchError::Connect,
        FetchError::Redirects,
        FetchError::RedirectScheme,
        FetchError::TooLarge,
        FetchError::Invalid,
        FetchError::Other,
    ]
    .into_iter()
    .find(|e| e.to_string() == text)
    .and_then(|e| e.text_key())
}

/// The redirect rules that are not about the address.
#[derive(Debug, Clone, Copy, thiserror::Error)]
enum RedirectError {
    #[error("too many redirects")]
    TooMany,
    #[error("redirect to a scheme that is not http or https")]
    Scheme,
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
            Some(RedirectError::Scheme) => return FetchError::RedirectScheme,
            None => {}
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

/// A client for integrations that gives up after `timeout` (in all). Tests
/// use short ones; everything else uses [`client`].
pub fn build_client(timeout: Duration) -> reqwest::Client {
    vole_kit::tls_roots::http_client()
        .user_agent(crate::product::USER_AGENT)
        // A proxy would connect for us and skip the guarded resolver.
        .no_proxy()
        .dns_resolver(Arc::new(GuardedResolver))
        .redirect(redirect_policy())
        .connect_timeout(timeout.min(Duration::from_secs(5)))
        .timeout(timeout)
        .build()
        .expect("the HTTP client for integrations can be built")
}

/// The shared client: [`TIMEOUT`], guarded addresses, at most
/// [`MAX_REDIRECTS`] redirects.
pub fn client() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(|| build_client(TIMEOUT))
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
