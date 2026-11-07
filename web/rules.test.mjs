// Tests for static/magpie/rules.js: the client checks that mirror the server's.
import assert from "node:assert/strict";
import { test } from "node:test";

import {
  LIMITS,
  checkBookmark,
  checkCard,
  checkGroup,
  emojiProblem,
  normalizeUrl,
  parsePath,
  urlProblem,
} from "../static/magpie/rules.js";

const ok = { title: "Sonarr", description: "", url: "https://sonarr.example/", emoji: "", colour: "teal" };

test("a_plain_card_passes", () => {
  assert.deepEqual(checkCard(ok), {});
  assert.deepEqual(checkBookmark({ title: "x", url: "http://a.example" }), {});
  assert.deepEqual(checkGroup({ title: "Media" }), {});
});

test("titles_are_required_and_counted_in_characters", () => {
  assert.equal(checkGroup({ title: "   " }).title, "required");
  assert.equal(checkGroup({ title: "a".repeat(60) }).title, undefined);
  assert.equal(checkGroup({ title: "a".repeat(61) }).title, "too_long");
  // 80 emoji are 80 characters but 160 UTF-16 units: they must pass.
  assert.deepEqual(checkCard({ ...ok, title: "\u{1f600}".repeat(LIMITS.title) }), {});
  assert.equal(checkCard({ ...ok, title: "\u{1f600}".repeat(LIMITS.title + 1) }).title, "too_long");
  assert.equal(checkCard({ ...ok, description: "d".repeat(161) }).description, "too_long");
  assert.deepEqual(checkCard({ ...ok, description: "d".repeat(160) }), {});
});

test("links_must_be_plain_http_or_https", () => {
  assert.equal(urlProblem(""), "required");
  assert.equal(urlProblem("javascript:alert(1)"), "bad_scheme");
  assert.equal(urlProblem("data:text/html,x"), "bad_scheme");
  assert.equal(urlProblem("ftp://example.com/"), "bad_scheme");
  assert.equal(urlProblem("https:example.com"), "bad_url");
  assert.equal(urlProblem("https:///example.com"), "bad_url");
  assert.equal(urlProblem("not a url"), "bad_url");
  assert.equal(urlProblem(" https://example.com/"), "bad_url");
  assert.equal(urlProblem("https://example.com/\n"), "bad_url");
  assert.equal(urlProblem("https://example.com/" + "a".repeat(2048)), "too_long");
  assert.equal(urlProblem("https://example.com/a?next=http://x.example"), null);
  assert.equal(urlProblem("http://10.0.0.1:8989/"), null);
});

test("normalizeUrl_adds_https_to_a_bare_address", () => {
  assert.equal(normalizeUrl(" example.com/path "), "https://example.com/path");
  assert.equal(normalizeUrl("localhost:8080"), "https://localhost:8080");
  assert.equal(normalizeUrl("http://a.example/"), "http://a.example/");
  assert.equal(normalizeUrl("javascript://x"), "javascript://x");
  assert.equal(urlProblem(normalizeUrl("javascript:alert(1)")), "bad_url");
  assert.equal(normalizeUrl(""), "");
});

test("emoji_is_one_visible_symbol_or_empty", () => {
  assert.equal(emojiProblem(""), null);
  assert.equal(emojiProblem("\u{1f600}"), null);
  assert.equal(emojiProblem("\u{1f468}‍\u{1f469}‍\u{1f467}"), null);
  assert.equal(emojiProblem("\u{1f1f3}\u{1f1f4}"), null);
  assert.equal(emojiProblem("❤️"), null);
  assert.equal(emojiProblem("ab"), "bad_emoji");
  assert.equal(emojiProblem("\u{1f600}\u{1f600}"), "bad_emoji");
  assert.equal(emojiProblem(" "), "bad_emoji");
  assert.equal(emojiProblem("​"), "bad_emoji");
  assert.equal(emojiProblem("‍"), "bad_emoji");
  assert.equal(emojiProblem("‮"), "bad_emoji");
  assert.equal(emojiProblem("a" + "́".repeat(80)), "bad_emoji");
});

test("an_unknown_colour_is_refused", () => {
  assert.equal(checkCard({ ...ok, colour: "magenta" }).colour, "bad_colour");
});

test("parsePath_reads_server_paths", () => {
  assert.deepEqual(parsePath("groups[1].cards[2].url"), { kind: "card", group: 1, card: 2, field: "url" });
  assert.deepEqual(parsePath("groups[1].cards[0].icon.emoji"), {
    kind: "card",
    group: 1,
    card: 0,
    field: "icon.emoji",
  });
  assert.deepEqual(parsePath("groups[0].title"), { kind: "group", group: 0, field: "title" });
  assert.deepEqual(parsePath("bookmarks[3].url"), { kind: "bookmark", bookmark: 3, field: "url" });
  assert.equal(parsePath("groups").kind, "list");
  assert.equal(parsePath("background.name").kind, "background");
});
