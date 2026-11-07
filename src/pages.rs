//! Magpie's own pages: the start page, which embeds the signed-in user's
//! layout as data, and the routes that save it.

use std::collections::BTreeMap;

use askama::Template;
use axum::body::Bytes;
use axum::extract::multipart::MultipartRejection;
use axum::extract::rejection::QueryRejection;
use axum::extract::{DefaultBodyLimit, Extension, Multipart, Path, Query, Request, State};
use axum::http::{HeaderMap, Method, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use axum_extra::extract::cookie::SignedCookieJar;
use serde_json::json;
use vole_kit::auth::{CurrentUser, guard, tokens};
use vole_kit::http::error::ApiError;
use vole_kit::http::{ApiResult, HtmlTemplate};
use vole_kit::i18n::{self, Tr as _};
use vole_kit::page::PageCtx;
use vole_kit::settings::SiteSettings;
use vole_kit::state::Core;

use crate::Magpie;
use crate::layout::background::MAX_BYTES;
use crate::layout::{self, Kind, Layout, SaveError, StoreError};

/// The largest body `POST /start/layout` takes. A document that passes
/// validation is about 1.2 MB with four-byte characters; 4 MiB leaves room for
/// text a client writes as `\uXXXX` escapes. A bigger body is refused with 413
/// before it is read.
const MAX_LAYOUT_BODY: usize = 4 * 1024 * 1024;

/// Room for the multipart framing around a picture of [`MAX_BYTES`] (the
/// boundaries and the part's headers): a body over `MAX_BYTES` plus this is
/// refused with 413 before it is read to the end.
const MULTIPART_OVERHEAD: usize = 64 * 1024;

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
        .route(
            "/start/background",
            post(upload_background)
                .layer(DefaultBodyLimit::max(MAX_BYTES + MULTIPART_OVERHEAD))
                .delete(delete_background),
        )
}

/// `GET /start/background/{id}`, which is outside the sign-in guard: that
/// guard sends a request without a session to the sign-in page, and the
/// browser extension fetches the picture with an API token and must not be
/// redirected (`redirect: "error"`). The handler does its own sign-in check.
pub fn image_router() -> Router<Core> {
    Router::new().route("/start/background/{id}", get(background_image))
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
    /// The signed-in user's id, on the page so that a draft kept in the browser
    /// can be tied to the user it belongs to.
    user_id: i64,
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

/// One entry of the labels map: the key the scripts use and its text in the
/// request's language (`start.<key>` in the locale files).
macro_rules! label_map {
    ($($key:literal),* $(,)?) => {
        BTreeMap::from([$(($key, i18n::t(concat!("start.", $key)))),*])
    };
}

/// The interface strings the page scripts need, in the request's language: the
/// renderer's (`render.js`) and edit mode's (the same keys as `LABEL_KEYS` in
/// `edit.js`; a test keeps the two lists in step). A `{name}` in a text is
/// filled in by the script.
fn labels() -> BTreeMap<&'static str, &'static str> {
    label_map!(
        "empty",
        "invalid_link",
        "group_label",
        "bookmarks_label",
        "edit_toolbar",
        "edit_done",
        "edit_cancel",
        "edit_saving",
        "add_group",
        "add_bookmark",
        "add_card",
        "rename_group",
        "edit_item",
        "move_item",
        "delete_item",
        "drag_hint",
        "dialog_save",
        "dialog_cancel",
        "dialog_close",
        "group_form_add",
        "group_form_rename",
        "card_form_add",
        "card_form_edit",
        "bookmark_form_add",
        "bookmark_form_edit",
        "field_group_name",
        "field_title",
        "field_url",
        "field_description",
        "field_emoji",
        "field_colour",
        "optional",
        "help_url",
        "help_emoji",
        "move_title",
        "move_up",
        "move_down",
        "move_first",
        "move_last",
        "move_to_group",
        "confirm_delete_group",
        "confirm_delete_group_text",
        "confirm_delete",
        "confirm_discard",
        "confirm_discard_text",
        "confirm_discard_yes",
        "confirm_keep",
        "confirm_leave",
        "confirm_leave_text",
        "confirm_leave_yes",
        "background",
        "bg_title",
        "bg_presets",
        "bg_preset_hedgerow",
        "bg_preset_natt",
        "bg_preset_skumring",
        "bg_preset_hav",
        "bg_upload",
        "bg_upload_help",
        "bg_uploading",
        "bg_remove",
        "bg_image_set",
        "bg_err_size",
        "bg_err_type",
        "bg_err_failed",
        "bg_gone",
        "restore_notice",
        "restore_yes",
        "restore_discard",
        "restore_stale",
        "conflict",
        "reload",
        "save_failed",
        "invalid_summary",
        "summary_more",
        "summary_group",
        "summary_card",
        "summary_bookmark",
        "summary_line",
        "limit_groups",
        "limit_cards",
        "limit_bookmarks",
        "err_required",
        "err_too_long",
        "err_bad_url",
        "err_bad_scheme",
        "err_bad_emoji",
        "err_bad_colour",
        "err_generic",
        "colour_red",
        "colour_orange",
        "colour_yellow",
        "colour_green",
        "colour_mint",
        "colour_teal",
        "colour_cyan",
        "colour_blue",
        "colour_indigo",
        "colour_purple",
        "colour_pink",
        "colour_brown",
        "colour_gray",
    )
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

/// The answer to `/` depends on this request header (a boosted request gets a
/// redirect), so a cache must not share it between them.
const VARY: &str = "HX-Boosted";

async fn start(
    State(core): State<Core>,
    user: CurrentUser,
    headers: HeaderMap,
    jar: SignedCookieJar,
) -> ApiResult<Response> {
    // The page's stylesheet and scripts are linked from `<head>`, which a
    // boosted navigation (the Start link in the top bar and the tab bar) does
    // not load, and htmx does not run scripts it swaps in (not even the page's
    // data blocks). So a boosted request for the page, from wherever it is made,
    // makes the browser load it in full.
    if headers.contains_key("hx-boosted") {
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
        user_id: user.id,
        labels_json: json_for_html(&labels()),
        corrupt,
    };
    // The page holds one person's layout: no cache (the browser's back/forward
    // cache and HTTP cache included) may show it to the next person in the tab.
    Ok((
        jar,
        [("vary", VARY), ("cache-control", "no-store")],
        HtmlTemplate(page),
    )
        .into_response())
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

/// A JSON answer with an error code and the status that goes with it.
fn error_json(status: StatusCode, code: &str) -> Response {
    (status, Json(json!({ "error": code }))).into_response()
}

/// `POST /start/background`: keeps the uploaded picture (multipart, the field
/// `image`) as the signed-in user's background, in place of any earlier one.
/// The format is decided by the first bytes, not by the file name or the
/// declared type.
async fn upload_background(
    State(core): State<Core>,
    Extension(magpie): Extension<Magpie>,
    user: CurrentUser,
    multipart: Result<Multipart, MultipartRejection>,
) -> ApiResult<Response> {
    // A body that is not multipart (or has no boundary) gets the same JSON
    // answer as the other bad requests, not axum's plain text.
    let Ok(mut multipart) = multipart else {
        return Ok(error_json(StatusCode::BAD_REQUEST, "bad_request"));
    };
    let data = loop {
        match multipart.next_field().await {
            Ok(Some(field)) if field.name() == Some("image") => match field.bytes().await {
                Ok(data) => break data,
                Err(err) => return Ok(multipart_failure(&err)),
            },
            // Other fields are not ours.
            Ok(Some(_)) => {}
            Ok(None) => return Ok(error_json(StatusCode::BAD_REQUEST, "bad_request")),
            Err(err) => return Ok(multipart_failure(&err)),
        }
    };
    if data.len() > MAX_BYTES {
        return Ok(error_json(StatusCode::PAYLOAD_TOO_LARGE, "too_large"));
    }
    let Some(kind) = Kind::sniff(&data) else {
        return Ok(error_json(StatusCode::UNPROCESSABLE_ENTITY, "bad_image"));
    };
    let id = magpie
        .backgrounds
        .store(&core, user.id, kind, data)
        .await
        .map_err(ApiError::internal)?;
    Ok(Json(json!({ "id": id })).into_response())
}

/// A body that is too big is 413, one that is not multipart or is cut off 400.
fn multipart_failure(err: &axum::extract::multipart::MultipartError) -> Response {
    if err.status() == StatusCode::PAYLOAD_TOO_LARGE {
        error_json(StatusCode::PAYLOAD_TOO_LARGE, "too_large")
    } else {
        error_json(StatusCode::BAD_REQUEST, "bad_request")
    }
}

/// The query of `DELETE /start/background`.
#[derive(serde::Deserialize)]
struct PictureQuery {
    id: Option<String>,
}

/// Whether `id` has the shape of a picture id (the shape `validate` and the
/// renderer accept).
fn picture_id_shape(id: &str) -> bool {
    (1..=64).contains(&id.len())
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// `DELETE /start/background?id=<id>`: takes the picture `id` off the server
/// when it is still the signed-in user's current picture and the stored layout
/// does not use it; in every other case it does nothing. It never changes the
/// layout or its version. The answer is 204 whether or not anything was
/// removed, so it says nothing the caller could not know; a missing or
/// malformed `id` is a 400.
async fn delete_background(
    State(core): State<Core>,
    Extension(magpie): Extension<Magpie>,
    user: CurrentUser,
    query: Result<Query<PictureQuery>, QueryRejection>,
) -> ApiResult<Response> {
    let id = query
        .ok()
        .and_then(|Query(q)| q.id)
        .filter(|id| picture_id_shape(id));
    let Some(id) = id else {
        return Ok(error_json(StatusCode::BAD_REQUEST, "bad_request"));
    };
    magpie
        .backgrounds
        .remove(&core, user.id, &id)
        .await
        .map_err(ApiError::internal)?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

/// No sign-in, or a token that does not work: 401, never a redirect.
fn image_unauthorized() -> Response {
    (
        StatusCode::UNAUTHORIZED,
        [
            (header::CACHE_CONTROL, "no-store"),
            (header::WWW_AUTHENTICATE, "Bearer"),
        ],
    )
        .into_response()
}

/// Every id that is not the signed-in user's own picture is the same answer.
fn image_not_found() -> Response {
    (StatusCode::NOT_FOUND, [(header::CACHE_CONTROL, "no-store")]).into_response()
}

/// `GET /start/background/{id}`: the signed-in user's picture, when `id` is
/// its id. Who is signed in is an API token when the request has an
/// `Authorization` header (and then only the token counts, as under `/api/`),
/// otherwise the session cookie. Nothing is revealed about other users'
/// pictures: another user's id and an id that exists nowhere both give 404.
async fn background_image(
    State(core): State<Core>,
    Extension(magpie): Extension<Magpie>,
    headers: HeaderMap,
    jar: SignedCookieJar,
    Path(id): Path<String>,
) -> ApiResult<Response> {
    let user = if tokens::has_authorization(&headers) {
        guard::authenticate_headers(&core, &headers).await?
    } else {
        guard::session_user(&core, &jar).await?
    };
    let Some(user) = user else {
        return Ok(image_unauthorized());
    };
    let found = magpie
        .backgrounds
        .read(&core, user.id, &id)
        .await
        .map_err(ApiError::internal)?;
    let Some((kind, data)) = found else {
        return Ok(image_not_found());
    };
    Ok((
        [
            (header::CONTENT_TYPE, kind.mime()),
            // The id is the hash of the content: the answer never changes.
            (
                header::CACHE_CONTROL,
                "private, max-age=31536000, immutable",
            ),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
        ],
        data,
    )
        .into_response())
}

/// The kit deletes a user (and their `backgrounds` row, by `ON DELETE
/// CASCADE`) at `POST /settings/users/{id}/delete` and has no hook for what
/// else the product keeps of the user. After an admin's request to that path,
/// sweep the files of users that have no picture any more.
pub async fn sweep_after_user_delete(
    Extension(magpie): Extension<Magpie>,
    request: Request,
    next: Next,
) -> Response {
    let by_admin = request
        .extensions()
        .get::<CurrentUser>()
        .is_some_and(CurrentUser::is_admin);
    let deletes_user = request.method() == Method::POST
        && request
            .uri()
            .path()
            .strip_prefix("/settings/users/")
            .and_then(|rest| rest.strip_suffix("/delete"))
            .is_some_and(|id| !id.is_empty() && id.bytes().all(|b| b.is_ascii_digit()));
    let response = next.run(request).await;
    if by_admin
        && deletes_user
        && let Err(err) = magpie.backgrounds.reconcile(&magpie.core).await
    {
        tracing::warn!("can't sweep the background files: {err}");
    }
    response
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
