//! What the kinds that read a JSON API with a bearer token have in common:
//! checking the address, joining a path to it, the token as a header, one GET
//! that turns the answer into a value or one of [`FetchError`]'s fixed
//! reasons, a memory of addresses that answered 404, and cleaning text that
//! came from outside.
//!
//! Nothing here puts a token, an address or a response text into an error.

use std::collections::VecDeque;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use reqwest::header::{ACCEPT, AUTHORIZATION, HeaderName, HeaderValue};
use serde::de::DeserializeOwned;
use serde_json::Value as Json;
use url::Url;
use vole_kit::net::check_host;

use super::{FetchError, classify, read_capped};

/// How long an address that answered 404 is left alone.
pub const OLD_FOR: Duration = Duration::from_secs(60 * 60);
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

/// The token as an `Authorization: Bearer` value, marked sensitive so it
/// stays out of debug output and is not sent on to another host by a
/// redirect. No token, or one that cannot be a header value, is
/// [`FetchError::Config`].
pub fn bearer(secret: Option<&str>) -> Result<HeaderValue, FetchError> {
    let secret = secret.filter(|s| !s.is_empty()).ok_or(FetchError::Config)?;
    let mut value =
        HeaderValue::from_str(&format!("Bearer {secret}")).map_err(|_| FetchError::Config)?;
    value.set_sensitive(true);
    Ok(value)
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

/// One GET with the token. 401 and 403 are [`FetchError::Auth`], 404 is
/// [`Reply::NotFound`], any other answer outside 2xx is
/// [`FetchError::Status`]; the body is read to at most
/// [`super::MAX_BODY`] bytes and parsed (anything else is
/// [`FetchError::Invalid`]).
pub async fn get_json<T: DeserializeOwned>(
    http: &reqwest::Client,
    url: Url,
    auth: &HeaderValue,
) -> Result<Reply<T>, FetchError> {
    let response = http
        .get(url)
        .header(AUTHORIZATION, auth.clone())
        .header(ACCEPT, "application/json")
        .send()
        .await
        .map_err(|err| classify(&err))?;
    reply(response).await
}

/// One POST of `body` as JSON, with the key as the header `name` (not
/// `Authorization`: a service such as Unraid takes `x-api-key`). The answer is
/// read as [`get_json`] reads it. A redirect to another origin is not followed
/// ([`FetchError::RedirectScheme`]): unlike `Authorization`, a header of this
/// kind would go along with it.
pub async fn post_json<T: DeserializeOwned>(
    http: &reqwest::Client,
    url: Url,
    name: HeaderName,
    auth: &HeaderValue,
    body: &Json,
) -> Result<Reply<T>, FetchError> {
    let request = http
        .post(url)
        .header(name, auth.clone())
        .header(ACCEPT, "application/json")
        .json(body)
        .send();
    let response = super::KEY_IN_OWN_HEADER
        .scope((), request)
        .await
        .map_err(|err| classify(&err))?;
    reply(response).await
}

/// What `response` says: see [`get_json`].
async fn reply<T: DeserializeOwned>(response: reqwest::Response) -> Result<Reply<T>, FetchError> {
    match response.status().as_u16() {
        401 | 403 => Err(FetchError::Auth),
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

/// The addresses that answered 404, and when: a kind asks them again only
/// after [`OLD_FOR`]. The keys never leave memory. Bounded by [`OLD_MAX`],
/// oldest first out.
#[derive(Debug, Default)]
pub struct OldAddresses {
    seen: Mutex<VecDeque<(String, Instant)>>,
}

impl OldAddresses {
    pub const fn new() -> Self {
        OldAddresses {
            seen: Mutex::new(VecDeque::new()),
        }
    }

    fn list(&self) -> std::sync::MutexGuard<'_, VecDeque<(String, Instant)>> {
        // A panic elsewhere cannot leave the list in a state that matters.
        self.seen.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Whether `key` answered 404 less than an hour before `now`.
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
    fn the_token_header_is_sensitive_and_checked() {
        let value = bearer(Some("ww_abc")).unwrap();
        assert!(value.is_sensitive());
        assert_eq!(value.to_str().unwrap(), "Bearer ww_abc");
        assert!(!format!("{value:?}").contains("ww_abc"));
        for bad in [None, Some(""), Some("a\r\nb")] {
            assert_eq!(bearer(bad).unwrap_err(), FetchError::Config, "{bad:?}");
        }
    }

    #[test]
    fn an_old_address_is_left_alone_for_an_hour() {
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
