import { checkConnection, connectMagpie, saveConnection } from "./options-core.js";

const serverUrlInput = document.getElementById("serverUrl");
const tokenInput = document.getElementById("token");
const result = document.getElementById("result");
const connectButton = document.getElementById("connect");
const testButton = document.getElementById("test");
const saveButton = document.getElementById("save");

const deps = {
  permissions: browser.permissions,
  storage: browser.storage,
  fetchImpl: fetch,
  identity: browser.identity,
  runtime: browser.runtime,
};

// Fills the fields from storage. The address field is the source of the server address for every
// button, so the click handlers never wait for a storage read before asking for the permission.
function loadFields() {
  return browser.storage.local.get(["serverUrl", "token"]).then(
    (stored) => {
      serverUrlInput.value = stored.serverUrl ?? "";
      tokenInput.value = stored.token ?? "";
    },
    () => {},
  );
}
loadFields();

function show(outcome) {
  result.textContent = outcome.message;
  result.className = outcome.ok ? "result ok" : "result error";
}

// Every handler calls into options-core synchronously from the click, so that the host
// permission request still counts as a user gesture. The token is only read from the field and
// passed on; it is not logged and not part of any message.
function run(action, busyText) {
  const pending = action(deps, { serverUrl: serverUrlInput.value, token: tokenInput.value });
  for (const button of [connectButton, testButton, saveButton]) button.disabled = true;
  result.className = "result";
  result.textContent = busyText;
  return pending
    .then(
      (outcome) => {
        show(outcome);
        // Show what was stored (the token of a connect flow), but never overwrite typed input
        // after a failure.
        return outcome.ok ? loadFields() : undefined;
      },
      () => show({ ok: false, message: "Noe gikk galt." }),
    )
    .finally(() => {
      for (const button of [connectButton, testButton, saveButton]) button.disabled = false;
    });
}

connectButton.addEventListener("click", () => run(connectMagpie, "Kobler til …"));
testButton.addEventListener("click", () => run(checkConnection, "Tester …"));
document.getElementById("form").addEventListener("submit", (event) => {
  event.preventDefault();
  run(saveConnection, "Tester …");
});
