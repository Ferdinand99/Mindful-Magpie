// Mindful Magpie: the service form.
//
// «Test» can show the fingerprint of a certificate that TLS did not accept,
// with a button that carries it in `data-use-fingerprint` and names the field
// in `data-target` (an id). Pressing it puts the fingerprint in the field. It
// saves nothing: the admin still has to save the form.

/** Puts the button's fingerprint in its target field; whether it did. */
export function useFingerprint(doc, button) {
  const value = button.getAttribute("data-use-fingerprint") || "";
  const input = doc.getElementById(button.getAttribute("data-target") || "");
  if (!value || !input || input.tagName !== "INPUT") return false;
  input.value = value;
  return true;
}

/** One listener for the whole page: the buttons arrive with the test result. */
export function init(doc) {
  doc.addEventListener("click", (event) => {
    const target = event.target;
    const button = target && typeof target.closest === "function" ? target.closest("[data-use-fingerprint]") : null;
    if (button) useFingerprint(doc, button);
  });
}

if (typeof document !== "undefined") init(document);
