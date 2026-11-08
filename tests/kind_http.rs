//! The details of the `http` kind («Nettside») against a stub site: the facts
//! of the last check and the range of the last hour, which is kept in memory per
//! stored service until history is kept in the database.

use mindful_magpie::integrations::block::{self, Block, SparkStyle};
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
async fn one_answer_makes_no_range_and_the_card_numbers_are_not_repeated() {
    let (base, site) = serve().await;
    site.route("/", Answer::Status(200));
    let snapshot = check(&format!("{base}/"), Some(1)).await;
    assert_eq!(snapshot.values["status"].num, Some(200.0));
    // Only the curve of the response time, which the page draws from its
    // own history.
    assert_eq!(stored(snapshot), vec![latency_curve()]);
}

/// The block of the response time's curve.
fn latency_curve() -> Block {
    Block::Spark {
        title_key: "http_latency_24h".into(),
        metric: "latency_ms".into(),
        style: SparkStyle::Line,
        daily: false,
    }
}

#[tokio::test]
async fn a_bad_status_still_has_a_window_and_a_silent_site_does_not_break_it() {
    let (base, site) = serve().await;
    site.route("/", Answer::Status(503));
    let url = format!("{base}/");
    check(&url, Some(21)).await;
    let snapshot = check(&url, Some(21)).await;
    assert_eq!(snapshot.status, Status::Bad);
    let blocks = stored(snapshot);
    assert_eq!(facts(&blocks, "http_last_hour").unwrap()[1].1, "2");
    // The curve comes first: it is the card's picture.
    assert_eq!(blocks[0], latency_curve());

    // A site that does not answer has no window yet: only the curve of what
    // it answered before.
    let snapshot = check("http://127.0.0.1:1/", Some(22)).await;
    assert_eq!(snapshot.status, Status::Bad);
    assert_eq!(stored(snapshot), vec![latency_curve()]);
}
