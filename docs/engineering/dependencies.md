# Dependency policy

Project code is GPL-3.0-or-later. Dependencies are pinned in `Cargo.lock`. `deny.toml` requires an explicit license allowlist, rejects unknown registries and git sources and checks the macOS arm64, Windows x64 and Linux x64 graphs. `cargo xtask audit` runs pinned cargo-deny 0.20.2:

```sh
cargo install --locked --version 0.20.2 --root .tools/cargo-deny cargo-deny
cargo xtask audit
```

## Expiring advisory exceptions

`deny.toml` has no static ignores. The audit wrapper validates each exception below against its UTC expiry, the exact resolved version and registry source, and an open task in the [dependency advisories](../../tasks/dependency-advisories.json) plan, then runs cargo-deny with a temporary configuration containing only those IDs. Expired, changed or retired exceptions and any new advisory fail.

| Advisory | Crate | Why it is tolerated | Expires (exclusive) | Task |
| --- | --- | --- | --- | --- |
| RUSTSEC-2024-0436 | paste 1.0.15 via metal and wgpu-hal | Build-time proc macro on dependency source, never on image data | 2026-12-18 | TASK-002 |
| RUSTSEC-2026-0192 | ttf-parser 0.25.1 via the Iced text stack | Parses installed system and bundled fonts only; Lightwell has no font import. Upstream mentions an undisclosed security report, so the window is short and must be re-reviewed before distribution or any font-input feature | 2026-10-19 | TASK-001 |

Both were investigated in September 2026: no released Iced, wgpu, cosmic-text, metal or fontdb line removes either crate, so bumping transitive versions alone is not a supported fix. Do not replace the GUI stack or carry a private fork to clear a maintenance advisory.

## Review status

License and source checks pass (BSL-1.0 in `clipboard-win` and `error-code` is GPL-compatible). `cargo xtask inventory` records resolved packages and copies top-level license files; it is not a notice audit. Before any distribution: inspect embedded fonts and assets, native linking and license-file declarations, assemble corresponding source and sanitize local manifest paths from dependency metadata. The manual license, native and asset review is deferred by the owner and remains incomplete.

## Bundled UI font

The workspace's text is set in Inter 4.1 (SIL Open Font License 1.1), vendored as two unmodified static TTF instances in `crates/lightwell-ui/assets/fonts/inter-4.1/` with the upstream licence beside them and compiled in with `include_bytes!`. `crates/lightwell-ui/THIRD_PARTY.md` records the release, archive and file hashes; `cargo xtask inventory` copies it and the licence into the notices under `fonts/` and lists the font in `dependencies.json`. Changing a font file is a dependency update. The embedded-font review above still applies and is not complete.

## RAW implementation dependencies

The [RAW backend selection](../research/raw-backend-selection.md) records pinned decoder/development candidates and why their processing stages are separate. The standalone Rawler comparison workspace has its own lockfile and is not an application runtime dependency. The private native adapter vendors the chosen LibRaw/librtprocess source with upstream notices and build configuration. These additions require the same source/notice and portable packaging checks; their experiments do not complete the deferred manual audit.

## Module capability dependencies

The [module capabilities](../design/module-capabilities.md) transport and secret store add `rustls` 0.23.45 with only the `ring` provider (no aws-lc-rs or CMake) and TLS 1.2/1.3, `rustls-platform-verifier` 0.7.0 so certificates are checked by the operating system's own verifier on macOS and Windows (native roots with webpki on Linux), `url` 2.5.8, `ureq-proto` 0.6.4 with only its `client` feature for the transport's HTTP/1.1 framing (it adds `http` 1.5.0, `httparse` 1.10.1 and `base64` 0.23.1, all MIT or Apache-2.0, and no TLS, pool or proxy code), `security-framework` 3.7.0 for the macOS Keychain (macOS only, keychain item APIs only) and `zeroize` 1.9.0. `ring` carries native C and assembly. The verifier's CDLA-licensed root bundle is a wasm32-only dependency, outside the three audited target graphs, so `deny.toml` is unchanged and the license and source checks pass. As with every other dependency, passing the automated checks is not the deferred manual review.
