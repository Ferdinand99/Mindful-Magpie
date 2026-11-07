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
  if (connectButton.disabled) return undefined; // a run is already going (a second Enter)
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

// «Koble til Magpie» is the first submit button of the form, so it is the form's default button:
// Enter in the address field clicks it. «Lagre» and «Test tilkobling» are plain buttons.
connectButton.addEventListener("click", (event) => {
  event.preventDefault();
  run(connectMagpie, "Kobler til …");
});
testButton.addEventListener("click", () => run(checkConnection, "Tester …"));
saveButton.addEventListener("click", () => run(saveConnection, "Tester …"));
// Enter in the token field saves the pasted token (the form's default button would connect).
tokenInput.addEventListener("keydown", (event) => {
  if (event.key !== "Enter") return;
  event.preventDefault();
  run(saveConnection, "Tester …");
});
// Never let the form navigate the page.
document.getElementById("form").addEventListener("submit", (event) => event.preventDefault());
