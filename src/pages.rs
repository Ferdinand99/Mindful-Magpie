//! Magpie's own pages: the start page, which embeds the signed-in user's
//! layout as data, and the routes that save it.

use std::collections::BTreeMap;

use askama::Template;
use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use axum_extra::extract::cookie::SignedCookieJar;
use serde_json::json;
use vole_kit::auth::CurrentUser;
use vole_kit::http::error::ApiError;
use vole_kit::http::{ApiResult, HtmlTemplate};
use vole_kit::i18n::{self, Tr as _};
use vole_kit::page::PageCtx;
use vole_kit::settings::SiteSettings;
use vole_kit::state::Core;

use crate::layout::{self, Layout, SaveError, StoreError};

/// The largest body `POST /start/layout` takes. A document that passes
/// validation is about 1.2 MB with four-byte characters; 4 MiB leaves room for
/// text a client writes as `\uXXXX` escapes. A bigger body is refused with 413
/// before it is read.
const MAX_LAYOUT_BODY: usize = 4 * 1024 * 1024;

/// Pages for signed-in users (the caller adds the sign-in guard).
pub fn router() -> Router<Core> {
    Router::new()
        .route("/", get(start))
        .route("/settings", get(settings_home))
        .route(
            "/start/layout",
            post(save_layout).layer(DefaultBodyLimit::max(MAX_LAYOUT_BODY)),
        )
        .route("/start/layout/reset", post(reset_layout))
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
    /// The user's layout as JSON for a `<script type="application/json">`
    /// block (see [`json_for_html`]).
    layout_json: String,
    /// The strings the renderer needs, as JSON for the same kind of block.
    labels_json: String,
    /// The stored layout could not be read: say so and offer a fresh start.
    corrupt: bool,
}

/// `value` as JSON that is safe inside `<script type="application/json">`.
/// The HTML parser ends such a block at the first `</script`, and `<!--` can
/// make it swallow the end tag, so both are written with escapes that JSON
/// reads back as the same text: `<\/` is `</`, and a backslash followed by
/// `u003c` is `<`.
fn json_for_html(value: &impl serde::Serialize) -> String {
    serde_json::to_string(value)
        .unwrap_or_else(|_| "null".into())
        .replace("</", "<\\/")
        .replace("<!--", "\\u003c!--")
}

/// The interface strings the page script needs to draw the layout, in the
/// request's language. A `{title}` in a text is filled in by the script.
fn labels() -> BTreeMap<&'static str, &'static str> {
    BTreeMap::from([
        ("empty", i18n::t("start.empty")),
        ("invalid_link", i18n::t("start.invalid_link")),
        ("group_label", i18n::t("start.group_label")),
        ("bookmarks_label", i18n::t("start.bookmarks_label")),
    ])
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
    // A stored layout that cannot be read (it was changed outside Magpie)
    // must not take the page down: show the empty default, at the stored
    // version so that a save over the broken document is accepted, and say so.
    let (layout, corrupt) = layout::load_or_default(&core, user.id).await?;
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
        layout_json: json_for_html(&layout),
        labels_json: json_for_html(&labels()),
        corrupt,
    };
    Ok((jar, [("vary", VARY)], HtmlTemplate(page)).into_response())
}

/// `POST /start/layout`: replaces the signed-in user's layout. The body is
/// the layout document, carrying the version it was loaded at. The user is
/// always the one signed in; no route or field names another.
async fn save_layout(
    State(core): State<Core>,
    user: CurrentUser,
    body: Bytes,
) -> ApiResult<Response> {
    let Ok(layout) = serde_json::from_slice::<Layout>(&body) else {
        let body = Json(json!({ "error": "bad_request" }));
        return Ok((StatusCode::BAD_REQUEST, body).into_response());
    };
    Ok(match layout::save(&core, user.id, &layout).await {
        Ok(version) => Json(json!({ "version": version })).into_response(),
        Err(SaveError::Conflict { current }) => (
            StatusCode::CONFLICT,
            Json(json!({ "error": "conflict", "current": current })),
        )
            .into_response(),
        Err(SaveError::Invalid(fields)) => (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(json!({ "error": "invalid", "fields": fields })),
        )
            .into_response(),
        Err(err @ (SaveError::Db(_) | SaveError::Encode(_))) => {
            return Err(ApiError::internal(err));
        }
    })
}

/// `POST /start/layout/reset`: the "Start over" button of the notice shown
/// when the stored layout cannot be read. It replaces an unreadable layout with
/// the empty one and leaves a readable layout alone. It is a plain form post,
/// so it works without JavaScript and the page can always be repaired.
async fn reset_layout(State(core): State<Core>, user: CurrentUser) -> ApiResult<Redirect> {
    if let Err(StoreError::Corrupt { version, .. }) = layout::load(&core, user.id).await {
        // The version comes from the same read that found the document
        // broken. If a valid save lands before this one, the versions differ
        // and the save below is a conflict: the new layout is never wiped.
        let fresh = Layout {
            version,
            ..Layout::default()
        };
        match layout::save(&core, user.id, &fresh).await {
            // Saved, or a save from another tab got there first: either way,
            // show the page as it is now.
            Ok(_) | Err(SaveError::Conflict { .. }) => {}
            Err(err) => return Err(ApiError::internal(err)),
        }
    }
    Ok(Redirect::to("/"))
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

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::json_for_html;

    #[test]
    fn json_for_html_cannot_end_or_swallow_the_block() {
        for text in [
            "</script>",
            "</SCRIPT",
            "</ScRiPt>",
            "</script ",
            "</script\n",
            "<!--",
            "<!--<!--",
            "<!-- <script>",
            r"\<!--",
            r"\</script>",
            "a</b><!--c-->",
        ] {
            let value = json!({ "title": text, "list": [text] });
            let out = json_for_html(&value);
            // What a browser would end the block at, or be confused by.
            assert!(!out.contains("</"), "{text:?} -> {out}");
            assert!(!out.contains("<!--"), "{text:?} -> {out}");
            // And the data comes back unchanged.
            let back: serde_json::Value = serde_json::from_str(&out).unwrap();
            assert_eq!(back, value, "{text:?} -> {out}");
        }
    }
}
