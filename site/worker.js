// Runs only for /media/*. Static hosting here answers a Range request with the whole
// file, and Safari will not play a video without real byte ranges, so this slices the
// stored file itself. Everything else on the site is served as plain files.
export default {
  async fetch(request, env) {
    if (request.method !== "GET" && request.method !== "HEAD") {
      return new Response("Method not allowed", { status: 405, headers: { Allow: "GET, HEAD" } });
    }
    const range = request.headers.get("Range");
    const plain = new Headers(request.headers);
    plain.delete("Range");
    plain.delete("If-Range");
    const asset = await env.ASSETS.fetch(new Request(request.url, { method: "GET", headers: plain }));
    if (asset.status !== 200) return asset;

    const headers = new Headers(asset.headers);
    headers.set("Accept-Ranges", "bytes");
    if (!range) {
      return new Response(request.method === "HEAD" ? null : asset.body, { status: 200, headers });
    }

    const body = await asset.arrayBuffer();
    const size = body.byteLength;
    const m = /^bytes=(\d*)-(\d*)$/.exec(range.trim());
    let start = 0;
    let end = size - 1;
    let ok = Boolean(m) && (m[1] !== "" || m[2] !== "");
    if (ok && m[1] === "") {
      // "bytes=-N": the last N bytes
      start = Math.max(0, size - Number(m[2]));
    } else if (ok) {
      start = Number(m[1]);
      if (m[2] !== "") end = Math.min(Number(m[2]), size - 1);
    }
    if (!ok || start > end || start >= size) {
      headers.set("Content-Range", `bytes */${size}`);
      headers.delete("Content-Length");
      return new Response(null, { status: 416, headers });
    }
    headers.set("Content-Range", `bytes ${start}-${end}/${size}`);
    headers.set("Content-Length", String(end - start + 1));
    return new Response(request.method === "HEAD" ? null : body.slice(start, end + 1), { status: 206, headers });
  },
};
