// Settings for the Confluo extension. Nothing leaves the browser except to 127.0.0.1.
const api = globalThis.browser ?? globalThis.chrome;
const $ = (id) => document.getElementById(id);

function show(text, kind) {
  const el = $("state");
  el.textContent = text;
  el.className = kind ?? "";
}

async function check() {
  const reply = await api.runtime.sendMessage({ type: "ping" });
  const { token } = await api.storage.local.get({ token: "" });
  if (!reply?.found) show("Confluo is not running on this computer. Open it, then come back here.", "bad");
  else if (!token) show("Confluo is running. Paste the pairing code from Confluo: Setup, then Browser.", "bad");
  else if (!reply.paired) show("Confluo is running, but this pairing code is not its code. Copy it again from Confluo: Setup, then Browser.", "bad");
  else show(`Connected to Confluo ${reply.version}.`, "ok");
}

async function load() {
  const s = await api.storage.local.get({ token: "", capture: true, cookies: false });
  $("token").value = s.token;
  $("capture").checked = s.capture;
  $("cookies").checked = s.cookies;
  await check();
}

$("save").addEventListener("click", async () => {
  let cookies = $("cookies").checked;
  if (cookies) {
    // Reading cookies is only asked for when the user turns this on.
    cookies = await api.permissions.request({ permissions: ["cookies"], origins: ["<all_urls>"] });
    $("cookies").checked = cookies;
  }
  await api.storage.local.set({ token: $("token").value.trim(), capture: $("capture").checked, cookies });
  await check();
});

load();
