//! What the kinds that read a JSON API with a key have in common: checking
//! the address, joining a path to it, the key as a header ([`Credential`]),
//! one GET or POST that turns the answer into a value or one of
//! [`FetchError`]'s fixed reasons, a memory of addresses that answered 404,
//! and cleaning text that came from outside.
//!
//! Every request sent with a [`Credential`] refuses a redirect to another
//! origin (see [`super::with_secret`]): which headers a redirect carries on
//! depends on their names, so the key is not left to that.
//!
//! Nothing here puts a token, an address or a response text into an error.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::{Duration, Instant};

use reqwest::header::{ACCEPT, HeaderName, HeaderValue};
use reqwest::{RequestBuilder, Response};
use serde::de::DeserializeOwned;
use serde_json::Value as Json;
use url::Url;
use vole_kit::net::check_host;

use super::{FetchError, classify, read_capped};

/// How long an address that answered 404 is left alone. Short, because an
/// app that was too old is often upgraded within minutes of being added.
pub const OLD_FOR: Duration = Duration::from_secs(5 * 60);
/// Most addresses an [`OldAddresses`] remembers; past that the oldest goes.
pub const OLD_MAX: usize = 128;

/// The service's address: `http` or `https`, a host, no user name or
/// password. It is read from the settings' `"url"`. A link-local, metadata,
/// unspecified or multicast address is [`FetchError::Forbidden`].
pub fn base_url(settings: &Json) -> Result<Url, FetchError> {
    let text = settings
        .get("url")
        .and_then(Json::as_str)
        .ok_or(FetchError::Config)?;
    let url = Url::parse(text.trim()).map_err(|_| FetchError::Config)?;
    if !matches!(url.scheme(), "http" | "https")
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err(FetchError::Config);
    }
    let host = url.host_str().ok_or(FetchError::Config)?;
    // An address in the URL never reaches the guarded resolver.
    check_host(host).map_err(|_| FetchError::Forbidden)?;
    Ok(url)
}

/// `segments` after the address's own path (the service may sit under a
/// prefix behind a proxy), with `query` and without the address's own query
/// and fragment.
pub fn endpoint(base: &Url, segments: &[&str], query: Option<&str>) -> Result<Url, FetchError> {
    let mut url = base.clone();
    url.set_query(None);
    url.set_fragment(None);
    url.path_segments_mut()
        .map_err(|()| FetchError::Config)?
        .pop_if_empty()
        .extend(segments);
    url.set_query(query);
    Ok(url)
}

/// A secret as one request header: the header's name and its value. The value
/// is always marked sensitive, so it stays out of debug output.
///
/// Sending it with [`get_json`] or [`post_json`] also makes the request refuse
/// a redirect to another origin. The sensitive mark does not do that, and
/// neither does the HTTP client for every name: it drops `Authorization` on
/// such a redirect, but a header like `x-api-key` would go along.
#[derive(Clone, Debug)]
pub struct Credential {
    name: HeaderName,
    value: HeaderValue,
}

impl Credential {
    /// `Authorization: Bearer <secret>`. No secret, or one that cannot be a
    /// header value, is [`FetchError::Config`].
    pub fn bearer(secret: Option<&str>) -> Result<Credential, FetchError> {
        let secret = secret.filter(|s| !s.is_empty()).ok_or(FetchError::Config)?;
        Credential::raw("authorization", &format!("Bearer {secret}"))
    }

    /// The secret as the whole value of the header `name` (`x-api-key`).
    pub fn header(name: &'static str, secret: Option<&str>) -> Result<Credential, FetchError> {
        let secret = secret.filter(|s| !s.is_empty()).ok_or(FetchError::Config)?;
        Credential::raw(name, secret)
    }

    /// The header `name` with exactly `value` (Proxmox's
    /// `Authorization: PVEAPIToken=id=secret`).
    pub fn raw(name: &'static str, value: &str) -> Result<Credential, FetchError> {
        let mut value = HeaderValue::from_str(value).map_err(|_| FetchError::Config)?;
        value.set_sensitive(true);
        Ok(Credential {
            name: HeaderName::from_static(name),
            value,
        })
    }

    /// `request` with the header.
    pub fn apply(&self, request: RequestBuilder) -> RequestBuilder {
        request.header(self.name.clone(), self.value.clone())
    }
}

/// Sends `request`, which carries a secret: a redirect to another origin is
/// refused ([`FetchError::RedirectScheme`]).
pub async fn send_credentialed(request: RequestBuilder) -> Result<Response, FetchError> {
    super::with_secret(request.send())
        .await
        .map_err(|err| classify(&err))
}

/// What a GET found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reply<T> {
    /// A 2xx answer, read.
    Found(T),
    /// A 404: the address has no such endpoint (an older server, say).
    NotFound,
}

impl<T> Reply<T> {
    /// The value, or the 404 as a [`FetchError::Status`] for a kind that has
    /// nothing to fall back on.
    pub fn found(self) -> Result<T, FetchError> {
        match self {
            Reply::Found(value) => Ok(value),
            Reply::NotFound => Err(FetchError::Status(404)),
        }
    }
}

/// One GET with the credential. 401 and 403 are [`FetchError::Auth`], 404 is
/// [`Reply::NotFound`], any other answer outside 2xx is
/// [`FetchError::Status`]; the body is read to at most
/// [`super::MAX_BODY`] bytes and parsed (anything else is
/// [`FetchError::Invalid`]). A redirect to another origin is refused.
pub async fn get_json<T: DeserializeOwned>(
    http: &reqwest::Client,
    url: Url,
    credential: &Credential,
) -> Result<Reply<T>, FetchError> {
    let request = credential.apply(http.get(url).header(ACCEPT, "application/json"));
    reply(send_credentialed(request).await?).await
}

/// [`get_json`] for an endpoint that is for admins: a 403 means the key is
/// known but is no admin key ([`FetchError::NotAdmin`]); a 401 is
/// [`FetchError::Auth`].
pub async fn get_json_admin<T: DeserializeOwned>(
    http: &reqwest::Client,
    url: Url,
    credential: &Credential,
) -> Result<Reply<T>, FetchError> {
    let request = credential.apply(http.get(url).header(ACCEPT, "application/json"));
    reply_with(send_credentialed(request).await?, FetchError::NotAdmin).await
}

/// One GET without any key, for an address that is public (a health
/// endpoint). Answered as [`get_json`] answers; a redirect is followed under
/// the ordinary rules, since nothing secret goes along.
pub async fn get_json_public<T: DeserializeOwned>(
    http: &reqwest::Client,
    url: Url,
) -> Result<Reply<T>, FetchError> {
    let response = http
        .get(url)
        .header(ACCEPT, "application/json")
        .send()
        .await
        .map_err(|err| classify(&err))?;
    reply(response).await
}

/// One POST of `body` as JSON with the credential, answered as [`get_json`]
/// answers it.
pub async fn post_json<T: DeserializeOwned>(
    http: &reqwest::Client,
    url: Url,
    credential: &Credential,
    body: &Json,
) -> Result<Reply<T>, FetchError> {
    let request = credential.apply(http.post(url).header(ACCEPT, "application/json").json(body));
    reply(send_credentialed(request).await?).await
}

/// What `response` says: see [`get_json`].
async fn reply<T: DeserializeOwned>(response: Response) -> Result<Reply<T>, FetchError> {
    reply_with(response, FetchError::Auth).await
}

/// What `response` says, with `forbidden` for a 403.
async fn reply_with<T: DeserializeOwned>(
    response: Response,
    forbidden: FetchError,
) -> Result<Reply<T>, FetchError> {
    match response.status().as_u16() {
        401 => Err(FetchError::Auth),
        403 => Err(forbidden),
        404 => Ok(Reply::NotFound),
        code if !(200..300).contains(&code) => Err(FetchError::Status(code)),
        _ => {
            let body = read_capped(response).await?;
            serde_json::from_slice(&body)
                .map(Reply::Found)
                .map_err(|_| FetchError::Invalid)
        }
    }
}

/// The addresses that answered 404, and when.
type Seen = Mutex<VecDeque<(String, Instant)>>;

/// Every [`OldAddresses`] that holds anything, so [`forget_all_for`] reaches
/// the memory of each kind without the kinds having to be listed. Dropped
/// memories are swept when it is used.
fn registry() -> std::sync::MutexGuard<'static, Vec<Weak<Seen>>> {
    static ALL: Mutex<Vec<Weak<Seen>>> = Mutex::new(Vec::new());
    ALL.lock().unwrap_or_else(|e| e.into_inner())
}

/// The addresses that answered 404, and when: a kind asks them again only
/// after [`OLD_FOR`]. The keys never leave memory. Bounded by [`OLD_MAX`],
/// oldest first out.
#[derive(Debug, Default)]
pub struct OldAddresses {
    seen: OnceLock<Arc<Seen>>,
}

impl OldAddresses {
    pub const fn new() -> Self {
        OldAddresses {
            seen: OnceLock::new(),
        }
    }

    fn list(&self) -> std::sync::MutexGuard<'_, VecDeque<(String, Instant)>> {
        let seen = self.seen.get_or_init(|| {
            let seen = Arc::new(Mutex::new(VecDeque::new()));
            registry().push(Arc::downgrade(&seen));
            seen
        });
        // A panic elsewhere cannot leave the list in a state that matters.
        seen.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Whether `key` answered 404 less than [`OLD_FOR`] before `now`.
    pub fn is_old(&self, key: &str, now: Instant) -> bool {
        let mut seen = self.list();
        seen.retain(|(_, at)| now.saturating_duration_since(*at) < OLD_FOR);
        seen.iter().any(|(k, _)| k == key)
    }

    /// Remembers that `key` answered 404 at `now`.
    pub fn mark(&self, key: &str, now: Instant) {
        let mut seen = self.list();
        seen.retain(|(k, at)| k != key && now.saturating_duration_since(*at) < OLD_FOR);
        while seen.len() >= OLD_MAX {
            seen.pop_front();
        }
        seen.push_back((key.to_string(), now));
    }

    /// Forgets `key` (it answered, so it is not old after all).
    pub fn forget(&self, key: &str) {
        self.list().retain(|(k, _)| k != key);
    }

    /// How many addresses are remembered.
    pub fn len(&self) -> usize {
        self.list().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Makes every kind ask the service at `base` again, whatever it answered
/// before: forgets each remembered 404 of an address on `base` (its origin and
/// path, without the query and fragment). Call it when the service is saved or
/// tested. A `base` that is not an address changes nothing.
pub fn forget_all_for(base: &str) {
    let Ok(url) = Url::parse(base.trim()) else {
        return;
    };
    if !url.origin().is_tuple() {
        return;
    }
    // The keys are endpoint addresses (see [`endpoint`]): the service's own
    // address and then the path of the call.
    let prefix = format!(
        "{}{}/",
        url.origin().ascii_serialization(),
        url.path().trim_end_matches('/')
    );
    let all: Vec<Arc<Seen>> = {
        let mut all = registry();
        all.retain(|weak| weak.strong_count() > 0);
        all.iter().filter_map(Weak::upgrade).collect()
    };
    for seen in all {
        seen.lock()
            .unwrap_or_else(|e| e.into_inner())
            .retain(|(key, _)| !key.starts_with(&prefix));
    }
}

/// Whether `c` is a format character (Unicode category Cf): zero-width and
/// joining characters, the bidirectional marks, embeddings, overrides and
/// isolates, the byte order mark, language tags.
fn is_format(c: char) -> bool {
    matches!(c,
        '\u{00AD}'
        | '\u{0600}'..='\u{0605}'
        | '\u{061C}'
        | '\u{06DD}'
        | '\u{070F}'
        | '\u{0890}'..='\u{0891}'
        | '\u{08E2}'
        | '\u{180E}'
        | '\u{200B}'..='\u{200F}'
        | '\u{202A}'..='\u{202E}'
        | '\u{2060}'..='\u{2064}'
        | '\u{2066}'..='\u{206F}'
        | '\u{FEFF}'
        | '\u{FFF9}'..='\u{FFFB}'
        | '\u{110BD}'
        | '\u{110CD}'
        | '\u{13430}'..='\u{1343F}'
        | '\u{1BCA0}'..='\u{1BCA3}'
        | '\u{1D173}'..='\u{1D17A}'
        | '\u{E0001}'
        | '\u{E0020}'..='\u{E007F}')
}

/// Text from outside as one short line that is safe to show: control
/// characters (Cc) and format characters (Cf, bidi overrides included) are
/// stripped, except that line breaks and tabs become a space; it is cut at
/// `max` characters and trimmed. Empty when nothing is left.
pub fn clean_text(text: &str, max: usize) -> String {
    let line: String = text
        .chars()
        .filter_map(|c| match c {
            '\t' | '\n' | '\r' | '\u{0B}' | '\u{0C}' | '\u{85}' => Some(' '),
            c if c.is_control() || is_format(c) => None,
            c => Some(c),
        })
        .take(max)
        .collect();
    line.trim().to_string()
}

/// A count: a JSON number that is finite and not below 0, rounded down. Odd
/// values (null, text, negative, huge) are `None`, so only that metric goes.
pub fn count(value: Option<&Json>) -> Option<f64> {
    value
        .and_then(Json::as_f64)
        .filter(|n| n.is_finite() && *n >= 0.0)
        .map(f64::floor)
}

/// A percentage: a finite JSON number, clamped to 0..=100. Odd values are
/// `None`.
pub fn percent(value: Option<&Json>) -> Option<f64> {
    value
        .and_then(Json::as_f64)
        .filter(|n| n.is_finite())
        .map(|n| n.clamp(0.0, 100.0))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn the_address_is_checked() {
        let at = |text: &str| base_url(&json!({ "url": text }));
        assert!(at("http://h:3000/").is_ok());
        assert!(at(" https://h/vole/ ").is_ok());
        for bad in ["", "nope", "ftp://h/", "http://u:p@h/", "http://u@h/"] {
            assert_eq!(at(bad).unwrap_err(), FetchError::Config, "{bad:?}");
        }
        assert_eq!(
            base_url(&json!({})).unwrap_err(),
            FetchError::Config,
            "no address"
        );
        for bad in ["http://169.254.169.254/", "http://[fe80::1]/"] {
            assert_eq!(at(bad).unwrap_err(), FetchError::Forbidden, "{bad}");
        }
    }

    #[test]
    fn the_endpoint_follows_the_path_of_the_address() {
        let at = |text: &str, query: Option<&str>| {
            endpoint(&Url::parse(text).unwrap(), &["api", "v1", "summary"], query)
                .unwrap()
                .to_string()
        };
        assert_eq!(at("http://h:3000", None), "http://h:3000/api/v1/summary");
        assert_eq!(
            at("https://h/vole/", Some("days=30")),
            "https://h/vole/api/v1/summary?days=30"
        );
        assert_eq!(
            at("https://h/vole?x=1#f", None),
            "https://h/vole/api/v1/summary"
        );
        // A name with a slash stays one segment.
        let url = endpoint(&Url::parse("http://h/").unwrap(), &["a/b", "c d"], None).unwrap();
        assert_eq!(url.path(), "/a%2Fb/c%20d");
    }

    #[test]
    fn the_credentials_are_sensitive_and_checked() {
        let credential = Credential::bearer(Some("ww_abc")).unwrap();
        assert!(credential.value.is_sensitive());
        assert_eq!(credential.name, reqwest::header::AUTHORIZATION);
        assert_eq!(credential.value.to_str().unwrap(), "Bearer ww_abc");
        assert!(!format!("{credential:?}").contains("ww_abc"));
        for bad in [None, Some(""), Some("a\r\nb")] {
            assert_eq!(
                Credential::bearer(bad).unwrap_err(),
                FetchError::Config,
                "{bad:?}"
            );
        }
        let key = Credential::header("x-api-key", Some("k-123")).unwrap();
        assert!(key.value.is_sensitive());
        assert_eq!(key.name.as_str(), "x-api-key");
        assert_eq!(key.value.to_str().unwrap(), "k-123");
        assert!(!format!("{key:?}").contains("k-123"));
        for bad in [None, Some(""), Some("a\nb")] {
            assert_eq!(
                Credential::header("x-api-key", bad).unwrap_err(),
                FetchError::Config
            );
        }
        let raw = Credential::raw("authorization", "PVEAPIToken=u@pam!t=s").unwrap();
        assert!(raw.value.is_sensitive());
        assert!(!format!("{raw:?}").contains("PVEAPIToken=u"));
        assert_eq!(
            Credential::raw("authorization", "a\r\nb").unwrap_err(),
            FetchError::Config
        );
    }

    #[test]
    fn an_old_address_is_left_alone_for_five_minutes() {
        assert_eq!(OLD_FOR, Duration::from_secs(5 * 60));
        let start = Instant::now();
        let old = OldAddresses::new();
        assert!(!old.is_old("a", start));
        old.mark("a", start);
        assert!(old.is_old("a", start));
        assert!(old.is_old("a", start + OLD_FOR - Duration::from_secs(1)));
        assert!(!old.is_old("b", start));
        assert!(!old.is_old("a", start + OLD_FOR));
        assert!(old.is_empty());
        old.mark("a", start);
        old.forget("a");
        assert!(!old.is_old("a", start));
    }

    #[test]
    fn an_address_is_forgotten_by_every_kind_that_remembers_it() {
        let now = Instant::now();
        let (vv, kk) = (OldAddresses::new(), OldAddresses::new());
        let key = |base: &str, path: &str| format!("{base}{path}");
        for memory in [&vv, &kk] {
            memory.mark(&key("http://forget-me.test:3000", "/api/v1/summary"), now);
            memory.mark(
                &key("http://forget-me.test:3000/vole", "/api/v1/summary"),
                now,
            );
            memory.mark(&key("http://keep-me.test:3000", "/api/v1/summary"), now);
            memory.mark(&key("http://forget-me.test:3001", "/api/v1/summary"), now);
        }
        // The address as an admin typed it: a trailing slash, a query, a fragment.
        forget_all_for(" http://forget-me.test:3000/?x=1#f ");
        for memory in [&vv, &kk] {
            assert!(!memory.is_old("http://forget-me.test:3000/api/v1/summary", now));
            assert!(
                !memory.is_old("http://forget-me.test:3000/vole/api/v1/summary", now),
                "a service under a prefix of the same origin goes too"
            );
            assert!(memory.is_old("http://keep-me.test:3000/api/v1/summary", now));
            assert!(
                memory.is_old("http://forget-me.test:3001/api/v1/summary", now),
                "another port is another address"
            );
        }
        // An address under a path goes without the rest of its origin.
        let other = OldAddresses::new();
        other.mark("http://path.test/a/api/v1/summary", now);
        other.mark("http://path.test/b/api/v1/summary", now);
        forget_all_for("http://path.test/a");
        assert!(!other.is_old("http://path.test/a/api/v1/summary", now));
        assert!(other.is_old("http://path.test/b/api/v1/summary", now));
        // Nonsense changes nothing.
        forget_all_for("");
        forget_all_for("not an address");
        assert!(other.is_old("http://path.test/b/api/v1/summary", now));
    }

    #[test]
    fn a_dropped_memory_does_not_stay_in_the_registry() {
        {
            let gone = OldAddresses::new();
            gone.mark("http://dropped.test/api", Instant::now());
        }
        // Must not panic or leak; the dead entry is swept.
        forget_all_for("http://dropped.test/");
        let live = OldAddresses::new();
        live.mark("http://dropped.test/api", Instant::now());
        forget_all_for("http://dropped.test/");
        assert!(live.is_empty());
    }

    #[test]
    fn the_memory_is_bounded_and_the_oldest_goes_first() {
        let start = Instant::now();
        let old = OldAddresses::new();
        for i in 0..(OLD_MAX + 10) {
            old.mark(
                &format!("http://h{i}/"),
                start + Duration::from_secs(i as u64),
            );
        }
        let now = start + Duration::from_secs(OLD_MAX as u64 + 10);
        assert_eq!(old.len(), OLD_MAX);
        assert!(!old.is_old("http://h0/", now));
        assert!(!old.is_old("http://h9/", now));
        assert!(old.is_old("http://h10/", now));
        assert!(old.is_old(&format!("http://h{}/", OLD_MAX + 9), now));
        // Marking one again does not make a second entry.
        old.mark("http://h10/", now);
        assert_eq!(old.len(), OLD_MAX);
    }

    #[test]
    fn text_from_outside_is_cleaned_and_cut() {
        assert_eq!(clean_text("a\u{0}b\u{7}c", 80), "abc");
        assert_eq!(clean_text("  a\nb\tc  ", 80), "a b c");
        // Bidi overrides, isolates, zero-width and the byte order mark.
        let hostile = "a\u{202E}b\u{2066}c\u{2069}d\u{200B}e\u{FEFF}f\u{00AD}g";
        assert_eq!(clean_text(hostile, 80), "abcdefg");
        assert_eq!(clean_text("\u{E0041}x\u{E007F}", 80), "x");
        assert_eq!(clean_text("\n\t \u{202E}", 80), "");
        assert_eq!(clean_text(&"é".repeat(500), 200).chars().count(), 200);
        // Ordinary text, also from other scripts and with emoji, stays.
        assert_eq!(clean_text("Café – 日本語 🚀", 80), "Café – 日本語 🚀");
    }

    #[test]
    fn odd_numbers_only_drop_their_own_metric() {
        let n = |v: Json| count(Some(&v));
        assert_eq!(n(json!(3)), Some(3.0));
        assert_eq!(n(json!(3.7)), Some(3.0));
        assert_eq!(n(json!(0)), Some(0.0));
        for odd in [json!(-1), json!(null), json!("3"), json!([]), json!({})] {
            assert_eq!(n(odd), None);
        }
        assert_eq!(count(None), None);
        let p = |v: Json| percent(Some(&v));
        assert_eq!(p(json!(99.5)), Some(99.5));
        assert_eq!(p(json!(140)), Some(100.0));
        assert_eq!(p(json!(-3)), Some(0.0));
        assert_eq!(p(json!(null)), None);
        assert_eq!(p(json!("99")), None);
        assert_eq!(percent(None), None);
    }
}
