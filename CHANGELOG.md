# Changelog

## Unreleased

New app icon
- The app, extension and site icon is now an element tile: "Co", Confluo, number 16, on orange.

Safety
- Finished files carry the Mark of the Web, so Windows checks them like browser downloads.
- A redirect from a public site to a local address is refused by the address the connection really
  reaches, so a name that resolves to a private address no longer slips through. IPv4-mapped IPv6,
  shared (100.64/10) and "this network" (0.0.0.0/8) addresses count as local. Links that point at a
  machine nearby may still be redirected within the local network.
- Proxy-Authorization, like Cookie and Authorization, goes only to the site of the link.
- The resume journal names links by a digest; signed links and passwords no longer sit in the
  download folder. Journals from 0.1 are still recognised.
- Download folders must be ordinary full paths (no `..`, no device paths); network shares still work.
- The bridge drops a caller that stops sending halfway (408) after ten seconds.
- Superscript COM/LPT and CONIN$/CONOUT$ are treated as reserved file names.
- The window's content policy also forbids plugins, frames, `<base>` and form targets.

Fixes
- A server answering a range that ends at the last possible byte no longer makes a download ask for
  the same bytes for ever.
- Probe answers and mirror samples are no longer read whole, however long they are.
- Two downloads of the same link into the same folder at once each get their own file.
- Resuming from a server that names no file version checks the bytes already on disk first; a file
  replaced on the server is fetched again instead of being mixed with the old one.
- A resume record that cannot be written for a while (a virus scanner holding it) no longer ends the
  download.
- A disk error is reported with its real cause to everyone who asks.
- Pausing a download that is being removed no longer keeps its partial file; removing a download
  that never started no longer deletes another download's partial file of the same name.
- In the window, a late update no longer brings back a removed row, and a failed network poll keeps
  the last answer.

Speed
- With the default even split, the answer to the probe is used as the first data stream again
  instead of being thrown away; a slow server answers a few hundred milliseconds sooner.

Accessibility
- Setup: Tab stays inside the drawer, focus returns to the Setup button on close, arrow keys move
  between options, and network switches are named.

Inside
- `download.rs` became a module; the window's parts got their own files.
- `scripts/check.sh` runs every check; CI uses it.

## 0.1.0

First release.
