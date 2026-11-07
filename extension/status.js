// The status line of the new-tab page. Everything is set with textContent, never as markup.
import { statusText } from "./state.js";

// Fills the status line. The "empty" status (not connected) gets a «Koble til Magpie» button that
// opens the options page, where the connect flow runs. It is a button, not an input: the new-tab
// page must stay free of form controls that take focus, to keep the address bar focused.
export function renderStatus(el, status, openOptions) {
  const text = statusText[status] ?? "";
  el.replaceChildren();
  el.textContent = text;
  if (status !== "empty") return;
  const button = el.ownerDocument.createElement("button");
  button.setAttribute("type", "button");
  button.setAttribute("class", "connect");
  button.textContent = "Koble til Magpie";
  button.addEventListener("click", (event) => {
    event.preventDefault();
    openOptions();
  });
  el.append(" ", button);
}
