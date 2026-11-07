// Tests for the service numbers on cards (static/magpie/render.js: formatValue, bound cards,
// tickAges), run with `node --test` in web/. Fixture values stand in for the server's snapshots.
import assert from "node:assert/strict";
import { test } from "node:test";
import { parseHTML } from "linkedom";

import { formatValue, render, tickAges } from "../static/magpie/render.js";

const LABELS_EN = {
  empty: "Your start page is empty.",
  invalid_link: "invalid link",
  group_label: "Group: {title}",
  bookmarks_label: "Bookmarks",
  service_missing: "This service no longer exists",
  service_waiting: "Waiting for the first answer",
  status_ok: "Status: OK",
  status_warn: "Status: warning",
  status_bad: "Status: problem",
  status_unknown: "Status: unknown",
  updated: "Last updated {when}",
};
const LABELS_NB = {
  empty: "Startsiden er tom.",
  invalid_link: "ugyldig lenke",
  group_label: "Gruppe: {title}",
  bookmarks_label: "Bokmerker",
  service_missing: "Tjenesten finnes ikke lenger",
  service_waiting: "Venter på første svar",
  status_ok: "Status: OK",
  status_warn: "Status: advarsel",
  status_bad: "Status: feil",
  status_unknown: "Status: ukjent",
  updated: "Sist oppdatert {when}",
};
const EN = { lang: "en", labels: LABELS_EN };
const NB = { lang: "nb", labels: LABELS_NB };

const NOW = "2026-11-07T12:00:00Z";
const NOW_MS = Date.parse(NOW);
const ago = (seconds) => new Date(NOW_MS - seconds * 1000).toISOString();

const SERVICES = [
  {
    id: 7,
    name: "NAS",
    kind: "http",
    interval_s: 60,
    metrics: [
      { id: "mem", label: "Memory", format: "bytes" },
      { id: "cpu", label: "CPU", format: "percent" },
      { id: "up", label: "Uptime", format: "duration" },
      { id: "net", label: "Traffic", format: "bytes_per_sec" },
      { id: "seen", label: "Seen", format: "since" },
      { id: "note", label: "Note", format: "text" },
      { id: "count", label: "Files", format: "number" },
    ],
  },
];

function snapshot(over = {}) {
  return {
    name: "NAS",
    kind: "http",
    interval_s: 60,
    status: "ok",
    values: {
      mem: { num: 1610612736, text: null },
      cpu: { num: 4.25, text: null },
      up: { num: 3 * 86400 + 4 * 3600 + 17, text: null },
      net: { num: 2621440, text: null },
      seen: { num: null, text: ago(120) },
      note: { num: null, text: "all fine" },
      count: { num: 1234567, text: null },
    },
    needs: [],
    fetched_at: ago(120),
    ...over,
  };
}

function valuesDoc(entry = snapshot(), id = "7") {
  return { now: NOW, services: { [id]: entry } };
}

function page() {
  const { document } = parseHTML(
    '<!doctype html><html><body><button id="keep">keep</button><section id="groups"></section></body></html>',
  );
  return { document, root: document.getElementById("groups") };
}

function bound(metrics, extra = {}) {
  return {
    id: "c1",
    title: "Storage",
    description: "",
    url: "https://nas.example/",
    icon: { emoji: null, colour: "teal" },
    service: { id: 7, metrics },
    ...extra,
  };
}

function layoutWith(card) {
  return {
    version: 1,
    background: { kind: "preset", name: "hedgerow" },
    groups: [{ id: "g1", title: "Home", cards: [card] }],
    bookmarks: [],
  };
}

function draw(card, opts = {}, lang = EN) {
  const p = page();
  render(p.root, layoutWith(card), { ...lang, services: SERVICES, values: valuesDoc(), ...opts });
  return { ...p, card: p.root.querySelector(".mm-card") };
}

const cells = (card) =>
  [...card.querySelectorAll(".mm-metric")].map((c) => [
    c.querySelector(".mm-metric-label").textContent,
    c.querySelector(".mm-metric-value").textContent,
  ]);

// ---- formatValue ----

const num = (n) => ({ num: n, text: null });
const NBSP = " ";

test("format_number_groups_by_locale", () => {
  assert.equal(formatValue(num(1234567), "number", "en"), "1,234,567");
  assert.equal(formatValue(num(1234567), "number", "nb"), `1${NBSP}234${NBSP}567`);
  assert.equal(formatValue(num(12.3456), "number", "en"), "12.35");
  assert.equal(formatValue(num(0), "number", "en"), "0");
  assert.equal(formatValue(num(-0.001), "number", "en"), "0", "no minus zero");
  assert.equal(formatValue(num(-42), "number", "en"), "-42");
  assert.equal(formatValue(num(1e15), "number", "en"), "1,000,000,000,000,000");
});

test("format_percent_has_one_decimal_under_ten", () => {
  assert.equal(formatValue(num(4.25), "percent", "en"), "4.3%");
  assert.equal(formatValue(num(0), "percent", "en"), "0.0%");
  assert.equal(formatValue(num(9.94), "percent", "en"), "9.9%");
  assert.equal(formatValue(num(9.96), "percent", "en"), "10%", "rounds up past ten: no decimal");
  assert.equal(formatValue(num(42.4), "percent", "en"), "42%");
  assert.equal(formatValue(num(100), "percent", "en"), "100%");
  assert.equal(formatValue(num(4.25), "percent", "nb"), `4,3${NBSP}%`);
  assert.equal(formatValue(num(250), "percent", "en"), "250%", "not clamped");
  assert.match(formatValue(num(-5), "percent", "en"), /^[-−]5\.0%$/);
});

test("format_bytes_uses_binary_units", () => {
  assert.equal(formatValue(num(0), "bytes", "en"), "0 B");
  assert.equal(formatValue(num(512), "bytes", "en"), "512 B");
  assert.equal(formatValue(num(1023), "bytes", "en"), "1,023 B");
  assert.equal(formatValue(num(1024), "bytes", "en"), "1.0 KiB");
  assert.equal(formatValue(num(1536), "bytes", "en"), "1.5 KiB");
  assert.equal(formatValue(num(10 * 1024), "bytes", "en"), "10 KiB");
  assert.equal(formatValue(num(1610612736), "bytes", "en"), "1.5 GiB");
  assert.equal(formatValue(num(1610612736), "bytes", "nb"), "1,5 GiB");
  assert.equal(formatValue(num(3 * 1024 ** 4), "bytes", "en"), "3.0 TiB");
  assert.equal(formatValue(num(1024 ** 2 - 1), "bytes", "en"), "1.0 MiB", "rounds into the next unit");
  assert.equal(formatValue(num(5000 * 1024 ** 4), "bytes", "en"), "5,000 TiB", "TiB is the top unit");
  assert.equal(formatValue(num(-1536), "bytes", "en"), "-1.5 KiB");
});

test("format_bytes_per_sec_adds_a_rate", () => {
  assert.equal(formatValue(num(0), "bytes_per_sec", "en"), "0 B/s");
  assert.equal(formatValue(num(2621440), "bytes_per_sec", "en"), "2.5 MiB/s");
  assert.equal(formatValue(num(2621440), "bytes_per_sec", "nb"), "2,5 MiB/s");
  assert.equal(formatValue(num(900), "bytes_per_sec", "en"), "900 B/s");
});

test("format_duration_shows_the_two_largest_units", () => {
  const d = 3 * 86400 + 4 * 3600 + 17;
  assert.equal(formatValue(num(d), "duration", "en"), "3 d 4 h");
  assert.equal(formatValue(num(d), "duration", "nb"), "3 d 4 t");
  assert.equal(formatValue(num(0), "duration", "en"), "0 s");
  assert.equal(formatValue(num(59), "duration", "en"), "59 s");
  assert.equal(formatValue(num(90), "duration", "en"), "1 min 30 s");
  assert.equal(formatValue(num(3600), "duration", "en"), "1 h");
  assert.equal(formatValue(num(86400 + 5 * 60), "duration", "en"), "1 d", "zero hours are not shown");
  assert.equal(formatValue(num(59.6), "duration", "en"), "1 min", "rounds to whole seconds first");
  assert.equal(formatValue(num(-90), "duration", "en"), "-1 min 30 s");
  assert.equal(formatValue(num(1e10), "duration", "en"), "115,740 d 17 h");
});

test("format_since_is_relative_to_now", () => {
  const now = NOW_MS;
  const iso = formatValue({ num: null, text: ago(120) }, "since", "en", now);
  assert.match(iso, /2 min/);
  assert.match(iso, /ago/);
  const nb = formatValue({ num: null, text: ago(120) }, "since", "nb", now);
  assert.match(nb, /for 2 min/);
  assert.match(nb, /siden/);
  // Epoch seconds work too.
  const epoch = formatValue(num(now / 1000 - 3 * 3600), "since", "en", now);
  assert.match(epoch, /3 hr|3 h/);
  assert.match(formatValue(num(now / 1000 - 3 * 86400), "since", "en", now), /3 days|3 d/);
  // The future is relative too.
  assert.match(formatValue(num(now / 1000 + 300), "since", "en", now), /in 5 min/);
  assert.equal(formatValue({ num: null, text: "not a date" }, "since", "en", now), "not a date");
  assert.equal(formatValue(num(NaN), "since", "en", now), "–");
});

test("format_text_is_as_is", () => {
  assert.equal(formatValue({ num: null, text: "all fine" }, "text", "en"), "all fine");
  assert.equal(formatValue({ num: 5, text: "five" }, "text", "en"), "five");
  assert.equal(formatValue({ num: null, text: "" }, "text", "en"), "–");
});

test("format_missing_or_odd_values_show_a_dash", () => {
  for (const format of ["number", "percent", "bytes", "bytes_per_sec", "duration", "since", "text"]) {
    assert.equal(formatValue(null, format, "en"), "–", `${format} null`);
    assert.equal(formatValue(undefined, format, "en"), "–", `${format} undefined`);
    assert.equal(formatValue({ num: null, text: null }, format, "en"), "–", `${format} empty`);
    assert.equal(formatValue(num(NaN), format, "en"), "–", `${format} NaN`);
    assert.equal(formatValue(num(Infinity), format, "en"), "–", `${format} Infinity`);
  }
  // A numeric format with no number still shows the text the server sent.
  assert.equal(formatValue({ num: null, text: "n/a" }, "bytes", "en"), "n/a");
  // An unknown format and a language tag the browser rejects do not throw.
  assert.equal(formatValue(num(1500), "weird", "en"), "1,500");
  assert.equal(formatValue(num(1500), "number", "not a tag!"), "1,500");
});

// ---- bound cards ----

test("a_bound_card_draws_its_cells_in_order_in_english", () => {
  const { card } = draw(bound(["cpu", "mem"]));
  assert.deepEqual(cells(card), [
    ["CPU", "4.3%"],
    ["Memory", "1.5 GiB"],
  ]);
  assert.equal(card.getAttribute("href"), "https://nas.example/");
  assert.equal(card.querySelector(".mm-card-title").textContent, "Storage");
  assert.ok(card.querySelector(".mm-tile"), "the tile is still drawn");
  assert.equal(card.getAttribute("data-service"), "7");
  assert.equal(card.querySelector(".mm-metrics").getAttribute("data-count"), "2");
});

test("a_bound_card_draws_its_cells_in_norwegian", () => {
  const nb = {
    ...SERVICES[0],
    metrics: SERVICES[0].metrics.map((m) => ({ ...m, label: { mem: "Minne", cpu: "Prosessor" }[m.id] ?? m.label })),
  };
  const { card } = draw(bound(["mem", "cpu", "up"]), { services: [nb] }, NB);
  assert.deepEqual(cells(card), [
    ["Minne", "1,5 GiB"],
    ["Prosessor", `4,3${NBSP}%`],
    ["Uptime", "3 d 4 t"],
  ]);
  assert.match(card.querySelector(".mm-age").textContent, /for 2 min.* siden/);
});

test("up_to_four_cells_and_no_more", () => {
  const { card } = draw(bound(["cpu", "mem", "up", "net", "seen", "note"]));
  assert.equal(cells(card).length, 4);
  assert.deepEqual(
    cells(card).map(([label]) => label),
    ["CPU", "Memory", "Uptime", "Traffic"],
  );
  const ok = draw(bound(["seen", "note", "count", "net"])).card;
  assert.match(cells(ok)[0][1], /2 min/);
  assert.deepEqual(cells(ok).slice(1), [
    ["Note", "all fine"],
    ["Files", "1,234,567"],
    ["Traffic", "2.5 MiB/s"],
  ]);
});

test("a_metric_that_is_not_in_the_catalogue_is_skipped_and_a_missing_value_is_a_dash", () => {
  const { card } = draw(bound(["nope", "cpu", "count"]), {
    values: valuesDoc(snapshot({ values: { cpu: { num: null, text: null } } })),
  });
  assert.deepEqual(cells(card), [
    ["CPU", "–"],
    ["Files", "–"],
  ]);
});

test("each_cell_keeps_its_full_value_in_a_title", () => {
  const { card } = draw(bound(["mem"]));
  const cell = card.querySelector(".mm-metric");
  assert.equal(cell.getAttribute("title"), "Memory: 1.5 GiB");
});

test("the_age_is_computed_against_now_in_the_values", () => {
  const { card } = draw(bound(["cpu"]));
  const age = card.querySelector(".mm-age");
  assert.match(age.textContent, /2 min/);
  assert.match(age.textContent, /ago/);
  // Another `now` gives another age; the wall clock is not read.
  const later = draw(bound(["cpu"]), {
    values: { ...valuesDoc(), now: new Date(NOW_MS + 3600 * 1000).toISOString() },
  }).card.querySelector(".mm-age");
  assert.match(later.textContent, /1 hr|1 h/);
});

test("now_and_fetched_at_may_be_epoch_seconds", () => {
  const doc = { now: NOW_MS / 1000, services: { 7: snapshot({ fetched_at: NOW_MS / 1000 - 120 }) } };
  const { card } = draw(bound(["cpu"]), { values: doc });
  assert.match(card.querySelector(".mm-age").textContent, /2 min/);
  assert.equal(card.hasAttribute("data-stale"), false);
});

// ---- stale ----

test("stale_is_set_only_when_older_than_three_intervals", () => {
  const at = (s) => draw(bound(["cpu"]), { values: valuesDoc(snapshot({ fetched_at: ago(s) })) }).card;
  assert.equal(at(179).hasAttribute("data-stale"), false);
  assert.equal(at(180).hasAttribute("data-stale"), false, "exactly three intervals is not yet stale");
  assert.equal(at(181).hasAttribute("data-stale"), true);
  assert.equal(at(0).hasAttribute("data-stale"), false);
  const stale = at(600);
  assert.match(stale.querySelector(".mm-age").textContent, /^Last updated .*10 min.*ago$/);
  // The values are still there.
  assert.equal(cells(stale)[0][1], "4.3%");
  assert.equal(at(60).querySelector(".mm-age").textContent.startsWith("Last updated"), false);
});

test("stale_text_in_norwegian", () => {
  const stale = draw(bound(["cpu"]), { values: valuesDoc(snapshot({ fetched_at: ago(900) })) }, NB).card;
  assert.match(stale.querySelector(".mm-age").textContent, /^Sist oppdatert for 15 min.* siden$/);
});

test("tick_ages_moves_the_age_and_the_stale_flag_without_a_redraw", () => {
  const { root, card } = draw(bound(["cpu"]));
  const age = card.querySelector(".mm-age");
  assert.equal(card.hasAttribute("data-stale"), false);
  tickAges(root, { ...EN, now: NOW_MS + 3 * 3600 * 1000 });
  assert.equal(root.querySelector(".mm-card"), card, "same element");
  assert.equal(card.querySelector(".mm-age"), age);
  assert.equal(card.hasAttribute("data-stale"), true);
  assert.match(age.textContent, /^Last updated .*3 hr.*ago$|^Last updated .*3 h.*ago$/);
  // And back: a clock that went backwards, or a fresh now, clears it.
  tickAges(root, { ...EN, now: NOW_MS });
  assert.equal(card.hasAttribute("data-stale"), false);
  assert.match(age.textContent, /2 min/);
});

test("tick_ages_ignores_cards_that_have_no_age", () => {
  const p = page();
  render(p.root, layoutWith({ ...bound(["cpu"]), service: null }), EN);
  tickAges(p.root, { ...EN, now: NOW_MS });
  assert.equal(p.root.querySelector(".mm-age"), null);
});

// ---- missing and waiting ----

test("a_service_that_is_gone_shows_its_notice_and_keeps_the_link", () => {
  const { card } = draw(bound(["cpu"]), { services: [] });
  assert.equal(card.tagName, "A");
  assert.equal(card.getAttribute("href"), "https://nas.example/");
  assert.equal(card.querySelector(".mm-svc-notice").textContent, "This service no longer exists");
  assert.equal(card.querySelectorAll(".mm-metric").length, 0);
  const nb = draw(bound(["cpu"]), { services: [] }, NB).card;
  assert.equal(nb.querySelector(".mm-svc-notice").textContent, "Tjenesten finnes ikke lenger");
});

test("a_service_with_no_snapshot_yet_is_waiting_with_status_unknown", () => {
  const { card } = draw(bound(["cpu", "mem"]), { values: { now: NOW, services: {} } });
  assert.equal(card.querySelector(".mm-svc-notice").textContent, "Waiting for the first answer");
  assert.equal(card.querySelector(".mm-status").getAttribute("data-status"), "unknown");
  assert.equal(card.querySelectorAll(".mm-metric").length, 0);
  assert.equal(card.hasAttribute("data-stale"), false);
  const nb = draw(bound(["cpu"]), { values: { now: NOW, services: {} } }, NB).card;
  assert.equal(nb.querySelector(".mm-svc-notice").textContent, "Venter på første svar");
  // No values document at all is the same.
  const none = draw(bound(["cpu"]), { values: undefined }).card;
  assert.equal(none.querySelector(".mm-svc-notice").textContent, "Waiting for the first answer");
});

test("an_error_label_is_shown_as_text", () => {
  const { card } = draw(bound(["cpu"]), {
    values: valuesDoc(snapshot({ status: "bad", error_label: "Connection refused" })),
  });
  assert.equal(card.querySelector(".mm-svc-error").textContent, "Connection refused");
  assert.equal(card.querySelector(".mm-status").getAttribute("data-status"), "bad");
  assert.equal(draw(bound(["cpu"])).card.querySelector(".mm-svc-error"), null);
});

// ---- status dot ----

test("the_status_dot_has_a_text_label_for_every_status", () => {
  const expect = {
    ok: "Status: OK",
    warn: "Status: warning",
    bad: "Status: problem",
    unknown: "Status: unknown",
    weird: "Status: unknown",
  };
  for (const [status, name] of Object.entries(expect)) {
    const { card } = draw(bound(["cpu"]), { values: valuesDoc(snapshot({ status })) });
    const dot = card.querySelector(".mm-status");
    assert.equal(dot.getAttribute("data-status"), status === "weird" ? "unknown" : status);
    assert.equal(dot.getAttribute("role"), "img");
    assert.equal(dot.getAttribute("aria-label"), name);
    assert.equal(dot.getAttribute("title"), name);
  }
  const nb = draw(bound(["cpu"]), { values: valuesDoc(snapshot({ status: "bad" })) }, NB).card;
  assert.equal(nb.querySelector(".mm-status").getAttribute("aria-label"), "Status: feil");
});

// ---- hostile input ----

test("hostile_names_labels_and_values_stay_text", () => {
  const hostile = '<img src=x onerror="alert(1)">';
  const rtl = "‮evil‬ שלום";
  const services = [
    {
      ...SERVICES[0],
      name: hostile,
      metrics: [
        { id: "note", label: hostile, format: "text" },
        { id: "mem", label: rtl, format: "text" },
      ],
    },
  ];
  const entry = snapshot({
    name: hostile,
    error_label: hostile,
    values: { note: { num: null, text: hostile }, mem: { num: null, text: rtl } },
  });
  const { root, card } = draw(bound(["note", "mem"]), { services, values: valuesDoc(entry) });
  assert.equal(root.querySelector("img"), null);
  assert.equal(root.querySelector("[onerror]"), null);
  assert.deepEqual(cells(card), [
    [hostile, hostile],
    [rtl, rtl],
  ]);
  assert.equal(card.querySelector(".mm-svc-error").textContent, hostile);
  for (const el of card.querySelectorAll(".mm-metric-label, .mm-metric-value, .mm-svc-error")) {
    assert.equal(el.getAttribute("dir"), "auto", "user text isolates its direction");
  }
  assert.equal(card.querySelector(".mm-metric").getAttribute("title"), `${hostile}: ${hostile}`);
});

test("a_hostile_status_or_id_cannot_become_an_attribute_or_html", () => {
  const { root, card } = draw(bound(["cpu"]), {
    values: valuesDoc(snapshot({ status: '"><script>x</script>' })),
  });
  assert.equal(root.querySelector("script"), null);
  assert.equal(card.querySelector(".mm-status").getAttribute("data-status"), "unknown");
});

// ---- focus and compatibility ----

test("drawing_never_moves_focus", () => {
  // linkedom has no focus(), so record every call to it (and to blur) on what the renderer makes.
  const p = page();
  const calls = [];
  const create = p.document.createElement.bind(p.document);
  p.document.createElement = (tag) => {
    const el = create(tag);
    el.focus = () => calls.push(`focus ${tag}`);
    el.blur = () => calls.push(`blur ${tag}`);
    return el;
  };
  p.root.focus = () => calls.push("focus root");
  render(p.root, layoutWith(bound(["cpu", "mem"])), { ...EN, services: SERVICES, values: valuesDoc() });
  render(p.root, layoutWith({ ...bound(["cpu"]), service: { id: 9, metrics: ["x"] } }), {
    ...EN,
    services: SERVICES,
    values: valuesDoc(),
  });
  tickAges(p.root, { ...EN, now: NOW_MS + 1e6 });
  assert.deepEqual(calls, []);
  // Nothing in the output asks for focus or joins the tab order but the links.
  const odd = p.root.querySelectorAll("[autofocus], [tabindex], button, input, select, textarea");
  assert.equal(odd.length, 0);
});

// The output of the renderer before cards could be bound, for a fixed layout (captured from
// render.js at the commit before this feature).
const GOLDEN =
  '<section class="mm-group"><h2 aria-label="Group: Media" dir="auto" class="mm-group-title">Media</h2><div class="mm-cards"><a rel="noreferrer" href="https://sonarr.example/" class="mm-card glass"><span aria-hidden="true" data-colour="teal" class="mm-tile">S</span><span class="mm-card-text"><span dir="auto" class="mm-card-title">Sonarr</span><span dir="auto" class="mm-card-desc">Series</span></span></a><a rel="noreferrer" href="http://radarr.example/" class="mm-card glass"><span aria-hidden="true" data-colour="indigo" class="mm-tile">🎬</span><span class="mm-card-text"><span dir="auto" class="mm-card-title">Radarr</span></span></a><div class="mm-card mm-card-invalid glass"><span aria-hidden="true" data-colour="teal" class="mm-tile">B</span><span class="mm-card-text"><span dir="auto" class="mm-card-title">Broken</span><span class="mm-invalid">invalid link</span></span></div></div></section><section class="mm-group"><h2 aria-label="Group: Dev" dir="auto" class="mm-group-title">Dev</h2><div class="mm-cards"></div></section><nav aria-label="Bookmarks" class="mm-bookmarks"><a rel="noreferrer" href="https://hedgerow.tools/" dir="auto" class="mm-pill">Hedgerow</a></nav>';

function plainLayout(service) {
  const c = (id, title, url, extra = {}) => ({
    id,
    title,
    description: "",
    url,
    icon: { emoji: null, colour: "teal" },
    ...extra,
  });
  const withService = (card) => (service === undefined ? card : { ...card, service });
  return {
    version: 1,
    background: { kind: "preset", name: "natt" },
    groups: [
      {
        id: "g1",
        title: "Media",
        cards: [
          withService(c("c1", "Sonarr", "https://sonarr.example/", { description: "Series" })),
          withService(c("c2", "Radarr", "http://radarr.example/", { icon: { emoji: "🎬", colour: "indigo" } })),
          withService(c("c3", "Broken", "javascript:alert(1)")),
        ],
      },
      { id: "g2", title: "Dev", cards: [] },
    ],
    bookmarks: [{ id: "b1", title: "Hedgerow", url: "https://hedgerow.tools/" }],
  };
}

test("an_unbound_card_renders_exactly_as_before", () => {
  const plain = { lang: "en", labels: { ...LABELS_EN } };
  for (const service of [undefined, null]) {
    for (const extra of [{}, { services: SERVICES, values: valuesDoc() }, { services: [], values: null }]) {
      const p = page();
      render(p.root, plainLayout(service), { ...plain, ...extra });
      assert.equal(p.root.innerHTML, GOLDEN, JSON.stringify([service, Object.keys(extra)]));
    }
  }
});

test("a_bound_card_with_no_services_list_renders_as_a_plain_card", () => {
  // The extension before it learns about services, and an old page script.
  for (const extra of [{}, { values: valuesDoc() }]) {
    const p = page();
    render(p.root, plainLayout({ id: 7, metrics: ["cpu"] }), { ...EN, ...extra });
    assert.equal(p.root.innerHTML, GOLDEN);
  }
});

test("an_odd_service_ref_is_a_plain_card", () => {
  for (const service of [5, "7", [], { id: "7", metrics: [] }, { id: 7, metrics: "cpu" }, { metrics: ["cpu"] }]) {
    const p = page();
    render(p.root, plainLayout(service), { ...EN, services: SERVICES, values: valuesDoc() });
    assert.equal(p.root.querySelector(".mm-svc"), null, JSON.stringify(service));
  }
});

test("odd_values_and_services_do_not_throw", () => {
  for (const values of [null, 5, "x", [], { services: null }, { services: { 7: null } }, { services: { 7: 5 } }]) {
    for (const services of [null, "x", [null, 5, { id: 7 }], [{ id: 7, metrics: null }]]) {
      const p = page();
      render(p.root, layoutWith(bound(["cpu"])), { ...EN, services, values });
      assert.ok(p.root.querySelector(".mm-card"));
    }
  }
});
