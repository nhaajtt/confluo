# Weft

A download manager that spreads one download over several connections and several
network interfaces, survives crashes and bad servers, and tells you what is limiting
the speed. Windows first; the engine also builds on Linux and macOS.

Weft was built after studying [Baseline](https://github.com/example/baseline) (MIT). It is an
independent implementation in Rust with a Tauri shell, not a fork. Baseline is used, unmodified,
as the baseline in the benchmarks below.

## Build and run

Needs Rust (stable, 1.88+), Node 22 and pnpm. On Windows the WebView2 runtime (part of Windows 11).

```sh
cargo test --workspace                 # engine, bridge and test-server tests
cargo build --release -p weft-cli      # target/release/weft(.exe), the engine without a window

cd app
pnpm install
pnpm tauri dev                         # run the desktop app
pnpm tauri build                       # installer in target/release/bundle/nsis/
pnpm typecheck && pnpm lint
```

Command line:

```sh
weft get https://example.com/big.iso -o D:\Downloads
weft get <link> <mirror-link> --checksum sha256:<hex>
weft get <link> -i "Ethernet" -i "Wi-Fi"     # use exactly these interfaces
weft interfaces
```

Ctrl-C pauses; running the same command again continues.

Browser extension: load the `extension/` folder as an unpacked extension, then paste the pairing
code from Weft (Setup, Browser) into the extension.

## Layout

| Path | What it is |
|---|---|
| `crates/weft-net` | Interface discovery, sockets pinned to an interface, change notifications |
| `crates/weft-engine` | Probe, scheduler, connection tuner, disk writer, journal, download list |
| `crates/weft-bridge` | Loopback service the browser extension talks to |
| `crates/weft-cli` | `weft` command |
| `crates/weft-testsrv` | File server that misbehaves on purpose (caps, drops, stalls, no ranges, expiring links) |
| `app/` | Desktop app (Tauri 2, SolidJS) |
| `extension/` | Browser extension (Manifest V3) |
| `bench/` | Benchmark harness and results |

## How it works, and why

**Sockets are pinned to the interface, not to an address.** Windows uses `IP_UNICAST_IF`, Linux
`SO_BINDTODEVICE`, macOS `IP_BOUND_IF`. Binding only the source address (what Baseline does on
Windows and macOS, see its issue #65) leaves the routing table in charge, so with two default
routes everything can leave through one adapter. Evidence here: a Weft connection pinned to an
adapter with no route to the server fails with "network unreachable" instead of quietly going out
through another adapter.

**Few long requests.** Every new range request makes the server restart TCP slow start, and some
servers count requests (one public mirror answered HTTP 429 to an earlier design that used many
medium-sized ranges). So the probe itself asks for the whole file and its response becomes the
first data stream; other connections are opened while the probe is in flight; each connection then
takes a share of the remaining bytes in proportion to its measured speed. A connection that
finishes early takes over the tail of the slowest one.

**The number of connections is measured, not assumed.** The tuner adds connections only while
that buys at least 10% more speed, gives them back otherwise, and lowers a ceiling when the server
refuses connections or the disk falls behind.

**One writer, one file handle, bounded queue.** A slow disk slows the sockets instead of filling
memory. Space is reserved without zero-filling (sparse on NTFS, `fallocate` on Linux).

**The journal never claims bytes that are not on disk.** About once a second the file is flushed
and then the list of completed byte ranges is replaced atomically. After a crash, exactly those
ranges are kept.

**Honest about limits.** Each download shows what holds it back: the network path, a per-connection
cap, a server refusing connections, a server that cannot resume, the disk, or your own limit. If two
interfaces share one router the app says that adding them up cannot help.

**Decisions on technology.** Rust for the engine because interface pinning needs raw socket options
and because the measured CPU and memory cost of an Electron main-process engine is high (see below).
Tauri for the shell because the system WebView makes the installer 2.3 MiB. HTTP/2 is not used on
purpose: it multiplexes everything over one TCP connection, which defeats both per-interface
routing and parallel flows.

## Safety

- The window can only call a fixed set of commands. It names downloads by id; it never passes a
  file path to open or delete.
- Server-supplied file names are reduced to a plain name; nothing can be written outside the
  download folder (tested with `..\..\` names).
- Cookies and credentials are sealed with Windows DPAPI before being saved; on other systems they
  are not saved at all. They are only sent to the host they were given for.
- The browser bridge listens on `127.0.0.1` only, refuses requests with a web `Origin` or a wrong
  `Host` (DNS rebinding), and requires a pairing token. Baseline's equivalent has no secret.
- With a VPN active, automatic mode leaves routing to the system so traffic does not go around the
  VPN. Choosing an adapter by hand overrides that, and the app says so.

## Benchmarks

Machine: Windows 11, i9-14900KS, 32 GB. Both apps download from `weft-testsrv` on this machine's
LAN address. Median of 3 runs. Baseline is commit `2e44a2f`, built with its own `npm run build:unpack`
and driven through its real window with Playwright. Reproduce with:

```sh
bench/.venv/Scripts/python bench/bench.py --ip <a LAN address of this machine> --runs 3
bench/.venv/Scripts/python bench/crash.py --ip <same> --runs 3
```

Raw output is in `bench/results/`.

| Scenario | Weft app | Baseline | Result |
|---|---|---|---|
| No limit, 1 GiB | 1.26 s (810 MiB/s) | 4.62 s (222 MiB/s) | Weft 3.7x faster |
| Server caps each connection at 1 MiB/s, 200 MiB | 8.32 s | 7.44 s | **Baseline 11% faster** |
| One 20 MiB/s bottleneck, 200 MiB | 10.31 s | 10.11 s | equal (the link is the limit) |
| Server without Range support, 200 MiB | 5.18 s | 5.25 s | equal |
| Server allows 4 connections, 200 MiB | 29.3 s (CLI) | 26.1 s | **Baseline 11% faster** |
| 150 ms before each response, 4 MiB/s per connection, 200 MiB | 4.48 s | 3.55 s | **Baseline 21% faster** |

| Cost | Weft engine (CLI) | Weft app | Baseline |
|---|---|---|---|
| CPU time per download (range over scenarios) | 0.1 - 1.3 s | 1.7 - 3.2 s | 10.6 - 50.9 s |
| Peak memory, whole process tree | 11 - 43 MiB | 388 - 420 MiB | 701 - 840 MiB |
| Requests for a 200 MiB file | 1 - 66 | same | about 200 |
| Installer | | 2.3 MiB | 258 MiB |
| Installed size | | 7 MiB | 464 MiB unpacked |
| Launch to ready | | 0.5 - 1.4 s | not measured the same way |

| Hard kill at 50% of a 200 MiB download, then relaunch | Weft | Baseline |
|---|---|---|
| File intact afterwards (3 of 3 runs) | yes | yes |
| Bytes fetched again | 21 - 24 MiB | 112 - 114 MiB |

Where Weft loses, the cause is known: it starts with 8 connections and reaches 32 about two
seconds later than Baseline, because it waits to see that more connections really help. On a 200 MiB
file that costs about a second; the gap shrinks as files get larger. With a server that allows only
4 connections, Weft backs off further than needed after the refusals and takes 15 s to climb back.

Not measured, and therefore not claimed:

- **Real gain from two internet connections.** This machine has one uplink. With emulated links
  (`weft get --emulate-links 4m,2m`), Weft reached 83-95% of the sum of the links on a 60 MiB file.
  That shows the scheduler; it does not show two real networks. Nothing can make two interfaces
  behind the same router faster than one.
- UI frame times under load (CPU of the renderer and GPU processes was measured instead:
  about 0.5 s over an 8 s download).
- Long soak runs.
- Public internet speed: one public mirror varied from 3.4 s to 11.9 s for the same file with
  plain `curl`, so it could not separate the tools. Files downloaded from it matched `curl` byte
  for byte.

## Tests

`cargo test --workspace`: 58 tests. The engine tests run real downloads against the misbehaving
server and compare every completed file byte for byte. Covered: parallel ranges, servers without
ranges or without a length, dropped and stalled connections, refused connections, HTTP 429,
per-connection caps, pause and resume, a kill without warning, a file changing on the server, an
expiring signed link, checksum mismatch, mirrors (including a wrong one), unequal routes, a dead
route, a route removed mid-download, hostile file names, the queue and restart of the list, and the
bridge refusing unpaired, web-page and rebinding requests. Scheduler and range-set invariants are
property-tested.

## Known gaps

- No torrents, Usenet, Telegram or FTP (Baseline has the first three).
- No proxy support. DNS uses the system resolver, not the DNS servers of each interface.
- The browser extension was checked against the bridge by script and for syntax, not exercised
  inside a browser.
- macOS code compiles in principle but was never built or run; Linux was not run in this round.
- The installer is not code-signed, so Windows SmartScreen will warn.
- Interface changes are pushed by the OS on Windows only; elsewhere they are polled each second.
- Completed-file hashing needs an expected checksum (given by you or advertised by the server).

## Licence

MIT. See `LICENSE` and `THIRD-PARTY.md`.
