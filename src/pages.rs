//! Magpie's own pages. The start page is empty until the links and services
//! are built; it already sits behind sign-in.

use askama::Template;
use axum::Router;
use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::get;
use axum_extra::extract::cookie::SignedCookieJar;
use vole_kit::auth::CurrentUser;
use vole_kit::http::{ApiResult, HtmlTemplate};
use vole_kit::i18n::{self, Tr as _};
use vole_kit::page::PageCtx;
use vole_kit::settings::SiteSettings;
use vole_kit::state::Core;

/// Pages for signed-in users (the caller adds the sign-in guard).
pub fn router() -> Router<Core> {
    Router::new()
        .route("/", get(start))
        .route("/settings", get(settings_home))
}

#[derive(Template)]
#[template(path = "start.html")]
struct StartPage {
    ctx: PageCtx,
    /// The time now in the site's time zone, `HH:MM`.
    time: String,
    /// The date now in the site's time zone, in the page's language.
    date: String,
    /// IANA name of the site's time zone (`clock.js` keeps time in it).
    timezone: String,
}

/// The time zone, the clock and the date for the page: now, in the site's
/// time zone (UTC when the setting is missing or no longer valid).
fn clock_in(timezone: &str) -> (String, String) {
    let zone = jiff::tz::TimeZone::get(timezone).unwrap_or(jiff::tz::TimeZone::UTC);
    let now = jiff::Timestamp::now().to_zoned(zone);
    let date = now.date();
    let weekday = i18n::weekday_name(now.weekday().to_monday_one_offset(), true);
    let date = i18n::tf(
        "start.date",
        &[
            ("weekday", weekday),
            ("day_month", &i18n::fmt_day_month_long(date)),
        ],
    );
    (now.strftime("%H:%M").to_string(), date)
}

/// The answer to `/` depends on these request headers (a boosted request from
/// another page gets a redirect), so a cache must not share it between them.
const VARY: &str = "HX-Boosted, HX-Current-URL";

/// Whether an htmx request comes from the start page itself (`HX-Current-URL`
/// is the full address of the page that made the request).
fn on_start_page(headers: &HeaderMap) -> bool {
    headers
        .get("hx-current-url")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| url::Url::parse(v).ok())
        .is_some_and(|u| u.path() == "/")
}

async fn start(
    State(core): State<Core>,
    user: CurrentUser,
    headers: HeaderMap,
    jar: SignedCookieJar,
) -> ApiResult<Response> {
    // The page's stylesheet and clock script are linked from `<head>`, which a
    // boosted navigation (the Start link in the top bar and the tab bar) does
    // not load, and htmx does not run scripts it swaps in. Reaching the page
    // from another page that way makes the browser load it in full. From the
    // start page itself (its head is already loaded) the boosted request just
    // swaps in the fresh body: a redirect to `/` would reload it for nothing.
    if headers.contains_key("hx-boosted") && !on_start_page(&headers) {
        return Ok(([("hx-redirect", "/"), ("vary", VARY)], ()).into_response());
    }
    let site = core.with_conn(|c| SiteSettings::load(c)).await?;
    let timezone = site.timezone_or_utc().to_string();
    let (time, date) = clock_in(&timezone);
    let (ctx, jar) = PageCtx::build(
        &core,
        Some(&user),
        &jar,
        i18n::t("start.title"),
        "start",
        "",
    )
    .await;
    let page = StartPage {
        ctx,
        time,
        date,
        timezone,
    };
    Ok((jar, [("vary", VARY)], HtmlTemplate(page)).into_response())
}

/// The Settings nav item: the first tab the user may open (General for
/// admins, Security for everyone else).
async fn settings_home(user: CurrentUser) -> Redirect {
    Redirect::to(if user.is_admin() {
        "/settings/general"
    } else {
        "/settings/security"
    })
}
