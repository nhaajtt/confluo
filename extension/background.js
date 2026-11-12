// Hands downloads from the browser to Confluo on this computer.
//
// The only network destination is http://127.0.0.1 on one of three fixed ports.
// A download is taken away from the browser only after Confluo has accepted it;
// if Confluo is not running or not paired, the browser simply downloads as usual.
const api = globalThis.browser ?? globalThis.chrome;
const PORTS = [47610, 47611, 47612];
let knownPort = null;

async function settings() {
  return api.storage.local.get({ token: "", capture: true, cookies: false });
}

const text = new TextEncoder();

function hex(bytes) {
  return [...new Uint8Array(bytes)].map((b) => b.toString(16).padStart(2, "0")).join("");
}

// HMAC-SHA256 with the pairing code as the key. The code itself never leaves the
// browser: both sides only show each other what they can compute with it.
async function sign(token, message) {
  const key = await crypto.subtle.importKey("raw", text.encode(token), { name: "HMAC", hash: "SHA-256" }, false, ["sign"]);
  return hex(await crypto.subtle.sign("HMAC", key, text.encode(message)));
}

// Looks for Confluo on the three ports. A listener only counts as Confluo's paired
// bridge when it can sign a fresh random number with the pairing code, so
// another program sitting on the port learns nothing and receives no downloads.
async function find(token) {
  const order = knownPort ? [knownPort, ...PORTS.filter((p) => p !== knownPort)] : PORTS;
  let unpaired = null;
  for (const port of order) {
    try {
      const nonce = hex(crypto.getRandomValues(new Uint8Array(16)));
      const res = await fetch(`http://127.0.0.1:${port}/v1/ping?nonce=${nonce}`, { signal: AbortSignal.timeout(1500) });
      const body = await res.json().catch(() => ({}));
      if (body.app !== "confluo") continue;
      if (token && body.proof === (await sign(token, nonce))) {
        knownPort = port;
        return { port, paired: true, version: body.version };
      }
      unpaired ??= { port, paired: false, version: body.version };
    } catch {
      // Nothing listening on this port; try the next.
    }
  }
  knownPort = null;
  return unpaired;
}

async function ping() {
  const { token } = await settings();
  const r = await find(token);
  return r ? { found: true, paired: r.paired, version: r.version } : { found: false };
}

async function cookieHeader(url) {
  const { cookies } = await settings();
  if (!cookies || !api.cookies) return undefined;
  try {
    const list = await api.cookies.getAll({ url });
    return list.length ? list.map((c) => `${c.name}=${c.value}`).join("; ") : undefined;
  } catch {
    return undefined;
  }
}

async function offer(url, extra = {}) {
  if (!/^https?:\/\//i.test(url)) return false;
  const { token } = await settings();
  if (!token) return false;
  const bridge = await find(token);
  if (!bridge?.paired) return false;
  const body = JSON.stringify({ url, cookie: await cookieHeader(url), user_agent: navigator.userAgent, ...extra, ts: Date.now() });
  try {
    const res = await fetch(`http://127.0.0.1:${bridge.port}/v1/offer`, {
      method: "POST",
      headers: { "Content-Type": "application/json", Authorization: `Confluo-HMAC ${await sign(token, body)}` },
      body,
      signal: AbortSignal.timeout(3000),
    });
    return res.ok;
  } catch {
    return false;
  }
}

api.runtime.onInstalled.addListener(() => {
  api.contextMenus.create({ id: "confluo-link", title: "Download with Confluo", contexts: ["link"] });
});

api.contextMenus.onClicked.addListener(async (info) => {
  if (info.menuItemId === "confluo-link" && info.linkUrl) {
    await offer(info.linkUrl, { referrer: info.pageUrl });
  }
});

const IS_FIREFOX = typeof browser !== "undefined" && typeof browser.runtime?.getBrowserInfo === "function";

// Downloads this extension started itself, which it must not take over again.
const own = new Set();
const pause = (ms) => new Promise((r) => setTimeout(r, ms));

// Gives a held download back to the browser. If it will not resume, this asks again for
// a few seconds and then starts it afresh rather than leaving it stuck.
async function giveBack(id, url) {
  for (let i = 0; i < 12; i++) {
    try {
      await api.downloads.resume(id);
    } catch {
      // asked again below
    }
    await pause(250);
    const [now] = await api.downloads.search({ id });
    if (!now || now.state !== "in_progress" || !now.paused) return;
  }
  own.add(url);
  try {
    await api.downloads.cancel(id);
    await api.downloads.erase({ id });
  } catch {
    // already gone
  }
  await api.downloads.download({ url });
}

api.downloads.onCreated.addListener(async (item) => {
  const { capture, token } = await settings();
  const url = item.finalUrl || item.url;
  if (own.delete(url)) return;
  // Without a pairing code nothing can be handed over, so the download is left alone.
  if (!capture || !token || !/^https?:\/\//i.test(url) || item.state !== "in_progress") return;
  // Chromium can hold a download while the app is asked, so nothing is fetched twice.
  // Firefox cannot resume a download that was paused before its first bytes arrived, so
  // there the download keeps running for the few milliseconds the question takes.
  let held = false;
  if (!IS_FIREFOX) {
    try {
      await api.downloads.pause(item.id);
      held = true;
    } catch {
      return;
    }
  }
  const name = item.filename ? item.filename.split(/[\\/]/).pop() : undefined;
  const taken = await offer(url, { file_name: name || undefined, referrer: item.referrer || undefined });
  if (!taken) {
    if (held) await giveBack(item.id, url);
    return;
  }
  try {
    await api.downloads.cancel(item.id);
  } catch {
    // It finished before it could be cancelled; the browser's copy is removed below.
  }
  try {
    const [now] = await api.downloads.search({ id: item.id });
    if (now?.state === "complete") await api.downloads.removeFile(item.id);
  } catch {
    // nothing on disk to remove
  }
  try {
    await api.downloads.erase({ id: item.id });
  } catch {
    // already gone from the list
  }
});

api.runtime.onMessage.addListener((msg, _sender, respond) => {
  if (msg?.type === "ping") {
    ping().then(respond);
    return true;
  }
  return false;
});
