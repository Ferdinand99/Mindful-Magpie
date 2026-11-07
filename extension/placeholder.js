// Placeholder drawing for the new-tab page. Task C replaces renderPlaceholder with the shared
// renderer of the start page. Everything from the server is set with textContent, never as markup.
import { statusText } from "./state.js";

// Fills the status line. The "empty" status is a link to the options page: a link, not an input,
// because the new-tab page must stay free of form controls to keep the address bar focused.
export function renderStatus(el, status, openOptions) {
  const text = statusText[status] ?? "";
  el.replaceChildren();
  if (status !== "empty") {
    el.textContent = text;
    return;
  }
  const link = el.ownerDocument.createElement("a");
  link.setAttribute("href", "#");
  link.textContent = text;
  link.addEventListener("click", (event) => {
    event.preventDefault();
    openOptions();
  });
  el.append(link);
}

// A plain list of group titles with their card titles. No links, no icons: only a check that the
// data arrives.
export function renderPlaceholder(root, data) {
  root.replaceChildren();
  if (!data) return;
  const doc = root.ownerDocument;
  for (const group of data.groups) {
    const section = doc.createElement("section");
    const heading = doc.createElement("h2");
    heading.textContent = String(group?.title ?? "");
    const list = doc.createElement("ul");
    for (const card of Array.isArray(group?.cards) ? group.cards : []) {
      const item = doc.createElement("li");
      item.textContent = String(card?.title ?? "");
      list.append(item);
    }
    section.append(heading, list);
    root.append(section);
  }
}
