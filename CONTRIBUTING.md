# Contributing

## Before you send a change

```sh
bash scripts/check.sh
```

It runs what CI runs: `cargo fmt --check`, `cargo clippy -D warnings`, every test, a syntax check of
the extension and the site scripts, and the window's type check, lint and production build. Rust
needs 1.88 or newer, the window needs Node 22 and pnpm.

Commit messages say what changed for the person using Confluo, in one line, with the reason
underneath when it is not obvious.

## Where things are

| You want to change | Look in |
|---|---|
| How a file is fetched: probing, ranges, retries, mirrors, finishing | `crates/confluo-engine/src/download/` (`mod.rs` is the loop; `types.rs` what goes in and out; `paths.rs` which partial file; `samples.rs` byte comparisons) |
| Who fetches which bytes, and when to split a slow connection | `crates/confluo-engine/src/sched.rs`, `ranges.rs` |
| How many connections each network gets | `crates/confluo-engine/src/tuner.rs` |
| Requests, redirects, which headers go where | `crates/confluo-engine/src/http.rs`, `probe.rs` |
| Writing to disk, the resume journal, file names | `disk.rs`, `journal.rs`, `name.rs` |
| The download list, settings, what is saved | `crates/confluo-engine/src/manager.rs` |
| Network interfaces and binding sockets to them | `crates/confluo-net/` |
| The browser bridge | `crates/confluo-bridge/` and `extension/` |
| The window | `app/src/`: `App.tsx` layout, `command.tsx`, `list.tsx`, `detail.tsx`, `setup.tsx`, `flow.tsx` and `canvas.tsx` charts, `store.ts` state, `api.ts` the only file that talks to Rust |
| What the window may ask Rust to do | `app/src-tauri/src/main.rs` (one function per command) |

A new kind of misbehaving server for the tests goes into `crates/confluo-testsrv` as a `Behavior`
field. A server that lies in ways the test server should not learn (impossible ranges, endless
bodies) is scripted inside the test instead, as in `crates/confluo-engine/tests/hostile.rs`.

## Rules the code keeps

- A completed download is exact. Every test that completes a file compares it byte for byte; a
  change to splitting, joining or resuming comes with such a test, including one where the
  connection or the server goes away halfway.
- Tests only talk to servers they start themselves on `127.0.0.1`.
- The journal lists only bytes that were flushed to disk first.
- The window names downloads by id, never by path; Rust looks the path up.
- Anything a server sends is untrusted: sizes, names, ranges and redirects are checked before use.

## Benchmarks

`bench/bench.py --ip 127.0.0.1 --subjects confluo-cli` measures the release `confluo` binary against
the local test server; `bench/crash.py` and `bench/soak.py` kill and restart it and check the file.
Run them before and after a change to the engine and compare.
