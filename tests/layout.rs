//! The layout model: the default, versioned saves, one layout per user,
//! cascade on user delete, validation and the strict document shape.

mod common;

use common::{App, add_user};
use mindful_magpie::layout::{
    self, Background, Bookmark, Card, FieldError, Group, Icon, Layout, SaveError, ServiceRef, Size,
    validate,
};

fn card(id: &str) -> Card {
    Card {
        id: id.into(),
        title: "Vigilant Vole".into(),
        description: "Uptime".into(),
        url: "https://vole.example.com/".into(),
        icon: Icon {
            emoji: None,
            colour: "teal".into(),
        },
        service: None,
        size: None,
    }
}

/// A valid layout with one group, one card and one bookmark.
fn sample() -> Layout {
    Layout {
        version: 0,
        background: Background::Preset { name: "hav".into() },
        groups: vec![Group {
            id: "g1".into(),
            title: "Services".into(),
            cards: vec![card("c1")],
        }],
        bookmarks: vec![Bookmark {
            id: "b1".into(),
            title: "Docs".into(),
            url: "https://docs.example.com/".into(),
        }],
    }
}

#[tokio::test]
async fn default_layout_for_a_new_user() {
    let app = App::new().await;
    let ada = add_user(&app, "ada", "admin").await;
    let l = layout::load(&app.magpie.core, ada).await.unwrap();
    assert_eq!(l.version, 0);
    assert!(l.groups.is_empty());
    assert!(l.bookmarks.is_empty());
    assert_eq!(
        l.background,
        Background::Preset {
            name: "hedgerow".into()
        }
    );
}

#[tokio::test]
async fn save_then_load_round_trips() {
    let app = App::new().await;
    let ada = add_user(&app, "ada", "admin").await;
    let v = layout::save(&app.magpie.core, ada, &sample())
        .await
        .unwrap();
    let loaded = layout::load(&app.magpie.core, ada).await.unwrap();
    assert_eq!(
        loaded,
        Layout {
            version: v,
            ..sample()
        }
    );
}

#[tokio::test]
async fn save_bumps_the_version() {
    let app = App::new().await;
    let ada = add_user(&app, "ada", "admin").await;
    let v1 = layout::save(&app.magpie.core, ada, &sample())
        .await
        .unwrap();
    assert_eq!(v1, 1);
    let v2 = layout::save(
        &app.magpie.core,
        ada,
        &Layout {
            version: v1,
            ..sample()
        },
    )
    .await
    .unwrap();
    assert_eq!(v2, 2);
    assert_eq!(
        layout::load(&app.magpie.core, ada).await.unwrap().version,
        2
    );
}

#[tokio::test]
async fn stale_version_is_a_conflict() {
    let app = App::new().await;
    let ada = add_user(&app, "ada", "admin").await;
    layout::save(&app.magpie.core, ada, &sample())
        .await
        .unwrap();
    let before = layout::load(&app.magpie.core, ada).await.unwrap();

    let mut other = sample();
    other.groups[0].title = "Overwrite".into();
    let err = layout::save(&app.magpie.core, ada, &other)
        .await
        .unwrap_err();
    assert!(matches!(err, SaveError::Conflict { current: 1 }), "{err:?}");
    assert_eq!(layout::load(&app.magpie.core, ada).await.unwrap(), before);
}

#[tokio::test]
async fn two_saves_at_once_with_the_same_version_do_not_both_win() {
    let app = App::new().await;
    let ada = add_user(&app, "ada", "admin").await;
    let (a, b) = (sample(), sample());
    let (ra, rb) = tokio::join!(
        layout::save(&app.magpie.core, ada, &a),
        layout::save(&app.magpie.core, ada, &b)
    );
    let wins = [&ra, &rb].iter().filter(|r| r.is_ok()).count();
    assert_eq!(wins, 1, "{ra:?} {rb:?}");
    // The loser is told the version that won.
    let loser = if ra.is_ok() { &rb } else { &ra };
    assert!(
        matches!(loser, Err(SaveError::Conflict { current: 1 })),
        "{loser:?}"
    );
    assert_eq!(
        layout::load(&app.magpie.core, ada).await.unwrap().version,
        1
    );
}

#[tokio::test]
async fn layouts_are_per_user() {
    let app = App::new().await;
    let ada = add_user(&app, "ada", "admin").await;
    let bo = add_user(&app, "bo", "editor").await;
    layout::save(&app.magpie.core, ada, &sample())
        .await
        .unwrap();

    let b = layout::load(&app.magpie.core, bo).await.unwrap();
    assert_eq!(b, Layout::default());
    // Bo starts at version 0 whatever Ada did, and saves do not cross.
    let mut bs = sample();
    bs.groups[0].title = "Bo's".into();
    layout::save(&app.magpie.core, bo, &bs).await.unwrap();
    let a = layout::load(&app.magpie.core, ada).await.unwrap();
    assert_eq!(a.groups[0].title, "Services");
    assert_eq!(a.version, 1);
}

#[tokio::test]
async fn deleting_a_user_deletes_their_layout() {
    let app = App::new().await;
    let ada = add_user(&app, "ada", "admin").await;
    let bo = add_user(&app, "bo", "editor").await;
    layout::save(&app.magpie.core, ada, &sample())
        .await
        .unwrap();
    layout::save(&app.magpie.core, bo, &sample()).await.unwrap();
    assert_eq!(app.count("SELECT COUNT(*) FROM layouts").await, 2);

    app.exec(format!("DELETE FROM users WHERE id = {bo}")).await;
    assert_eq!(app.count("SELECT COUNT(*) FROM layouts").await, 1);
    assert_eq!(
        layout::load(&app.magpie.core, bo).await.unwrap(),
        Layout::default()
    );
}

#[tokio::test]
async fn an_invalid_layout_is_not_stored() {
    let app = App::new().await;
    let ada = add_user(&app, "ada", "admin").await;
    let mut bad = sample();
    bad.bookmarks[0].url = "javascript:alert(1)".into();
    let err = layout::save(&app.magpie.core, ada, &bad).await.unwrap_err();
    assert!(matches!(err, SaveError::Invalid(_)), "{err:?}");
    assert_eq!(app.count("SELECT COUNT(*) FROM layouts").await, 0);
}

fn codes(l: &Layout) -> Vec<(String, &'static str)> {
    match validate(l) {
        Ok(()) => Vec::new(),
        Err(errors) => errors
            .into_iter()
            .map(|FieldError { path, code }| (path, code))
            .collect(),
    }
}

/// Runs `change` on a valid layout and expects exactly one error.
fn one_error(change: impl FnOnce(&mut Layout)) -> (String, &'static str) {
    let mut l = sample();
    change(&mut l);
    let mut errors = codes(&l);
    assert_eq!(errors.len(), 1, "{errors:?}");
    errors.remove(0)
}

fn expect(change: impl FnOnce(&mut Layout), path: &str, code: &'static str) {
    assert_eq!(one_error(change), (path.to_string(), code));
}

#[test]
fn a_valid_layout_passes() {
    assert_eq!(codes(&sample()), vec![]);
    assert_eq!(codes(&Layout::default()), vec![]);
    for name in ["hedgerow", "natt", "skumring", "hav"] {
        let mut l = sample();
        l.background = Background::Preset { name: name.into() };
        assert_eq!(codes(&l), vec![], "{name}");
    }
}

#[test]
fn validation_refuses_links_that_are_not_http() {
    for url in [
        "javascript:alert(1)",
        "data:text/html,x",
        "ftp://x",
        "JavaScript:alert(1)",
    ] {
        expect(
            |l| l.groups[0].cards[0].url = url.into(),
            "groups[0].cards[0].url",
            "bad_scheme",
        );
        expect(
            |l| l.bookmarks[0].url = url.into(),
            "bookmarks[0].url",
            "bad_scheme",
        );
    }
}

#[test]
fn validation_refuses_malformed_links() {
    for url in [
        "example.com",
        "http://",
        "https:///path",
        "https:example.com",
        "https:\\example.com",
        "https://\\example.com",
        // The `://` is not right after the scheme: a later one in the query
        // must not pass for it.
        "https:example.com/?q=://",
        "https:/example.com/?q=://",
        "https:///example.com/?q=://",
        "http://exa mple.com",
        " https://example.com",
        "https://example.com\n",
        "https://exam\tple.com",
        "/relative",
    ] {
        expect(
            |l| l.groups[0].cards[0].url = url.into(),
            "groups[0].cards[0].url",
            "bad_url",
        );
    }
    expect(
        |l| l.bookmarks[0].url = "".into(),
        "bookmarks[0].url",
        "required",
    );
    expect(
        |l| l.bookmarks[0].url = format!("https://example.com/{}", "a".repeat(2048)),
        "bookmarks[0].url",
        "too_long",
    );
}

#[test]
fn validation_table() {
    // Text lengths: 80 is fine, 81 is too long.
    expect(
        |l| l.groups[0].cards[0].title = "x".repeat(81),
        "groups[0].cards[0].title",
        "too_long",
    );
    assert_eq!(
        codes(&{
            let mut l = sample();
            l.groups[0].cards[0].title = "x".repeat(80);
            l
        }),
        vec![]
    );
    expect(
        |l| l.groups[0].cards[0].description = "x".repeat(161),
        "groups[0].cards[0].description",
        "too_long",
    );
    expect(
        |l| l.groups[0].title = "x".repeat(61),
        "groups[0].title",
        "too_long",
    );
    expect(
        |l| l.bookmarks[0].title = "x".repeat(81),
        "bookmarks[0].title",
        "too_long",
    );
    // Characters, not bytes.
    assert_eq!(
        codes(&{
            let mut l = sample();
            l.groups[0].cards[0].title = "å".repeat(80);
            l
        }),
        vec![]
    );
    // Required fields.
    expect(
        |l| l.groups[0].title = "  ".into(),
        "groups[0].title",
        "required",
    );
    expect(
        |l| l.groups[0].cards[0].title = String::new(),
        "groups[0].cards[0].title",
        "required",
    );
    expect(
        |l| l.bookmarks[0].title = String::new(),
        "bookmarks[0].title",
        "required",
    );
    // Too many of each.
    expect(
        |l| {
            l.groups = (0..31)
                .map(|i| Group {
                    id: format!("g{i}"),
                    title: "G".into(),
                    cards: vec![],
                })
                .collect();
        },
        "groups",
        "too_many",
    );
    expect(
        |l| {
            l.groups = (0..30)
                .map(|g| Group {
                    id: format!("g{g}"),
                    title: "G".into(),
                    cards: (0..11).map(|c| card(&format!("c{g}-{c}"))).collect(),
                })
                .collect();
        },
        "cards",
        "too_many",
    );
    expect(
        |l| {
            l.bookmarks = (0..101)
                .map(|i| Bookmark {
                    id: format!("b{i}"),
                    title: "B".into(),
                    url: "https://example.com/".into(),
                })
                .collect();
        },
        "bookmarks",
        "too_many",
    );
    // The limits themselves pass.
    assert_eq!(
        codes(&{
            let mut l = sample();
            l.groups = (0..30)
                .map(|g| Group {
                    id: format!("g{g}"),
                    title: "G".into(),
                    cards: (0..10).map(|c| card(&format!("c{g}-{c}"))).collect(),
                })
                .collect();
            l.bookmarks = (0..100)
                .map(|i| Bookmark {
                    id: format!("b{i}"),
                    title: "B".into(),
                    url: "https://example.com/".into(),
                })
                .collect();
            l
        }),
        vec![]
    );
}

#[test]
fn validation_ids() {
    // The same id twice, anywhere in the document.
    expect(
        |l| l.groups[0].cards[0].id = "g1".into(),
        "groups[0].cards[0].id",
        "duplicate_id",
    );
    expect(
        |l| l.bookmarks[0].id = "c1".into(),
        "bookmarks[0].id",
        "duplicate_id",
    );
    for bad in ["", "a b", "a/b", "<x>", "ä", "a".repeat(41).as_str()] {
        expect(|l| l.groups[0].id = bad.into(), "groups[0].id", "bad_id");
    }
    assert_eq!(
        codes(&{
            let mut l = sample();
            l.groups[0].id = format!("{}-_Z9", "a".repeat(36));
            l
        }),
        vec![]
    );
}

#[test]
fn validation_icons() {
    for colour in ["grey", "Pink", "plum", "", "#ff0000", "var(--pal-red)"] {
        expect(
            |l| l.groups[0].cards[0].icon.colour = colour.into(),
            "groups[0].cards[0].icon.colour",
            "bad_colour",
        );
    }
    for colour in layout::PALETTE {
        let mut l = sample();
        l.groups[0].cards[0].icon.colour = colour.into();
        assert_eq!(codes(&l), vec![], "{colour}");
    }
    assert_eq!(layout::PALETTE.len(), 13);
    // One grapheme is an emoji, however many code points it has.
    // (The last two are the subdivision flags of England and Scotland: the
    // black flag, tag letters and the cancel tag.)
    for ok in [
        "🙂",
        "👨‍👩‍👧‍👦",
        "🇳🇴",
        "1️⃣",
        "👍🏽",
        "\u{1f3f4}\u{e0067}\u{e0062}\u{e0065}\u{e006e}\u{e0067}\u{e007f}",
        "\u{1f3f4}\u{e0067}\u{e0062}\u{e0073}\u{e0063}\u{e0074}\u{e007f}",
    ] {
        let mut l = sample();
        l.groups[0].cards[0].icon.emoji = Some(ok.into());
        assert_eq!(codes(&l), vec![], "{ok}");
    }
    for bad in [
        "🙂🙂",
        "ab",
        "",
        " 🙂",
        // Nothing to see: whitespace, control and format characters.
        " ",
        "\n",
        "\t",
        "\u{a0}",
        "\u{200b}",
        "\u{200d}",
        "\u{202e}",
        "\u{feff}",
        "\u{2060}",
        "\u{7f}",
        // Tag characters hide text: only a whole subdivision flag may have them.
        "A\u{e0067}\u{e007f}",
        "A\u{e0067}\u{e0062}\u{e007f}",
        "\u{e0067}",
        "\u{1f3f4}\u{e0067}\u{e0062}\u{e0065}\u{e006e}\u{e0067}",
        "\u{1f642}\u{e0067}\u{e007f}",
        "\u{1f3f4}\u{e007f}",
        "a\u{300}\u{301}".repeat(2).as_str(),
    ] {
        expect(
            |l| l.groups[0].cards[0].icon.emoji = Some(bad.into()),
            "groups[0].cards[0].icon.emoji",
            "bad_emoji",
        );
    }
    // One grapheme, but thousands of combining marks.
    expect(
        |l| l.groups[0].cards[0].icon.emoji = Some(format!("e{}", "\u{301}".repeat(5000))),
        "groups[0].cards[0].icon.emoji",
        "bad_emoji",
    );
}

#[test]
fn validation_backgrounds() {
    expect(
        |l| {
            l.background = Background::Preset {
                name: "neon".into(),
            }
        },
        "background.name",
        "bad_preset",
    );
    // `validate` never touches the database, so whether an image id is the
    // user's own is the save's business (tests/background.rs). It only
    // refuses an id that cannot be one: 1 to 64 of `[A-Za-z0-9_-]`.
    for id in ["", "has space", "dot.png", "../x", &"x".repeat(65)] {
        expect(
            |l| l.background = Background::Image { id: id.into() },
            "background.id",
            "bad_image",
        );
    }
    for id in [
        "img1",
        "0123456789abcdef0123456789abcdef",
        "a_b-C",
        &"x".repeat(64),
    ] {
        let mut l = sample();
        l.background = Background::Image { id: id.into() };
        assert_eq!(codes(&l), vec![], "{id}");
    }
}

#[test]
fn markup_in_text_is_accepted_because_it_is_only_text() {
    let mut l = sample();
    l.groups[0].title = "<script>alert(1)</script>".into();
    l.groups[0].cards[0].title = "<script>alert(1)</script>".into();
    l.groups[0].cards[0].description = "<img src=x onerror=alert(1)>".into();
    l.bookmarks[0].title = "\u{202e}rtl<b>".into();
    assert_eq!(codes(&l), vec![]);
}

#[test]
fn every_problem_is_reported() {
    let mut l = sample();
    l.groups[0].cards[0].url = "ftp://x".into();
    l.groups[0].cards[0].icon.colour = "plum".into();
    l.bookmarks[0].title = "x".repeat(81);
    assert_eq!(codes(&l).len(), 3);
}

#[test]
fn unknown_fields_are_refused() {
    let good = serde_json::to_value(sample()).unwrap();
    assert!(serde_json::from_value::<Layout>(good.clone()).is_ok());

    let mut top = good.clone();
    top["extra"] = serde_json::json!(1);
    assert!(serde_json::from_value::<Layout>(top).is_err());

    let mut nested = good.clone();
    nested["groups"][0]["cards"][0]["icon"]["size"] = serde_json::json!("big");
    assert!(serde_json::from_value::<Layout>(nested).is_err());

    let mut bg = good.clone();
    bg["background"]["blur"] = serde_json::json!(3);
    assert!(serde_json::from_value::<Layout>(bg).is_err());

    let mut bookmark = good;
    bookmark["bookmarks"][0]["pinned"] = serde_json::json!(true);
    assert!(serde_json::from_value::<Layout>(bookmark).is_err());
}

#[test]
fn the_document_shape_is_the_one_the_client_sends() {
    let l: Layout = serde_json::from_str(
        r#"{"version":3,
            "background":{"kind":"preset","name":"natt"},
            "groups":[{"id":"g","title":"T","cards":[
                {"id":"c","title":"t","description":"","url":"https://a.b/",
                 "icon":{"emoji":null,"colour":"red"}}]}],
            "bookmarks":[]}"#,
    )
    .unwrap();
    assert_eq!(l.version, 3);
    let image: Layout = serde_json::from_str(
        r#"{"version":0,"background":{"kind":"image","id":"abc"},"groups":[],"bookmarks":[]}"#,
    )
    .unwrap();
    assert_eq!(image.background, Background::Image { id: "abc".into() });
    // The wire form of a preset keeps the `kind` tag.
    let v = serde_json::to_value(&l).unwrap();
    assert_eq!(v["background"]["kind"], "preset");
}

#[test]
fn too_many_stops_the_item_checks() {
    // 31 groups, every title blank: only the count is reported, the items
    // are not walked.
    let mut l = sample();
    l.groups = (0..31)
        .map(|i| Group {
            id: format!("g{i}"),
            title: String::new(),
            cards: vec![],
        })
        .collect();
    assert_eq!(codes(&l), vec![("groups".to_string(), "too_many")]);

    // Same for bookmarks.
    let mut l = sample();
    l.bookmarks = (0..101)
        .map(|i| Bookmark {
            id: format!("b{i}"),
            title: String::new(),
            url: "x".into(),
        })
        .collect();
    assert_eq!(codes(&l), vec![("bookmarks".to_string(), "too_many")]);
}

#[test]
fn the_error_list_is_capped_at_100() {
    // 30 groups of 10 cards with a bad link and a bad colour: 600 problems.
    let mut l = sample();
    l.groups = (0..30)
        .map(|g| Group {
            id: format!("g{g}"),
            title: "G".into(),
            cards: (0..10)
                .map(|c| {
                    let mut k = card(&format!("c{g}-{c}"));
                    k.url = "ftp://x".into();
                    k.icon.colour = "plum".into();
                    k
                })
                .collect(),
        })
        .collect();
    let errors = codes(&l);
    assert_eq!(errors.len(), 100);
    // The first ones, in document order.
    assert_eq!(errors[0].0, "groups[0].cards[0].url");
}

#[tokio::test]
async fn an_unreadable_layout_reports_the_version_it_was_read_at() {
    let app = App::new().await;
    let ada = add_user(&app, "ada", "admin").await;
    app.exec(format!(
        "INSERT INTO layouts (user_id, version, doc, updated_at) \
         VALUES ({ada}, 7, '{{\"groups\": 1', '2026-01-01T00:00:00Z')"
    ))
    .await;
    match layout::load(&app.magpie.core, ada).await {
        Err(layout::StoreError::Corrupt { version, .. }) => assert_eq!(version, 7),
        other => panic!("{other:?}"),
    }
    // A save that carries that version replaces the document; one that
    // carries any other (a valid save landed in between) is a conflict.
    let stale = Layout {
        version: 6,
        ..Layout::default()
    };
    let err = layout::save(&app.magpie.core, ada, &stale)
        .await
        .unwrap_err();
    assert!(matches!(err, SaveError::Conflict { current: 7 }), "{err:?}");
    let fresh = Layout {
        version: 7,
        ..Layout::default()
    };
    assert_eq!(
        layout::save(&app.magpie.core, ada, &fresh).await.unwrap(),
        8
    );
    assert_eq!(
        layout::load(&app.magpie.core, ada).await.unwrap().version,
        8
    );
}

// ---------------------------------------------------------------------------
// Cards bound to a service

fn bound(metrics: &[&str]) -> ServiceRef {
    ServiceRef {
        id: 7,
        metrics: metrics.iter().map(|m| (*m).to_string()).collect(),
    }
}

#[test]
fn a_card_with_a_service_round_trips() {
    let mut l = sample();
    l.groups[0].cards[0].service = Some(bound(&["status", "checked"]));
    assert_eq!(codes(&l), vec![]);
    let json = serde_json::to_value(&l).unwrap();
    assert_eq!(
        json["groups"][0]["cards"][0]["service"],
        serde_json::json!({ "id": 7, "metrics": ["status", "checked"] })
    );
    let back: Layout = serde_json::from_value(json).unwrap();
    assert_eq!(back, l);
}

#[test]
fn an_old_card_without_service_loads_and_is_written_without_it() {
    let old = serde_json::to_value(sample()).unwrap();
    // The document of a layout stored before services existed.
    assert!(old["groups"][0]["cards"][0].get("service").is_none());
    let l: Layout = serde_json::from_value(old.clone()).unwrap();
    assert_eq!(l.groups[0].cards[0].service, None);
    assert_eq!(serde_json::to_value(&l).unwrap(), old);
    // `null` reads as none, and is not written back.
    let mut with_null = old.clone();
    with_null["groups"][0]["cards"][0]["service"] = serde_json::Value::Null;
    let l: Layout = serde_json::from_value(with_null).unwrap();
    assert_eq!(serde_json::to_value(&l).unwrap(), old);
}

#[test]
fn a_service_binding_refuses_unknown_fields() {
    let mut v = serde_json::to_value(sample()).unwrap();
    v["groups"][0]["cards"][0]["service"] =
        serde_json::json!({ "id": 1, "metrics": ["a"], "name": "x" });
    assert!(serde_json::from_value::<Layout>(v).is_err());
}

#[test]
fn service_binding_validation_table() {
    let path = "groups[0].cards[0].service.metrics";
    let with =
        |service: ServiceRef| move |l: &mut Layout| l.groups[0].cards[0].service = Some(service);
    expect(with(bound(&[])), path, "required");
    // More than any kind has: refused by its length.
    let over: Vec<String> = (0..=layout::model::MAX_METRICS)
        .map(|i| format!("m{i}"))
        .collect();
    let over: Vec<&str> = over.iter().map(String::as_str).collect();
    expect(with(bound(&over)), path, "too_many");
    expect(with(bound(&["a", "b", "a"])), path, "duplicate_id");
    expect(with(bound(&["has space"])), path, "bad_id");
    expect(with(bound(&[""])), path, "bad_id");
    expect(with(bound(&[&"x".repeat(41)])), path, "bad_id");
    expect(
        with(ServiceRef {
            id: 0,
            metrics: vec!["a".into()],
        }),
        "groups[0].cards[0].service",
        "bad_service",
    );
    expect(
        with(ServiceRef {
            id: -3,
            metrics: vec!["a".into()],
        }),
        "groups[0].cards[0].service",
        "bad_service",
    );
    // One problem at most: a bad id is reported alone, whatever the metrics are.
    expect(
        with(ServiceRef {
            id: 0,
            metrics: vec![],
        }),
        "groups[0].cards[0].service",
        "bad_service",
    );
    // One up to as many as a kind has, in the shape of an id, passes (the
    // save checks the names against the kind's own).
    for n in 1..=12 {
        let metrics: Vec<String> = (0..n).map(|i| format!("m_{i}-x")).collect();
        let mut l = sample();
        l.groups[0].cards[0].service = Some(ServiceRef { id: 1, metrics });
        assert_eq!(codes(&l), vec![], "{n} metrics");
    }
    // A huge list is refused by its length, not walked.
    let many: Vec<String> = (0..50_000).map(|i| format!("m{i}")).collect();
    let mut l = sample();
    l.groups[0].cards[0].service = Some(ServiceRef {
        id: 1,
        metrics: many,
    });
    assert_eq!(codes(&l), vec![(path.to_string(), "too_many")]);
}

#[test]
fn no_kind_has_more_metrics_than_a_card_may_name() {
    for kind in mindful_magpie::integrations::registry() {
        assert!(
            kind.metrics().len() <= layout::model::MAX_METRICS,
            "{} has {} metrics",
            kind.kind(),
            kind.metrics().len()
        );
    }
}

#[test]
fn a_card_may_name_every_metric_of_its_kind_once() {
    for kind in mindful_magpie::integrations::registry() {
        let all: Vec<String> = kind.metrics().iter().map(|m| m.id.to_string()).collect();
        let mut l = sample();
        l.groups[0].cards[0].service = Some(ServiceRef {
            id: 1,
            metrics: all.clone(),
        });
        assert_eq!(codes(&l), vec![], "{}", kind.kind());
        // One twice is refused, even when the count is within the catalogue.
        let mut twice = all;
        twice.push(twice[0].clone());
        l.groups[0].cards[0].service = Some(ServiceRef {
            id: 1,
            metrics: twice,
        });
        assert_eq!(
            codes(&l),
            vec![(
                "groups[0].cards[0].service.metrics".to_string(),
                "duplicate_id"
            )],
            "{}",
            kind.kind()
        );
    }
}

#[test]
fn a_size_reads_writes_and_is_left_out_when_absent() {
    // An old card: no size, written without one.
    let old = serde_json::to_value(sample()).unwrap();
    assert!(old["groups"][0]["cards"][0].get("size").is_none());
    let l: Layout = serde_json::from_value(old.clone()).unwrap();
    assert_eq!(l.groups[0].cards[0].size, None);
    assert_eq!(serde_json::to_value(&l).unwrap(), old);
    // A bound card with a size round-trips as plain words.
    for (size, word) in [(Size::Wide, "wide"), (Size::Normal, "normal")] {
        let mut l = sample();
        l.groups[0].cards[0].service = Some(ServiceRef {
            id: 7,
            metrics: vec!["status".into()],
        });
        l.groups[0].cards[0].size = Some(size);
        assert_eq!(codes(&l), vec![]);
        let json = serde_json::to_value(&l).unwrap();
        assert_eq!(json["groups"][0]["cards"][0]["size"], word);
        assert_eq!(serde_json::from_value::<Layout>(json).unwrap(), l);
    }
    // Anything else is not a size.
    for bad in [
        serde_json::json!("huge"),
        serde_json::json!(2),
        serde_json::json!("WIDE"),
    ] {
        let mut v = serde_json::to_value(sample()).unwrap();
        v["groups"][0]["cards"][0]["size"] = bad;
        assert!(serde_json::from_value::<Layout>(v).is_err());
    }
    // null reads as absent.
    let mut v = serde_json::to_value(sample()).unwrap();
    v["groups"][0]["cards"][0]["size"] = serde_json::Value::Null;
    let l: Layout = serde_json::from_value(v).unwrap();
    assert_eq!(l.groups[0].cards[0].size, None);
}

#[test]
fn a_size_belongs_to_a_card_with_a_service() {
    let mut l = sample();
    l.groups[0].cards[0].size = Some(Size::Wide);
    assert_eq!(
        codes(&l),
        vec![("groups[0].cards[0].size".to_string(), "bad_size")]
    );
}

#[test]
fn the_kinds_that_have_a_lot_to_show_default_to_wide() {
    let wide: Vec<&str> = mindful_magpie::integrations::registry()
        .iter()
        .filter(|k| k.default_size() == Size::Wide)
        .map(|k| k.kind())
        .collect();
    assert_eq!(wide, ["proxmox", "unraid", "tofa", "github"]);
}
