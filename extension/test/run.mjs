// Loads the real extension into Chromium and Firefox and drives it against the real app.
//   CHROMIUM=path/to/chrome.exe node run.mjs <repo> chromium
//   FIREFOX=path/to/firefox.exe node run.mjs <repo> firefox
// Needs release builds of confluo-app and confluo-testsrv, and nothing else on port 47610.
import { spawn } from "node:child_process";
import { mkdtempSync, readFileSync, writeFileSync, mkdirSync, readdirSync, existsSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";

const [repo, which] = process.argv.slice(2);
const rel = path.join(repo, "target", "release");
const ext = path.join(repo, "extension");
const work = mkdtempSync(path.join(tmpdir(), "confluo-ext-"));
const state = path.join(work, "state");
const confluoDir = path.join(work, "confluo-downloads");
const browserDir = path.join(work, "browser-downloads");
for (const d of [state, confluoDir, browserDir]) mkdirSync(d, { recursive: true });
const PORT = 8772;
const SIZE = 3 * 1024 * 1024;
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const results = [];
const check = (name, ok, detail = "") => {
  results.push({ name, ok });
  console.log(`${ok ? "PASS" : "FAIL"}  ${name}${detail ? "  — " + detail : ""}`);
};
const procs = [];
const start = (exe, args, env) => {
  const p = spawn(exe, args, { env: { ...process.env, ...env }, stdio: "ignore" });
  procs.push(p);
  return p;
};
const items = () => {
  try {
    return JSON.parse(readFileSync(path.join(state, "downloads.json"), "utf8")).items ?? [];
  } catch {
    return [];
  }
};
const until = async (fn, ms = 15000) => {
  const end = Date.now() + ms;
  while (Date.now() < end) {
    const v = await fn();
    if (v) return v;
    await sleep(150);
  }
  return null;
};
const browserFiles = () => readdirSync(browserDir).filter((f) => !f.endsWith(".crdownload") && !f.endsWith(".part"));

let cleanup = async () => {};
try {
  start(path.join(rel, "confluo-testsrv.exe"), ["--bind", `127.0.0.1:${PORT}`, "--size", String(SIZE), "--rate-total", "2m"]);
  writeFileSync(
    path.join(state, "downloads.json"),
    JSON.stringify({
      settings: { download_dir: confluoDir, max_concurrent: 3, connections_per_network: 32, speed_limit: 0, network_limits: {}, networks: { mode: "system" } },
      next_id: 1,
      items: [],
    }),
  );
  writeFileSync(path.join(state, "bridge-autostart"), "1");
  start(path.join(rel, "confluo-app.exe"), [], { CONFLUO_STATE_DIR: state });
  const alive = await until(async () => {
    try {
      return (await (await fetch("http://127.0.0.1:47610/v1/ping")).json()).app === "confluo";
    } catch {
      return false;
    }
  }, 30000);
  if (!alive) throw new Error("Confluo did not start");
  const token = readFileSync(path.join(state, "bridge-token"), "utf8").trim();
  const page1 = `http://127.0.0.1:${PORT}/file`;

  // What the page does: a plain download link, clicked like a person would.
  const linkPage = (name) =>
    "data:text/html," + encodeURIComponent(`<a id="dl" href="${page1}?n=${name}" download="${name}.bin">get</a>`);

  let openOptions, fill, stateText, clickLink, browserName;
  if (which === "chromium") {
    const { chromium } = await import("playwright-core");
    const exe = process.env.CHROMIUM; // a Chromium or Chrome for Testing binary (branded Chrome no longer loads unpacked extensions from the command line)
    const ctx = await chromium.launchPersistentContext(path.join(work, "profile"), {
      executablePath: exe,
      headless: false,
      acceptDownloads: true,
      downloadsPath: browserDir,
      args: [`--disable-extensions-except=${ext}`, `--load-extension=${ext}`, "--no-first-run", "--window-size=900,700"],
    });
    cleanup = () => ctx.close();
    browserName = `Chromium ${ctx.browser()?.version() ?? ""}`;
    const sw = ctx.serviceWorkers()[0] ?? (await ctx.waitForEvent("serviceworker", { timeout: 15000 }));
    const id = new URL(sw.url()).host;
    check("extension loaded (service worker running)", !!id, `${browserName}, id ${id}`);
    const opt = await ctx.newPage();
    openOptions = async () => opt.goto(`chrome-extension://${id}/options.html`);
    fill = async (t) => {
      await opt.fill("#token", t);
      await opt.click("#save");
    };
    stateText = async () => (await opt.textContent("#state")) ?? "";
    const pg = await ctx.newPage();
    pg.on("download", (d) => d.saveAs(path.join(browserDir, d.suggestedFilename())).catch(() => {}));
    clickLink = async (name) => {
      await pg.goto(linkPage(name));
      await pg.click("#dl");
    };
  } else {
    const { Builder, By } = await import("selenium-webdriver");
    const firefox = (await import("selenium-webdriver/firefox.js")).default;
    const uuid = "6f1c2f0e-8f2b-4d55-9a3e-2b7c1d9e4a10";
    const o = new firefox.Options();
    o.setBinary("C:\\Program Files\\Mozilla Firefox\\firefox.exe");
    o.setPreference("extensions.webextensions.uuids", JSON.stringify({ "confluo@confluo.dev": uuid }));
    o.setPreference("browser.download.folderList", 2);
    o.setPreference("browser.download.dir", browserDir);
    o.setPreference("browser.download.useDownloadDir", true);
    o.setPreference("browser.download.always_ask_before_handling_new_types", false);
    o.setPreference("browser.helperApps.neverAsk.saveToDisk", "application/octet-stream");
    o.setPreference("browser.download.manager.showWhenStarting", false);
    const driver = await new Builder()
      .forBrowser("firefox")
      .setFirefoxOptions(o)
      .setFirefoxService(new firefox.ServiceBuilder().addArguments("--allow-system-access"))
      .build();
    cleanup = () => driver.quit();
    const caps = await driver.getCapabilities();
    browserName = `Firefox ${caps.get("browserVersion")}`;
    const addon = await driver.installAddon(ext, true);
    check("extension loaded (temporary add-on installed)", addon === "confluo@confluo.dev", `${browserName}, id ${addon}`);
    // WebDriver may not navigate to an extension page; the browser itself may open one.
    openOptions = async () => {
      await driver.setContext("chrome");
      await driver.executeScript(
        "const t = gBrowser.addTab(arguments[0], { triggeringPrincipal: Services.scriptSecurityManager.getSystemPrincipal() }); gBrowser.selectedTab = t;",
        `moz-extension://${uuid}/options.html`,
      );
      await driver.setContext("content");
      await sleep(800);
      const handles = await driver.getAllWindowHandles();
      await driver.switchTo().window(handles[handles.length - 1]);
      await driver.wait(async () => (await driver.findElements(By.id("token"))).length > 0, 10000);
    };
    fill = async (t) => {
      const el = await driver.findElement(By.id("token"));
      await el.clear();
      await el.sendKeys(t);
      await driver.findElement(By.id("save")).click();
    };
    stateText = async () => driver.findElement(By.id("state")).getText();
    clickLink = async (name) => {
      await driver.switchTo().newWindow("tab");
      await driver.get(linkPage(name));
      await driver.findElement(By.id("dl")).click();
    };
  }

  await openOptions();
  const first = await until(async () => {
    const t = await stateText();
    return t && !/^\s*$/.test(t) ? t : null;
  });
  check("options page finds the running app, unpaired", /pairing code does not match/i.test(first ?? ""), first ?? "");

  // 1. Wrong code: the browser must keep its download, Confluo must get nothing.
  await fill("00".repeat(24));
  await sleep(600);
  check("wrong pairing code is reported", /does not match/i.test(await stateText()), await stateText());
  await clickLink("unpaired");
  const kept = await until(() => browserFiles().length > 0, 20000);
  check("unpaired: the browser downloads the file itself", !!kept, browserFiles().join(","));
  check("unpaired: nothing reached Confluo", items().length === 0);

  // 2. Right code: Confluo takes the download, the browser's copy is cancelled.
  await openOptions();
  await fill(token);
  const paired = await until(async () => (/Connected to Confluo/.test(await stateText()) ? await stateText() : null));
  check("pairing with the real code", !!paired, paired ?? (await stateText()));
  const before = browserFiles().length;
  await clickLink("paired");
  const got = await until(() => items().find((i) => i.urls?.[0]?.includes("n=paired")), 15000);
  check("paired: the download is handed to Confluo", !!got, got ? `${got.file_name}` : "");
  const done = await until(() => items().find((i) => i.urls?.[0]?.includes("n=paired") && i.status === "completed"), 30000);
  check("paired: Confluo completes it", !!done && done.size === SIZE, done ? `${done.size} bytes` : JSON.stringify(items().map((i) => i.status)));
  await sleep(1500);
  check("paired: the browser did not also download it", browserFiles().length === before, browserFiles().join(","));
  const confluoFile = done?.path && existsSync(done.path);
  check("paired: the file is on disk in Confluo's folder", !!confluoFile, done?.path ?? "");
} catch (e) {
  check("run completed without an exception", false, String(e?.stack ?? e).slice(0, 600));
} finally {
  try {
    await cleanup();
  } catch {}
  for (const p of procs) {
    try {
      spawn("taskkill", ["/F", "/T", "/PID", String(p.pid)], { stdio: "ignore" });
    } catch {}
  }
  await sleep(1200);
  try {
    rmSync(work, { recursive: true, force: true });
  } catch {}
  const failed = results.filter((r) => !r.ok).length;
  console.log(`${results.length - failed}/${results.length} passed`);
  process.exit(failed ? 1 : 0);
}
