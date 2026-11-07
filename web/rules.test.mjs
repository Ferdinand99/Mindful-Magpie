// Tests for static/magpie/rules.js: the client checks that mirror the server's.
import assert from "node:assert/strict";
import { test } from "node:test";

import {
  LIMITS,
  SIZES,
  checkBookmark,
  checkCard,
  checkGroup,
  emojiProblem,
  normalizeUrl,
  parsePath,
  serviceProblem,
  sizeProblem,
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
  assert.equal(normalizeUrl(""), "");
});

test("normalizeUrl_leaves_a_scheme_alone_so_it_is_refused_as_the_wrong_scheme", () => {
  for (const value of ["mailto:a@example.com", "javascript:alert(1)", "data:text/html,x", "tel:+4712345678", "file:foo"]) {
    assert.equal(normalizeUrl(value), value, value);
    assert.equal(urlProblem(normalizeUrl(value)), "bad_scheme", value);
  }
});

test("normalizeUrl_still_treats_host_and_port_as_an_address", () => {
  assert.equal(normalizeUrl("example.com:8080"), "https://example.com:8080");
  assert.equal(normalizeUrl("localhost:3000"), "https://localhost:3000");
  assert.equal(normalizeUrl("example.com:8080/path?x=1"), "https://example.com:8080/path?x=1");
  assert.equal(normalizeUrl("[::1]:8080"), "https://[::1]:8080");
  assert.equal(urlProblem(normalizeUrl("localhost:3000")), null);
  assert.equal(urlProblem(normalizeUrl("example.com:8080")), null);
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

// Service bindings mirror `validate` in src/layout/model.rs.
test("a_card_without_a_service_or_with_one_to_four_metrics_passes", () => {
  assert.deepEqual(checkCard({ ...ok, service: null }), {});
  assert.deepEqual(checkCard({ ...ok, service: undefined }), {});
  for (let n = 1; n <= 4; n++) {
    const metrics = Array.from({ length: n }, (_, i) => `m_${i}-x`);
    assert.deepEqual(checkCard({ ...ok, service: { id: 3, metrics } }), {}, `${n}`);
  }
});

test("service_bindings_are_refused_by_the_same_codes_as_the_server", () => {
  const one = (service) => checkCard({ ...ok, service }).service;
  assert.equal(one({ id: 3, metrics: [] }), "required");
  // Any number from one up is fine, each once; a list longer than any kind has is refused by its length.
  assert.equal(one({ id: 3, metrics: ["a", "b", "c", "d", "e", "f", "g", "h"] }), undefined);
  const many = Array.from({ length: LIMITS.serviceMetrics + 1 }, (_, i) => `m${i}`);
  assert.equal(one({ id: 3, metrics: many }), "too_many");
  assert.equal(one({ id: 3, metrics: many.slice(0, LIMITS.serviceMetrics) }), undefined);
  assert.equal(one({ id: 3, metrics: ["a", "b", "a"] }), "duplicate_id");
  assert.equal(one({ id: 3, metrics: ["has space"] }), "bad_id");
  assert.equal(one({ id: 3, metrics: [""] }), "bad_id");
  assert.equal(one({ id: 3, metrics: ["x".repeat(LIMITS.metricId + 1)] }), "bad_id");
  assert.equal(one({ id: 3, metrics: ["x".repeat(LIMITS.metricId)] }), undefined);
  assert.equal(one({ id: 0, metrics: ["a"] }), "bad_service");
  assert.equal(one({ id: 1.5, metrics: ["a"] }), "bad_service");
  assert.equal(one({ id: "3", metrics: ["a"] }), "bad_service");
  assert.equal(one({ id: 3, metrics: "a" }), "required");
  assert.equal(LIMITS.serviceMetrics, 32);
  assert.equal(serviceProblem(null), null);
});

test("a_card_link_may_be_empty_only_when_the_card_has_a_service", () => {
  const base = { title: "T", description: "", emoji: "", colour: "teal" };
  const service = { id: 1, metrics: ["status"] };
  assert.deepEqual(checkCard({ ...base, url: "", service }), {});
  assert.deepEqual(checkCard({ ...base, url: "", service: null }), { url: "required" });
  assert.deepEqual(checkCard({ ...base, url: " ", service }), { url: "required" });
  assert.deepEqual(checkCard({ ...base, url: "ftp://x/", service }), { url: "bad_scheme" });
  assert.deepEqual(checkCard({ ...base, url: "https://ok.example/", service }), {});
});

test("a_size_is_normal_or_wide_and_only_with_a_service", () => {
  const base = { title: "T", description: "", emoji: "", colour: "teal", url: "https://ok.example/" };
  const service = { id: 1, metrics: ["status"] };
  assert.deepEqual(SIZES, ["normal", "wide"]);
  for (const size of ["normal", "wide", null, undefined]) {
    assert.deepEqual(checkCard({ ...base, service, size }), {}, String(size));
    assert.deepEqual(checkCard({ ...base, service: null, size: size ?? null }), size ? { size: "bad_size" } : {}, String(size));
  }
  for (const bad of ["huge", "WIDE", "", 2, {}]) {
    assert.deepEqual(checkCard({ ...base, service, size: bad }), { size: "bad_size" }, String(bad));
  }
  assert.equal(sizeProblem("wide", service), null);
  assert.equal(sizeProblem("wide", null), "bad_size");
});
