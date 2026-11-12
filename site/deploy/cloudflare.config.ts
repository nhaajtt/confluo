import { bindings, defineConfig } from "cf/config";

export default defineConfig({
  worker: {
    name: "confluo-site",
    compatibilityDate: "2026-10-06",
    entrypoint: "./src/index.js",
    domains: ["confluo.nhaajt.com"],
    workersDev: false,
    env: { ASSETS: bindings.assets() },
    assets: { notFoundHandling: "404-page", runWorkerFirst: ["/media/*"] },
  },
});
