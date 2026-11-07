import { checkConnection, saveConnection } from "./options-core.js";

const serverUrlInput = document.getElementById("serverUrl");
const tokenInput = document.getElementById("token");
const result = document.getElementById("result");
const testButton = document.getElementById("test");
const saveButton = document.getElementById("save");

const deps = { permissions: browser.permissions, storage: browser.storage, fetchImpl: fetch };

browser.storage.local.get(["serverUrl", "token"]).then((stored) => {
  serverUrlInput.value = stored.serverUrl ?? "";
  tokenInput.value = stored.token ?? "";
});

function show(outcome) {
  result.textContent = outcome.message;
  result.className = outcome.ok ? "result ok" : "result error";
}

// Both handlers call into options-core synchronously from the click, so that the host
// permission request still counts as a user gesture. The token is only read from the field and
// passed on; it is not logged and not part of any message.
function run(action) {
  const pending = action(deps, { serverUrl: serverUrlInput.value, token: tokenInput.value });
  testButton.disabled = true;
  saveButton.disabled = true;
  result.className = "result";
  result.textContent = "Tester …";
  pending.then(show, () => show({ ok: false, message: "Noe gikk galt." })).finally(() => {
    testButton.disabled = false;
    saveButton.disabled = false;
  });
}

testButton.addEventListener("click", () => run(checkConnection));
document.getElementById("form").addEventListener("submit", (event) => {
  event.preventDefault();
  run(saveConnection);
});
