// Language switch, copy button, and the hero drawing. No dependencies, no network calls.
(() => {
  "use strict";
  const $ = (s) => document.querySelector(s);
  const reduced = matchMedia("(prefers-reduced-motion: reduce)").matches;

  // ---- Language: English in the markup, Vietnamese in data-vi. ----
  const nodes = [...document.querySelectorAll("[data-vi]")];
  for (const n of nodes) n.dataset.en = n.textContent;
  let lang = "en";
  function setLang(next) {
    lang = next === "vi" ? "vi" : "en";
    document.documentElement.lang = lang;
    for (const n of nodes) n.textContent = n.dataset[lang];
    for (const b of document.querySelectorAll("[data-lang]")) b.setAttribute("aria-pressed", String(b.dataset.lang === lang));
    document.title =
      lang === "vi" ? "Weft: mọi đường mạng bạn có, dệt thành một lần tải" : "Weft: every network you have, woven into one download";
    try {
      localStorage.setItem("weft.lang", lang);
    } catch {
      /* not remembered, still applied */
    }
  }
  let saved = null;
  try {
    saved = localStorage.getItem("weft.lang");
  } catch {
    /* storage unavailable */
  }
  setLang(saved ?? ((navigator.language || "").toLowerCase().startsWith("vi") ? "vi" : "en"));
  for (const b of document.querySelectorAll("[data-lang]")) b.addEventListener("click", () => setLang(b.dataset.lang));

  // ---- Copy the install command. ----
  const copy = $("#copy");
  copy?.addEventListener("click", async () => {
    const text = $("#cmd").textContent;
    try {
      await navigator.clipboard.writeText(text);
    } catch {
      const r = document.createRange();
      r.selectNodeContents($("#cmd"));
      const sel = getSelection();
      sel.removeAllRanges();
      sel.addRange(r);
      return;
    }
    copy.classList.add("ok");
    copy.textContent = lang === "vi" ? copy.dataset.doneVi : copy.dataset.doneEn;
    setTimeout(() => {
      copy.classList.remove("ok");
      copy.textContent = copy.dataset[lang];
    }, 1600);
  });

  // ---- The installer's checksum, written next to it at publish time. ----
  fetch("download/SHA256SUMS", { cache: "no-cache" })
    .then((r) => (r.ok ? r.text() : ""))
    .then((t) => {
      const hash = t.trim().split(/\s+/)[0];
      if (/^[0-9a-f]{64}$/.test(hash)) $("#sha").textContent = hash;
    })
    .catch(() => {});

  // ---- Sections ease in once. ----
  const reveal = [...document.querySelectorAll(".block, .facts, .closer")];
  if (!reduced && "IntersectionObserver" in window) {
    for (const el of reveal) el.classList.add("reveal");
    const io = new IntersectionObserver(
      (entries) => {
        for (const e of entries) {
          if (e.isIntersecting) {
            e.target.classList.add("in");
            io.unobserve(e.target);
          }
        }
      },
      { rootMargin: "0px 0px -8% 0px" },
    );
    for (const el of reveal) io.observe(el);
  }

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
      ctx.fillText(n[lang], labelW, y - 3);
      ctx.font = `600 12px ${mono}`;
      ctx.fillStyle = on ? color : dim;
      ctx.fillText(on ? `${fmt(v)} MiB/s` : lang === "vi" ? "mất kết nối" : "dropped", labelW, y + 12);
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
    ctx.fillText(lang === "vi" ? "GỘP LẠI" : "COMBINED", mergeX - 6, mergeY - 16);
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
      ctx.strokeStyle = css("--line");
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
      lang === "vi" ? `tb ${fmt(sum / history.length)} · đỉnh ${fmt(peak)}` : `avg ${fmt(sum / history.length)} · peak ${fmt(peak)}`;
  }

  function frame() {
    const now = history[history.length - 1];
    drawThreads(now);
    drawChart();
    readout(now);
  }
  frame();
  addEventListener("resize", frame);
  for (const b of document.querySelectorAll("[data-lang]")) b.addEventListener("click", frame);
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
  }).observe($(".stage"));
  document.addEventListener("visibilitychange", wake);
  wake();
})();
