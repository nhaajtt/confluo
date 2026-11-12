# Deploying the download site

The site is a Cloudflare Worker that serves static files, plus `../worker.js` for byte ranges
under `/media/`.

```sh
node site/prepare.mjs https://confluo.nhaajt.com    # builds site/dist
mkdir -p <staging>/public <staging>/src              # any folder outside the repository
cp -r site/dist/. <staging>/public/
cp site/worker.js <staging>/src/index.js
cp site/deploy/*.ts site/deploy/package.json <staging>/
cd <staging> && npm install && cf deploy
```

`cf` is the Cloudflare CLI (`npm install --global cf`), signed in with `cf auth login`.
