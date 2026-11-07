// Tests for static/magpie/start.js (the page script around render.js).
import assert from "node:assert/strict";
import { test } from "node:test";
import { parseHTML } from "linkedom";

import { draw, drawIfNeeded, initEditing } from "../static/magpie/start.js";

const LABELS = {
  empty: "Nothing here.",
  invalid_link: "invalid link",
  group_label: "Group: {title}",
  bookmarks_label: "Bookmarks",
  service_waiting: "Waiting",
  service_missing: "Gone",
};

const LAYOUT = {
  version: 3,
  background: { kind: "preset", name: "natt" },
  groups: [
    {
      id: "g1",
      title: "Media",
      cards: [
        {
          id: "c1",
          title: "Sonarr",
          description: "",
          url: "https://sonarr.example/",
          icon: { emoji: null, colour: "teal" },
        },
      ],
    },
  ],
  bookmarks: [{ id: "b1", title: "Hedgerow", url: "https://hedgerow.tools/" }],
};

function startPage({ layout = JSON.stringify(LAYOUT), labels = JSON.stringify(LABELS), services, lang = "nb" } = {}) {
  const blocks =
    (layout === null ? "" : `<script type="application/json" id="mm-layout">${layout}</script>`) +
    (labels === null ? "" : `<script type="application/json" id="mm-labels">${labels}</script>`) +
    (services === undefined ? "" : `<script type="application/json" id="mm-services">${services}</script>`);
  const { document } = parseHTML(
    `<!doctype html><html lang="${lang}"><body><main><section id="groups" class="mm-root"></section>${blocks}</main></body></html>`,
  );
  return document;
}

function settingsPage(document) {
  // What htmx does on a boosted visit: the body is swapped, <html> stays.
  document.body.innerHTML = "<main><h1>Settings</h1></main>";
}

test("draw_reads_the_embedded_layout_and_labels", () => {
  const document = startPage();
  draw(document);
  const root = document.getElementById("groups");
  assert.equal(root.querySelectorAll("section.mm-group").length, 1);
  assert.equal(root.querySelector(".mm-card-title").textContent, "Sonarr");
  assert.equal(root.querySelector("h2").getAttribute("aria-label"), "Group: Media");
  assert.equal(root.querySelector(".mm-pill").textContent, "Hedgerow");
  assert.equal(document.documentElement.getAttribute("data-bg"), "natt");
});

test("draw_survives_a_missing_or_invalid_block", () => {
  // No layout block: the empty page, with the labels that are there.
  let document = startPage({ layout: null });
  draw(document);
  assert.equal(document.querySelector(".empty-title").textContent, "Nothing here.");
  assert.equal(document.documentElement.getAttribute("data-bg"), "hedgerow");

  // A layout that is not JSON: the same.
  document = startPage({ layout: "{not json" });
  draw(document);
  assert.ok(document.querySelector(".mm-empty"));

  // No labels block, or a broken one: still draws, with empty strings.
  for (const labels of [null, "{nope"]) {
    document = startPage({ labels });
    draw(document);
    assert.equal(document.querySelectorAll(".mm-card").length, 1);
    assert.ok(!document.body.textContent.includes("undefined"));
  }
});

test("draw_cleans_the_background_when_the_root_is_gone", () => {
  const document = startPage({
    layout: JSON.stringify({ ...LAYOUT, background: { kind: "image", id: "abc" } }),
  });
  draw(document);
  const html = document.documentElement;
  assert.equal(html.getAttribute("data-bg"), "image");
  assert.equal(html.style.getPropertyValue("--mm-bg-image"), 'url("/start/background/abc")');

  settingsPage(document);
  draw(document);
  assert.equal(html.hasAttribute("data-bg"), false);
  assert.ok(!html.style.getPropertyValue("--mm-bg-image"));

  // The same through the htmx hook.
  const again = startPage({
    layout: JSON.stringify({ ...LAYOUT, background: { kind: "image", id: "abc" } }),
  });
  draw(again);
  settingsPage(again);
  drawIfNeeded(again);
  assert.equal(again.documentElement.hasAttribute("data-bg"), false);
  assert.ok(!again.documentElement.style.getPropertyValue("--mm-bg-image"));
});

test("draw_if_needed_never_draws_over_existing_content", () => {
  const document = startPage();
  draw(document);
  const root = document.getElementById("groups");

  // Something changed the drawn page (edit mode will): a settle must not undo it.
  root.querySelector(".mm-card-title").textContent = "Edited";
  const marker = document.createElement("div");
  marker.setAttribute("id", "marker");
  root.appendChild(marker);
  drawIfNeeded(document);
  assert.equal(root.querySelector(".mm-card-title").textContent, "Edited");
  assert.ok(document.getElementById("marker"));

  // A fresh body (empty #groups) is drawn.
  while (root.firstChild) root.removeChild(root.firstChild);
  drawIfNeeded(document);
  assert.equal(root.querySelectorAll(".mm-card").length, 1);
});

test("back_navigation_restores_the_background", () => {
  const document = startPage({
    layout: JSON.stringify({ ...LAYOUT, background: { kind: "image", id: "abc" } }),
  });
  const html = document.documentElement;
  draw(document);
  const drawn = document.body.innerHTML;

  // Start -> Settings: the body is swapped and the background cleared.
  settingsPage(document);
  drawIfNeeded(document);
  assert.equal(html.hasAttribute("data-bg"), false);

  // Back: htmx puts the saved, drawn body back. The root has content but
  // <html> has no background, so it is drawn again.
  document.body.innerHTML = drawn;
  assert.ok(document.getElementById("groups").firstElementChild);
  drawIfNeeded(document);
  assert.equal(html.getAttribute("data-bg"), "image");
  assert.equal(html.style.getPropertyValue("--mm-bg-image"), 'url("/start/background/abc")');
  assert.equal(document.querySelectorAll("section.mm-group").length, 1);
});

// Plan 4, task 3b: start.js hands #mm-services to the renderer.
const BOUND = {
  ...LAYOUT,
  groups: [
    {
      id: "g1",
      title: "Media",
      cards: [{ ...LAYOUT.groups[0].cards[0], service: { id: 4, metrics: ["status"] } }],
    },
  ],
};
const BLOG = { id: 4, name: "Blog", kind: "http", interval_s: 60, metrics: [{ id: "status", label: "HTTP-status", format: "number" }] };

test("draw_passes_the_services_block_to_the_renderer", () => {
  const document = startPage({ layout: JSON.stringify(BOUND), services: JSON.stringify([BLOG]) });
  draw(document);
  const card = document.querySelector(".mm-card");
  assert.ok(card.classList.contains("mm-bound"), "a bound card");
  assert.equal(card.getAttribute("data-service"), "4");
  // No answer yet (the values come in the next task): the card says it is waiting.
  assert.equal(card.querySelector(".mm-svc-notice").textContent, "Waiting");
  assert.equal(card.querySelector(".mm-status").getAttribute("data-status"), "unknown");
});

test("a_service_that_is_not_in_the_block_is_shown_as_gone", () => {
  const document = startPage({ layout: JSON.stringify(BOUND), services: "[]" });
  draw(document);
  const card = document.querySelector(".mm-card");
  assert.ok(card.classList.contains("mm-bound"));
  assert.equal(card.querySelector(".mm-svc-notice").textContent, "Gone");
  assert.equal(card.querySelector(".mm-metric"), null);
});

test("a_page_without_a_usable_services_block_draws_bound_cards_as_plain_cards", () => {
  for (const services of [undefined, "not json", "{}", "null"]) {
    const document = startPage({ layout: JSON.stringify(BOUND), services });
    draw(document);
    const card = document.querySelector(".mm-card");
    assert.equal(card.classList.contains("mm-bound"), false, String(services));
    assert.equal(card.querySelector(".mm-svc"), null);
    assert.equal(card.querySelector(".mm-card-title").textContent, "Sonarr");
  }
});

test("the_edit_button_hands_the_services_to_edit_mode", async () => {
  const document = startPage({ layout: JSON.stringify(BOUND), services: JSON.stringify([BLOG]) });
  document.body.insertAdjacentHTML("beforeend", '<button id="mm-edit" type="button">Edit</button>');
  let seen = null;
  initEditing(document, {
    load: async () => ({
      startEditing: (root, getLayout, save, opts) => {
        seen = opts;
        return { isActive: () => true, done() {}, stop() {}, isDirty: () => false, draft: () => getLayout() };
      },
    }),
    save: async () => ({ status: 200, body: {} }),
    storage: null,
  });
  document.getElementById("mm-edit").dispatchEvent(new document.defaultView.Event("click", { bubbles: true }));
  await new Promise((resolve) => setTimeout(resolve, 0));
  assert.deepEqual(seen.services, [BLOG]);
});
