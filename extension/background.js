// Hands downloads from the browser to Weft on this computer.
//
// The only network destination is http://127.0.0.1 on one of three fixed ports.
// A download is taken away from the browser only after Weft has accepted it;
// if Weft is not running or not paired, the browser simply downloads as usual.
const api = globalThis.browser ?? globalThis.chrome;
const PORTS = [47610, 47611, 47612];
let knownPort = null;

async function settings() {
  return api.storage.local.get({ token: "", capture: true, cookies: false });
}

async function call(path, init) {
  const order = knownPort ? [knownPort, ...PORTS.filter((p) => p !== knownPort)] : PORTS;
  for (const port of order) {
    try {
      const res = await fetch(`http://127.0.0.1:${port}${path}`, { ...init, signal: AbortSignal.timeout(1500) });
      const body = await res.json().catch(() => ({}));
      if (body.app === "weft" || path !== "/v1/ping") {
        knownPort = port;
        return { ok: res.ok, status: res.status, body };
      }
    } catch {
      // Nothing listening on this port; try the next.
    }
  }
  knownPort = null;
  return null;
}

async function ping() {
  const { token } = await settings();
  const r = await call("/v1/ping", { headers: token ? { Authorization: `Bearer ${token}` } : {} });
  return r ? { found: true, paired: !!r.body.paired, version: r.body.version } : { found: false };
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
  const body = { url, cookie: await cookieHeader(url), user_agent: navigator.userAgent, ...extra };
  const r = await call("/v1/offer", {
    method: "POST",
    headers: { "Content-Type": "application/json", Authorization: `Bearer ${token}` },
    body: JSON.stringify(body),
  });
  return !!r?.ok;
}

api.runtime.onInstalled.addListener(() => {
  api.contextMenus.create({ id: "weft-link", title: "Download with Weft", contexts: ["link"] });
});

api.contextMenus.onClicked.addListener(async (info) => {
  if (info.menuItemId === "weft-link" && info.linkUrl) {
    await offer(info.linkUrl, { referrer: info.pageUrl });
  }
});

api.downloads.onCreated.addListener(async (item) => {
  const { capture } = await settings();
  const url = item.finalUrl || item.url;
  if (!capture || !/^https?:\/\//i.test(url) || item.state !== "in_progress") return;
  // Hold the download in the browser while asking, so nothing is fetched twice.
  try {
    await api.downloads.pause(item.id);
  } catch {
    return;
  }
  const name = item.filename ? item.filename.split(/[\\/]/).pop() : undefined;
  const taken = await offer(url, { file_name: name || undefined, referrer: item.referrer || undefined });
  try {
    if (taken) {
      await api.downloads.cancel(item.id);
      await api.downloads.erase({ id: item.id });
    } else {
      await api.downloads.resume(item.id);
    }
  } catch {
    // The download finished or was removed in the meantime.
  }
});

api.runtime.onMessage.addListener((msg, _sender, respond) => {
  if (msg?.type === "ping") {
    ping().then(respond);
    return true;
  }
  return false;
});
