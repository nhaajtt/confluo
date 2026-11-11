# Third-party software

Weft is MIT licensed (see `LICENSE`). It is an independent implementation: no source
code was copied from Baseline (MIT, Copyright (c) 2026 its author), which was studied
as a reference and is used unmodified, from its own repository, as the benchmark baseline.

Weft links the following components. Each keeps its own licence; the exact
versions are pinned in `Cargo.lock` and `app/pnpm-lock.yaml`.

| Component | Licence |
|---|---|
| tokio, hyper, hyper-util, http-body-util, bytes | MIT |
| rustls, tokio-rustls | Apache-2.0 / MIT / ISC |
| ring | Apache-2.0 and ISC |
| webpki-roots (Mozilla CA list) | CDLA-Permissive-2.0 |
| socket2, libc, url, serde, serde_json, thiserror, base64, percent-encoding, sha2, sha1, md-5 | MIT / Apache-2.0 |
| windows-sys | MIT / Apache-2.0 |
| Tauri and its plugins | MIT / Apache-2.0 |
| SolidJS | MIT |
| Geist and Geist Mono typefaces | SIL Open Font License 1.1 |

Run `cargo tree --workspace --edges normal` for the complete list.
