//! The `proxmox` kind («Proxmox VE») against a stub server on 127.0.0.1: a
//! realistic `cluster/resources` answer, an offline node, full storage, a
//! refused token, a token that sees nothing, answers that are not what was
//! expected, the needs, the node rows, and that the token secret goes nowhere
//! but into the request.

mod common;

use std::time::Duration;

use common::stub::{self, Answer, Stub};
use common::{locale, log};
use mindful_magpie::integrations::proxmox::{NodeRow, node_rows};
use mindful_magpie::integrations::{
    self, FetchCtx, FetchError, Integration, Level, Snapshot, Status,
};
use serde_json::{Value as Json, json};

const SECRET: &str = "5f1c9e2a-77b0-4c1e-9d3a-Zq81x7w4mK";
const TOKEN_ID: &str = "magpie@pve!start";
const RESOURCES: &str = "/api2/json/cluster/resources";

/// A stub whose `cluster/resources` answers `what`.
async fn serve(what: Answer) -> (String, Stub) {
    stub::serve_one(RESOURCES, what).await
}

fn kind() -> &'static dyn Integration {
    integrations::find("proxmox").expect("the proxmox kind is registered")
}

fn ctx_with(url: &str, token_id: Option<&str>, secret: Option<&str>) -> FetchCtx {
    let mut settings = json!({ "url": url });
    if let Some(id) = token_id {
        settings["token_id"] = json!(id);
    }
    FetchCtx {
        http: integrations::client().clone(),
        settings,
        secret: secret.map(str::to_string),
    }
}

fn ctx(url: &str) -> FetchCtx {
    ctx_with(url, Some(TOKEN_ID), Some(SECRET))
}

/// A fetch that is expected to give a reading.
async fn fetch(url: &str) -> Snapshot {
    kind()
        .fetch(&ctx(url))
        .await
        .expect("the proxmox kind gives a reading")
}

/// A fetch that is expected to give none: the reason.
async fn fetch_err(url: &str) -> FetchError {
    kind()
        .fetch(&ctx(url))
        .await
        .expect_err("the proxmox kind gives no reading")
}

const GIB: u64 = 1024 * 1024 * 1024;

/// A cluster of two nodes with guests, a template and storage, as Proxmox
/// answers it (trimmed to what matters).
fn cluster() -> Json {
    json!({ "data": [
        { "id": "node/pve1", "type": "node", "node": "pve1", "status": "online", "level": "",
          "cpu": 0.10, "maxcpu": 8, "mem": 16 * GIB, "maxmem": 64 * GIB,
          "disk": 20 * GIB, "maxdisk": 100 * GIB, "uptime": 123456 },
        { "id": "node/pve2", "type": "node", "node": "pve2", "status": "online", "level": "",
          "cpu": 0.30, "maxcpu": 8, "mem": 48 * GIB, "maxmem": 64 * GIB,
          "disk": 60 * GIB, "maxdisk": 100 * GIB, "uptime": 99999 },
        { "id": "qemu/100", "type": "qemu", "vmid": 100, "name": "web", "node": "pve1",
          "status": "running", "template": 0, "cpu": 0.02, "mem": 2 * GIB, "maxmem": 4 * GIB,
          "disk": 0, "maxdisk": 32 * GIB, "uptime": 5000 },
        { "id": "qemu/101", "type": "qemu", "vmid": 101, "name": "db", "node": "pve2",
          "status": "running", "template": 0, "cpu": 0.05, "mem": 3 * GIB, "maxmem": 8 * GIB,
          "uptime": 5000 },
        { "id": "qemu/102", "type": "qemu", "vmid": 102, "name": "old", "node": "pve2",
          "status": "stopped", "template": 0, "maxmem": 4 * GIB },
        { "id": "qemu/900", "type": "qemu", "vmid": 900, "name": "base", "node": "pve1",
          "status": "stopped", "template": 1, "maxmem": 4 * GIB },
        { "id": "lxc/110", "type": "lxc", "vmid": 110, "name": "svc", "node": "pve1",
          "status": "running", "template": 0, "cpu": 0.0, "mem": GIB, "maxmem": 2 * GIB },
        { "id": "lxc/111", "type": "lxc", "vmid": 111, "name": "idle", "node": "pve2",
          "status": "stopped" },
        { "id": "storage/pve1/local", "type": "storage", "storage": "local", "node": "pve1",
          "status": "available", "disk": 30 * GIB, "maxdisk": 100 * GIB },
        { "id": "storage/pve2/local", "type": "storage", "storage": "local", "node": "pve2",
          "status": "available", "disk": 50 * GIB, "maxdisk": 100 * GIB },
        { "id": "storage/pve1/nas", "type": "storage", "storage": "nas", "node": "pve1",
          "status": "available", "shared": 1, "disk": 40 * GIB, "maxdisk": 200 * GIB },
        { "id": "storage/pve2/nas", "type": "storage", "storage": "nas", "node": "pve2",
          "status": "available", "shared": 1, "disk": 40 * GIB, "maxdisk": 200 * GIB },
        { "id": "storage/pve2/usb", "type": "storage", "storage": "usb", "node": "pve2",
          "status": "unknown" },
        { "id": "pool/lab", "type": "pool", "poolid": "lab" },
        { "id": "sdn/pve1/localnetwork", "type": "sdn", "node": "pve1", "status": "ok" },
    ]})
}

/// `cluster()` with one entry's fields changed: `edit` gets the entry whose
/// `id` is `id`.
fn changed(id: &str, edit: impl FnOnce(&mut Json)) -> Json {
    let mut doc = cluster();
    let item = doc["data"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|i| i["id"] == id)
        .unwrap();
    edit(item);
    doc
}

fn offline(name: &str) -> Json {
    json!({ "id": format!("node/{name}"), "type": "node", "node": name, "status": "offline" })
}

/// A metric's number; `None` when the metric is left out.
fn num(s: &Snapshot, id: &str) -> Option<f64> {
    s.values.get(id).and_then(|v| v.num)
}

fn close(a: Option<f64>, b: f64) -> bool {
    a.is_some_and(|a| (a - b).abs() < 1e-6)
}

/// The text called `name` in `[section]` of a locale file, when there is a
/// non-empty one.
fn text_of(file: &str, section: &str, name: &str) -> Option<String> {
    locale::text(file, section, name).filter(|t| !t.is_empty())
}

fn locales() -> [(&'static str, &'static str); 2] {
    locale::LOCALES
}

// ---------------------------------------------------------------------------
// The kind

#[test]
fn the_kind_describes_itself() {
    let k = kind();
    assert_eq!(k.kind(), "proxmox");
    assert_eq!(k.label_key(), "kind.proxmox");
    assert_eq!(k.default_interval(), Duration::from_secs(30));
    assert_eq!(k.min_interval(), Duration::from_secs(15));
    assert!(k.url_required());
    let ids: Vec<_> = k.metrics().iter().map(|m| m.id).collect();
    assert_eq!(
        ids,
        [
            "nodes_online",
            "nodes_total",
            "guests_running",
            "guests_total",
            "cpu",
            "ram",
            "storage_max"
        ]
    );
    // The token id is a plain setting, the secret the one secret field.
    let fields = k.fields();
    assert_eq!(fields.len(), 2);
    assert_eq!(fields[0].key, "token_id");
    assert!(!fields[0].secret && fields[0].required);
    assert_eq!(fields[1].key, "secret");
    assert!(fields[1].secret && fields[1].required);
    let registered = integrations::registry()
        .iter()
        .filter(|k| k.kind() == "proxmox")
        .count();
    assert_eq!(registered, 1);
}

#[test]
fn every_text_it_needs_is_in_both_languages() {
    let k = kind();
    let mut keys: Vec<&str> = vec![k.label_key()];
    for f in k.fields() {
        keys.extend([f.label_key, f.help_key]);
    }
    keys.extend(k.metrics().iter().map(|m| m.label_key));
    keys.extend([
        "need.pve_node_down",
        "need.pve_storage",
        "err.pve_token_id",
        "fetch_err.auth",
    ]);
    for (lang, file) in locales() {
        for key in &keys {
            let (section, name) = key.split_once('.').unwrap();
            assert!(text_of(file, section, name).is_some(), "{lang}: {key}");
        }
    }
}

#[test]
fn the_form_help_says_how_to_make_a_read_only_token() {
    for (lang, file) in locales() {
        let help = text_of(file, "field", "pve_token_id_help").unwrap();
        for needle in [
            "Datacenter",
            "Permissions",
            "API Tokens",
            "PVEAuditor",
            "Privilege Separation",
        ] {
            assert!(help.contains(needle), "{lang}: {needle}");
        }
        let secret = text_of(file, "field", "pve_secret_help").unwrap();
        assert!(secret.contains("Authorization: PVEAPIToken"), "{lang}");
    }
}

#[test]
fn need_labels_are_a_noun_a_colon_and_the_count() {
    for (lang, file) in locales() {
        for key in ["pve_node_down", "pve_storage"] {
            let label = text_of(file, "need", key).unwrap();
            let noun = label
                .strip_suffix(": {count}")
                .unwrap_or_else(|| panic!("{lang} {key}: {label}"));
            assert!(
                !noun.is_empty() && !noun.contains(['{', '}', ':']),
                "{lang} {key}: {label}"
            );
        }
    }
}

#[test]
fn the_token_id_setting_is_checked() {
    let check = |id: &str| {
        let settings = json!({ "token_id": id });
        kind().check_settings(settings.as_object().unwrap())
    };
    assert!(check("root@pam!magpie").is_ok());
    assert!(check(" root@pam!magpie ").is_ok());
    for bad in ["", "root@pam", "x\r\nY: 1@a!b", "root@pam!a b"] {
        let err = check(bad).unwrap_err();
        assert_eq!(err.field, "token_id", "{bad:?}");
        assert_eq!(err.message_key, "err.pve_token_id");
    }
    // Not given at all is the form's «required», not this check's.
    assert!(kind().check_settings(&serde_json::Map::new()).is_ok());
}

// ---------------------------------------------------------------------------
// Readings

#[tokio::test]
async fn a_realistic_cluster_becomes_values_and_an_ok_status() {
    let (url, stub) = serve(Answer::Json(cluster())).await;
    let s = fetch(&url).await;

    assert_eq!(s.status, Status::Ok);
    assert_eq!(s.error, None);
    assert!(s.needs.is_empty(), "{:?}", s.needs);
    assert_eq!(num(&s, "nodes_online"), Some(2.0));
    assert_eq!(num(&s, "nodes_total"), Some(2.0));
    // Guests: qemu + lxc, the template left out.
    assert_eq!(num(&s, "guests_running"), Some(3.0));
    assert_eq!(num(&s, "guests_total"), Some(5.0));
    assert!(close(num(&s, "cpu"), 20.0));
    // (16 + 48) / (64 + 64)
    assert!(close(num(&s, "ram"), 50.0));
    // local on pve2 is the fullest: 50 %.
    assert!(close(num(&s, "storage_max"), 50.0));

    // One call, with the token in Proxmox's own header.
    let hits = stub.hits_on(RESOURCES);
    assert_eq!(hits.len(), 1);
    assert_eq!(stub.seen().len(), 1);
    assert_eq!(hits[0].query, None);
    assert_eq!(
        hits[0].authorization.as_deref(),
        Some(format!("PVEAPIToken={TOKEN_ID}={SECRET}").as_str())
    );
}

#[tokio::test]
async fn a_standalone_host_with_nothing_but_the_node_is_a_reading() {
    let doc = json!({ "data": [
        { "id": "node/pve", "type": "node", "node": "pve", "status": "online",
          "cpu": 0.5, "mem": 1, "maxmem": 4 },
    ]});
    let (url, _) = serve(Answer::Json(doc)).await;
    let s = fetch(&url).await;
    assert_eq!(s.status, Status::Ok);
    assert_eq!(num(&s, "guests_total"), Some(0.0));
    assert_eq!(num(&s, "guests_running"), Some(0.0));
    assert!(close(num(&s, "cpu"), 50.0));
    assert!(close(num(&s, "ram"), 25.0));
    assert!(!s.values.contains_key("storage_max"));
}

#[tokio::test]
async fn an_offline_node_is_bad_and_a_need_with_the_count() {
    let doc = changed("node/pve2", |n| *n = offline("pve2"));
    let (url, _) = serve(Answer::Json(doc)).await;
    let s = fetch(&url).await;

    assert_eq!(s.status, Status::Bad);
    assert_eq!(num(&s, "nodes_online"), Some(1.0));
    assert_eq!(num(&s, "nodes_total"), Some(2.0));
    // The offline node has no figures: the means are the online node's.
    assert!(close(num(&s, "cpu"), 10.0));
    assert!(close(num(&s, "ram"), 25.0));
    assert_eq!(s.needs.len(), 1);
    let need = &s.needs[0];
    assert_eq!(need.key, "pve:node_down");
    assert_eq!(need.level, Level::Bad);
    assert_eq!(need.label_key, "need.pve_node_down");
    assert_eq!(need.score, 1.0);
    assert_eq!(need.params.len(), 1);
    assert_eq!(need.params["count"], "1");
    // No node name anywhere in the need.
    assert!(!format!("{need:?}").contains("pve2"));
}

#[tokio::test]
async fn full_storage_warns_at_85_and_is_bad_at_95() {
    // (percent used of `local` on pve1, status, need level)
    for (used, status, level) in [
        (84, Status::Ok, None),
        (85, Status::Warn, Some(Level::Warn)),
        (94, Status::Warn, Some(Level::Warn)),
        (95, Status::Bad, Some(Level::Bad)),
        (100, Status::Bad, Some(Level::Bad)),
    ] {
        // The disk is 100 GiB, so a GiB is a percent.
        let doc = changed("storage/pve1/local", |s| s["disk"] = json!(used * GIB));
        let (url, _) = serve(Answer::Json(doc)).await;
        let s = fetch(&url).await;
        assert_eq!(s.status, status, "{used} %");
        match level {
            None => assert!(s.needs.is_empty(), "{used} %"),
            Some(level) => {
                assert_eq!(s.needs.len(), 1, "{used} %");
                let need = &s.needs[0];
                assert_eq!(need.key, "pve:storage");
                assert_eq!(need.label_key, "need.pve_storage");
                assert_eq!(need.level, level, "{used} %");
                assert_eq!(need.params["count"], used.to_string());
                assert!(close(Some(need.score), used as f64), "{used} %");
                assert!(close(num(&s, "storage_max"), used as f64));
            }
        }
    }
}

#[tokio::test]
async fn a_storage_just_under_a_threshold_is_not_over_it() {
    // 94.99 % is a warning that says 94, not a bad one.
    let doc = changed("storage/pve1/local", |s| {
        s["disk"] = json!(9499 * GIB / 100)
    });
    let (url, _) = serve(Answer::Json(doc)).await;
    let s = fetch(&url).await;
    assert_eq!(s.needs[0].level, Level::Warn);
    assert_eq!(s.needs[0].params["count"], "94");
}

#[tokio::test]
async fn full_storage_and_a_down_node_give_both_needs_and_a_bad_status() {
    let mut doc = changed("storage/pve1/local", |s| s["disk"] = json!(97 * GIB));
    doc["data"][1] = offline("pve2");
    let (url, _) = serve(Answer::Json(doc)).await;
    let s = fetch(&url).await;
    assert_eq!(s.status, Status::Bad);
    let keys: Vec<_> = s.needs.iter().map(|n| n.key.as_str()).collect();
    assert_eq!(keys, ["pve:node_down", "pve:storage"]);
}

#[tokio::test]
async fn disabled_and_unknown_storage_is_skipped() {
    // A full storage that is not available says nothing about space.
    let doc = json!({ "data": [
        { "type": "node", "node": "pve", "status": "online" },
        { "type": "storage", "storage": "usb", "status": "unknown", "disk": 99, "maxdisk": 100 },
        { "type": "storage", "storage": "off", "status": "disabled", "disk": 99, "maxdisk": 100 },
        { "type": "storage", "storage": "none", "status": "available", "disk": 0, "maxdisk": 0 },
        { "type": "storage", "storage": "ok", "status": "available", "disk": 10, "maxdisk": 100 },
    ]});
    let (url, _) = serve(Answer::Json(doc)).await;
    let s = fetch(&url).await;
    assert_eq!(s.status, Status::Ok);
    assert!(close(num(&s, "storage_max"), 10.0));
}

#[tokio::test]
async fn templates_are_not_counted_as_guests() {
    let doc = json!({ "data": [
        { "type": "node", "node": "pve", "status": "online" },
        { "type": "qemu", "vmid": 1, "status": "running", "template": 1 },
        { "type": "qemu", "vmid": 2, "status": "stopped", "template": true },
        { "type": "lxc", "vmid": 3, "status": "running", "template": 0 },
    ]});
    let (url, _) = serve(Answer::Json(doc)).await;
    let s = fetch(&url).await;
    assert_eq!(num(&s, "guests_total"), Some(1.0));
    assert_eq!(num(&s, "guests_running"), Some(1.0));
}

#[tokio::test]
async fn odd_fields_only_drop_their_own_metric() {
    // Both nodes' cpu odd: no cpu at all, the rest stays.
    let mut doc = cluster();
    for i in 0..2 {
        doc["data"][i]["cpu"] = json!(["a"]);
    }
    let (url, _) = serve(Answer::Json(doc)).await;
    let s = fetch(&url).await;
    assert!(!s.values.contains_key("cpu"));
    assert_eq!(num(&s, "nodes_online"), Some(2.0));
    assert!(close(num(&s, "ram"), 50.0));
    assert_eq!(num(&s, "guests_total"), Some(5.0));

    // No memory figures on either node: no ram, the rest stays.
    let mut doc = cluster();
    for i in 0..2 {
        doc["data"][i]["maxmem"] = json!(null);
    }
    let (url, _) = serve(Answer::Json(doc)).await;
    let s = fetch(&url).await;
    assert!(!s.values.contains_key("ram"));
    assert!(close(num(&s, "cpu"), 20.0));
    assert!(close(num(&s, "storage_max"), 50.0));

    // Storage with nonsense sizes: no storage figure, no need, the rest stays.
    let mut doc = cluster();
    for item in doc["data"].as_array_mut().unwrap() {
        if item["type"] == "storage" {
            item["maxdisk"] = json!("big");
        }
    }
    let (url, _) = serve(Answer::Json(doc)).await;
    let s = fetch(&url).await;
    assert!(!s.values.contains_key("storage_max"));
    assert!(s.needs.is_empty());
    assert_eq!(s.status, Status::Ok);
    assert!(close(num(&s, "ram"), 50.0));

    // A guest with an odd status still counts (as not running), and a node
    // whose name is not text still counts as a node.
    let mut doc = cluster();
    doc["data"][2]["status"] = json!(7);
    doc["data"][0]["node"] = json!(5);
    let (url, _) = serve(Answer::Json(doc)).await;
    let s = fetch(&url).await;
    assert_eq!(num(&s, "guests_total"), Some(5.0));
    assert_eq!(num(&s, "guests_running"), Some(2.0));
    assert_eq!(num(&s, "nodes_total"), Some(2.0));

    // Entries that are not objects are skipped.
    let mut doc = cluster();
    doc["data"]
        .as_array_mut()
        .unwrap()
        .extend([json!(1), json!("x"), json!(null), json!([])]);
    let (url, _) = serve(Answer::Json(doc)).await;
    assert_eq!(fetch(&url).await.status, Status::Ok);
}

#[tokio::test]
async fn percentages_are_kept_between_0_and_100() {
    let doc = changed("storage/pve1/local", |s| s["disk"] = json!(500 * GIB));
    let (url, _) = serve(Answer::Json(doc)).await;
    let s = fetch(&url).await;
    assert_eq!(num(&s, "storage_max"), Some(100.0));
}

// ---------------------------------------------------------------------------
// Answers that are no good

#[tokio::test]
async fn a_refused_token_gives_no_reading_and_the_auth_text() {
    for code in [401, 403] {
        let (url, _) = serve(Answer::Status(code)).await;
        let err = fetch_err(&url).await;
        assert_eq!(err, FetchError::Auth, "{code}");
        let text = err.to_string();
        assert_eq!(text, "key refused");
        assert_eq!(integrations::error_text_key(&text), Some("fetch_err.auth"));
        assert!(!text.contains(SECRET) && !text.contains(TOKEN_ID));
    }
}

#[tokio::test]
async fn malformed_answers_give_no_reading_and_do_not_panic() {
    for raw in [
        "",
        "not json",
        "{",
        "[]",
        "null",
        "{}",
        r#"{"data": null}"#,
        r#"{"data": 5}"#,
        r#"{"data": {}}"#,
        "\u{0}\u{1}\u{2}",
    ] {
        let (url, _) = serve(Answer::Raw(raw)).await;
        assert_eq!(fetch_err(&url).await, FetchError::Invalid, "{raw:?}");
    }
}

#[tokio::test]
async fn a_token_that_sees_nothing_has_its_own_error_and_hint() {
    // A token without a role gets an empty list; one that sees guests but no
    // node is as good as that.
    for raw in [
        r#"{"data": []}"#,
        r#"{"data": [{"type": "qemu", "status": "running"}]}"#,
        r#"{"data": [5, null, "x"]}"#,
    ] {
        let (url, _) = serve(Answer::Raw(raw)).await;
        let err = fetch_err(&url).await;
        assert_eq!(err, FetchError::NothingVisible, "{raw:?}");
        assert_eq!(err.to_string(), "key sees nothing");
        assert_eq!(
            integrations::error_text_key(&err.to_string()),
            Some("fetch_err.nothing_visible")
        );
    }
    // A `data` that is not a list stays "unexpected answer".
    for raw in [
        r#"{"data": {}}"#,
        r#"{"data": null}"#,
        r#"{"data": 5}"#,
        "{}",
    ] {
        let (url, _) = serve(Answer::Raw(raw)).await;
        assert_eq!(fetch_err(&url).await, FetchError::Invalid, "{raw:?}");
    }
    let (en, nb) = (locale::LOCALES[0].1, locale::LOCALES[1].1);
    assert_eq!(
        text_of(nb, "fetch_err", "nothing_visible").as_deref(),
        Some("Nøkkelen ser ingenting – sjekk rollen")
    );
    assert!(
        text_of(en, "fetch_err", "nothing_visible")
            .unwrap()
            .contains("check its role")
    );
}

#[tokio::test]
async fn server_errors_a_missing_endpoint_and_a_refused_connection_give_no_reading() {
    let (url, _) = serve(Answer::Status(500)).await;
    assert_eq!(fetch_err(&url).await, FetchError::Status(500));
    let (url, _) = serve(Answer::Status(404)).await;
    assert_eq!(fetch_err(&url).await, FetchError::Status(404));

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
    let (url, _) = serve(Answer::Json(
        json!({ "data": [{ "type": "node", "node": "a" }], "pad": "x".repeat(2 * 1024 * 1024) }),
    ))
    .await;
    assert_eq!(fetch_err(&url).await, FetchError::TooLarge);
}

#[tokio::test]
async fn a_server_that_never_answers_times_out() {
    let (url, _) = serve(Answer::Hang).await;
    let mut c = ctx(&url);
    c.http = integrations::build_client(Duration::from_millis(400));
    assert_eq!(kind().fetch(&c).await.unwrap_err(), FetchError::Timeout);
}

#[tokio::test]
async fn settings_it_cannot_use_are_errors_that_name_no_value() {
    let (url, stub) = serve(Answer::Json(cluster())).await;
    // No secret, a secret that cannot go into a header.
    for secret in [None, Some(""), Some("bad\nsecret")] {
        let err = kind()
            .fetch(&ctx_with(&url, Some(TOKEN_ID), secret))
            .await
            .unwrap_err();
        assert_eq!(err, FetchError::Config, "{secret:?}");
    }
    // No token id, or one that is not user@realm!tokenid, or one that would
    // end the header line.
    for id in [
        None,
        Some(""),
        Some("magpie"),
        Some("magpie@pve"),
        Some("magpie@pve!a b"),
        Some("magpie@pve!x\r\nX-Evil: 1"),
        Some("magpie@pve!x=y"),
    ] {
        let err = kind()
            .fetch(&ctx_with(&url, id, Some(SECRET)))
            .await
            .unwrap_err();
        assert_eq!(err, FetchError::Config, "{id:?}");
    }
    // A token id that is not text.
    let mut c = ctx(&url);
    c.settings["token_id"] = json!(5);
    assert_eq!(kind().fetch(&c).await.unwrap_err(), FetchError::Config);
    // Addresses that are no good.
    for bad in [
        "",
        "not a url",
        "ftp://example.com/",
        "http://user:pw@127.0.0.1:1/",
    ] {
        let err = kind()
            .fetch(&ctx_with(bad, Some(TOKEN_ID), Some(SECRET)))
            .await
            .unwrap_err();
        assert_eq!(err, FetchError::Config, "{bad:?}");
        assert!(!err.to_string().contains("pw"));
    }
    let err = kind()
        .fetch(&ctx_with(
            "http://169.254.169.254/",
            Some(TOKEN_ID),
            Some(SECRET),
        ))
        .await
        .unwrap_err();
    assert_eq!(err, FetchError::Forbidden);
    // Nothing of this was sent.
    assert!(stub.seen().is_empty());
}

#[tokio::test]
async fn a_path_in_the_address_is_kept() {
    // Proxmox behind a proxy prefix.
    let (url, stub) = serve(Answer::Status(404)).await;
    stub.route("/pve/api2/json/cluster/resources", Answer::Status(404));
    let _ = fetch_err(&format!("{url}/pve/")).await;
    let paths: Vec<_> = stub.seen().iter().map(|h| h.path.clone()).collect();
    assert_eq!(paths, ["/pve/api2/json/cluster/resources"]);
}

// ---------------------------------------------------------------------------
// The node rows

#[test]
fn node_rows_come_from_the_same_answer() {
    let rows = node_rows(&cluster());
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].name, "pve1");
    assert!(rows[0].online);
    assert!(close(rows[0].cpu, 10.0));
    assert!(close(rows[0].ram, 25.0));
    assert!(close(rows[0].disk, 20.0));
    assert_eq!(rows[1].name, "pve2");
    assert!(close(rows[1].cpu, 30.0));
    assert!(close(rows[1].ram, 75.0));
    assert!(close(rows[1].disk, 60.0));

    let doc = changed("node/pve2", |n| *n = offline("pve2"));
    assert_eq!(
        node_rows(&doc)[1],
        NodeRow {
            name: "pve2".into(),
            online: false,
            cpu: None,
            ram: None,
            disk: None
        }
    );
    assert!(node_rows(&json!("nope")).is_empty());
}

#[tokio::test]
async fn the_rows_are_not_in_the_snapshot() {
    let (url, _) = serve(Answer::Json(cluster())).await;
    let s = fetch(&url).await;
    let all = format!("{s:?}{}", serde_json::to_string(&s).unwrap());
    assert!(!all.contains("pve1") && !all.contains("pve2"), "{all}");
    assert!(s.needs.iter().all(|n| n.key.starts_with("pve:")));
}

// ---------------------------------------------------------------------------
// The secret

#[tokio::test]
async fn the_token_secret_is_in_the_request_only() {
    let _ = log::buffer();
    let mut snapshots = Vec::new();
    let mut errors = Vec::new();

    let cases = [
        Answer::Json(cluster()),
        Answer::Json(changed("node/pve2", |n| *n = offline("pve2"))),
        Answer::Status(401),
        Answer::Status(403),
        Answer::Raw("garbage"),
        Answer::Status(500),
    ];
    let total = cases.len() + 1;
    for resources in cases {
        let (url, _) = serve(resources).await;
        let c = ctx(&url);
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
    assert!(!all.contains(SECRET), "a snapshot holds the secret");
    assert!(
        !all.contains("PVEAPIToken"),
        "a snapshot mentions the header"
    );

    let log = log::logged();
    assert!(
        log::client_lines_seen(&log),
        "the capture sees the HTTP client's own lines"
    );
    assert!(!log.contains(SECRET), "the log holds the secret");
    assert!(!log.contains("PVEAPIToken"), "the log mentions the header");
}

#[tokio::test]
async fn a_redirect_to_another_origin_never_carries_the_header() {
    // `Authorization` is the header the client drops itself, but Magpie
    // refuses the redirect instead, so the other origin sees nothing at all.
    for redirect in stub::redirects() {
        let (target, target_stub) = serve(Answer::Json(cluster())).await;
        let origin = stub::redirector(redirect, format!("{target}{RESOURCES}")).await;
        assert_eq!(fetch_err(&origin).await, FetchError::RedirectScheme);
        assert!(target_stub.seen().is_empty(), "{:?}", target_stub.seen());
    }
}

#[tokio::test]
async fn the_status_is_as_bad_as_the_worst_need() {
    // Storage at 95 or more is a bad need and so a bad card, not a warning.
    for (used, status) in [(84, Status::Ok), (85, Status::Warn), (95, Status::Bad)] {
        let doc = changed("storage/pve1/local", |s| s["disk"] = json!(used * GIB));
        let (url, _) = serve(Answer::Json(doc)).await;
        let s = fetch(&url).await;
        assert_eq!(s.status, status, "{used} %");
        let worst = s.needs.iter().map(|n| n.level).max();
        match worst {
            None => assert_eq!(s.status, Status::Ok),
            Some(Level::Warn) => assert_eq!(s.status, Status::Warn),
            Some(Level::Bad) => assert_eq!(s.status, Status::Bad),
        }
    }
}

#[tokio::test]
async fn cpu_and_memory_come_from_the_nodes_that_are_online() {
    // An offline node that still carries figures (the last it had) does not
    // pull the means: only pve1 counts.
    let doc = changed("node/pve2", |n| {
        n["status"] = json!("offline");
        n["cpu"] = json!(0.90);
        n["mem"] = json!(64 * GIB);
        n["maxmem"] = json!(64 * GIB);
    });
    let (url, _) = serve(Answer::Json(doc)).await;
    let s = fetch(&url).await;
    assert!(close(num(&s, "cpu"), 10.0), "{:?}", num(&s, "cpu"));
    assert!(close(num(&s, "ram"), 25.0), "{:?}", num(&s, "ram"));
    // All nodes offline: no figure rather than a stale one.
    let mut doc = cluster();
    for i in 0..2 {
        doc["data"][i]["status"] = json!("offline");
    }
    let (url, _) = serve(Answer::Json(doc)).await;
    let s = fetch(&url).await;
    assert!(!s.values.contains_key("cpu"));
    assert!(!s.values.contains_key("ram"));
    assert_eq!(s.status, Status::Bad);
}

#[test]
fn the_texts_read_like_the_other_app_kinds() {
    let (en, nb) = (locale::LOCALES[0].1, locale::LOCALES[1].1);
    assert_eq!(
        text_of(en, "metric", "pve_nodes_total").as_deref(),
        Some("Nodes total")
    );
    assert_eq!(
        text_of(en, "metric", "pve_guests_total").as_deref(),
        Some("Guests total")
    );
    assert_eq!(
        text_of(nb, "metric", "pve_nodes_total").as_deref(),
        Some("Noder i alt")
    );
    // The token id's error says that only ASCII names are accepted.
    assert!(
        text_of(en, "err", "pve_token_id")
            .unwrap()
            .contains("ASCII")
    );
    assert!(
        text_of(nb, "err", "pve_token_id")
            .unwrap()
            .contains("ASCII")
    );
}
