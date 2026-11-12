// Language and theme switches, the word-by-word lede, the clock, the timeline, the jump
// palette and the live flow drawing. No dependencies; the only request is for the checksum.

const reduced = matchMedia("(prefers-reduced-motion: reduce)").matches;
const store = {
  get(key) {
    try {
      return localStorage.getItem(key);
    } catch {
      return null;
    }
  },
  set(key, value) {
    try {
      localStorage.setItem(key, value);
    } catch {
      /* not remembered, still applied */
    }
  },
};

// --- the lede arrives word by word; highlighted phrases and links move as one piece
let wordsShown = false;
function splitWords(el) {
  let i = 0;
  const wrap = (node) => {
    const w = document.createElement("span");
    w.className = "lab-intro-w";
    w.style.setProperty("--d", `${120 + i++ * 45}ms`);
    w.append(node);
    return w;
  };
  const out = [];
  for (const n of [...el.childNodes]) {
    if (n.nodeType !== 3) {
      out.push(wrap(n));
      continue;
    }
    for (const t of n.textContent.split(/(\s+)/)) if (t) out.push(/^\s+$/.test(t) ? " " : wrap(t));
  }
  el.replaceChildren(...out);
  el.classList.toggle("no-anim", wordsShown || reduced);
  wordsShown = true;
}

// --- English / Tiếng Việt: elements keep their Vietnamese text in data-vi
const TITLES = {
  en: "Confluo: every network you have, flowing into one download",
  vi: "Confluo: mọi đường mạng bạn có, chảy về một lần tải",
};
const nodes = [...document.querySelectorAll("[data-vi]")];
for (const n of nodes) n.dataset.en = n.innerHTML;
function setLang(lang) {
  const vi = lang === "vi";
  for (const n of nodes) n.innerHTML = vi ? n.dataset.vi : n.dataset.en;
  document.querySelectorAll("[data-words]").forEach(splitWords);
  for (const n of document.querySelectorAll("[data-vi-ph]")) {
    n.dataset.enPh ??= n.placeholder;
    n.placeholder = vi ? n.dataset.viPh : n.dataset.enPh;
  }
  document.documentElement.lang = vi ? "vi" : "en";
  document.title = TITLES[vi ? "vi" : "en"];
  for (const b of document.querySelectorAll("[data-lang]")) b.setAttribute("aria-pressed", String(b.dataset.lang === lang));
  store.set("lang", vi ? "vi" : "en");
  document.dispatchEvent(new Event("lang"));
}
setLang(store.get("lang") || ((navigator.language || "").toLowerCase().startsWith("vi") ? "vi" : "en"));
for (const b of document.querySelectorAll("[data-lang]")) b.addEventListener("click", () => setLang(b.dataset.lang));

// --- light / dark: a circle grows from the button into the new theme
document.getElementById("theme").addEventListener("click", (e) => {
  const dark = document.documentElement.dataset.theme !== "dark";
  const apply = () => {
    document.documentElement.dataset.theme = dark ? "dark" : "light";
    store.set("theme", dark ? "dark" : "light");
    document.dispatchEvent(new Event("theme"));
  };
  if (!document.startViewTransition || reduced) return apply();
  const x = e.clientX || innerWidth - 60;
  const y = e.clientY || 30;
  const r = Math.hypot(Math.max(x, innerWidth - x), Math.max(y, innerHeight - y));
  document.startViewTransition(apply).ready.then(() =>
    document.documentElement.animate(
      { clipPath: [`circle(0 at ${x}px ${y}px)`, `circle(${r}px at ${x}px ${y}px)`] },
      { duration: 520, easing: "cubic-bezier(0.2, 0.8, 0.2, 1)", pseudoElement: "::view-transition-new(root)" },
    ),
  );
});

// --- sections ease in as they arrive
const io = new IntersectionObserver(
  (entries) => {
    for (const e of entries) {
      if (!e.isIntersecting) continue;
      e.target.classList.add("in");
      io.unobserve(e.target);
    }
  },
  { rootMargin: "0px 0px -12% 0px" },
);
document.querySelectorAll(".reveal").forEach((el, i) => {
  el.style.transitionDelay = `${(i % 2) * 70}ms`;
  io.observe(el);
});

// --- the clock in the readout
const clockEl = document.getElementById("clock");
const tickClock = () => (clockEl.textContent = new Date().toLocaleTimeString("en-US"));
tickClock();
setInterval(tickClock, 1000);

// --- copy the install command; phones send the page to a PC instead
const PAGE = "https://confluo.nhaajt.com";
async function copyText(btn, text) {
  const vi = document.documentElement.lang === "vi";
  try {
    await navigator.clipboard.writeText(text);
    const old = btn.textContent;
    btn.textContent = vi ? "Đã chép ✓" : "Copied ✓";
    setTimeout(() => (btn.textContent = old), 1600);
  } catch {
    /* clipboard refused: the text stays selectable */
  }
}
async function shareLink(btn) {
  const vi = document.documentElement.lang === "vi";
  const text = vi ? "Confluo: tải file qua mọi mạng cùng lúc. Cài trên máy tính Windows:" : "Confluo: one download over every network. Install it on a Windows PC:";
  try {
    if (navigator.share) return await navigator.share({ title: "Confluo", text, url: PAGE });
  } catch {
    return;
  }
  copyText(btn, PAGE);
}
for (const b of document.querySelectorAll(".copy-cmd")) {
  b.addEventListener("click", () => copyText(b, b.parentElement.querySelector(".cmdtext").textContent));
}
document.getElementById("shareLink")?.addEventListener("click", (e) => shareLink(e.currentTarget));
document.getElementById("copyLink")?.addEventListener("click", (e) => copyText(e.currentTarget, PAGE));
for (const b of document.querySelectorAll(".share-again")) b.addEventListener("click", () => shareLink(b));

// --- the installer's checksum, written next to it at publish time
fetch("download/SHA256SUMS", { cache: "no-cache" })
  .then((r) => (r.ok ? r.text() : ""))
  .then((t) => {
    const hash = t.trim().split(/\s+/)[0];
    if (/^[0-9a-f]{64}$/.test(hash)) document.getElementById("sha").textContent = hash;
  })
  .catch(() => {});

// --- the film pauses when it leaves the screen
for (const v of document.querySelectorAll("video")) {
  new IntersectionObserver(([e]) => {
    if (!e.isIntersecting) v.pause();
  }).observe(v);
}

// --- timeline: years across, one bar per thing, a line at today
const T0 = 2024;
const YEARS = 6;
const PX = 150;
const BARS = [
  { from: 2026.84, row: 0, cls: "is-current", en: ["Confluo 0.1", "Windows · 2026"], vi: ["Confluo 0.1", "Windows · 2026"] },
  { from: 2026.86, row: 1, cls: "", en: ["Browser hand-off", "Chrome · Edge · Firefox"], vi: ["Nhận tải từ trình duyệt", "Chrome · Edge · Firefox"] },
  { from: 2026.87, row: 2, cls: "", en: ["One-line install", "PowerShell"], vi: ["Cài bằng một dòng lệnh", "PowerShell"] },
  { from: 2027.05, row: 3, cls: "is-future", en: ["macOS and Linux app", "planned"], vi: ["App cho macOS và Linux", "dự kiến"] },
  { from: 2024.76, row: 4, cls: "is-muted is-faint", href: "https://tauri.app", en: ["Tauri 2", "the shell"], vi: ["Tauri 2", "khung app"] },
];
const tlTrack = document.getElementById("tlTrack");
const tlScroll = tlTrack.parentElement;
const tlX = (year) => 24 + (year - T0) * PX;
function buildTimeline() {
  const vi = document.documentElement.lang === "vi";
  const d = new Date();
  const now = tlX(d.getFullYear() + (d.getMonth() + d.getDate() / 31) / 12);
  const mk = (tag, cls, left) => {
    const n = document.createElement(tag);
    n.className = cls;
    if (left != null) n.style.left = `${left}px`;
    return n;
  };
  const out = [];
  for (let y = 0; y < YEARS; y++) {
    const tick = mk("span", tlX(T0 + y) > now ? "tl-tick is-future" : "tl-tick", tlX(T0 + y));
    tick.textContent = T0 + y;
    out.push(tick, mk("span", "tl-grid", tlX(T0 + y)));
    if (y < YEARS - 1) for (let q = 1; q < 4; q++) out.push(mk("span", "tl-grid tl-grid--q", tlX(T0 + y + q / 4)));
  }
  out.push(mk("span", "tl-future", now));
  const line = mk("span", "tl-now", now);
  line.append(document.createElement("i"));
  out.push(line);
  for (const b of BARS) {
    const bar = mk(b.href ? "a" : "div", `tl-bar ${b.cls}`, tlX(b.from));
    if (b.href) Object.assign(bar, { href: b.href, target: "_blank", rel: "noopener" });
    bar.style.top = `${49 + b.row * 56}px`;
    const [name, sub] = vi ? b.vi : b.en;
    const label = document.createElement("b");
    label.textContent = name;
    const note = document.createElement("span");
    note.textContent = sub;
    bar.append(label, note);
    out.push(bar);
  }
  tlTrack.style.width = `${tlX(T0 + YEARS - 1) + 24}px`;
  tlTrack.style.height = `${49 + 5 * 56}px`;
  tlTrack.replaceChildren(...out);
  tlScroll.scrollLeft = now - tlScroll.clientWidth / 2;
}
buildTimeline();
document.addEventListener("lang", buildTimeline);
let dragFrom = null;
tlScroll.addEventListener("pointerdown", (e) => {
  if (e.pointerType !== "mouse" || e.target.closest("a")) return;
  dragFrom = e.clientX + tlScroll.scrollLeft;
  tlScroll.classList.add("is-drag");
});
addEventListener("pointermove", (e) => {
  if (dragFrom != null) tlScroll.scrollLeft = dragFrom - e.clientX;
});
addEventListener("pointerup", () => {
  dragFrom = null;
  tlScroll.classList.remove("is-drag");
});

// --- jump palette: Ctrl+K, type to filter, arrows and Enter
const JUMPS = [
  ["#top", "Top", "Đầu trang"],
  ["#film", "The 30-second film", "Phim 30 giây"],
  ["#timeline", "Timeline", "Dòng thời gian"],
  ["#features", "What it does", "Làm được gì"],
  ["#how", "Three steps", "Ba bước"],
  ["#demo", "Live flow", "Dòng chảy trực tiếp"],
  ["#numbers", "Measured numbers", "Số đo"],
  ["#private", "Privacy", "Riêng tư"],
  ["#faq", "FAQ", "FAQ"],
  ["#install", "Install", "Cài đặt"],
  ["download/Confluo-setup.exe", "Download .exe", "Tải file .exe"],
  ["https://github.com/nhaajtt/confluo", "Source on GitHub", "Mã nguồn trên GitHub"],
];
const jump = document.getElementById("jump");
const jumpIn = document.getElementById("jumpIn");
const jumpList = document.getElementById("jumpList");
let jumpAt = 0;
function drawJump() {
  const vi = document.documentElement.lang === "vi";
  const q = jumpIn.value.trim().toLowerCase();
  const rows = JUMPS.filter((r) => `${r[1]} ${r[2]}`.toLowerCase().includes(q));
  jumpAt = Math.max(0, Math.min(jumpAt, rows.length - 1));
  jumpList.replaceChildren(
    ...rows.map((r, i) => {
      const li = document.createElement("li");
      li.setAttribute("role", "option");
      li.setAttribute("aria-selected", String(i === jumpAt));
      li.dataset.to = r[0];
      li.textContent = vi ? r[2] : r[1];
      const hint = document.createElement("span");
      hint.textContent = r[0].startsWith("#") ? r[0] : r[0].startsWith("http") ? "↗" : "↓";
      li.append(hint);
      return li;
    }),
  );
  jumpList.querySelector('[aria-selected="true"]')?.scrollIntoView({ block: "nearest" });
}
function goJump(to) {
  if (!to) return;
  jump.close();
  if (to.startsWith("#")) document.querySelector(to)?.scrollIntoView();
  else location.href = to;
}
function openJump() {
  if (jump.open) return jump.close();
  jumpIn.value = "";
  jumpAt = 0;
  drawJump();
  jump.showModal();
}
document.getElementById("jumpOpen").addEventListener("click", openJump);
addEventListener("keydown", (e) => {
  if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === "k") {
    e.preventDefault();
    openJump();
  }
});
jumpIn.addEventListener("input", () => {
  jumpAt = 0;
  drawJump();
});
jumpIn.addEventListener("keydown", (e) => {
  if (e.key === "ArrowDown" || e.key === "ArrowUp") {
    e.preventDefault();
    jumpAt += e.key === "ArrowDown" ? 1 : -1;
    drawJump();
  }
  if (e.key === "Enter") goJump(jumpList.querySelector('[aria-selected="true"]')?.dataset.to);
});
jumpList.addEventListener("click", (e) => goJump(e.target.closest("li")?.dataset.to));
jump.addEventListener("click", (e) => {
  if (e.target === jump) jump.close();
});

(() => {
  "use strict";
  const $ = (s) => document.querySelector(s);
  const L = () => (document.documentElement.lang === "vi" ? "vi" : "en");
  // ---- Hero: three networks as threads joining one stream, and a minute of throughput. ----
  const threads = $("#threads");
  const chart = $("#chart");
  if (!threads || !chart) return;
  const css = (n) => getComputedStyle(document.documentElement).getPropertyValue(n).trim();
  const MIB = 1048576;
  const nets = [
    { vi: "ETHERNET", en: "ETHERNET", base: 31, wob: 2.4, color: "--s1" },
    { vi: "WI-FI", en: "WI-FI", base: 12.5, wob: 3.2, color: "--s2" },
    { vi: "4G TỪ ĐIỆN THOẠI", en: "PHONE HOTSPOT", base: 6.2, wob: 2.1, color: "--s3" },
  ];
  const SPAN = 60;
  const STEP = 0.25;
  const history = [];
  let clock = 0;

  // A scripted minute that repeats: ramp-up, steady, the hotspot dropping out and returning.
  function sample(t) {
    const loop = t % 48;
    const ramp = Math.min(1, loop / 2.2);
    return nets.map((n, i) => {
      const noise = Math.sin(t * (0.9 + i * 0.37)) * 0.5 + Math.sin(t * (2.3 + i)) * 0.3 + Math.sin(t * 5.1 + i * 2) * 0.2;
      let v = (n.base + noise * n.wob) * ramp * Math.min(1, Math.max(0, (loop - i * 0.9) / 1.4));
      if (i === 2 && loop > 26 && loop < 33) v *= Math.max(0, 1 - (loop - 26) * 3) + Math.max(0, (loop - 32) * 1);
      if (i === 1 && loop > 38 && loop < 41) v *= 0.45;
      return Math.max(0, v) * MIB;
    });
  }
  function push(t) {
    const routes = sample(t);
    history.push({ t, routes, total: routes.reduce((a, b) => a + b, 0) });
    while (history.length && history[0].t < t - SPAN) history.shift();
  }
  // Start with a full minute already drawn, part-way through the script.
  clock = 48 * 3 + 21;
  for (let t = clock - SPAN; t <= clock; t += STEP) push(t);

  function prep(canvas) {
    const rect = canvas.getBoundingClientRect();
    if (rect.width < 2) return null;
    const dpr = Math.min(devicePixelRatio || 1, 2);
    const w = Math.round(rect.width * dpr);
    const h = Math.round(rect.height * dpr);
    if (canvas.width !== w || canvas.height !== h) {
      canvas.width = w;
      canvas.height = h;
    }
    const ctx = canvas.getContext("2d");
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    ctx.clearRect(0, 0, rect.width, rect.height);
    return { ctx, w: rect.width, h: rect.height };
  }
  const fmt = (v) => (v / MIB).toFixed(1);

  let phase = 0;
  function drawThreads(now) {
    const p = prep(threads);
    if (!p) return;
    const { ctx, w, h } = p;
    const total = Math.max(1, now.total);
    const mono = css("--mono");
    const dim = css("--tt");
    const labelW = Math.min(150, w * 0.42);
    const nodeX = labelW + 12;
    const mergeX = w - 40;
    const mergeY = h / 2;
    nets.forEach((n, i) => {
      const y = 26 + (i * (h - 52)) / (nets.length - 1);
      const v = now.routes[i];
      const on = v > 0.2 * MIB;
      const color = css(n.color);
      ctx.textAlign = "right";
      ctx.textBaseline = "alphabetic";
      ctx.font = `10px ${mono}`;
      ctx.fillStyle = dim;
      ctx.fillText(n[L()], labelW, y - 3);
      ctx.font = `600 12px ${mono}`;
      ctx.fillStyle = on ? color : dim;
      ctx.fillText(on ? `${fmt(v)} MiB/s` : L() === "vi" ? "mất kết nối" : "dropped", labelW, y + 12);
      const span = mergeX - nodeX;
      ctx.beginPath();
      ctx.moveTo(nodeX, y);
      ctx.bezierCurveTo(nodeX + span * 0.5, y, mergeX - span * 0.42, mergeY, mergeX - 10, mergeY);
      ctx.strokeStyle = color;
      ctx.lineCap = "round";
      ctx.globalAlpha = on ? 0.95 : 0.3;
      ctx.lineWidth = on ? 1.5 + 6 * (v / total) : 1.5;
      ctx.setLineDash(on ? [8, 8] : [2, 7]);
      ctx.lineDashOffset = -phase * (on ? 16 + 24 * Math.log10(1 + v / 65536) : 0);
      ctx.stroke();
      ctx.setLineDash([]);
      ctx.globalAlpha = on ? 1 : 0.5;
      ctx.beginPath();
      ctx.arc(nodeX, y, 4, 0, Math.PI * 2);
      ctx.fillStyle = color;
      ctx.fill();
      ctx.globalAlpha = 1;
    });
    const ac = css("--ac");
    ctx.beginPath();
    ctx.arc(mergeX, mergeY, 8, 0, Math.PI * 2);
    ctx.strokeStyle = ac;
    ctx.lineWidth = 1.5;
    ctx.stroke();
    ctx.beginPath();
    ctx.arc(mergeX, mergeY, 3, 0, Math.PI * 2);
    ctx.fillStyle = ac;
    ctx.fill();
    ctx.textAlign = "center";
    ctx.font = `9px ${mono}`;
    ctx.fillStyle = dim;
    ctx.fillText(L() === "vi" ? "GỘP LẠI" : "COMBINED", mergeX - 6, mergeY - 16);
    ctx.beginPath();
    ctx.moveTo(mergeX + 10, mergeY);
    ctx.lineTo(w - 8, mergeY);
    ctx.strokeStyle = css("--ts");
    ctx.lineWidth = 3;
    ctx.lineCap = "butt";
    ctx.stroke();
    ctx.beginPath();
    ctx.moveTo(w, mergeY);
    ctx.lineTo(w - 9, mergeY - 5.5);
    ctx.lineTo(w - 9, mergeY + 5.5);
    ctx.closePath();
    ctx.fillStyle = css("--ts");
    ctx.fill();
  }

  function drawChart() {
    const p = prep(chart);
    if (!p) return;
    const { ctx, w, h } = p;
    const mono = css("--mono");
    const dim = css("--tt");
    const top = 60 * MIB;
    const end = history[history.length - 1].t;
    const X = (t) => w - ((end - t) / SPAN) * w;
    const Y = (v) => h - 1 - (Math.min(v, top) / top) * (h - 14);
    ctx.font = `10px ${mono}`;
    ctx.textBaseline = "bottom";
    for (const v of [20, 40, 60]) {
      const y = Math.round(Y(v * MIB)) + 0.5;
      ctx.strokeStyle = css("--border");
      ctx.lineWidth = 1;
      ctx.beginPath();
      ctx.moveTo(0, y);
      ctx.lineTo(w, y);
      ctx.stroke();
      ctx.fillStyle = dim;
      ctx.textAlign = "left";
      ctx.fillText(`${v} MiB/s`, 5, y - 2);
    }
    const base = history.map(() => 0);
    nets.forEach((n, i) => {
      const next = history.map((s, k) => base[k] + s.routes[i]);
      ctx.beginPath();
      history.forEach((s, k) => (k ? ctx.lineTo(X(s.t), Y(next[k])) : ctx.moveTo(X(s.t), Y(next[k]))));
      for (let k = history.length - 1; k >= 0; k--) ctx.lineTo(X(history[k].t), Y(base[k]));
      ctx.closePath();
      ctx.fillStyle = css(n.color);
      ctx.globalAlpha = 0.5;
      ctx.fill();
      ctx.globalAlpha = 1;
      next.forEach((v, k) => (base[k] = v));
    });
    ctx.beginPath();
    history.forEach((s, k) => (k ? ctx.lineTo(X(s.t), Y(s.total)) : ctx.moveTo(X(s.t), Y(s.total))));
    ctx.strokeStyle = css("--tp");
    ctx.lineWidth = 1.5;
    ctx.lineJoin = "round";
    ctx.stroke();
  }

  const totalEl = $("#total");
  const avgEl = $("#avgpeak");
  function readout(now) {
    let peak = 0;
    let sum = 0;
    for (const s of history) {
      peak = Math.max(peak, s.total);
      sum += s.total;
    }
    totalEl.textContent = fmt(now.total);
    avgEl.textContent =
      L() === "vi" ? `tb ${fmt(sum / history.length)} · đỉnh ${fmt(peak)}` : `avg ${fmt(sum / history.length)} · peak ${fmt(peak)}`;
  }

  function frame() {
    const now = history[history.length - 1];
    drawThreads(now);
    drawChart();
    readout(now);
  }
  frame();
  addEventListener("resize", frame);
  document.addEventListener("lang", frame);
  document.addEventListener("theme", frame);
  if (reduced) return;

  // Runs only while the drawing is on screen and the tab is visible.
  let visible = true;
  let raf = 0;
  let last = 0;
  let acc = 0;
  const tickFrame = (ts) => {
    raf = 0;
    const dt = last ? Math.min(0.1, (ts - last) / 1000) : 0;
    last = ts;
    phase += dt;
    acc += dt;
    while (acc >= STEP) {
      acc -= STEP;
      clock += STEP;
      push(clock);
    }
    frame();
    if (visible && !document.hidden) raf = requestAnimationFrame(tickFrame);
  };
  const wake = () => {
    if (!raf && visible && !document.hidden) {
      last = 0;
      raf = requestAnimationFrame(tickFrame);
    }
  };
  new IntersectionObserver((e) => {
    visible = e[0].isIntersecting;
    wake();
  }).observe($("#demo"));
  document.addEventListener("visibilitychange", wake);
  wake();
})();
