//! The layout model: the default, versioned saves, one layout per user,
//! cascade on user delete, validation and the strict document shape.

mod common;

use common::{App, add_user};
use mindful_magpie::layout::{
    self, Background, Bookmark, Card, FieldError, Group, Icon, Layout, SaveError, validate,
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
    for ok in ["🙂", "👨‍👩‍👧‍👦", "🇳🇴", "1️⃣", "👍🏽"] {
        let mut l = sample();
        l.groups[0].cards[0].icon.emoji = Some(ok.into());
        assert_eq!(codes(&l), vec![], "{ok}");
    }
    for bad in [
        "🙂🙂",
        "ab",
        "",
        " 🙂",
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
    // Until Task 5 checks uploaded images against the database, validate
    // refuses every image background.
    expect(
        |l| l.background = Background::Image { id: "img1".into() },
        "background.id",
        "bad_image",
    );
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
