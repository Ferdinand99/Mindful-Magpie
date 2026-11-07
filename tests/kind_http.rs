//! The details of the `http` kind («Nettside») against a stub site: the facts
//! of the last check and the range of the last hour, which is kept in memory per
//! stored service until history is kept in the database.

use mindful_magpie::integrations::block::{self, Block};
use mindful_magpie::integrations::{self, FetchCtx, Snapshot, Status};
use serde_json::json;

mod common;
use common::stub::{Answer, serve};

fn ctx(url: &str, service_id: Option<i64>) -> FetchCtx {
    let mut settings = json!({ "url": url });
    if let Some(id) = service_id {
        settings["service_id"] = json!(id);
    }
    FetchCtx {
        http: integrations::client().clone(),
        settings,
        secret: None,
    }
}

async fn check(url: &str, service_id: Option<i64>) -> Snapshot {
    integrations::find("http")
        .expect("the http kind is registered")
        .fetch(&ctx(url, service_id))
        .await
        .expect("a reading, good or bad")
}

/// What the poller stores: the details cut down, and every key known.
fn stored(mut snapshot: Snapshot) -> Vec<Block> {
    // The locale lookup needs the kit's configuration.
    static KIT: std::sync::Once = std::sync::Once::new();
    KIT.call_once(mindful_magpie::install);
    snapshot.bound_details();
    for block in &snapshot.details {
        for key in block.keys() {
            assert!(block::key_known(key), "no text for detail.{key}");
        }
    }
    snapshot.details
}

fn facts<'a>(blocks: &'a [Block], title: &str) -> Option<Vec<(&'a str, &'a str)>> {
    blocks.iter().find_map(|b| match b {
        Block::Facts { title_key, rows } if title_key == title => Some(
            rows.iter()
                .map(|r| (r.label_key.as_deref().unwrap_or_default(), r.value.as_str()))
                .collect(),
        ),
        _ => None,
    })
}

/// The numbers of «min / avg / max ms».
fn range(text: &str) -> [f64; 3] {
    let numbers: Vec<f64> = text
        .strip_suffix(" ms")
        .expect("ends in ms")
        .split(" / ")
        .map(|n| n.parse().expect("a number"))
        .collect();
    numbers.try_into().expect("three numbers")
}

#[tokio::test]
async fn a_check_gives_the_status_the_response_time_and_when() {
    let (base, site) = serve().await;
    site.route("/", Answer::Status(200));
    let blocks = stored(check(&format!("{base}/"), Some(1)).await);
    let rows = facts(&blocks, "http_last_check").expect("the facts");
    let keys: Vec<&str> = rows.iter().map(|r| r.0).collect();
    assert_eq!(keys, ["http_status", "http_latency", "http_checked"]);
    assert_eq!(rows[0].1, "200");
    assert!(rows[1].1.ends_with(" ms"), "{}", rows[1].1);
    // UTC, minute precision.
    assert!(rows[2].1.ends_with(" UTC") && rows[2].1.len() == "2026-11-08 14:03 UTC".len());
    // One answer makes no range.
    assert!(facts(&blocks, "http_last_hour").is_none());
}

#[tokio::test]
async fn the_last_hour_is_kept_per_stored_service_and_forgotten_on_save() {
    let (base, site) = serve().await;
    site.route("/", Answer::Status(200));
    let url = format!("{base}/");

    for _ in 0..3 {
        check(&url, Some(7)).await;
    }
    let blocks = stored(check(&url, Some(7)).await);
    let rows = facts(&blocks, "http_last_hour").expect("the last hour");
    assert_eq!(rows[0].0, "http_latency_range");
    let [min, mean, max] = range(rows[0].1);
    assert!(min <= mean && mean <= max, "{min} {mean} {max}");
    assert_eq!(rows[1], ("http_samples", "4"));

    // Another service at the same address has a window of its own.
    let other = stored(check(&url, Some(8)).await);
    assert!(facts(&other, "http_last_hour").is_none());

    // A test of the form (no number) keeps nothing and shows no range.
    for _ in 0..3 {
        let blocks = stored(check(&url, None).await);
        assert!(facts(&blocks, "http_last_hour").is_none());
    }

    // Saving the service forgets the window.
    integrations::api::forget_all_for(&base);
    let blocks = stored(check(&url, Some(7)).await);
    assert!(facts(&blocks, "http_last_hour").is_none());
    let blocks = stored(check(&url, Some(7)).await);
    assert_eq!(facts(&blocks, "http_last_hour").unwrap()[1].1, "2");
}

#[tokio::test]
async fn a_bad_status_still_has_its_facts_and_a_site_that_does_not_answer_only_the_time() {
    let (base, site) = serve().await;
    site.route("/", Answer::Status(503));
    let snapshot = check(&format!("{base}/"), Some(21)).await;
    assert_eq!(snapshot.status, Status::Bad);
    let blocks = stored(snapshot);
    assert_eq!(
        facts(&blocks, "http_last_check").unwrap()[0],
        ("http_status", "503")
    );

    let down = check("http://127.0.0.1:1/", Some(22)).await;
    assert_eq!(down.status, Status::Bad);
    let blocks = stored(down);
    let rows = facts(&blocks, "http_last_check").expect("the facts");
    assert_eq!(
        rows.iter().map(|r| r.0).collect::<Vec<_>>(),
        ["http_checked"]
    );
    assert!(facts(&blocks, "http_last_hour").is_none());
}
