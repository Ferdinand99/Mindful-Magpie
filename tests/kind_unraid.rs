//! The `unraid` kind («Unraid») against a stub server on 127.0.0.1: the
//! GraphQL answer for a healthy server, a stopped array, parity errors, a
//! full disk, GraphQL errors (all of them and some of them), a server that
//! rejects the full query, a refused key, answers that are not what was
//! expected, and that the key goes nowhere but into the `x-api-key` header of
//! the request.

mod common;

use std::sync::Arc;
use std::time::Duration;

use common::stub::{self, Answer, Stub};
use common::{locale, log};
use mindful_magpie::integrations::block::{self, Bar, Block};
use mindful_magpie::integrations::{
    self, FetchCtx, FetchError, Integration, Level, Need, Snapshot, Status,
};
use serde_json::{Value as Json, json};

const KEY: &str = "unr-K3y-Zq81x7w4mK-0f9a";

/// A stub whose `/graphql` answers `what`.
async fn serve(what: Answer) -> (String, Stub) {
    stub::serve_one("/graphql", what).await
}

fn kind() -> &'static dyn Integration {
    integrations::find("unraid").expect("the unraid kind is registered")
}

fn ctx(url: &str, key: Option<&str>) -> FetchCtx {
    FetchCtx {
        http: integrations::client().clone(),
        settings: json!({ "url": url }),
        secret: key.map(str::to_string),
    }
}

/// A fetch with the key that is expected to give a reading.
async fn fetch(url: &str) -> Snapshot {
    kind()
        .fetch(&ctx(url, Some(KEY)))
        .await
        .expect("the unraid kind gives a reading")
}

/// A fetch with the key that is expected to give none: the reason.
async fn fetch_err(url: &str) -> FetchError {
    kind()
        .fetch(&ctx(url, Some(KEY)))
        .await
        .expect_err("the unraid kind gives no reading")
}

/// A healthy server: array started, no parity errors, three containers (two
/// running), a data disk at 62 % and a cache at 18 %.
fn healthy() -> Json {
    json!({ "data": {
        "array": {
            "state": "STARTED",
            "capacity": { "kilobytes": { "used": "6200000000", "total": "10000000000" } },
            "parityCheckStatus": {
                "running": false, "errors": 0, "date": "2026-11-01T02:00:00.000Z",
                "duration": 29520,
            },
            "disks": [
                { "name": "disk1", "temp": 34, "status": "DISK_OK", "fsSize": "4000000000", "fsUsed": "2480000000", "fsFree": "1520000000" },
                { "name": "disk2", "temp": 31, "status": "DISK_OK", "fsSize": "4000000000", "fsUsed": "1000000000", "fsFree": "3000000000" },
                { "name": "disk3", "temp": null, "status": "DISK_NP", "fsSize": null, "fsUsed": null, "fsFree": null },
            ],
            "caches": [
                { "name": "cache", "temp": null, "status": "DISK_OK", "fsSize": "500000000", "fsUsed": "90000000", "fsFree": "410000000" },
            ],
        },
        "docker": { "containers": [
            { "state": "RUNNING", "names": ["/plex"] },
            { "state": "RUNNING", "names": ["/sonarr"] },
            { "state": "EXITED", "names": ["/old-thing"] },
        ] },
        "info": { "os": { "hostname": "Tower" } },
        "metrics": { "cpu": { "percentTotal": 7.4 }, "memory": { "percentTotal": 41.2 } },
        "notifications": { "overview": { "unread": { "total": 1, "alert": 0, "warning": 1 } } },
    } })
}

/// `healthy()` with a change at a JSON pointer.
fn with(pointer: &str, value: Json) -> Json {
    let mut doc = healthy();
    *doc.pointer_mut(pointer)
        .unwrap_or_else(|| panic!("{pointer}")) = value;
    doc
}

/// A metric's number; `None` when the metric is left out.
fn num(s: &Snapshot, id: &str) -> Option<f64> {
    s.values.get(id).and_then(|v| v.num)
}

fn text<'a>(s: &'a Snapshot, id: &str) -> Option<&'a str> {
    s.values.get(id).and_then(|v| v.text.as_deref())
}

fn need<'a>(s: &'a Snapshot, key: &str) -> Option<&'a Need> {
    s.needs.iter().find(|n| n.key == key)
}

fn count_param(n: &Need) -> Option<&str> {
    n.params.get("count").map(String::as_str)
}

// ---------------------------------------------------------------------------
// The kind

#[test]
fn the_kind_describes_itself() {
    let k = kind();
    assert_eq!(k.kind(), "unraid");
    assert_eq!(k.label_key(), "kind.unraid");
    assert_eq!(k.default_interval(), Duration::from_secs(60));
    assert_eq!(k.min_interval(), Duration::from_secs(30));
    assert!(k.url_required());
    let ids: Vec<_> = k.metrics().iter().map(|m| m.id).collect();
    assert_eq!(
        ids,
        [
            "containers_running",
            "containers_stopped",
            "array",
            "disk_used",
            "parity_errors",
            "parity_last"
        ]
    );
    let fields = k.fields();
    assert_eq!(fields.len(), 1, "address and API key, nothing else");
    assert!(fields[0].secret && fields[0].required);
    assert_eq!(fields[0].key, "secret");
    assert_eq!(fields[0].label_key, "field.unraid_secret");
    // Every text it needs is in both languages.
    let mut keys: Vec<&str> = vec![k.label_key(), fields[0].label_key, fields[0].help_key];
    keys.extend(k.metrics().iter().map(|m| m.label_key));
    keys.extend([
        "need.unraid_array",
        "need.unraid_parity_errors",
        "need.unraid_disk",
        "fetch_err.auth",
    ]);
    for key in &keys {
        locale::in_both(key).unwrap();
    }
}

#[test]
fn the_form_help_and_the_needs_read_well_in_both_languages() {
    for (lang, file) in locale::LOCALES {
        let help = locale::text(file, "field", "unraid_secret_help").unwrap();
        for part in [
            "Settings → Management Access → API Keys",
            "VIEWER",
            "7.2",
            "x-api-key",
        ] {
            assert!(help.contains(part), "{lang}: {help}");
        }
        // Noun, a colon, the count; the array's need has no count.
        for name in ["unraid_parity_errors", "unraid_disk"] {
            let text = locale::text(file, "need", name).unwrap();
            let noun = text.strip_suffix(": {count}").expect(&text);
            assert!(
                !noun.is_empty() && !noun.contains(['{', '}', ':']),
                "{text}"
            );
        }
        let array = locale::text(file, "need", "unraid_array").unwrap();
        assert!(!array.contains('{'), "{lang}: {array}");
    }
    let nb = include_str!("../locales/nb.toml");
    assert_eq!(
        locale::text(nb, "need", "unraid_array").as_deref(),
        Some("Arrayet er ikke i drift")
    );
    assert_eq!(
        locale::text(nb, "need", "unraid_parity_errors").as_deref(),
        Some("Paritetsfeil: {count}")
    );
    let en = include_str!("../locales/en.toml");
    assert_eq!(
        locale::text(en, "need", "unraid_array").as_deref(),
        Some("Array not running")
    );
}

// ---------------------------------------------------------------------------
// Readings

#[tokio::test]
async fn a_healthy_server_becomes_values_and_an_ok_status() {
    let (url, stub) = serve(Answer::Json(healthy())).await;
    let s = fetch(&url).await;
    assert_eq!(s.status, Status::Ok);
    assert!(s.needs.is_empty(), "{:?}", s.needs);
    assert_eq!(s.error, None);
    assert_eq!(num(&s, "containers_running"), Some(2.0));
    assert_eq!(num(&s, "containers_stopped"), Some(1.0));
    assert_eq!(text(&s, "array"), Some("STARTED"));
    assert_eq!(num(&s, "disk_used"), Some(62.0), "the fullest disk");
    assert_eq!(num(&s, "parity_errors"), Some(0.0));
    assert_eq!(num(&s, "parity_last"), Some(1_793_498_400.0));
    // One call, a JSON POST to /graphql with the key in x-api-key and
    // nothing in Authorization.
    let hits = stub.seen();
    assert_eq!(hits.len(), 1);
    let hit = &hits[0];
    assert_eq!(hit.method, "POST");
    assert_eq!(hit.path, "/graphql");
    assert_eq!(hit.api_key.as_deref(), Some(KEY));
    assert_eq!(hit.authorization, None);
    assert!(
        hit.content_type
            .as_deref()
            .is_some_and(|t| t.starts_with("application/json"))
    );
    let body: Json = serde_json::from_str(&hit.body).unwrap();
    let query = body["query"].as_str().expect("a query");
    for part in ["array", "parityCheckStatus", "docker", "containers"] {
        assert!(query.contains(part), "{part}");
    }
}

#[tokio::test]
async fn a_stopped_array_is_bad_with_the_array_need() {
    let doc = with("/data/array/state", json!("STOPPED"));
    let (url, _) = serve(Answer::Json(doc)).await;
    let s = fetch(&url).await;
    assert_eq!(s.status, Status::Bad);
    assert_eq!(text(&s, "array"), Some("STOPPED"));
    let n = need(&s, "unraid:array").expect("the array need");
    assert_eq!(n.level, Level::Bad);
    assert_eq!(n.score, 1.0);
    assert_eq!(n.label_key, "need.unraid_array");
    assert!(n.params.is_empty(), "no count");
    assert!(need(&s, "unraid:parity_errors").is_none());
}

#[tokio::test]
async fn any_state_but_started_is_not_running_and_started_is_not_case_sensitive() {
    for state in ["NEW_ARRAY", "RECON_DISK", "DISABLE_DISK", "NO_DATA_DISKS"] {
        let (url, _) = serve(Answer::Json(with("/data/array/state", json!(state)))).await;
        let s = fetch(&url).await;
        assert_eq!(s.status, Status::Bad, "{state}");
        assert!(need(&s, "unraid:array").is_some(), "{state}");
    }
    let (url, _) = serve(Answer::Json(with("/data/array/state", json!("started")))).await;
    assert_eq!(fetch(&url).await.status, Status::Ok);
    // No state at all does not say the array is down.
    let (url, _) = serve(Answer::Json(with("/data/array/state", json!(null)))).await;
    let s = fetch(&url).await;
    assert_eq!(s.status, Status::Ok);
    assert!(!s.values.contains_key("array"));
}

#[tokio::test]
async fn parity_errors_are_bad_and_the_count_is_the_score() {
    let doc = with("/data/array/parityCheckStatus/errors", json!(12));
    let (url, _) = serve(Answer::Json(doc)).await;
    let s = fetch(&url).await;
    assert_eq!(s.status, Status::Bad);
    assert_eq!(num(&s, "parity_errors"), Some(12.0));
    let n = need(&s, "unraid:parity_errors").expect("the parity need");
    assert_eq!(n.level, Level::Bad);
    assert_eq!(n.score, 12.0);
    assert_eq!(n.label_key, "need.unraid_parity_errors");
    assert_eq!(count_param(n), Some("12"));
    assert_eq!(s.needs.len(), 1);
}

#[tokio::test]
async fn a_full_disk_warns_at_85_and_is_bad_at_95() {
    // 87 % on the fullest disk.
    let doc = with("/data/array/disks/1/fsUsed", json!("3480000000"));
    let (url, _) = serve(Answer::Json(doc)).await;
    let s = fetch(&url).await;
    assert_eq!(s.status, Status::Warn);
    assert_eq!(num(&s, "disk_used"), Some(87.0));
    let n = need(&s, "unraid:disk").expect("the disk need");
    assert_eq!(n.level, Level::Warn);
    assert_eq!(n.score, 87.0);
    assert_eq!(count_param(n), Some("87"));
    assert_eq!(n.label_key, "need.unraid_disk");

    // Exactly 85 is a warning, just below is not.
    let at = with("/data/array/disks/1/fsUsed", json!("3400000000"));
    let (url, _) = serve(Answer::Json(at)).await;
    assert_eq!(fetch(&url).await.status, Status::Warn);
    let below = with("/data/array/disks/1/fsUsed", json!("3396000000"));
    let (url, _) = serve(Answer::Json(below)).await;
    let s = fetch(&url).await;
    assert_eq!(s.status, Status::Ok);
    assert!(s.needs.is_empty());

    // 96 %: the need is bad, and so is the card.
    let doc = with("/data/array/caches/0/fsUsed", json!("480000000"));
    let (url, _) = serve(Answer::Json(doc)).await;
    let s = fetch(&url).await;
    assert_eq!(s.status, Status::Bad);
    assert_eq!(num(&s, "disk_used"), Some(96.0), "a cache counts too");
    let n = need(&s, "unraid:disk").unwrap();
    assert_eq!(n.level, Level::Bad);
    assert_eq!(n.score, 96.0);
    let at = with("/data/array/caches/0/fsUsed", json!("475000000"));
    let (url, _) = serve(Answer::Json(at)).await;
    assert_eq!(
        need(&fetch(&url).await, "unraid:disk").unwrap().level,
        Level::Bad,
        "95 is bad"
    );
}

#[tokio::test]
async fn the_units_and_the_types_of_the_file_system_figures_do_not_matter() {
    // KiB as numbers, bytes as strings, a mix: the same 62 %.
    for (size, used) in [
        (json!(4_000_000), json!(2_480_000)),
        (json!("4000000000000"), json!("2480000000000")),
        (json!(4_000_000), json!("2480000")),
    ] {
        let mut doc = healthy();
        doc["data"]["array"]["disks"] = json!([{ "fsSize": size, "fsUsed": used }]);
        doc["data"]["array"]["caches"] = json!([]);
        let (url, _) = serve(Answer::Json(doc)).await;
        let s = fetch(&url).await;
        assert!((num(&s, "disk_used").unwrap() - 62.0).abs() < 1e-9);
    }
}

#[tokio::test]
async fn a_stopped_array_without_disk_figures_uses_the_array_capacity() {
    let mut doc = healthy();
    doc["data"]["array"]["state"] = json!("STOPPED");
    doc["data"]["array"]["disks"] = json!([{ "fsSize": null, "fsUsed": null }]);
    doc["data"]["array"]["caches"] = json!(null);
    let (url, _) = serve(Answer::Json(doc)).await;
    let s = fetch(&url).await;
    assert_eq!(num(&s, "disk_used"), Some(62.0));
}

#[tokio::test]
async fn a_running_parity_check_has_no_last_date_and_a_missing_date_costs_only_itself() {
    let mut doc = healthy();
    doc["data"]["array"]["parityCheckStatus"]["running"] = json!(true);
    let (url, _) = serve(Answer::Json(doc)).await;
    let s = fetch(&url).await;
    assert_eq!(num(&s, "parity_last"), None);
    assert_eq!(num(&s, "parity_errors"), Some(0.0));

    for odd in [json!(null), json!("never"), json!(5)] {
        let doc = with("/data/array/parityCheckStatus/date", odd);
        let (url, _) = serve(Answer::Json(doc)).await;
        let s = fetch(&url).await;
        assert_eq!(num(&s, "parity_last"), None);
        assert_eq!(s.status, Status::Ok);
        assert_eq!(num(&s, "disk_used"), Some(62.0));
    }
}

#[tokio::test]
async fn a_hostile_array_state_is_cleaned() {
    let doc = with("/data/array/state", json!("ST\u{202E}ARTED\nX"));
    let (url, _) = serve(Answer::Json(doc)).await;
    let s = fetch(&url).await;
    assert_eq!(text(&s, "array"), Some("STARTED X"));
    assert_eq!(s.status, Status::Bad, "it is not the word STARTED");
}

#[tokio::test]
async fn paused_containers_count_as_stopped_and_no_containers_is_zero() {
    let doc = with(
        "/data/docker/containers",
        json!([{ "state": "RUNNING" }, { "state": "PAUSED" }, { "state": "EXITED" }, {}]),
    );
    let (url, _) = serve(Answer::Json(doc)).await;
    let s = fetch(&url).await;
    assert_eq!(num(&s, "containers_running"), Some(1.0));
    assert_eq!(num(&s, "containers_stopped"), Some(2.0));
    let doc = with("/data/docker/containers", json!([]));
    let (url, _) = serve(Answer::Json(doc)).await;
    let s = fetch(&url).await;
    assert_eq!(num(&s, "containers_running"), Some(0.0));
    assert_eq!(num(&s, "containers_stopped"), Some(0.0));
}

// ---------------------------------------------------------------------------
// GraphQL errors

#[tokio::test]
async fn errors_and_no_data_give_no_reading() {
    for doc in [
        json!({ "errors": [{ "message": "Cannot query field \"metrics\" on type \"Query\"" }] }),
        json!({ "data": null, "errors": [{ "message": "boom" }] }),
        json!({ "errors": [] }),
        json!({ "data": { "array": null, "docker": null }, "errors": [{ "message": "x" }] }),
    ] {
        let (url, _) = serve(Answer::Json(doc.clone())).await;
        assert_eq!(fetch_err(&url).await, FetchError::Invalid, "{doc}");
    }
}

#[tokio::test]
async fn errors_that_all_say_forbidden_are_a_refused_key() {
    for code in ["UNAUTHENTICATED", "FORBIDDEN", "unauthorized"] {
        let doc = json!({
            "data": null,
            "errors": [{ "message": "no", "extensions": { "code": code } }],
        });
        let (url, _) = serve(Answer::Json(doc)).await;
        assert_eq!(fetch_err(&url).await, FetchError::Auth, "{code}");
    }
    // Some other error among them is not that.
    let doc = json!({ "data": null, "errors": [
        { "message": "no", "extensions": { "code": "FORBIDDEN" } },
        { "message": "boom" },
    ] });
    let (url, _) = serve(Answer::Json(doc)).await;
    assert_eq!(fetch_err(&url).await, FetchError::Invalid);
}

#[tokio::test]
async fn partial_data_gives_fewer_metrics_and_still_a_status() {
    // The role may not read the metrics and the containers.
    let mut doc = healthy();
    doc["data"]["metrics"] = json!(null);
    doc["data"]["docker"] = json!(null);
    doc["errors"] = json!([
        { "message": "no", "path": ["metrics"], "extensions": { "code": "FORBIDDEN" } },
        { "message": "no", "path": ["docker"], "extensions": { "code": "FORBIDDEN" } },
    ]);
    doc["data"]["array"]["state"] = json!("STOPPED");
    let (url, _) = serve(Answer::Json(doc)).await;
    let s = fetch(&url).await;
    assert_eq!(s.status, Status::Bad);
    assert!(!s.values.contains_key("containers_running"));
    assert!(!s.values.contains_key("containers_stopped"));
    assert_eq!(text(&s, "array"), Some("STOPPED"));
    assert_eq!(num(&s, "disk_used"), Some(62.0));
    assert!(need(&s, "unraid:array").is_some());
    assert_eq!(s.error.as_deref(), Some("key refused"), "the containers");
}

#[tokio::test]
async fn an_error_about_a_core_field_warns_and_one_about_the_extras_does_not() {
    let denied = |path: &str| json!([{ "message": "no", "path": [path], "extensions": { "code": "FORBIDDEN" } }]);
    // The containers could not be read: a warning with the reason.
    let mut doc = healthy();
    doc["data"]["docker"] = json!(null);
    doc["errors"] = denied("docker");
    let (url, _) = serve(Answer::Json(doc)).await;
    let s = fetch(&url).await;
    assert_eq!(s.status, Status::Warn);
    assert_eq!(s.error.as_deref(), Some("key refused"));
    assert!(s.values.contains_key("array"));
    assert!(!s.values.contains_key("containers_running"));

    // The array, with another kind of error.
    let mut doc = healthy();
    doc["data"]["array"] = json!(null);
    doc["errors"] = json!([{ "message": "boom", "path": ["array"] }]);
    let (url, _) = serve(Answer::Json(doc)).await;
    let s = fetch(&url).await;
    assert_eq!(s.status, Status::Warn);
    assert_eq!(s.error.as_deref(), Some("unexpected answer"));

    // Only the top bar's figures could not be read: nothing to say.
    for path in ["metrics", "info"] {
        let mut doc = healthy();
        doc["data"][path] = json!(null);
        doc["errors"] = denied(path);
        let (url, _) = serve(Answer::Json(doc)).await;
        let s = fetch(&url).await;
        assert_eq!(s.status, Status::Ok, "{path}");
        assert_eq!(s.error, None, "{path}");
    }

    // A warning does not hide a bad status.
    let mut doc = with("/data/array/state", json!("STOPPED"));
    doc["data"]["docker"] = json!(null);
    doc["errors"] = denied("docker");
    let (url, _) = serve(Answer::Json(doc)).await;
    assert_eq!(fetch(&url).await.status, Status::Bad);
}

/// A server of an older Unraid: a query that asks for `metrics` gets `reject`,
/// any other the healthy answer without the extras.
fn older(reject: Answer) -> Answer {
    let core = Answer::Json(with("/data/metrics", json!(null)));
    Answer::With(Arc::new(move |hit| {
        if hit.body.contains("metrics") {
            reject.clone()
        } else {
            core.clone()
        }
    }))
}

fn validation_failed() -> Answer {
    Answer::Json(json!({ "errors": [{
        "message": "Cannot query field \"metrics\" on type \"Query\".",
        "extensions": { "code": "GRAPHQL_VALIDATION_FAILED" },
    }] }))
}

#[tokio::test]
async fn a_server_that_rejects_the_full_query_is_asked_for_the_core_and_remembered() {
    // The server answers 400 (Apollo does for a query that does not validate)
    // or 200 with a validation error and no data.
    for reject in [Answer::Status(400), validation_failed()] {
        let (url, stub) = serve(older(reject)).await;
        let s = fetch(&url).await;
        assert_eq!(s.status, Status::Ok);
        assert_eq!(text(&s, "array"), Some("STARTED"));
        assert_eq!(num(&s, "containers_running"), Some(2.0));
        assert_eq!(num(&s, "disk_used"), Some(62.0));
        assert_eq!(s.error, None);
        let seen = stub.seen();
        assert_eq!(seen.len(), 2, "the full query, then the core");
        assert!(seen[0].body.contains("metrics") && seen[0].body.contains("hostname"));
        assert!(!seen[1].body.contains("metrics") && !seen[1].body.contains("hostname"));
        for hit in &seen {
            assert_eq!(hit.api_key.as_deref(), Some(KEY));
        }
        // The next reading goes straight to the core query.
        let again = fetch(&url).await;
        assert_eq!(again.status, Status::Ok);
        let seen = stub.seen();
        assert_eq!(seen.len(), 3, "one request more, not two");
        assert!(!seen[2].body.contains("metrics"));
    }
}

#[tokio::test]
async fn the_core_query_still_reads_everything_the_card_shows() {
    let (url, stub) = serve(older(Answer::Status(400))).await;
    let s = fetch(&url).await;
    for id in [
        "containers_running",
        "containers_stopped",
        "array",
        "disk_used",
        "parity_errors",
        "parity_last",
    ] {
        assert!(s.values.contains_key(id), "{id}");
    }
    let core = &stub.seen()[1].body;
    for part in ["state", "parityCheckStatus", "fsSize", "containers"] {
        assert!(core.contains(part), "{part}");
    }
    // The fields that are not read are not asked for.
    assert!(!core.contains("status"), "{core}");
}

#[tokio::test]
async fn a_failure_that_is_not_a_rejected_query_is_not_retried() {
    for answer in [
        Answer::Status(500),
        Answer::Json(json!({ "errors": [{ "message": "boom" }] })),
        Answer::Json(
            json!({ "errors": [{ "message": "no", "extensions": { "code": "FORBIDDEN" } }] }),
        ),
        Answer::Raw("garbage"),
    ] {
        let (url, stub) = serve(answer).await;
        let _ = fetch_err(&url).await;
        assert_eq!(stub.seen().len(), 1);
    }
}

#[tokio::test]
async fn when_both_queries_are_rejected_there_is_no_reading() {
    let (url, stub) = serve(Answer::Status(400)).await;
    assert_eq!(fetch_err(&url).await, FetchError::Status(400));
    assert_eq!(stub.seen().len(), 2);
    let (url, _) = serve(validation_failed()).await;
    assert_eq!(fetch_err(&url).await, FetchError::Invalid);
}

#[tokio::test]
async fn the_array_state_is_shown_in_the_users_language_with_the_word_as_fallback() {
    // The texts come from the kit's catalogue, which a Magpie installs.
    let _app = common::App::new().await;
    let (en, nb) = (locale::LOCALES[0].1, locale::LOCALES[1].1);
    for (raw, nb_text, en_text) in [
        ("started", "Startet", "Started"),
        ("stopped", "Stoppet", "Stopped"),
        ("new_array", "Ny array", "New array"),
    ] {
        let key = format!("unraid_array_{raw}");
        assert_eq!(locale::text(nb, "value", &key).as_deref(), Some(nb_text));
        assert_eq!(locale::text(en, "value", &key).as_deref(), Some(en_text));
    }
    // Every state the schema has a text in both languages.
    for state in [
        "started",
        "stopped",
        "new_array",
        "recon_disk",
        "disable_disk",
        "swap_dsbl",
        "invalid_expansion",
        "parity_not_biggest",
        "too_many_missing_disks",
        "new_disk_too_small",
        "no_data_disks",
    ] {
        locale::in_both(&format!("value.unraid_array_{state}")).unwrap();
    }
    // Outside a request the language is English; a word the files do not
    // know stays as the server sent it.
    assert_eq!(
        integrations::value_label("unraid", "array", "STARTED"),
        "Started"
    );
    for odd in ["SOMETHING_NEW", "  ", "", &"X".repeat(80)] {
        assert_eq!(integrations::value_label("unraid", "array", odd), odd);
    }
    // Only text metrics are translated.
    let mut values = std::collections::BTreeMap::from([
        ("array".to_string(), integrations::Value::text("STOPPED")),
        ("disk_used".to_string(), integrations::Value::num(4.0)),
    ]);
    integrations::localize_texts("unraid", &mut values);
    assert_eq!(values["array"].text.as_deref(), Some("Stopped"));
    assert_eq!(values["disk_used"].num, Some(4.0));
}

#[tokio::test]
async fn a_count_is_never_above_the_real_figure() {
    // 87.9 % shows as 87, the need's score keeps the exact value.
    let doc = with("/data/array/disks/1/fsUsed", json!("3516000000"));
    let (url, _) = serve(Answer::Json(doc)).await;
    let s = fetch(&url).await;
    let n = need(&s, "unraid:disk").unwrap();
    assert_eq!(count_param(n), Some("87"));
    assert!((n.score - 87.9).abs() < 1e-9);
}

// ---------------------------------------------------------------------------
// Answers that are no good

#[tokio::test]
async fn a_refused_key_gives_no_reading_and_the_auth_text() {
    for code in [401, 403] {
        let (url, _) = serve(Answer::Status(code)).await;
        let err = fetch_err(&url).await;
        assert_eq!(err, FetchError::Auth, "{code}");
        assert_eq!(err.to_string(), "key refused");
    }
}

#[tokio::test]
async fn malformed_answers_give_no_reading_and_do_not_panic() {
    for raw in [
        "",
        "garbage",
        "{",
        "null",
        "[]",
        "42",
        "\"data\"",
        "{\"data\": 5}",
        "{\"data\": []}",
        "{\"data\": {\"array\": 7, \"docker\": \"x\", \"metrics\": []}}",
    ] {
        let (url, _) = serve(Answer::Raw(raw)).await;
        assert_eq!(fetch_err(&url).await, FetchError::Invalid, "{raw:?}");
    }
}

#[tokio::test]
async fn odd_fields_in_a_good_answer_only_drop_their_metric() {
    let mut doc = healthy();
    doc["data"]["array"]["parityCheckStatus"]["errors"] = json!(-3);
    doc["data"]["array"]["disks"] = json!("x");
    doc["data"]["array"]["caches"] = json!([5, null, { "fsSize": "n/a" }]);
    doc["data"]["array"]["capacity"] = json!(null);
    doc["data"]["docker"]["containers"] = json!([{ "state": 7 }, null]);
    let (url, _) = serve(Answer::Json(doc)).await;
    let s = fetch(&url).await;
    assert_eq!(s.status, Status::Ok);
    assert_eq!(num(&s, "parity_errors"), None);
    assert_eq!(num(&s, "disk_used"), None);
    assert_eq!(num(&s, "containers_running"), Some(0.0));
    assert_eq!(text(&s, "array"), Some("STARTED"));
}

#[tokio::test]
async fn server_errors_and_a_refused_connection_give_no_reading() {
    let (url, _) = serve(Answer::Status(500)).await;
    assert_eq!(fetch_err(&url).await, FetchError::Status(500));
    // A GraphQL endpoint that is not there (not Unraid, or before 7.2).
    let (url, _) = serve(Answer::Status(404)).await;
    assert_eq!(fetch_err(&url).await, FetchError::Status(404));

    // A port nothing listens on.
    let free = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap()
    };
    assert_eq!(
        fetch_err(&format!("http://{free}")).await,
        FetchError::Connect
    );
}

#[tokio::test]
async fn a_huge_answer_is_cut_off() {
    let mut doc = healthy();
    doc["pad"] = json!("x".repeat(2 * 1024 * 1024));
    let (url, _) = serve(Answer::Json(doc)).await;
    assert_eq!(fetch_err(&url).await, FetchError::TooLarge);
}

#[tokio::test]
async fn a_server_that_never_answers_times_out() {
    let (url, _) = serve(Answer::Hang).await;
    let c = FetchCtx {
        http: integrations::build_client(Duration::from_millis(400)),
        settings: json!({ "url": url }),
        secret: Some(KEY.into()),
    };
    assert_eq!(kind().fetch(&c).await.unwrap_err(), FetchError::Timeout);
}

#[tokio::test]
async fn settings_it_cannot_use_are_errors_that_name_no_value() {
    let (url, stub) = serve(Answer::Json(healthy())).await;
    // No key.
    for key in [None, Some("")] {
        let err = kind().fetch(&ctx(&url, key)).await.unwrap_err();
        assert_eq!(err, FetchError::Config);
    }
    // A key that cannot go into a header.
    let err = kind()
        .fetch(&ctx(&url, Some("bad\nkey")))
        .await
        .unwrap_err();
    assert_eq!(err, FetchError::Config);
    for bad in [
        "",
        "not a url",
        "ftp://example.com/",
        "http://user:pw@127.0.0.1:1/",
    ] {
        let err = kind().fetch(&ctx(bad, Some(KEY))).await.unwrap_err();
        assert_eq!(err, FetchError::Config, "{bad:?}");
        assert!(!err.to_string().contains("pw"));
    }
    // The metadata address is refused before anything is sent.
    let err = kind()
        .fetch(&ctx("http://169.254.169.254/", Some(KEY)))
        .await
        .unwrap_err();
    assert_eq!(err, FetchError::Forbidden);
    let c = FetchCtx {
        http: integrations::client().clone(),
        settings: json!({}),
        secret: Some(KEY.into()),
    };
    assert_eq!(kind().fetch(&c).await.unwrap_err(), FetchError::Config);
    assert!(stub.seen().is_empty());
}

#[tokio::test]
async fn a_path_in_the_address_is_kept() {
    // Unraid behind a proxy prefix.
    let (url, stub) = stub::serve_one("/tower/graphql", Answer::Json(healthy())).await;
    let s = fetch(&format!("{url}/tower/")).await;
    assert_eq!(s.status, Status::Ok);
    assert_eq!(stub.seen()[0].path, "/tower/graphql");
}

// ---------------------------------------------------------------------------
// The key

#[tokio::test]
async fn the_key_is_in_the_request_only() {
    let _ = log::buffer();
    let mut snapshots = Vec::new();
    let mut errors = Vec::new();

    let cases = [
        Answer::Json(healthy()),
        Answer::Json(with("/data/array/state", json!("STOPPED"))),
        Answer::Json(
            json!({ "errors": [{ "message": "no", "extensions": { "code": "FORBIDDEN" } }] }),
        ),
        Answer::Json(json!({ "errors": [{ "message": "boom" }] })),
        Answer::Status(401),
        Answer::Status(403),
        Answer::Status(404),
        Answer::Raw("garbage"),
        Answer::Status(500),
    ];
    let total = cases.len() + 1;
    for case in cases {
        let (url, _) = serve(case).await;
        let c = ctx(&url, Some(KEY));
        // What the poller keeps and what a page would show.
        match kind().fetch(&c).await {
            Ok(s) => snapshots.push(s),
            Err(e) => errors.push(e),
        }
        let _ = format!("{c:?}");
    }
    // An address nothing listens on.
    errors.push(fetch_err("http://127.0.0.1:1").await);
    assert_eq!(snapshots.len() + errors.len(), total);

    let mut all = String::new();
    for s in &snapshots {
        all.push_str(&format!("{s:?}"));
        all.push_str(&serde_json::to_string(s).unwrap());
    }
    for e in &errors {
        all.push_str(&format!("{e} {e:?}"));
    }
    assert!(!all.contains(KEY), "a snapshot holds the key");
    assert!(!all.to_lowercase().contains("x-api-key"));

    let log = log::logged();
    assert!(
        log::client_lines_seen(&log),
        "the capture sees the HTTP client's own lines"
    );
    assert!(!log.contains(KEY), "the log holds the key");
}

#[tokio::test]
async fn a_redirect_to_another_origin_never_carries_the_key() {
    // reqwest drops `Authorization` on such a redirect, but not `x-api-key`:
    // the call is refused instead, and the other origin sees nothing. All
    // three redirects that GraphQL clients meet (POST kept or turned to GET).
    for redirect in stub::redirects() {
        let (target, target_stub) = serve(Answer::Json(healthy())).await;
        let origin = stub::redirector(redirect, format!("{target}/graphql")).await;
        assert_eq!(fetch_err(&origin).await, FetchError::RedirectScheme);
        assert!(target_stub.seen().is_empty(), "{:?}", target_stub.seen());
    }
    // Another port on the same host is another origin too (Unraid sends
    // http to https that way).
    let (target, target_stub) = serve(Answer::Json(healthy())).await;
    let port_of = |url: &str| {
        url.split('/')
            .nth(2)
            .unwrap()
            .rsplit(':')
            .next()
            .unwrap()
            .to_string()
    };
    let origin = stub::redirector(
        axum::response::Redirect::temporary,
        format!("http://localhost:{}/graphql", port_of(&target)),
    )
    .await;
    assert_eq!(fetch_err(&origin).await, FetchError::RedirectScheme);
    assert!(target_stub.seen().is_empty());
}

#[tokio::test]
async fn a_redirect_within_the_origin_keeps_the_key() {
    let (url, stub) = serve(Answer::Json(healthy())).await;
    stub.route("/old/graphql", Answer::Moved("/graphql"));
    let s = fetch(&format!("{url}/old")).await;
    assert_eq!(s.status, Status::Ok);
    let seen = stub.seen();
    assert_eq!(seen.len(), 2);
    assert_eq!(seen[0].path, "/old/graphql");
    assert_eq!(seen[1].path, "/graphql");
    assert_eq!(seen[1].api_key.as_deref(), Some(KEY));
    assert_eq!(seen[1].method, "POST");
}

#[tokio::test]
async fn the_text_of_a_value_is_looked_up_by_a_safe_key_only() {
    // The texts come from the kit's catalogue, which a Magpie installs.
    let _app = common::App::new().await;
    use mindful_magpie::integrations::value_label;
    // A known word gets its text; anything else shows as it is.
    assert_eq!(value_label("unraid", "array", "STARTED"), "Started");
    for hostile in [
        "../",
        "../../etc/passwd",
        "a.b",
        "unraid_array_started.",
        "value.unraid_array_started",
        "[value]",
        "started\u{0}",
        "star ted",
        "<b>started</b>",
        "",
        "   ",
        "é",
        &"x".repeat(200),
    ] {
        let shown = value_label("unraid", "array", hostile);
        // Never one of the fixed texts by accident, and never changed.
        assert_eq!(shown, hostile, "{hostile:?}");
    }
    // The kind and metric are not looked up with anything but the key's own
    // characters either: a hostile one finds nothing and shows the raw text.
    assert_eq!(value_label("../x", "array", "started"), "started");
    assert_eq!(value_label("unraid", "a.b", "started"), "started");
}

// ---------------------------------------------------------------------------
// The details

fn titles(s: &Snapshot) -> Vec<&str> {
    s.details
        .iter()
        .map(|b| match b {
            Block::List { title_key, .. }
            | Block::Bars { title_key, .. }
            | Block::Strip { title_key, .. }
            | Block::Facts { title_key, .. }
            | Block::Spark { title_key, .. } => title_key.as_str(),
        })
        .collect()
}

fn bars_of<'a>(s: &'a Snapshot, title: &str) -> Option<&'a Vec<Bar>> {
    s.details.iter().find_map(|b| match b {
        Block::Bars { title_key, bars } if title_key == title => Some(bars),
        _ => None,
    })
}

fn facts_of<'a>(s: &'a Snapshot, title: &str) -> Option<Vec<(&'a str, &'a str)>> {
    s.details.iter().find_map(|b| match b {
        Block::Facts { title_key, rows } if title_key == title => Some(
            rows.iter()
                .map(|r| (r.label_key.as_deref().unwrap_or(""), r.value.as_str()))
                .collect(),
        ),
        _ => None,
    })
}

/// `healthy()` as an Unraid without the extras of the full query answers it:
/// no names or temperatures, no check length, no container names, no
/// notifications.
fn plain() -> Json {
    let mut doc = healthy();
    for group in ["disks", "caches"] {
        for disk in doc["data"]["array"][group].as_array_mut().unwrap() {
            let disk = disk.as_object_mut().unwrap();
            disk.remove("name");
            disk.remove("temp");
        }
    }
    doc["data"]["array"]["parityCheckStatus"]
        .as_object_mut()
        .unwrap()
        .remove("duration");
    for container in doc["data"]["docker"]["containers"].as_array_mut().unwrap() {
        container.as_object_mut().unwrap().remove("names");
    }
    doc["data"].as_object_mut().unwrap().remove("notifications");
    doc["data"]["metrics"] = json!(null);
    doc
}

#[tokio::test]
async fn the_disks_become_bars_with_size_and_temperature() {
    let (url, _) = serve(Answer::Json(healthy())).await;
    let s = fetch(&url).await;
    let bars = bars_of(&s, "unraid_disks").expect("the disks");
    let got: Vec<_> = bars
        .iter()
        .map(|b| (b.label.as_str(), b.pct.round(), b.text.as_str()))
        .collect();
    assert_eq!(
        got,
        [
            ("disk1", 62.0, "2.5 / 4.1 TB · 34 °C"),
            ("disk2", 25.0, "1 / 4.1 TB · 31 °C"),
            ("cache", 18.0, "92.2 / 512 GB"),
        ],
        "the empty slot has no bar, a missing temperature no text"
    );
}

#[tokio::test]
async fn a_disk_bar_turns_yellow_and_red_at_the_thresholds_of_the_needs() {
    // disk1 at 85 % (the warning), disk2 at 95 % (the problem), the cache at
    // 18 %: neutral.
    let mut doc = healthy();
    doc["data"]["array"]["disks"][0]["fsUsed"] = json!("3400000000");
    doc["data"]["array"]["disks"][1]["fsUsed"] = json!("3800000000");
    let (url, _) = serve(Answer::Json(doc)).await;
    let s = fetch(&url).await;
    let bars = bars_of(&s, "unraid_disks").expect("the disks");
    let got: Vec<_> = bars.iter().map(|b| (b.label.as_str(), b.status)).collect();
    assert_eq!(
        got,
        [
            ("disk1", Some(Status::Warn)),
            ("disk2", Some(Status::Bad)),
            ("cache", None),
        ]
    );
    // Just below the warning: neutral.
    let doc = with("/data/array/disks/0/fsUsed", json!("3396000000"));
    let (url, _) = serve(Answer::Json(doc)).await;
    let s = fetch(&url).await;
    assert_eq!(bars_of(&s, "unraid_disks").unwrap()[0].status, None);
}

#[tokio::test]
async fn the_last_parity_check_is_a_moment_with_its_date_as_the_text() {
    let (url, _) = serve(Answer::Json(healthy())).await;
    let s = fetch(&url).await;
    let rows = s
        .details
        .iter()
        .find_map(|b| match b {
            Block::Facts { title_key, rows } if title_key == "unraid_parity" => Some(rows),
            _ => None,
        })
        .expect("the parity facts");
    let at = rows[0].at.expect("a moment");
    let day = jiff::Timestamp::from_second(at)
        .unwrap()
        .to_zoned(jiff::tz::TimeZone::UTC)
        .date()
        .to_string();
    assert_eq!(day, rows[0].value);
    assert!(rows[1..].iter().all(|r| r.at.is_none()));
}

#[tokio::test]
async fn the_parity_check_becomes_facts() {
    let (url, _) = serve(Answer::Json(healthy())).await;
    let s = fetch(&url).await;
    assert_eq!(
        facts_of(&s, "unraid_parity").expect("the parity facts"),
        [
            ("unraid_parity_last", "2026-11-01"),
            ("unraid_parity_duration", "8 h 12 min"),
            ("unraid_parity_errors", "0"),
        ]
    );
    // A running check has no finished date or length, its errors stay.
    let doc = with("/data/array/parityCheckStatus/running", json!(true));
    let (url, _) = serve(Answer::Json(doc)).await;
    let s = fetch(&url).await;
    assert_eq!(
        facts_of(&s, "unraid_parity").unwrap(),
        [("unraid_parity_errors", "0")]
    );
    // Errors are shown as the count.
    let doc = with("/data/array/parityCheckStatus/errors", json!(12));
    let (url, _) = serve(Answer::Json(doc)).await;
    let s = fetch(&url).await;
    assert_eq!(
        facts_of(&s, "unraid_parity").unwrap()[2],
        ("unraid_parity_errors", "12")
    );
}

#[tokio::test]
async fn the_containers_that_are_not_running_are_listed_greyed() {
    let doc = with(
        "/data/docker/containers",
        json!([
            { "state": "RUNNING", "names": ["/plex"] },
            { "state": "EXITED", "names": ["/old-thing"] },
            { "state": "PAUSED", "names": ["paused-one"] },
            { "state": "EXITED" },
        ]),
    );
    let (url, _) = serve(Answer::Json(doc)).await;
    let s = fetch(&url).await;
    let Some(Block::List { items, .. }) = s
        .details
        .iter()
        .find(|b| matches!(b, Block::List { title_key, .. } if title_key == "unraid_stopped"))
    else {
        panic!("the stopped containers")
    };
    let got: Vec<_> = items.iter().map(|i| i.text.as_str()).collect();
    assert_eq!(
        got,
        ["old-thing", "paused-one"],
        "no leading slash, no nameless"
    );
    assert!(items.iter().all(|i| i.status == Some(Status::Unknown)));
    // The metrics still count all three of them.
    assert_eq!(num(&s, "containers_stopped"), Some(3.0));
    // No stopped container, no list.
    let doc = with(
        "/data/docker/containers",
        json!([{ "state": "RUNNING", "names": ["/a"] }]),
    );
    let (url, _) = serve(Answer::Json(doc)).await;
    assert!(!titles(&fetch(&url).await).contains(&"unraid_stopped"));
}

#[tokio::test]
async fn unread_notifications_are_facts_only_when_there_are_some() {
    let (url, _) = serve(Answer::Json(healthy())).await;
    let s = fetch(&url).await;
    assert_eq!(
        facts_of(&s, "unraid_notices").expect("the notifications"),
        [("unraid_unread", "1"), ("unraid_warnings", "1")]
    );
    let doc = with(
        "/data/notifications/overview/unread",
        json!({ "total": 3, "alert": 2, "warning": 0 }),
    );
    let (url, _) = serve(Answer::Json(doc)).await;
    let s = fetch(&url).await;
    assert_eq!(
        facts_of(&s, "unraid_notices").unwrap(),
        [("unraid_unread", "3"), ("unraid_alerts", "2")]
    );
    let doc = with(
        "/data/notifications/overview/unread",
        json!({ "total": 0, "alert": 0, "warning": 0 }),
    );
    let (url, _) = serve(Answer::Json(doc)).await;
    assert!(facts_of(&fetch(&url).await, "unraid_notices").is_none());
}

#[tokio::test]
async fn the_blocks_come_in_order_every_key_resolves_and_the_numbers_are_unchanged() {
    let _app = common::App::new().await;
    let (url, stub) = serve(Answer::Json(healthy())).await;
    let s = fetch(&url).await;
    assert_eq!(
        titles(&s),
        [
            "unraid_disks",
            "unraid_parity",
            "unraid_stopped",
            "unraid_notices"
        ]
    );
    for block in &s.details {
        for key in block.keys() {
            assert!(block::key_known(key), "{key}");
        }
    }
    for (lang, file) in locale::LOCALES {
        for key in [
            "unraid_disks",
            "unraid_parity",
            "unraid_parity_last",
            "unraid_parity_duration",
            "unraid_parity_errors",
            "unraid_stopped",
            "unraid_notices",
            "unraid_unread",
            "unraid_alerts",
            "unraid_warnings",
        ] {
            assert!(locale::has_text(file, "detail", key), "{lang}: {key}");
        }
    }
    assert_eq!(s.status, Status::Ok);
    assert_eq!(num(&s, "disk_used"), Some(62.0));
    // The full query asks for what the details read, and still in one call.
    let hits = stub.seen();
    assert_eq!(hits.len(), 1);
    let body: Json = serde_json::from_str(&hits[0].body).unwrap();
    let query = body["query"].as_str().unwrap();
    for part in ["temp", "names", "duration", "notifications", "name"] {
        assert!(query.contains(part), "{part}");
    }
}

#[tokio::test]
async fn an_older_unraid_gives_the_core_figures_and_fewer_blocks() {
    let full = {
        let (url, _) = serve(Answer::Json(healthy())).await;
        fetch(&url).await
    };
    // The full query is rejected; the core query is answered without the extras.
    let core = Answer::Json(plain());
    let stub_answer = Answer::With(Arc::new(move |hit| {
        if hit.body.contains("metrics") {
            validation_failed()
        } else {
            core.clone()
        }
    }));
    let (url, stub) = serve(stub_answer).await;
    let s = fetch(&url).await;
    assert_eq!(s.status, Status::Ok);
    assert_eq!(s.error, None);
    assert_eq!(num(&s, "disk_used"), Some(62.0));
    assert_eq!(num(&s, "containers_stopped"), Some(1.0));
    // Only what the core query has: the parity date and errors.
    assert_eq!(titles(&s), ["unraid_parity"]);
    assert_eq!(
        facts_of(&s, "unraid_parity").unwrap(),
        [
            ("unraid_parity_last", "2026-11-01"),
            ("unraid_parity_errors", "0")
        ]
    );
    assert!(s.details.len() < full.details.len());
    // The core query asks for none of the new fields, so it still works there.
    let seen = stub.seen();
    assert_eq!(seen.len(), 2);
    for part in ["temp", "names", "duration", "notifications", "name"] {
        assert!(!seen[1].body.contains(part), "{part}: {}", seen[1].body);
    }
}

#[tokio::test]
async fn an_error_about_a_detail_field_is_silent() {
    let denied = |path: Json| json!([{ "message": "no", "path": path, "extensions": { "code": "FORBIDDEN" } }]);
    // The key may not read the notifications.
    let mut doc = healthy();
    doc["data"]["notifications"] = json!(null);
    doc["errors"] = denied(json!(["notifications"]));
    let (url, _) = serve(Answer::Json(doc)).await;
    let s = fetch(&url).await;
    assert_eq!(s.status, Status::Ok);
    assert_eq!(s.error, None);
    assert!(!titles(&s).contains(&"unraid_notices"));
    assert!(titles(&s).contains(&"unraid_disks"), "the rest stays");
    // A disk's temperature, a disk's name, a container's names, the check's length.
    for path in [
        json!(["array", "disks", 0, "temp"]),
        json!(["array", "caches", 0, "name"]),
        json!(["docker", "containers", 2, "names"]),
        json!(["array", "parityCheckStatus", "duration"]),
    ] {
        let mut doc = healthy();
        doc["errors"] = denied(path.clone());
        let (url, _) = serve(Answer::Json(doc)).await;
        let s = fetch(&url).await;
        assert_eq!((s.status, s.error.as_deref()), (Status::Ok, None), "{path}");
    }
    // An error about a core field of the same part still warns.
    let mut doc = healthy();
    doc["errors"] = denied(json!(["array", "disks", 0, "fsSize"]));
    let (url, _) = serve(Answer::Json(doc)).await;
    assert_eq!(fetch(&url).await.status, Status::Warn);
}

#[tokio::test]
async fn blocks_are_cut_to_eight_keeping_the_fullest_disks_in_order() {
    let mut doc = healthy();
    let disks: Vec<Json> = (1..=20)
        .map(|n| {
            let used = if n == 13 { 900 } else { 100 + n };
            json!({ "name": format!("disk{n}"), "fsSize": "1000000", "fsUsed": used.to_string() })
        })
        .collect();
    doc["data"]["array"]["disks"] = json!(disks);
    doc["data"]["array"]["caches"] = json!([]);
    doc["data"]["docker"]["containers"] = json!(
        (0..30)
            .map(|n| json!({ "state": "EXITED", "names": [format!("/c{n}")] }))
            .collect::<Vec<_>>()
    );
    let (url, _) = serve(Answer::Json(doc)).await;
    let mut s = fetch(&url).await;
    s.bound_details();
    let bars = bars_of(&s, "unraid_disks").unwrap();
    assert_eq!(bars.len(), 8);
    let labels: Vec<_> = bars.iter().map(|b| b.label.as_str()).collect();
    assert_eq!(
        labels,
        [
            "disk13", "disk14", "disk15", "disk16", "disk17", "disk18", "disk19", "disk20"
        ],
        "the fullest eight, in the order of the array"
    );
    for block in &s.details {
        let n = match block {
            Block::List { items, .. } => items.len(),
            Block::Bars { bars, .. } => bars.len(),
            Block::Facts { rows, .. } => rows.len(),
            Block::Strip { cells, .. } => cells.len(),
            Block::Spark { .. } => 1,
        };
        assert!(n <= block::MAX_ITEMS, "{block:?}");
    }
    assert!(s.details.len() <= block::MAX_BLOCKS);
}

#[tokio::test]
async fn hostile_names_stay_text_and_out_of_needs_and_values() {
    let hostile = "<script>alert(1)</script>\u{202E}\n";
    let long = "d".repeat(500);
    let mut doc = healthy();
    doc["data"]["array"]["disks"][0]["name"] = json!(hostile);
    doc["data"]["array"]["disks"][1]["name"] = json!(long);
    doc["data"]["docker"]["containers"] = json!([
        { "state": "EXITED", "names": [format!("/{hostile}")] },
        { "state": "EXITED", "names": [long] },
        { "state": "EXITED", "names": "\u{202E}\u{0}" },
    ]);
    let (url, _) = serve(Answer::Json(doc)).await;
    let mut s = fetch(&url).await;
    s.bound_details();
    let json = serde_json::to_string(&s.details).unwrap();
    assert!(
        !json.contains('\u{202E}') && !json.contains("\\n"),
        "{json}"
    );
    assert!(
        json.contains("<script>alert(1)</script>"),
        "text, not removed"
    );
    for block in &s.details {
        if let Block::List { items, .. } = block {
            assert_eq!(items.len(), 2, "the nameless one is dropped");
            for item in items {
                assert!(item.text.chars().count() <= block::TEXT_CHARS);
            }
        }
    }
    let rest = format!("{:?}{:?}{:?}", s.values, s.needs, s.error);
    assert!(!rest.contains("script") && !rest.contains("dddd"), "{rest}");
}

#[tokio::test]
async fn odd_figures_cost_only_their_own_part_of_the_details() {
    let mut doc = healthy();
    doc["data"]["array"]["disks"] = json!([
        { "name": "a", "temp": "hot", "fsSize": 1000, "fsUsed": 500 },
        { "name": 5, "fsSize": 1000, "fsUsed": 500 },
        { "name": "c", "temp": 9999, "fsSize": 0, "fsUsed": 0 },
        { "name": "d", "temp": -3, "fsSize": 1000, "fsFree": 250 },
    ]);
    doc["data"]["array"]["caches"] = json!([]);
    doc["data"]["array"]["parityCheckStatus"] =
        json!({ "running": false, "errors": "x", "date": 5, "duration": "long" });
    doc["data"]["notifications"] = json!({ "overview": { "unread": { "total": "many" } } });
    let (url, _) = serve(Answer::Json(doc)).await;
    let s = fetch(&url).await;
    let bars = bars_of(&s, "unraid_disks").unwrap();
    let got: Vec<_> = bars
        .iter()
        .map(|b| (b.label.as_str(), b.text.as_str()))
        .collect();
    assert_eq!(
        got,
        [("a", "0.5 / 1 MB"), ("d", "0.8 / 1 MB")],
        "no bar without a name or figures; a bad temperature is not shown"
    );
    assert!(!titles(&s).contains(&"unraid_parity"));
    assert!(!titles(&s).contains(&"unraid_notices"));
}

#[tokio::test]
async fn a_key_that_may_not_read_the_extras_gets_the_core_query() {
    // The server drops the whole data when a field it may not read cannot be
    // null; the core query has none of the extras and is answered.
    for path in ["notifications", "info", "metrics"] {
        let refused = Answer::Json(json!({
            "data": null,
            "errors": [{ "message": "no", "path": [path], "extensions": { "code": "FORBIDDEN" } }],
        }));
        let core = Answer::Json(plain());
        let answer = Answer::With(Arc::new(move |hit| {
            if hit.body.contains("notifications") {
                refused.clone()
            } else {
                core.clone()
            }
        }));
        let (url, stub) = serve(answer).await;
        let s = fetch(&url).await;
        assert_eq!(s.status, Status::Ok, "{path}");
        assert_eq!(s.error, None, "{path}");
        assert_eq!(num(&s, "disk_used"), Some(62.0), "{path}");
        assert_eq!(num(&s, "containers_running"), Some(2.0), "{path}");
        assert_eq!(
            stub.seen().len(),
            2,
            "the full query, then the core: {path}"
        );
        // The next reading goes straight to the core query.
        let again = fetch(&url).await;
        assert_eq!(again.status, Status::Ok);
        assert_eq!(stub.seen().len(), 3, "{path}");
    }
}

#[tokio::test]
async fn a_refusal_of_a_core_field_without_data_is_still_a_refused_key() {
    for path in [json!(["array"]), json!(["docker"])] {
        let doc = json!({
            "data": null,
            "errors": [{ "message": "no", "path": path, "extensions": { "code": "FORBIDDEN" } }],
        });
        let (url, stub) = serve(Answer::Json(doc)).await;
        assert_eq!(fetch_err(&url).await, FetchError::Auth, "{path}");
        assert_eq!(stub.seen().len(), 1, "not retried: {path}");
    }
}

#[tokio::test]
async fn huge_figures_keep_the_details_short() {
    let mut doc = healthy();
    doc["data"]["array"]["disks"] = json!([
        { "name": "big", "temp": 40, "fsSize": 1.7e308, "fsUsed": 1e308 },
    ]);
    doc["data"]["array"]["caches"] = json!([]);
    doc["data"]["array"]["parityCheckStatus"]["duration"] = json!(1e300);
    let (url, _) = serve(Answer::Json(doc)).await;
    let s = fetch(&url).await;
    let bars = bars_of(&s, "unraid_disks").unwrap();
    assert!(bars[0].text.chars().count() <= 60, "{}", bars[0].text);
    let rows = facts_of(&s, "unraid_parity").unwrap();
    assert!(
        rows.iter().all(|(_, v)| v.chars().count() <= 40),
        "{rows:?}"
    );
}
