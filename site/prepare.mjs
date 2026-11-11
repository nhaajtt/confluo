// Puts the installer and its checksum where the site expects them.
//
//   node site/prepare.mjs [https://weft.nhaajt.com]
//
// Output goes to site/dist/, a complete copy of the site ready to upload:
// the pages, download/Weft-setup.exe, download/SHA256SUMS and install.ps1 with
// the checksum and the address filled in.
import { createHash } from "node:crypto";
import { cpSync, mkdirSync, readFileSync, readdirSync, rmSync, writeFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const here = path.dirname(fileURLToPath(import.meta.url));
const root = path.dirname(here);
const base = (process.argv[2] ?? "https://weft.nhaajt.com").replace(/\/+$/, "");
const nsis = path.join(root, "target", "release", "bundle", "nsis");
const setup = readdirSync(nsis).find((f) => /^Weft_.*-setup\.exe$/.test(f));
if (!setup) throw new Error(`no installer in ${nsis}; run "pnpm tauri build" in app/ first`);

const dist = path.join(here, "dist");
rmSync(dist, { recursive: true, force: true });
mkdirSync(path.join(dist, "download"), { recursive: true });
for (const f of ["index.html", "style.css", "main.js", "theme.js", "backdrop.js", "icon.png", "_headers", "fonts"]) {
  cpSync(path.join(here, f), path.join(dist, f), { recursive: true });
}
const bytes = readFileSync(path.join(nsis, setup));
const sha = createHash("sha256").update(bytes).digest("hex");
writeFileSync(path.join(dist, "download", "Weft-setup.exe"), bytes);
writeFileSync(path.join(dist, "download", "SHA256SUMS"), `${sha}  Weft-setup.exe\n`);

const fill = (text) => text.replaceAll("https://weft.nhaajt.com", base).replaceAll("__SHA256__", sha);
writeFileSync(path.join(dist, "install.ps1"), fill(readFileSync(path.join(here, "install.ps1"), "utf8")));
writeFileSync(path.join(dist, "index.html"), fill(readFileSync(path.join(here, "index.html"), "utf8")));

console.log(`${setup}  ${(bytes.length / 1048576).toFixed(2)} MiB  sha256 ${sha}`);
console.log(`site ready in ${dist} for ${base}`);
