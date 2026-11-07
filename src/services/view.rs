//! What a page or the extension may know about a service it can see: its
//! number, name, kind, interval, the metrics a card can show and the link a
//! card may point at. Never the address, the settings or the secret (none of
//! them is a field here; the link is the service's «Lenke på kort», or the
//! origin of its address when there is none, which is the part of the address
//! a browser needs to open it and no more).

use serde::Serialize;
use vole_kit::db::DbError;
use vole_kit::i18n;
use vole_kit::state::Core;

use super::{ServiceSummary, Viewer, store};
use crate::integrations::{self, Format};

/// One metric a card can show, with its label in the request's language.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MetricView {
    pub id: &'static str,
    pub label: &'static str,
    /// How the renderer draws the number (`formatValue` in `render.js`):
    /// `number`, `percent`, `bytes`, `bytes_per_sec`, `duration`, `since`
    /// or `text`.
    pub format: Format,
}

/// A service as the start page's `#mm-services` block and the extension's
/// `GET /api/v1/me/start` carry it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ServiceView {
    pub id: i64,
    pub name: String,
    pub kind: String,
    pub interval_s: i64,
    /// The kind's metrics, in its own order. Empty for a kind this build does
    /// not have (a service made by a newer version).
    pub metrics: Vec<MetricView>,
    /// Where a card of the service links when it has no link of its own.
    /// Empty when the service has no link and no address.
    pub link: String,
}

/// The services `viewer` may see, by name, with their metrics. Labels come in
/// the current request's language (English outside a request).
///
/// Visibility is [`store::visible_to`]'s: admins see every service, anyone
/// else the ones for everybody and the ones that name them.
pub async fn services_for(
    core: &Core,
    viewer: impl Into<Viewer>,
) -> Result<Vec<ServiceView>, DbError> {
    let visible = store::visible_to(core, viewer).await?;
    Ok(visible.iter().map(view_of).collect())
}

/// The view of one visible service (see [`services_for`]).
pub fn view_of(s: &ServiceSummary) -> ServiceView {
    let metrics = integrations::find(&s.kind)
        .map(|kind| {
            kind.metrics()
                .iter()
                .map(|m| MetricView {
                    id: m.id,
                    label: i18n::t(m.label_key),
                    format: m.format,
                })
                .collect()
        })
        .unwrap_or_default();
    ServiceView {
        id: s.id,
        name: s.name.clone(),
        kind: s.kind.clone(),
        interval_s: s.interval_s,
        metrics,
        link: s.link.clone(),
    }
}
