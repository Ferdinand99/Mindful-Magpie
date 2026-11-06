const serverUrlInput = document.getElementById("serverUrl");
const tokenInput = document.getElementById("token");
const msg = document.getElementById("msg");

let savedServerUrl = "";

browser.storage.local.get(["serverUrl", "token"]).then((s) => {
  savedServerUrl = s.serverUrl ?? "";
  serverUrlInput.value = savedServerUrl;
  tokenInput.value = s.token ?? "";
});

document.getElementById("form").addEventListener("submit", async (e) => {
  e.preventDefault();
  const serverUrl = serverUrlInput.value.trim().replace(/\/+$/, "");
  const token = tokenInput.value.trim();
  const url = new URL(serverUrl);
  // Match patterns take no port, so build the pattern from protocol and hostname only.
  // permissions.request must run inside the user gesture, so do it before any other await.
  const granted = await browser.permissions.request({ origins: [`${url.protocol}//${url.hostname}/*`] });
  if (serverUrl !== savedServerUrl) {
    // Cached data belongs to the old server, so drop it.
    await browser.storage.local.remove("last");
  }
  await browser.storage.local.set({ serverUrl, token });
  savedServerUrl = serverUrl;
  msg.textContent = granted ? "Saved." : "Saved, but host permission was denied; fetches will fail.";
});
