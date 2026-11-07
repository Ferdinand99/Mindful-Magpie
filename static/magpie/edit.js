// Mindful Magpie: edit mode for the start page.
//
// `startEditing(root, getLayout, save, opts)` turns the page the renderer drew
// into an editor. It works on a copy of the layout (the draft), changed only
// through the pure functions in ops.js, draws the draft with render.js and adds
// the controls on top. Nothing is stored until «Done»: then the whole draft goes
// to `save`. Leaving with unsaved changes asks first.
//
// This is the only start page script that moves focus, and only after the
// person opened edit mode: after each action focus lands on a control that makes
// sense (see `focusControl`). Every control is a real <button>. Every move that
// dragging offers is also in the move menu, so the keyboard needs no pointer.
//
// All text goes in with textContent and setAttribute, never as HTML.
//
// `save(layout)` resolves to `{ status, body }` (the server's answer) and rejects
// when the network fails. `opts`: `labels` (interface strings), `lang`,
// `editButton` (the header button; it shows aria-pressed and gets focus back),
// `reload` (called by «Reload» after a conflict), `draft` (a layout to start from
// instead of the stored one: an unsaved draft that was kept, see start.js) and
// `onDiscard` (called when the person gives the changes up).
import {
  addBookmark,
  addCard,
  addGroup,
  moveBookmark,
  moveCard,
  moveGroup,
  removeBookmark,
  removeCard,
  removeGroup,
  renameGroup,
  updateBookmark,
  updateCard,
} from "./ops.js";
import { render } from "./render.js";
import {
  LIMITS,
  PALETTE,
  checkBookmark,
  checkCard,
  checkGroup,
  normalizeUrl,
  parsePath,
} from "./rules.js";

/** Every interface string edit mode uses (the server puts them in #mm-labels). */
export const LABEL_KEYS = [
  "edit_toolbar",
  "edit_done",
  "edit_cancel",
  "edit_saving",
  "add_group",
  "add_bookmark",
  "add_card",
  "rename_group",
  "edit_item",
  "move_item",
  "delete_item",
  "drag_hint",
  "dialog_save",
  "dialog_cancel",
  "dialog_close",
  "group_form_add",
  "group_form_rename",
  "card_form_add",
  "card_form_edit",
  "bookmark_form_add",
  "bookmark_form_edit",
  "field_group_name",
  "field_title",
  "field_url",
  "field_description",
  "field_emoji",
  "field_colour",
  "optional",
  "help_url",
  "help_emoji",
  "move_title",
  "move_up",
  "move_down",
  "move_first",
  "move_last",
  "move_to_group",
  "confirm_delete_group",
  "confirm_delete_group_text",
  "confirm_delete",
  "confirm_discard",
  "confirm_discard_text",
  "confirm_discard_yes",
  "confirm_keep",
  "confirm_leave",
  "confirm_leave_text",
  "confirm_leave_yes",
  "restore_notice",
  "restore_yes",
  "restore_discard",
  "conflict",
  "reload",
  "save_failed",
  "invalid_summary",
  "summary_more",
  "summary_group",
  "summary_card",
  "summary_bookmark",
  "summary_line",
  "limit_groups",
  "limit_cards",
  "limit_bookmarks",
  "err_required",
  "err_too_long",
  "err_bad_url",
  "err_bad_scheme",
  "err_bad_emoji",
  "err_bad_colour",
  "err_generic",
  "colour_red",
  "colour_orange",
  "colour_yellow",
  "colour_green",
  "colour_mint",
  "colour_teal",
  "colour_cyan",
  "colour_blue",
  "colour_indigo",
  "colour_purple",
  "colour_pink",
  "colour_brown",
  "colour_gray",
];

const SVG_NS = "http://www.w3.org/2000/svg";
const DEFAULT_COLOUR = "blue";
const SUMMARY_LIMIT = 10;

// The strings of the editor that is open. Edit mode is one at a time.
const ui = { labels: {} };

/** The interface string `key` with `{name}` parts filled in from `vars`. */
function L(key, vars = {}) {
  const value = ui.labels[key];
  let text = typeof value === "string" ? value : key;
  // split/join, not replace: a title may contain `$&` and the like.
  for (const [name, replacement] of Object.entries(vars)) {
    text = text.split(`{${name}}`).join(String(replacement));
  }
  return text;
}

// ---- small DOM helpers ----------------------------------------------------

function h(doc, tag, props = {}, children = []) {
  const el = doc.createElement(tag);
  for (const [name, value] of Object.entries(props)) {
    if (value === undefined || value === null || value === false) continue;
    if (name === "text") el.textContent = value;
    else el.setAttribute(name, value === true ? "" : String(value));
  }
  for (const child of children) {
    if (child) el.appendChild(typeof child === "string" ? doc.createTextNode(child) : child);
  }
  return el;
}

function icon(doc, name) {
  const svg = doc.createElementNS(SVG_NS, "svg");
  svg.setAttribute("class", "icon");
  svg.setAttribute("aria-hidden", "true");
  const use = doc.createElementNS(SVG_NS, "use");
  use.setAttribute("href", `/static/icons.svg#${name}`);
  svg.appendChild(use);
  return svg;
}

/** A quiet icon-only button. Its name is the aria-label. */
function iconButton(doc, act, name, label) {
  const button = h(doc, "button", {
    type: "button",
    class: "btn btn-icon btn-plain btn-sm",
    "data-act": act,
    "aria-label": label,
    "data-tip": label,
  });
  button.appendChild(icon(doc, name));
  return button;
}

/** A button with a label, and optionally an icon in front. */
function textButton(doc, act, label, { kind = "secondary", size = "", iconName = "" } = {}) {
  const button = h(doc, "button", {
    type: "button",
    class: ["btn", `btn-${kind}`, size && `btn-${size}`].filter(Boolean).join(" "),
    "data-act": act,
  });
  if (iconName) button.appendChild(icon(doc, iconName));
  button.appendChild(h(doc, "span", { text: label }));
  return button;
}

const children = (el, className) => [...el.children].filter((c) => c.classList.contains(className));

let idCounter = 0;
const uid = (prefix) => `${prefix}-${++idCounter}`;

// ---- sheets -----------------------------------------------------------------
// The kit's sheet (dialog.sheet). With the kit's script loaded (window.VV) the
// kit opens and closes it with its animation, focus trap and drag to dismiss;
// without it the dialog's own methods do, and where there is no <dialog> at all
// (tests) the `open` attribute stands in.

function openDialog(dialog, trigger, onClosed) {
  const doc = dialog.ownerDocument;
  const VV = doc.defaultView?.VV;
  doc.body.appendChild(dialog);
  if (VV && typeof VV.init === "function") VV.init(dialog);
  // After the kit's own listener (VV.init above), so that this one has the last word on focus.
  dialog.addEventListener("close", () => {
    dialog.remove();
    onClosed();
  });
  dialog.addEventListener("keydown", (event) => {
    if (event.key !== "Escape") return;
    event.preventDefault();
    event.stopPropagation();
    closeDialog(dialog);
  });
  if (VV && typeof VV.openSheet === "function" && typeof dialog.showModal === "function") {
    VV.openSheet(dialog, trigger);
  } else if (typeof dialog.showModal === "function") {
    dialog.showModal();
  } else {
    dialog.setAttribute("open", "");
  }
}

function closeDialog(dialog) {
  const VV = dialog.ownerDocument.defaultView?.VV;
  if (VV && typeof VV.closeSheet === "function" && typeof dialog.showModal === "function") {
    VV.closeSheet(dialog);
  } else if (typeof dialog.close === "function") {
    dialog.close();
  } else if (dialog.hasAttribute("open")) {
    dialog.removeAttribute("open");
    dialog.dispatchEvent(new (dialog.ownerDocument.defaultView.Event)("close"));
  }
}

// ---- the editor -------------------------------------------------------------

export function startEditing(root, getLayout, save, opts = {}) {
  const doc = root.ownerDocument;
  const win = doc.defaultView ?? globalThis;
  ui.labels = opts.labels ?? {};
  const lang = opts.lang ?? doc.documentElement.lang;
  const editButton = opts.editButton ?? null;
  const reload = opts.reload ?? (() => win.location.reload());
  const navigate = opts.navigate ?? ((href) => win.location.assign(href));

  const stored = getLayout();
  let draft = structuredClone(opts.draft ?? stored);
  // What "unsaved" is measured against: the stored layout (a kept draft is unsaved by definition).
  const baseline = JSON.stringify(opts.draft ? stored : draft);
  const onDiscard = opts.onDiscard ?? (() => {});
  let active = true;
  let saving = false;
  let notice = null;
  let sent = null;
  let drag = null;
  let marked = null;
  const listeners = [];

  const dirty = () => JSON.stringify(draft) !== baseline;

  function on(target, type, handler, options) {
    target.addEventListener(type, handler, options);
    listeners.push(() => target.removeEventListener(type, handler, options));
  }

  // -- the bar above the page, and the message under it --

  const host = h(doc, "div", { class: "mm-edit-host" });
  const toolbar = h(doc, "div", {
    class: "mm-edit-bar glass",
    role: "group",
    "aria-label": L("edit_toolbar"),
  });
  const adds = h(doc, "div", { class: "mm-edit-adds" });
  adds.appendChild(textButton(doc, "add-group", L("add_group"), { iconName: "plus" }));
  adds.appendChild(textButton(doc, "add-bookmark", L("add_bookmark"), { iconName: "plus" }));
  const ends = h(doc, "div", { class: "mm-edit-ends" });
  ends.appendChild(textButton(doc, "cancel", L("edit_cancel"), { kind: "ghost" }));
  ends.appendChild(textButton(doc, "done", L("edit_done"), { kind: "primary" }));
  toolbar.appendChild(adds);
  toolbar.appendChild(ends);
  host.appendChild(toolbar);
  let alertEl = null;
  // The server's last list of problems (what the summary's buttons open).
  let problems = [];

  const toolbarButton = (name) => toolbar.querySelector(`[data-act="${name}"]`);

  function setBusy(busy) {
    for (const button of toolbar.querySelectorAll("button")) {
      if (busy) button.setAttribute("disabled", "");
      else button.removeAttribute("disabled");
    }
    toolbar.setAttribute("aria-busy", busy ? "true" : "false");
    const label = toolbarButton("done")?.querySelector("span");
    if (label) label.textContent = busy ? L("edit_saving") : L("edit_done");
  }

  function renderNotice() {
    if (alertEl) alertEl.remove();
    alertEl = null;
    if (!notice) return;
    alertEl = h(doc, "div", { class: "mm-edit-alert glass", role: "alert" });
    if (notice.type === "conflict") {
      alertEl.appendChild(h(doc, "p", { text: L("conflict") }));
      const button = textButton(doc, "reload", L("reload"), { kind: "primary", size: "sm" });
      alertEl.appendChild(button);
    } else if (notice.type === "summary") {
      alertEl.appendChild(h(doc, "p", { text: L("invalid_summary") }));
      const list = h(doc, "ul", { class: "mm-edit-problems" });
      for (const entry of notice.entries.slice(0, SUMMARY_LIMIT)) {
        const item = h(doc, "li");
        if (entry.path) {
          item.appendChild(h(doc, "button", { type: "button", class: "btn btn-plain btn-sm", "data-path": entry.path, text: entry.text }));
        } else {
          item.textContent = entry.text;
        }
        list.appendChild(item);
      }
      alertEl.appendChild(list);
      if (notice.entries.length > SUMMARY_LIMIT) {
        alertEl.appendChild(h(doc, "p", { text: L("summary_more", { n: notice.entries.length - SUMMARY_LIMIT }) }));
      }
    } else {
      alertEl.appendChild(h(doc, "p", { text: notice.text }));
    }
    host.appendChild(alertEl);
  }

  function setNotice(next) {
    notice = next;
    renderNotice();
  }

  // -- drawing: the renderer's output with the controls on top --

  function moveOptions(kind, id) {
    if (kind === "card") {
      const group = draft.groups.find((g) => g.cards.some((c) => c.id === id));
      if (!group) return [];
      const at = group.cards.findIndex((c) => c.id === id);
      const options = [];
      if (at > 0) options.push({ key: "up", label: L("move_up") });
      if (at < group.cards.length - 1) options.push({ key: "down", label: L("move_down") });
      for (const other of draft.groups) {
        if (other.id !== group.id) {
          options.push({ key: `group:${other.id}`, label: L("move_to_group", { title: other.title }) });
        }
      }
      return options;
    }
    const list = kind === "group" ? draft.groups : draft.bookmarks;
    const at = list.findIndex((x) => x.id === id);
    const options = [];
    if (at < 0) return options;
    if (at > 0) options.push({ key: "up", label: L("move_up") });
    if (at < list.length - 1) options.push({ key: "down", label: L("move_down") });
    if (at > 0) options.push({ key: "first", label: L("move_first") });
    if (at < list.length - 1) options.push({ key: "last", label: L("move_last") });
    return options;
  }

  /** The three controls under a card or beside a pill. */
  function itemControls(kind, id, title) {
    const bar = h(doc, "div", { class: "mm-edit-controls" });
    bar.appendChild(iconButton(doc, "edit", "pencil", L("edit_item", { title })));
    const move = iconButton(doc, "move", "chevron-up-down", L("move_item", { title }));
    if (moveOptions(kind, id).length === 0) move.setAttribute("disabled", "");
    bar.appendChild(move);
    bar.appendChild(iconButton(doc, "delete", "trash", L("delete_item", { title })));
    return bar;
  }

  function wrapItem(el, kind, id, title, tag) {
    const wrapper = h(doc, tag, {
      class: `mm-edit-item mm-edit-${kind}`,
      "data-kind": kind,
      "data-id": id,
      draggable: "true",
    });
    // Edit mode never follows a link.
    el.removeAttribute("href");
    el.removeAttribute("rel");
    el.parentNode.replaceChild(wrapper, el);
    wrapper.appendChild(el);
    wrapper.appendChild(itemControls(kind, id, title));
    return wrapper;
  }

  function decorateGroup(section, group) {
    section.setAttribute("data-kind", "group");
    section.setAttribute("data-id", group.id);
    const heading = section.querySelector(".mm-group-title");
    const head = h(doc, "div", { class: "mm-edit-head" });
    const grip = h(doc, "span", {
      class: "mm-grip",
      draggable: "true",
      "aria-hidden": "true",
      "data-tip": L("drag_hint"),
    });
    grip.appendChild(icon(doc, "grip"));
    section.insertBefore(head, heading);
    head.appendChild(grip);
    head.appendChild(heading);
    const actions = h(doc, "div", { class: "mm-edit-actions" });
    actions.appendChild(iconButton(doc, "rename", "pencil", L("rename_group", { title: group.title })));
    const move = iconButton(doc, "move", "chevron-up-down", L("move_item", { title: group.title }));
    if (moveOptions("group", group.id).length === 0) move.setAttribute("disabled", "");
    actions.appendChild(move);
    actions.appendChild(iconButton(doc, "delete", "trash", L("delete_item", { title: group.title })));
    head.appendChild(actions);

    const grid = section.querySelector(".mm-cards");
    const cardEls = [...grid.children];
    group.cards.forEach((card, index) => {
      if (cardEls[index]) wrapItem(cardEls[index], "card", card.id, card.title, "div");
    });
    section.appendChild(
      h(doc, "div", { class: "mm-edit-foot" }, [textButton(doc, "add-card", L("add_card"), { size: "sm", iconName: "plus" })]),
    );
  }

  function redraw() {
    render(root, draft, { lang, labels: ui.labels });
    root.setAttribute("data-editing", "");
    const sections = children(root, "mm-group");
    draft.groups.forEach((group, index) => {
      if (sections[index]) decorateGroup(sections[index], group);
    });
    const nav = children(root, "mm-bookmarks")[0];
    if (nav) {
      const pills = [...nav.children];
      draft.bookmarks.forEach((bookmark, index) => {
        if (pills[index]) wrapItem(pills[index], "bookmark", bookmark.id, bookmark.title, "span");
      });
    }
    marked = null;
  }

  // -- focus --

  function find(kind, id) {
    for (const el of root.querySelectorAll(`[data-kind="${kind}"]`)) {
      if (el.getAttribute("data-id") === id) return el;
    }
    return null;
  }

  /**
   * Puts focus on a control. `spec` is `{ toolbar: name }`, `{ el }` (an
   * element outside the editor, such as the link that was clicked) or
   * `{ kind, id, act }` (an item's control, by name). A control that is
   * disabled or gone falls back to the item's first usable button.
   */
  function focusControl(spec) {
    if (!active || !spec) return;
    let target = null;
    if (spec.el) {
      target = spec.el.isConnected ? spec.el : null;
    } else if (spec.toolbar) {
      target = toolbarButton(spec.toolbar);
    } else {
      const el = find(spec.kind, spec.id);
      if (el) {
        target = el.querySelector(`[data-act="${spec.act}"]`);
        if (!target || target.hasAttribute("disabled")) {
          target = [...el.querySelectorAll("button")].find((b) => !b.hasAttribute("disabled")) ?? null;
        }
      }
    }
    if (target) target.focus();
  }

  // -- sheets of this editor --

  /**
   * Opens a sheet. Returns a handle: `dialog`, `body`, `foot`, `close(spec)`
   * (closes it and then focuses `spec`), and the settable `onClosed`, which
   * replaces the focus step.
   */
  function openSheet({ title, trigger, alert = false }) {
    const dialog = h(doc, "dialog", {
      class: alert ? "sheet sheet-alert" : "sheet",
      "aria-labelledby": uid("mm-sheet-title"),
    });
    const titleId = dialog.getAttribute("aria-labelledby");
    const body = h(doc, "div", { class: "sheet-body" });
    const foot = h(doc, "footer", { class: "sheet-foot" });
    dialog.appendChild(h(doc, "div", { class: "sheet-grabber", "aria-hidden": "true" }));
    if (alert) {
      body.appendChild(h(doc, "h2", { class: "sheet-title", id: titleId, text: title }));
    } else {
      const head = h(doc, "header", { class: "sheet-head" });
      head.appendChild(h(doc, "h2", { class: "sheet-title", id: titleId, text: title }));
      const close = h(doc, "button", {
        type: "button",
        class: "btn btn-icon btn-plain btn-sm",
        "aria-label": L("dialog_close"),
      });
      close.appendChild(icon(doc, "x"));
      close.addEventListener("click", () => handle.close());
      head.appendChild(close);
      dialog.appendChild(head);
    }
    dialog.appendChild(body);
    dialog.appendChild(foot);
    const handle = {
      dialog,
      body,
      foot,
      focusAfter: trigger,
      onClosed: null,
      close(spec) {
        if (spec) handle.focusAfter = spec;
        closeDialog(dialog);
      },
    };
    handle.open = () =>
      openDialog(dialog, trigger?.toolbar ? toolbarButton(trigger.toolbar) : null, () => {
        if (handle.onClosed) handle.onClosed();
        else focusControl(handle.focusAfter);
      });
    return handle;
  }

  /** The text of the error `code` for `field` of `kind` (group, card, bookmark). */
  function problemText(code, field, kind) {
    switch (code) {
      case "required":
        return L("err_required");
      case "too_long": {
        const max =
          field === "url"
            ? LIMITS.url
            : field === "description"
              ? LIMITS.description
              : kind === "group"
                ? LIMITS.groupTitle
                : LIMITS.title;
        return L("err_too_long", { max });
      }
      case "bad_url":
        return L("err_bad_url");
      case "bad_scheme":
        return L("err_bad_scheme");
      case "bad_emoji":
        return L("err_bad_emoji");
      case "bad_colour":
        return L("err_bad_colour");
      default:
        return L("err_generic");
    }
  }

  /** A labelled text field with room for help and an error message. */
  function field(name, label, value, { help = "", optional = false, inputmode = "", kind = "" } = {}) {
    const id = uid(`mm-f-${name}`);
    const wrap = h(doc, "div", { class: "field", "data-field": name });
    const labelEl = h(doc, "label", { class: "label", for: id }, [label]);
    if (optional) labelEl.appendChild(h(doc, "span", { class: "label-optional", text: ` ${L("optional")}` }));
    const input = h(doc, "input", {
      type: "text",
      id,
      name,
      autocomplete: "off",
      autocapitalize: kind === "url" ? "none" : undefined,
      spellcheck: kind === "url" ? "false" : undefined,
      inputmode: inputmode || undefined,
      dir: "auto",
    });
    input.value = value;
    wrap.appendChild(labelEl);
    wrap.appendChild(input);
    const described = [];
    if (help) {
      const helpId = `${id}-help`;
      wrap.appendChild(h(doc, "p", { class: "help", id: helpId, text: help }));
      described.push(helpId);
    }
    const errorId = `${id}-error`;
    wrap.appendChild(h(doc, "p", { class: "field-error", id: errorId, "data-for": name, hidden: true }));
    described.push(errorId);
    input.setAttribute("aria-describedby", described.join(" "));
    return wrap;
  }

  function colourField(selected) {
    const labelId = uid("mm-f-colour");
    const wrap = h(doc, "div", { class: "field", role: "radiogroup", "aria-labelledby": labelId, "data-field": "colour" });
    wrap.appendChild(h(doc, "span", { class: "label", id: labelId, text: L("field_colour") }));
    const swatches = h(doc, "div", { class: "mm-swatches" });
    for (const colour of PALETTE) {
      const radio = h(doc, "input", { type: "radio", name: "colour", value: colour, class: "vh" });
      if (colour === selected) radio.setAttribute("checked", "");
      const dot = h(doc, "span", { class: "mm-swatch-dot", "data-colour": colour, "aria-hidden": "true" });
      const name = h(doc, "span", { class: "vh", text: L(`colour_${colour}`) });
      swatches.appendChild(h(doc, "label", { class: "mm-swatch", "data-tip": L(`colour_${colour}`) }, [radio, dot, name]));
    }
    wrap.appendChild(swatches);
    wrap.appendChild(h(doc, "p", { class: "field-error", "data-for": "colour", hidden: true }));
    return wrap;
  }

  const chosenColour = (form) => {
    for (const radio of form.querySelectorAll('input[name="colour"]')) {
      // Where `checked` is not a property (a bare DOM), the attribute says.
      if (radio.checked ?? radio.hasAttribute("checked")) return radio.value;
    }
    return DEFAULT_COLOUR;
  };

  const valueOf = (form, name) => form.querySelector(`[name="${name}"]`)?.value ?? "";

  /** Shows `errors` (field to code) under the fields and puts focus on the first. */
  function showErrors(form, errors, kind) {
    let first = null;
    for (const message of form.querySelectorAll(".field-error[data-for]")) {
      const name = message.getAttribute("data-for");
      const input = form.querySelector(`input[name="${name}"]`);
      const code = errors[name];
      if (code) {
        message.textContent = problemText(code, name, kind);
        message.removeAttribute("hidden");
        if (name !== "colour" && input) input.setAttribute("aria-invalid", "true");
        if (!first && name !== "colour" && input) first = input;
      } else {
        message.textContent = "";
        message.setAttribute("hidden", "");
        if (input) input.removeAttribute("aria-invalid");
      }
    }
    if (first) first.focus();
  }

  /**
   * A sheet with a form: `fields` (elements), `read(form)` gives the values,
   * `check(values)` the errors, `apply(values)` makes the change and returns
   * the control to focus afterwards.
   */
  function formSheet({ title, kind, fields, read, check, apply, trigger, errors }) {
    const sheet = openSheet({ title, trigger });
    const formId = uid("mm-form");
    const form = h(doc, "form", { class: "form", id: formId, novalidate: true });
    for (const f of fields) form.appendChild(f);
    sheet.body.appendChild(form);
    const cancel = h(doc, "button", { type: "button", class: "btn btn-secondary", text: L("dialog_cancel") });
    cancel.addEventListener("click", () => sheet.close());
    const submitButton = h(doc, "button", { type: "submit", class: "btn btn-primary", form: formId, text: L("dialog_save") });
    sheet.foot.appendChild(cancel);
    sheet.foot.appendChild(submitButton);
    form.addEventListener("submit", (event) => {
      event.preventDefault();
      if (saving) return;
      const values = read(form);
      const found = check(values);
      if (Object.keys(found).length > 0) {
        showErrors(form, found, kind);
        return;
      }
      const spec = apply(values);
      notice = null;
      renderNotice();
      sheet.close(spec);
    });
    sheet.open();
    if (errors && Object.keys(errors).length > 0) showErrors(form, errors, kind);
    else form.querySelector("input")?.focus();
    return sheet;
  }

  function groupForm(group, trigger, errors) {
    const adding = !group;
    formSheet({
      title: adding ? L("group_form_add") : L("group_form_rename"),
      kind: "group",
      fields: [field("title", L("field_group_name"), group?.title ?? "", { kind: "group" })],
      read: (form) => ({ title: valueOf(form, "title").trim() }),
      check: checkGroup,
      trigger,
      errors,
      apply: ({ title }) => {
        if (adding) {
          draft = addGroup(draft, title);
          const id = draft.groups.at(-1).id;
          redraw();
          return { kind: "group", id, act: "add-card" };
        }
        draft = renameGroup(draft, group.id, title);
        redraw();
        return { kind: "group", id: group.id, act: "rename" };
      },
    });
  }

  function cardForm({ groupId, card }, trigger, errors) {
    const adding = !card;
    formSheet({
      title: adding ? L("card_form_add") : L("card_form_edit"),
      kind: "card",
      fields: [
        field("title", L("field_title"), card?.title ?? "", { kind: "card" }),
        field("url", L("field_url"), card?.url ?? "", { help: L("help_url"), inputmode: "url", kind: "url" }),
        field("description", L("field_description"), card?.description ?? "", { optional: true, kind: "card" }),
        field("emoji", L("field_emoji"), card?.icon?.emoji ?? "", { help: L("help_emoji"), optional: true, kind: "card" }),
        colourField(card?.icon?.colour ?? DEFAULT_COLOUR),
      ],
      read: (form) => ({
        title: valueOf(form, "title").trim(),
        url: normalizeUrl(valueOf(form, "url")),
        description: valueOf(form, "description").trim(),
        emoji: valueOf(form, "emoji").trim(),
        colour: chosenColour(form),
      }),
      check: checkCard,
      trigger,
      errors,
      apply: (values) => {
        const data = {
          title: values.title,
          description: values.description,
          url: values.url,
          // An empty field is null: the server refuses an empty string.
          icon: { emoji: values.emoji === "" ? null : values.emoji, colour: values.colour },
        };
        if (adding) {
          draft = addCard(draft, groupId, data);
          const id = draft.groups.find((g) => g.id === groupId).cards.at(-1).id;
          redraw();
          return { kind: "card", id, act: "edit" };
        }
        draft = updateCard(draft, card.id, data);
        redraw();
        return { kind: "card", id: card.id, act: "edit" };
      },
    });
  }

  function bookmarkForm(bookmark, trigger, errors) {
    const adding = !bookmark;
    formSheet({
      title: adding ? L("bookmark_form_add") : L("bookmark_form_edit"),
      kind: "bookmark",
      fields: [
        field("title", L("field_title"), bookmark?.title ?? "", { kind: "bookmark" }),
        field("url", L("field_url"), bookmark?.url ?? "", { help: L("help_url"), inputmode: "url", kind: "url" }),
      ],
      read: (form) => ({ title: valueOf(form, "title").trim(), url: normalizeUrl(valueOf(form, "url")) }),
      check: checkBookmark,
      trigger,
      errors,
      apply: (values) => {
        if (adding) {
          draft = addBookmark(draft, values);
          const id = draft.bookmarks.at(-1).id;
          redraw();
          return { kind: "bookmark", id, act: "edit" };
        }
        draft = updateBookmark(draft, bookmark.id, values);
        redraw();
        return { kind: "bookmark", id: bookmark.id, act: "edit" };
      },
    });
  }

  function applyMove(kind, id, key) {
    const list = kind === "group" ? draft.groups : kind === "bookmark" ? draft.bookmarks : null;
    if (kind === "card") {
      const group = draft.groups.find((g) => g.cards.some((c) => c.id === id));
      const at = group.cards.findIndex((c) => c.id === id);
      if (key === "up") draft = moveCard(draft, id, group.id, at - 1);
      else if (key === "down") draft = moveCard(draft, id, group.id, at + 1);
      else if (key.startsWith("group:")) draft = moveCard(draft, id, key.slice(6), Infinity);
    } else {
      const at = list.findIndex((x) => x.id === id);
      const target = key === "up" ? at - 1 : key === "down" ? at + 1 : key === "first" ? 0 : list.length - 1;
      draft = kind === "group" ? moveGroup(draft, id, target) : moveBookmark(draft, id, target);
    }
    redraw();
  }

  function moveMenu(kind, id, title) {
    const options = moveOptions(kind, id);
    if (options.length === 0) return;
    const trigger = { kind, id, act: "move" };
    const sheet = openSheet({ title: L("move_title", { title }), trigger });
    const list = h(doc, "div", { class: "mm-move-list" });
    for (const option of options) {
      const button = h(doc, "button", {
        type: "button",
        class: "btn btn-secondary btn-block",
        "data-move": option.key,
        text: option.label,
      });
      button.addEventListener("click", () => {
        applyMove(kind, id, option.key);
        notice = null;
        renderNotice();
        sheet.close(trigger);
      });
      list.appendChild(button);
    }
    sheet.body.appendChild(list);
    const cancel = h(doc, "button", { type: "button", class: "btn btn-secondary", text: L("dialog_cancel") });
    cancel.addEventListener("click", () => sheet.close());
    sheet.foot.appendChild(cancel);
    sheet.open();
    list.querySelector("button")?.focus();
  }

  /** A yes/no sheet. `onResult(yes)` runs after it has closed and decides the focus. */
  function confirmSheet({ title, message, yesLabel, noLabel, destructive, trigger, onResult }) {
    const sheet = openSheet({ title, trigger, alert: true });
    if (message) sheet.body.appendChild(h(doc, "p", { class: "alert-message", text: message }));
    let answer = false;
    const no = h(doc, "button", { type: "button", class: "btn btn-secondary", "data-answer": "no", text: noLabel });
    const yes = h(doc, "button", {
      type: "button",
      class: `btn ${destructive ? "btn-danger-solid" : "btn-primary"}`,
      "data-answer": "yes",
      text: yesLabel,
    });
    for (const [button, value] of [
      [no, false],
      [yes, true],
    ]) {
      button.addEventListener("click", () => {
        answer = value;
        sheet.close();
      });
    }
    sheet.foot.appendChild(no);
    sheet.foot.appendChild(yes);
    sheet.onClosed = () => onResult(answer);
    sheet.open();
    // The safe choice has focus.
    no.focus();
  }

  // -- actions --

  function limitReached(kind) {
    const counts = {
      group: [draft.groups.length, LIMITS.groups, "limit_groups"],
      card: [draft.groups.reduce((n, g) => n + g.cards.length, 0), LIMITS.cards, "limit_cards"],
      bookmark: [draft.bookmarks.length, LIMITS.bookmarks, "limit_bookmarks"],
    }[kind];
    if (counts[0] < counts[1]) return false;
    setNotice({ type: "limit", text: L(counts[2], { max: counts[1] }) });
    return true;
  }

  function changed(spec) {
    notice = null;
    redraw();
    renderNotice();
    focusControl(spec);
  }

  function deleteGroup(group) {
    const trigger = { kind: "group", id: group.id, act: "delete" };
    const remove = () => {
      draft = removeGroup(draft, group.id);
      changed({ toolbar: "add-group" });
    };
    if (group.cards.length === 0) return remove();
    confirmSheet({
      title: L("confirm_delete_group", { title: group.title }),
      message: L("confirm_delete_group_text"),
      yesLabel: L("confirm_delete"),
      noLabel: L("dialog_cancel"),
      destructive: true,
      trigger,
      onResult: (yes) => (yes ? remove() : focusControl(trigger)),
    });
  }

  function findCard(id) {
    for (const group of draft.groups) {
      const card = group.cards.find((c) => c.id === id);
      if (card) return { group, card };
    }
    return null;
  }

  /** Handles a click on one of the page's controls. */
  function handleClick(event) {
    const button = event.target.closest?.("button[data-act]");
    // While a save is under way the draft is what was sent: no changes.
    if (!button || button.hasAttribute("disabled") || !active || saving) return;
    const act = button.getAttribute("data-act");
    const owner = button.closest("[data-kind]");
    const kind = owner?.getAttribute("data-kind");
    const id = owner?.getAttribute("data-id");
    const trigger = owner ? { kind, id, act } : { toolbar: act };
    if (act === "add-group") {
      if (!limitReached("group")) groupForm(null, trigger);
    } else if (act === "add-bookmark") {
      if (!limitReached("bookmark")) bookmarkForm(null, trigger);
    } else if (act === "done") {
      done();
    } else if (act === "cancel") {
      cancel();
    } else if (act === "reload") {
      // Off with the page guards first: the draft is given up, and the browser
      // must not ask a second time.
      finish(getLayout(), { focus: false });
      reload();
    } else if (kind === "group") {
      const group = draft.groups.find((g) => g.id === id);
      if (!group) return;
      if (act === "rename") groupForm(group, trigger);
      else if (act === "move") moveMenu("group", id, group.title);
      else if (act === "delete") deleteGroup(group);
      else if (act === "add-card" && !limitReached("card")) cardForm({ groupId: id }, trigger);
    } else if (kind === "card") {
      const found = findCard(id);
      if (!found) return;
      if (act === "edit") cardForm({ groupId: found.group.id, card: found.card }, trigger);
      else if (act === "move") moveMenu("card", id, found.card.title);
      else if (act === "delete") {
        draft = removeCard(draft, id);
        changed({ kind: "group", id: found.group.id, act: "add-card" });
      }
    } else if (kind === "bookmark") {
      const bookmark = draft.bookmarks.find((b) => b.id === id);
      if (!bookmark) return;
      if (act === "edit") bookmarkForm(bookmark, trigger);
      else if (act === "move") moveMenu("bookmark", id, bookmark.title);
      else if (act === "delete") {
        draft = removeBookmark(draft, id);
        changed({ toolbar: "add-bookmark" });
      }
    }
  }

  // -- saving and leaving --

  /** The server's problems as lines for the summary: where, which field, what. */
  function summarise(fields) {
    const entries = [];
    for (const { path, code } of fields) {
      const at = parsePath(path);
      let where = "";
      let field = at.field;
      if (at.kind === "group") where = L("summary_group", { title: sent.groups[at.group]?.title ?? "" });
      else if (at.kind === "card") {
        where = L("summary_card", { title: sent.groups[at.group]?.cards[at.card]?.title ?? "" });
        field = field.replace("icon.", "");
      } else if (at.kind === "bookmark") where = L("summary_bookmark", { title: sent.bookmarks[at.bookmark]?.title ?? "" });
      const known = at.kind === "group" || at.kind === "card" || at.kind === "bookmark";
      let text;
      if (code === "too_many") {
        const key = path === "groups" ? "limit_groups" : path === "cards" ? "limit_cards" : "limit_bookmarks";
        text = L(key, { max: LIMITS[path] ?? "" });
      } else if (known) {
        text = L("summary_line", { where, field: fieldName(field), message: problemText(code, field, at.kind) });
      } else {
        text = `${path}: ${problemText(code, field, at.kind)}`;
      }
      entries.push({ path: known ? path : "", text });
    }
    return entries;
  }

  function fieldName(field) {
    const key = { title: "field_title", url: "field_url", description: "field_description", emoji: "field_emoji", colour: "field_colour" }[field];
    return key ? L(key) : field;
  }

  /** Opens the form the server's problem at `path` belongs to, with all that item's problems. */
  function openProblem(path, fields) {
    const at = parsePath(path);
    const same = (p) => {
      const other = parsePath(p);
      return other.kind === at.kind && other.group === at.group && other.card === at.card && other.bookmark === at.bookmark;
    };
    const errors = {};
    for (const { path: p, code } of fields) {
      if (same(p)) errors[parsePath(p).field.replace("icon.", "")] = code;
    }
    // The saved copy says which item it was; the draft is what the form edits.
    if (at.kind === "group") {
      const group = draft.groups.find((g) => g.id === sent.groups[at.group]?.id);
      if (group) groupForm(group, { toolbar: "done" }, errors);
    } else if (at.kind === "card") {
      const found = findCard(sent.groups[at.group]?.cards[at.card]?.id);
      if (found) cardForm({ groupId: found.group.id, card: found.card }, { toolbar: "done" }, errors);
    } else if (at.kind === "bookmark") {
      const bookmark = draft.bookmarks.find((b) => b.id === sent.bookmarks[at.bookmark]?.id);
      if (bookmark) bookmarkForm(bookmark, { toolbar: "done" }, errors);
    }
  }

  async function done() {
    if (saving || !active) return;
    if (!dirty()) {
      finish(getLayout());
      return;
    }
    saving = true;
    setBusy(true);
    setNotice(null);
    sent = structuredClone(draft);
    let result = null;
    try {
      result = await save(sent);
    } catch {
      result = null;
    }
    saving = false;
    if (!active) return;
    setBusy(false);
    const version = result?.body?.version;
    if (result?.status === 200 && Number.isFinite(version)) {
      finish({ ...sent, version });
    } else if (result?.status === 409) {
      setNotice({ type: "conflict" });
      focusControl({ toolbar: "done" });
    } else if (result?.status === 422 && Array.isArray(result.body?.fields)) {
      const fields = result.body.fields;
      problems = fields;
      setNotice({ type: "summary", entries: summarise(fields) });
      focusControl({ toolbar: "done" });
    } else {
      setNotice({ type: "failed", text: L("save_failed") });
      focusControl({ toolbar: "done" });
    }
  }

  function cancel() {
    if (!active || saving) return;
    if (!dirty()) {
      finish(getLayout());
      return;
    }
    confirmSheet({
      title: L("confirm_discard"),
      message: L("confirm_discard_text"),
      yesLabel: L("confirm_discard_yes"),
      noLabel: L("confirm_keep"),
      destructive: true,
      trigger: { toolbar: "cancel" },
      onResult: (yes) => {
        if (!yes) return focusControl({ toolbar: "cancel" });
        finish(getLayout());
        onDiscard();
      },
    });
  }

  /** Leaves edit mode and draws `layout` as a plain page. */
  function finish(layout, { focus = true } = {}) {
    if (!active) return;
    teardown();
    render(root, layout, { lang, labels: ui.labels });
    if (focus && editButton) editButton.focus();
  }

  function teardown() {
    active = false;
    for (const off of listeners.splice(0)) off();
    host.remove();
    root.removeAttribute("data-editing");
    if (editButton) editButton.setAttribute("aria-pressed", "false");
  }

  // Unsaved changes. The browser prompt covers leaving the page (a reload, a
  // typed address, a closed tab). A link or a form inside the app is followed by
  // htmx without a page load, so a capture-phase listener for each of them (it
  // runs before htmx) asks with a sheet and lets the visit go on after a "yes".
  // Back and Forward cannot be stopped: start.js keeps the draft then.
  on(win, "beforeunload", (event) => {
    if (!dirty()) return;
    event.preventDefault();
    event.returnValue = "";
  });

  /** Asks whether to leave; on yes the changes are given up and `proceed` runs. */
  function askToLeave(trigger, proceed) {
    confirmSheet({
      title: L("confirm_leave"),
      message: L("confirm_leave_text"),
      yesLabel: L("confirm_leave_yes"),
      noLabel: L("confirm_keep"),
      destructive: true,
      trigger,
      onResult: (yes) => {
        if (!yes) return focusControl(trigger);
        // The plain page goes into the history, not the edit controls.
        finish(getLayout(), { focus: false });
        onDiscard();
        proceed();
      },
    });
  }

  on(
    doc,
    "click",
    (event) => {
      if (!active || !dirty() || event.defaultPrevented) return;
      if (event.button || event.metaKey || event.ctrlKey || event.shiftKey || event.altKey) return;
      const link = event.target.closest?.("a[href]");
      if (!link || link.closest("dialog") || link.getAttribute("target") === "_blank") return;
      const href = link.getAttribute("href");
      if (href.startsWith("#") || /^\s*javascript:/i.test(href)) return;
      event.preventDefault();
      event.stopPropagation();
      askToLeave({ el: link }, () => navigate(href));
    },
    true,
  );
  on(
    doc,
    "submit",
    (event) => {
      if (!active || !dirty() || event.defaultPrevented) return;
      const form = event.target;
      // The editor's own forms are in sheets.
      if (!form?.closest || form.closest("dialog")) return;
      event.preventDefault();
      event.stopPropagation();
      const submitter = event.submitter ?? null;
      const trigger = { el: submitter ?? form.querySelector("button, [type=submit]") };
      askToLeave(trigger, () => {
        // The guard is off now, so this submit goes through (to htmx, if it boosts the form).
        try {
          form.requestSubmit(submitter ?? undefined);
        } catch {
          form.requestSubmit();
        }
      });
    },
    true,
  );

  on(root, "click", handleClick);
  on(host, "click", (event) => {
    handleClick(event);
    const entry = event.target.closest?.("button[data-path]");
    if (entry && !saving) openProblem(entry.getAttribute("data-path"), problems);
  });

  // -- drag and drop (the HTML drag API) --
  // A card, a bookmark or a group's grip is dragged; where it lands is worked
  // out from the pointer over the thing under it: before or after it, or at the
  // end of a group. The move menu does the same moves from the keyboard.

  const draggable = (el) => el?.closest?.('[draggable="true"]') ?? null;

  function clearMark() {
    if (marked) for (const name of ["mm-drop-before", "mm-drop-after", "mm-drop-end"]) marked.classList.remove(name);
    marked = null;
  }

  function mark(el, className) {
    if (marked !== el) clearMark();
    marked = el;
    el.classList.remove("mm-drop-before", "mm-drop-after", "mm-drop-end");
    el.classList.add(className);
  }

  // Whether the pointer is in the first half of `el` (the top, or the start side
  // of the line: the right half in a right-to-left page).
  const before = (el, event, axis) => {
    const rect = el.getBoundingClientRect();
    if (axis === "y") return event.clientY < rect.top + rect.height / 2;
    const rtl = win.getComputedStyle?.(el)?.direction === "rtl";
    return rtl ? event.clientX > rect.left + rect.width / 2 : event.clientX < rect.left + rect.width / 2;
  };

  /** Where the dragged thing would land: `{ groupId?, index, el, className }` or null. */
  function target(event) {
    if (!drag || saving) return null;
    const under = event.target;
    if (drag.kind === "card") {
      const cardEl = under.closest?.('[data-kind="card"]');
      const groupEl = under.closest?.('[data-kind="group"]');
      if (!groupEl) return null;
      const groupId = groupEl.getAttribute("data-id");
      const cards = groupEl.querySelectorAll('[data-kind="card"]');
      if (cardEl) {
        const index = [...cards].indexOf(cardEl);
        const first = before(cardEl, event, "x");
        return { groupId, index: index + (first ? 0 : 1), el: cardEl, className: first ? "mm-drop-before" : "mm-drop-after" };
      }
      return { groupId, index: cards.length, el: groupEl.querySelector(".mm-cards"), className: "mm-drop-end" };
    }
    if (drag.kind === "group") {
      const groupEl = under.closest?.('[data-kind="group"]');
      if (!groupEl) return null;
      const index = draft.groups.findIndex((g) => g.id === groupEl.getAttribute("data-id"));
      const first = before(groupEl, event, "y");
      return { index: index + (first ? 0 : 1), el: groupEl, className: first ? "mm-drop-before" : "mm-drop-after" };
    }
    const pillEl = under.closest?.('[data-kind="bookmark"]');
    if (!pillEl) return null;
    const index = draft.bookmarks.findIndex((b) => b.id === pillEl.getAttribute("data-id"));
    const first = before(pillEl, event, "x");
    return { index: index + (first ? 0 : 1), el: pillEl, className: first ? "mm-drop-before" : "mm-drop-after" };
  }

  on(root, "dragstart", (event) => {
    const source = draggable(event.target);
    const owner = source?.closest("[data-kind]");
    if (!owner) return;
    if (saving) {
      event.preventDefault();
      return;
    }
    drag = { kind: owner.getAttribute("data-kind"), id: owner.getAttribute("data-id") };
    owner.classList.add("mm-dragging");
    const transfer = event.dataTransfer;
    if (transfer) {
      transfer.effectAllowed = "move";
      // Firefox starts no drag without data.
      transfer.setData("text/plain", drag.id);
      if (typeof transfer.setDragImage === "function") transfer.setDragImage(owner, 16, 16);
    }
  });
  on(root, "dragover", (event) => {
    const spot = target(event);
    if (!spot) return clearMark();
    event.preventDefault();
    if (event.dataTransfer) event.dataTransfer.dropEffect = "move";
    mark(spot.el, spot.className);
  });
  on(root, "drop", (event) => {
    const spot = target(event);
    const dragged = drag;
    endDrag();
    if (!spot || !dragged || saving) return;
    event.preventDefault();
    if (dragged.kind === "card") {
      const from = findCard(dragged.id);
      if (!from) return;
      let index = spot.index;
      if (from.group.id === spot.groupId && from.group.cards.findIndex((c) => c.id === dragged.id) < index) index -= 1;
      draft = moveCard(draft, dragged.id, spot.groupId, index);
    } else {
      const list = dragged.kind === "group" ? draft.groups : draft.bookmarks;
      let index = spot.index;
      if (list.findIndex((x) => x.id === dragged.id) < index) index -= 1;
      draft = dragged.kind === "group" ? moveGroup(draft, dragged.id, index) : moveBookmark(draft, dragged.id, index);
    }
    changed(null);
  });
  on(root, "dragend", endDrag);
  function endDrag() {
    clearMark();
    for (const el of root.querySelectorAll(".mm-dragging")) el.classList.remove("mm-dragging");
    drag = null;
  }

  // -- go --

  root.parentNode.insertBefore(host, root);
  if (editButton) editButton.setAttribute("aria-pressed", "true");
  redraw();
  focusControl({ toolbar: "add-group" });

  return {
    root,
    done,
    cancel,
    /** Leaves edit mode without asking or saving (the page is going away). */
    stop: (options) => finish(getLayout(), options),
    /** Takes the editor's listeners off, without touching the page. */
    dispose: teardown,
    isActive: () => active,
    isDirty: dirty,
    draft: () => structuredClone(draft),
  };
}
