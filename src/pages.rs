//! Magpie's own pages. The start page is a placeholder until the links and
//! services are built; it already sits behind sign-in.

use askama::Template;
use axum::Router;
use axum::extract::State;
use axum::response::IntoResponse;
use axum::routing::get;
use axum_extra::extract::cookie::SignedCookieJar;
use vole_kit::auth::CurrentUser;
use vole_kit::http::{ApiResult, HtmlTemplate};
use vole_kit::i18n::{self, Tr as _};
use vole_kit::page::PageCtx;
use vole_kit::state::Core;

/// Pages for signed-in users (the caller adds the sign-in guard).
pub fn router() -> Router<Core> {
    Router::new().route("/", get(start))
}

#[derive(Template)]
#[template(path = "start.html")]
struct StartPage {
    ctx: PageCtx,
}

async fn start(
    State(core): State<Core>,
    user: CurrentUser,
    jar: SignedCookieJar,
) -> ApiResult<impl IntoResponse> {
    let (ctx, jar) = PageCtx::build(
        &core,
        Some(&user),
        &jar,
        i18n::t("start.title"),
        "start",
        "",
    )
    .await;
    Ok((jar, HtmlTemplate(StartPage { ctx })))
}
