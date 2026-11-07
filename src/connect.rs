//! The consent half of the browser extension's "Koble til Magpie" flow.
//!
//! The extension opens `GET /connect/extension` in `identity.launchWebAuthFlow`
//! with a PKCE challenge (RFC 7636, S256). A signed-in user approves or
//! cancels; either way the browser is sent back to the extension's redirect
//! address, with a one-time code or with `error=access_denied`. The code is
//! swapped for a read-only API token at `POST /api/v1/extension/token`
//! (see [`crate::api`]).
//!
//! Nothing here redirects to an address that was not validated first: the
//! redirect address must be exactly the one Firefox's
//! `identity.getRedirectURL()` gives for an extension this instance accepts:
//! `https://<host>.extensions.allizom.org/`, where `<host>` is the SHA-1 of the
//! extension's ID in lowercase hex (see [`redirect_host`]). By default that is
//! the official extension only; `MM_EXTENSION_IDS` adds development builds.
//! Anything else is refused with a page, before anything is shown.
//!
//! The token this flow ends in never expires, so approving asks for the
//! user's password, as the kit's Tokens page does for such a token.

use std::time::Instant;

use askama::Template;
use axum::Router;
use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, Extension, RawQuery, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::get;
use axum_extra::extract::cookie::SignedCookieJar;
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use rusqlite::{TransactionBehavior, params};
use sha1::{Digest as _, Sha1};
use vole_kit::auth::{self, CurrentUser, reauth, tokens};
use vole_kit::config::CredentialMode;
use vole_kit::db;
use vole_kit::http::headers::APP_CSP;
use vole_kit::http::{ApiError, ApiResult, HtmlTemplate};
use vole_kit::i18n::{self, Tr as _};
use vole_kit::page::PageCtx;
use vole_kit::state::Core;

use crate::Magpie;
use crate::layout::model::is_format;

/// The extension ID of the official Mindful Magpie extension
/// (`browser_specific_settings.gecko.id` in its manifest).
pub const OFFICIAL_EXTENSION_ID: &str = "mindful-magpie@hedgerow.tools";

/// How long a code lives, in seconds.
pub const CODE_TTL_SECS: i64 = 120;
/// Codes one user may have waiting at the same time; older ones are dropped.
const MAX_PENDING_PER_USER: i64 = 10;
/// The largest body of the approval form (it holds five short fields).
const MAX_FORM_BODY: usize = 16 * 1024;

/// What the consent form carries, validated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Consent {
    pub redirect_uri: String,
    pub state: String,
    pub code_challenge: String,
    pub device: String,
}

/// The host label Firefox's `identity.getRedirectURL()` gives an extension:
/// the SHA-1 of its ID (`browser_specific_settings.gecko.id`), UTF-8, as
/// lowercase hex, 40 characters. From Firefox's
/// `toolkit/components/extensions/child/ext-identity.js`: `computeHash(id)`
/// uses `CryptoHash("sha1")` and `bytesAsHex`, and the address is
/// `https://${hash}.${redirectDomain}/` with the default `redirectDomain`
/// `extensions.allizom.org`. For `mindful-magpie@hedgerow.tools` it is
/// `9bd6a096502a3a8555d679b3f64704219dffb501`.
pub fn redirect_host(extension_id: &str) -> String {
    Sha1::digest(extension_id.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// The redirect hosts this instance accepts: the official extension's, and
/// those of `extra_ids` (blank entries are ignored).
pub fn allowed_hosts(extra_ids: &[String]) -> Vec<String> {
    let mut hosts = vec![redirect_host(OFFICIAL_EXTENSION_ID)];
    for id in extra_ids
        .iter()
        .map(|id| id.trim())
        .filter(|id| !id.is_empty())
    {
        let host = redirect_host(id);
        if !hosts.contains(&host) {
            hosts.push(host);
        }
    }
    hosts
}

/// Whether `uri` is exactly the redirect address of one of `hosts`:
/// `https://<host>.extensions.allizom.org/`. Compared, not parsed: no
/// userinfo, port, path, query, fragment, case or spacing variant passes.
pub fn is_extension_redirect(uri: &str, hosts: &[String]) -> bool {
    uri.strip_prefix("https://")
        .and_then(|rest| rest.strip_suffix(".extensions.allizom.org/"))
        .is_some_and(|host| hosts.iter().any(|allowed| allowed == host))
}

/// `state`: 1 to 128 URL-safe characters (the unreserved set of RFC 3986).
fn is_state(value: &str) -> bool {
    (1..=128).contains(&value.len())
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~'))
}

/// A PKCE S256 challenge: 43 base64url characters (a SHA-256 without padding).
fn is_challenge(value: &str) -> bool {
    value.len() == 43
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
}

/// The device name: 1 to 60 characters once trimmed, no control or format
/// characters (bidirectional overrides, zero-width characters).
fn device_name(value: &str) -> Option<String> {
    let value = value.trim();
    ((1..=60).contains(&value.chars().count())
        && !value.chars().any(|c| c.is_control() || is_format(c)))
    .then(|| value.to_string())
}

/// The value of `key`, or `None` when it is missing or given twice (two
/// values are ambiguous, so the request is refused rather than guessed at).
fn single<'a>(pairs: &'a [(String, String)], key: &str) -> Option<&'a str> {
    let mut found = pairs
        .iter()
        .filter(|(k, _)| k == key)
        .map(|(_, v)| v.as_str());
    let first = found.next()?;
    found.next().is_none().then_some(first)
}

impl Consent {
    /// Validates the five parameters of a connect request, `redirect_uri`
    /// first. `None` for anything wrong.
    fn parse(pairs: &[(String, String)], hosts: &[String]) -> Option<Consent> {
        let redirect_uri =
            single(pairs, "redirect_uri").filter(|u| is_extension_redirect(u, hosts))?;
        let state = single(pairs, "state").filter(|s| is_state(s))?;
        let code_challenge = single(pairs, "code_challenge").filter(|c| is_challenge(c))?;
        (single(pairs, "code_challenge_method")? == "S256").then_some(())?;
        let device = device_name(single(pairs, "device")?)?;
        Some(Consent {
            redirect_uri: redirect_uri.to_string(),
            state: state.to_string(),
            code_challenge: code_challenge.to_string(),
            device,
        })
    }

    /// `redirect_uri` with `query` appended (it ends in `/`).
    fn redirect_with(&self, query: &str) -> Redirect {
        Redirect::to(&format!("{}?{query}", self.redirect_uri))
    }
}

fn pairs(raw: &[u8]) -> Vec<(String, String)> {
    url::form_urlencoded::parse(raw)
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect()
}

/// Pages for signed-in users (the caller adds the sign-in guard).
pub fn router() -> Router<Core> {
    Router::new().route(
        "/connect/extension",
        get(page)
            .post(decide)
            .layer(DefaultBodyLimit::max(MAX_FORM_BODY)),
    )
}

#[derive(Template)]
#[template(path = "connect.html")]
struct ConsentPage {
    ctx: PageCtx,
    /// "Give <device> access to read your start page?", filled in here and
    /// escaped by the template.
    prompt: String,
    consent: Consent,
    /// Why the password was not accepted (shown above its field).
    error: Option<String>,
}

#[derive(Template)]
#[template(path = "connect_refused.html")]
struct RefusedPage {
    ctx: PageCtx,
}

/// The page for a request that failed validation: 400, and nothing from the
/// request is shown or followed.
async fn refused(core: &Core, user: &CurrentUser, jar: SignedCookieJar) -> Response {
    let (ctx, jar) = PageCtx::build(
        core,
        Some(user),
        &jar,
        i18n::t("connect.refused_title"),
        "",
        "",
    )
    .await;
    (
        StatusCode::BAD_REQUEST,
        jar,
        [(header::CACHE_CONTROL, "no-store")],
        HtmlTemplate(RefusedPage { ctx }),
    )
        .into_response()
}

/// The consent page for a validated request. `error` is shown above the
/// password field (the password was missing or wrong).
async fn consent_page(
    core: &Core,
    user: &CurrentUser,
    jar: SignedCookieJar,
    consent: Consent,
    error: Option<String>,
) -> Response {
    let (ctx, jar) = PageCtx::build(core, Some(user), &jar, i18n::t("connect.title"), "", "").await;
    // The form posts back here and the answer redirects to the extension's
    // address, which `form-action 'self'` would block in browsers that check
    // redirects of a form post. The address is validated, so naming its
    // origin is safe. Everything else of the app's CSP stays (it also keeps
    // `frame-ancestors 'none'`, so the page cannot be framed).
    let origin = consent.redirect_uri.trim_end_matches('/');
    let csp = APP_CSP.replace(
        "form-action 'self'",
        &format!("form-action 'self' {origin}"),
    );
    let prompt = i18n::tf("connect.prompt", &[("device", &consent.device)]);
    (
        jar,
        [
            (header::CACHE_CONTROL, "no-store".to_string()),
            (header::CONTENT_SECURITY_POLICY, csp),
        ],
        HtmlTemplate(ConsentPage {
            ctx,
            prompt,
            consent,
            error,
        }),
    )
        .into_response()
}

/// `GET /connect/extension`: validates the request, then asks the user.
async fn page(
    State(core): State<Core>,
    Extension(magpie): Extension<Magpie>,
    user: CurrentUser,
    jar: SignedCookieJar,
    RawQuery(query): RawQuery,
) -> Response {
    let form = pairs(query.as_deref().unwrap_or("").as_bytes());
    let Some(consent) = Consent::parse(&form, &magpie.extension_hosts) else {
        return refused(&core, &user, jar).await;
    };
    consent_page(&core, &user, jar, consent, None).await
}

/// Whether the token this flow makes has to be confirmed with the password,
/// by the kit's rule for its Tokens page (`needs_reauth` there): write access
/// or no expiry. This token is read-only and never expires, so it does.
const NEEDS_REAUTH: bool = true;

/// The kit's re-authentication for sensitive changes (its
/// `confirm_password`, which is private to the kit's pages), with the same
/// limiter key, so password guesses here and on the kit's pages share one
/// throttle per user. `Err` holds what to tell the user.
async fn confirm_password(
    core: &Core,
    user: &CurrentUser,
    jar: &SignedCookieJar,
    password: &str,
) -> ApiResult<Result<(), String>> {
    if matches!(
        vole_kit::config::get().features.credentials,
        CredentialMode::External(_)
    ) {
        // The product checks credentials elsewhere: a fresh grant is needed.
        return Ok(if reauth::is_fresh(jar, &core.session_key) {
            Ok(())
        } else {
            Err(i18n::t("sec.confirm_its_you_help").into())
        });
    }
    // A missing password is asked for without counting as a guess.
    if password.is_empty() {
        return Ok(Err(i18n::t("sec.reauth_needed").into()));
    }
    let key = format!("reauth:{}", user.id);
    // Counted before the (slow) check, so parallel guesses can't all pass.
    if let Err(wait) = core.limits.account.reserve(&key, Instant::now()) {
        let secs = wait.as_secs().max(1).to_string();
        return Ok(Err(i18n::tf("sec.too_many_secs", &[("n", &secs)])));
    }
    let id = user.id;
    let record = match core
        .with_conn(move |c| vole_kit::db::users::get_user(c, id))
        .await
    {
        Ok(Some(record)) => record,
        other => {
            // Not a guess: don't count it.
            core.limits.account.release(&key);
            return Err(other.err().unwrap_or(ApiError::Unauthorized));
        }
    };
    if password.len() <= auth::MAX_PASSWORD_LEN
        && auth::verify_password_async(record.password_hash, password.to_string()).await?
    {
        core.limits.account.clear(&key);
        Ok(Ok(()))
    } else {
        Ok(Err(i18n::t("sec.wrong_password").into()))
    }
}

/// `POST /connect/extension`: re-validates everything the form carries, then
/// redirects to the extension with a code (`approve`, after the password) or
/// `access_denied` (`deny`).
async fn decide(
    State(core): State<Core>,
    Extension(magpie): Extension<Magpie>,
    user: CurrentUser,
    jar: SignedCookieJar,
    body: Bytes,
) -> ApiResult<Response> {
    let form = pairs(&body);
    let Some(consent) = Consent::parse(&form, &magpie.extension_hosts) else {
        return Ok(refused(&core, &user, jar).await);
    };
    match single(&form, "decision") {
        Some("approve") => {
            if NEEDS_REAUTH {
                let password = single(&form, "current_password").unwrap_or("");
                if let Err(message) = confirm_password(&core, &user, &jar, password).await? {
                    return Ok(consent_page(&core, &user, jar, consent, Some(message)).await);
                }
            }
            let code = create_code(&core, user.id, &consent).await?;
            let response = consent
                .redirect_with(&format!("code={code}&state={}", consent.state))
                .into_response();
            Ok((no_store(), response).into_response())
        }
        Some("deny") => {
            let response = consent
                .redirect_with(&format!("error=access_denied&state={}", consent.state))
                .into_response();
            Ok((no_store(), response).into_response())
        }
        _ => Ok(refused(&core, &user, jar).await),
    }
}

/// The code is in the address; keep the redirect out of caches.
fn no_store() -> [(header::HeaderName, &'static str); 1] {
    [(header::CACHE_CONTROL, "no-store")]
}

/// A new one-time code for `user_id`: 32 random bytes as base64url. Only its
/// SHA-256 is stored. Expired codes (anyone's) and a user's codes beyond the
/// newest few are dropped on the way.
async fn create_code(core: &Core, user_id: i64, consent: &Consent) -> ApiResult<String> {
    let mut bytes = [0u8; 32];
    rand::fill(&mut bytes);
    let code = URL_SAFE_NO_PAD.encode(bytes);
    let code_hash = tokens::hash(&code);
    let (challenge, device) = (consent.code_challenge.clone(), consent.device.clone());
    db::interact(&core.db, move |c| {
        let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
        purge_expired(&tx)?;
        tx.execute(
            "DELETE FROM extension_codes
             WHERE user_id = ?1 AND code_hash NOT IN (
                 SELECT code_hash FROM extension_codes WHERE user_id = ?1
                 ORDER BY expires_at DESC, rowid DESC LIMIT ?2)",
            params![user_id, MAX_PENDING_PER_USER - 1],
        )?;
        tx.execute(
            "INSERT INTO extension_codes
                 (code_hash, user_id, code_challenge, device, expires_at)
             VALUES (?1, ?2, ?3, ?4, CAST(strftime('%s','now') AS INTEGER) + ?5)",
            params![code_hash, user_id, challenge, device, CODE_TTL_SECS],
        )?;
        tx.commit()
    })
    .await?;
    Ok(code)
}

/// Deletes the codes whose time is up.
pub(crate) fn purge_expired(conn: &rusqlite::Connection) -> rusqlite::Result<usize> {
    conn.execute(
        "DELETE FROM extension_codes WHERE expires_at <= CAST(strftime('%s','now') AS INTEGER)",
        [],
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const HASH: &str = "0123456789abcdef0123456789abcdef01234567";

    #[test]
    fn the_official_extension_has_its_known_redirect_host() {
        assert_eq!(
            redirect_host(OFFICIAL_EXTENSION_ID),
            "9bd6a096502a3a8555d679b3f64704219dffb501"
        );
        let hosts = allowed_hosts(&[
            "dev@example.com".into(),
            " ".into(),
            OFFICIAL_EXTENSION_ID.into(),
        ]);
        assert_eq!(hosts.len(), 2, "blank and repeated entries are dropped");
        assert_eq!(hosts[1], redirect_host("dev@example.com"));
    }

    #[test]
    fn only_the_firefox_identity_address_is_a_redirect() {
        let hosts = vec![HASH.to_string()];
        let is_extension_redirect = |uri: &str| super::is_extension_redirect(uri, &hosts);
        assert!(is_extension_redirect(&format!(
            "https://{HASH}.extensions.allizom.org/"
        )));
        assert!(!is_extension_redirect(&format!(
            "https://{}.extensions.allizom.org/",
            "a".repeat(40)
        )));
        for bad in [
            "",
            "https://evil.com/",
            &format!("https://{HASH}.extensions.allizom.org"),
            &format!("https://{HASH}.extensions.allizom.org/x"),
            &format!("http://{HASH}.extensions.allizom.org/"),
            &format!("https://{HASH}.extensions.allizom.org:443/"),
            &format!("https://u@{HASH}.extensions.allizom.org/"),
            &format!("https://{}.extensions.allizom.org/", HASH.to_uppercase()),
            &format!("https://x{HASH}.extensions.allizom.org/"),
            &format!("https://{HASH}.extensions.allizom.org/ "),
        ] {
            assert!(!is_extension_redirect(bad), "{bad}");
        }
    }

    #[test]
    fn the_rfc_7636_challenge_has_the_right_shape() {
        assert!(is_challenge("E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"));
        assert!(!is_challenge("E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-c"));
        assert!(!is_challenge("E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw+cM"));
    }
}
