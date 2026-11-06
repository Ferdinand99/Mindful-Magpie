const serverUrlInput = document.getElementById("serverUrl");
const tokenInput = document.getElementById("token");
const msg = document.getElementById("msg");

browser.storage.local.get(["serverUrl", "token"]).then((s) => {
  serverUrlInput.value = s.serverUrl ?? "";
  tokenInput.value = s.token ?? "";
});

document.getElementById("form").addEventListener("submit", async (e) => {
  e.preventDefault();
  const serverUrl = serverUrlInput.value.trim().replace(/\/+$/, "");
  const token = tokenInput.value.trim();
  const origin = new URL(serverUrl).origin;
  // permissions.request must run inside the user gesture, so do it before any other await.
  const granted = await browser.permissions.request({ origins: [`${origin}/*`] });
  await browser.storage.local.set({ serverUrl, token });
  msg.textContent = granted ? "Saved." : "Saved, but host permission was denied; fetches will fail.";
});
