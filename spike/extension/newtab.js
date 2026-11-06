import { nextView } from "./state.js";

const statusText = {
  fresh: "Fresh from server",
  cached: "Cached, checking server...",
  offline: "Server unreachable, showing cached data",
  unauthorized: "Token rejected, showing cached data (check the options page)",
  empty: "No data yet. Open the extension options and set server URL and token.",
};

function render(view) {
  document.getElementById("status").textContent = statusText[view.status];
  const meta = document.getElementById("meta");
  const list = document.getElementById("items");
  list.replaceChildren();
  if (!view.data) {
    meta.textContent = "";
    return;
  }
  meta.textContent = `generated_at ${view.data.generated_at}, counter ${view.data.counter}`;
  for (const item of view.data.items) {
    const li = document.createElement("li");
    const b = document.createElement("b");
    b.textContent = item.label;
    li.append(b, document.createTextNode(item.value));
    list.append(li);
  }
}

async function fetchData(serverUrl, token) {
  if (!serverUrl || !token) return { ok: false, kind: "network" };
  try {
    const res = await fetch(`${serverUrl.replace(/\/+$/, "")}/api/spike/data`, {
      headers: { Authorization: `Bearer ${token}` },
    });
    if (res.status === 401) return { ok: false, kind: "unauthorized" };
    if (!res.ok) return { ok: false, kind: "network" };
    return { ok: true, data: await res.json() };
  } catch {
    return { ok: false, kind: "network" };
  }
}

async function start() {
  const stored = await browser.storage.local.get(["last", "serverUrl", "token"]);
  const cached = stored.last ?? null;
  render(nextView(cached, undefined));
  const ms = Math.round(performance.now());
  document.getElementById("timing").textContent = `vist på ${ms} ms`;

  const result = await fetchData(stored.serverUrl, stored.token);
  const view = nextView(cached, result);
  render(view);
  if (view.status === "fresh") {
    await browser.storage.local.set({ last: view.data });
  }
}

document.addEventListener("DOMContentLoaded", start);
