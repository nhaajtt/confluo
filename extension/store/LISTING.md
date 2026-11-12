# Store listing for the Confluo extension

The same package goes to both stores: every file directly inside `extension/`, zipped with
`manifest.json` at the top level (no `test/` or `store/`). The zip for 0.1.0 is attached to the
GitHub release as `confluo-extension-0.1.0.zip`.

## Common fields

| Field | Value |
|---|---|
| Name | Confluo |
| Summary (132 characters at most) | Hands your browser's downloads to the Confluo download manager on this computer. Needs the free Confluo app for Windows. |
| Category | Chrome: Workflow & Planning (Productivity). Firefox: Download Management |
| Language | English |
| Homepage | https://confluo.nhaajt.com |
| Support | https://github.com/nhaajtt/confluo/issues |
| Privacy policy | https://confluo.nhaajt.com/privacy |
| Licence | MIT |
| Icon | `extension/icon128.png` |
| Screenshot (1280x800) | `extension/store/screenshot-1280x800.png` |
| Small promo tile (440x280, Chrome) | `extension/store/promo-440x280.png` |

## Description

Confluo is a free download manager for Windows that splits one download over several connections
and several networks at once, carries on after a crash, and tells you what is holding the speed
back. This extension is the bridge between your browser and that app.

With the extension paired:

- A download you start in the browser is handed to Confluo instead, and the browser does not
  fetch it a second time.
- "Download with Confluo" appears when you right-click a link.
- If Confluo is not running, or the extension is not paired, the browser downloads as usual.

Setting it up takes a minute: install Confluo from https://confluo.nhaajt.com, open Setup, then
Browser, copy the pairing code, and paste it into this extension.

Privacy: the extension talks to one address only, 127.0.0.1, which is the Confluo app on your own
computer. It collects nothing and sends nothing to any server. The pairing code is never
transmitted; each side only proves that it knows it. Cookies are passed to the app only if you
turn that option on.

## Why each permission is needed (Chrome asks for this)

| Permission | Reason |
|---|---|
| `downloads` | To notice a download starting, hold it, and cancel the browser's copy once the app has taken it over. |
| `storage` | To keep the pairing code and the two switches (take over downloads, send cookies) in the browser's local storage. |
| `contextMenus` | For the "Download with Confluo" entry on links. |
| Host `http://127.0.0.1/*` | The only address the extension contacts: the Confluo app on the same computer. |
| `cookies` (optional) | Requested only when the user turns on "send cookies", so that downloads behind a login work in the app. |
| `<all_urls>` (optional) | Requested together with `cookies`, because cookies can only be read for sites the extension may access. |

Single purpose: handing downloads from the browser to the Confluo desktop app.

Remote code: none. All code is in the package.

## Data disclosure

- Chrome "Privacy practices": no user data is collected, sold or transferred. Website content and
  cookies are passed only to a program on the user's own machine, and only when the user asks.
- Firefox: `data_collection_permissions` is `none` in the manifest.

## Notes for reviewers

The extension does nothing visible without the desktop app. To test: install Confluo from
https://confluo.nhaajt.com (Windows 10 or 11), open Setup, Browser, copy the pairing code into the
extension's popup, then download any file. The source is at https://github.com/nhaajtt/confluo
(folder `extension/`); it is plain JavaScript with no build step.
