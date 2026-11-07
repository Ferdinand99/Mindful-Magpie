//! What makes this build Mindful Magpie as far as vole-kit is concerned:
//! names, cookies, token prefixes, migrations, translations, API reference,
//! the settings tabs, the webhook's words and what the shared layouts show.

use rust_embed::Embed;
use vole_kit::config::KitConfig;
use vole_kit::docs::{DocsSpec, RouteDoc};
use vole_kit::http::assets::StaticFile;
use vole_kit::notify::webhook::Wording;
use vole_kit::page::{NavItem, SettingsTab, Ui};
use vole_kit::settings::GeneralSettings;

/// The product's version (`CARGO_PKG_VERSION`), reported by the OpenAPI
/// document and the MCP server.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
/// The product's name.
pub const NAME: &str = "Mindful Magpie";
/// `User-Agent` of outgoing requests.
pub const USER_AGENT: &str = concat!("MindfulMagpie/", env!("CARGO_PKG_VERSION"));
/// Name of the signed session cookie.
pub const SESSION_COOKIE: &str = "mm_session";
/// Prefix of API tokens.
pub const TOKEN_PREFIX: &str = "mm_";
/// The database file in the data directory.
pub const DB_FILE: &str = "magpie.sqlite";

/// Magpie's own static files: icons, the web app manifest and `magpie.css`.
/// The design system (stylesheet, scripts, icon sprite, fonts) is the kit's.
#[derive(Embed)]
#[folder = "static/"]
struct StaticAssets;

/// Looks up one of Magpie's static files (for the kit's `/static/` handler).
pub fn static_file(path: &str) -> Option<StaticFile> {
    let file = StaticAssets::get(path)?;
    Some(StaticFile {
        sha256: file.metadata.sha256_hash(),
        data: file.data,
    })
}

const ERROR_EXAMPLE: &str = r#"{"error": {"code": "not_found", "message": "Not found."}}"#;

fn curl_param(_: &RouteDoc, _: &str) -> Option<String> {
    None
}

fn intro(out: &mut String, base: &str) {
    out.push_str(&format!(
        "# Mindful Magpie API\n\nBase URL: {base}/api/v1. Send `Authorization: Bearer mm_…`.\n\n"
    ));
}

fn nothing(_: &mut String) {}

/// The API reference. Magpie has no REST routes of its own yet; the kit
/// documents the routes it serves.
pub static DOCS: DocsSpec = DocsSpec {
    routes: &[],
    openapi_title: "Mindful Magpie API",
    openapi_description: "REST API for Mindful Magpie. Authenticate with `Authorization: Bearer mm_…`. Reads need any token; writes need a token with write access (a session cookie is never enough).",
    bearer_description: "API token (mm_…)",
    credentials: &[],
    error_example: ERROR_EXAMPLE,
    error_codes: &[],
    config_document: None,
    curl_path_param: curl_param,
    markdown_intro: intro,
    markdown_middle: nothing,
    markdown_outro: nothing,
    page_subtitle: "tokens.sub",
    openapi_path: "/api/v1/openapi.json",
    api_base: "/api/v1",
    example_path: "/api/v1/openapi.json",
    metrics_path: None,
    fallback_host: Some("localhost:3010"),
    param_types: &[],
    strict_access: true,
};

pub static KIT: KitConfig = KitConfig {
    product: NAME,
    slug: "mindful-magpie",
    short_name: "Magpie",
    session_cookie: SESSION_COOKIE,
    pending_2fa_cookie: "mm_2fa",
    token_prefix: TOKEN_PREFIX,
    other_token_prefixes: &[],
    version: VERSION,
    user_agent: USER_AGENT,
    backup_prefix: "magpie-before-",
    flash_cookie: "mm_flash",
    env_prefix: "MM",
    csrf_exempt_prefixes: &["/api/v1"],
    csrf_exempt_path: no_exempt_path,
    extra_common_passwords: &["mindfulmagpie", "magpie1234"],
    migrations: &crate::db::MIGRATIONS,
    embeddable_prefixes: &[],
    static_file,
    machine_path_prefixes: &["/static/", "/api/"],
    machine_paths: &["/healthz"],
    browser_language_prefixes: &[],
    docs: &DOCS,
    locales: &[
        ("en", include_str!("../locales/en.toml")),
        ("nb", include_str!("../locales/nb.toml")),
    ],
    ui: &Ui {
        logo: "/static/favicon.svg",
        favicon: "/static/favicon.svg",
        apple_touch_icon: "/static/apple-touch-icon.png",
        manifest: "/static/site.webmanifest",
        nav: &[
            NavItem {
                id: "start",
                href: "/",
                label: "start.title",
                short_label: "start.title",
                icon: "dashboard",
            },
            NavItem {
                id: "settings",
                href: "/settings",
                label: "nav.settings",
                short_label: "nav.settings",
                icon: "gear",
            },
        ],
        settings_tabs: &[
            SettingsTab::GENERAL,
            SettingsTab::USERS,
            SettingsTab::SECURITY,
            SettingsTab::TOKENS,
        ],
        home_url: "https://hedgerow.tools/Mindful-Magpie/",
    },
    // Set on the General page; `MM_PUBLIC_URL` stores it at start-up.
    public_url_setting: Some("Settings → General"),
    webhook: Wording {
        subject: "subject",
        duration: "duration",
        url_example: "https://example.com/hooks/magpie…",
    },
    token_scopes: &[],
    // Site name, public URL, time zone and language.
    general: Some(&GeneralSettings::CORE_ONLY),
    // The signed-in footer links the running version to its release notes.
    releases_url: Some("https://github.com/Ferdinand99/Mindful-Magpie/releases/tag/v{version}"),
    features: &vole_kit::config::KitFeatures::DEFAULT,
};

/// Every exempt path is a prefix above.
fn no_exempt_path(_: &str) -> bool {
    false
}

/// Installs [`KIT`]. Call it first thing at start-up, and in test set-up.
pub fn install() {
    vole_kit::config::install(&KIT);
}
