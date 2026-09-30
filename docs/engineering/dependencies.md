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
| RUSTSEC-2026-0192 | ttf-parser 0.25.1 via fontdb 0.23 in the Iced text stack, and on Linux via ab_glyph in winit's Wayland decorations | Parses installed system fonts and the bundled Inter files only; Luxforge has no font import. Upstream has an undisclosed, unfixed denial-of-service report, so the window is short and must be re-reviewed before distribution or any font-input feature | 2026-10-29 | TASK-001 |

Both were investigated in September 2026: no released Iced, wgpu, cosmic-text or metal line removes either crate, so bumping transitive versions alone is not a supported fix. Do not replace the GUI stack or carry a private fork to clear a maintenance advisory.

The ttf-parser exception was re-reviewed on 2026-09-29:

- **Path.** `iced` 0.14.0 → `iced_graphics` 0.14.0 and `cryoglyph` 0.1.0 → `cosmic-text` 0.15.0 → `fontdb` 0.23.0 → `ttf-parser` 0.25.1 on every target. On Linux, `winit` 0.30.13's `wayland` feature (enabled by Iced's `wayland` feature) also reaches it through `sctk-adwaita` 0.10.1 → `ab_glyph` 0.2.32 → `owned_ttf_parser` 0.25.1, which draws client-side window decorations with a system font.
- **Upstream.** `fontdb` 0.24.0 no longer depends on ttf-parser, and `cosmic-text`'s main branch has moved to it, but the latest `cosmic-text` release (0.19.0) still requires `fontdb` 0.23, and Iced 0.14 requires `cosmic-text` 0.15, which has no later patch release. Iced's unreleased 0.15 line pins a `cosmic-text` fork by git revision. `winit` 0.30 requires `sctk-adwaita` 0.10; newer `sctk-adwaita` releases still use `ab_glyph` by default and need a newer `smithay-client-toolkit` than `winit` 0.30 uses (`winit` 0.31 is in beta). No lockfile-only or minor update removes the crate; the only route is a future Iced major release.
- **Advisory.** The advisory is informational (unmaintained) with no patched version. The upstream issue behind it describes an algorithmic-complexity denial of service (excessive CPU on a crafted font); its details are still private and no fix has been released. Upstream now states a bug-fixes-only maintenance scope, and a 0.26.0 was prepared and then withdrawn.
- **Exposure.** Fonts reach the parser only from the operating system's installed fonts and the two compiled-in Inter files; no command, recipe, catalog or import path accepts a font. A malicious installed font would already be a local compromise. The window stays at 30 days because the report is undisclosed; review sooner if its details are published, a `cosmic-text` or Iced release adopts `fontdb` 0.24, or distribution or font input is planned.

## Review status

License and source checks pass (BSL-1.0 in `clipboard-win` and `error-code` is GPL-compatible). `cargo xtask inventory` records resolved packages and copies top-level license files; it is not a notice audit. Before any distribution: inspect embedded fonts and assets, native linking and license-file declarations, assemble corresponding source and sanitize local manifest paths from dependency metadata. The manual license, native and asset review is deferred by the owner and remains incomplete.

## Bundled UI font

The workspace's text is set in Inter 4.1 (SIL Open Font License 1.1), vendored as two unmodified static TTF instances in `crates/luxforge-ui/assets/fonts/inter-4.1/` with the upstream licence beside them and compiled in with `include_bytes!`. `crates/luxforge-ui/THIRD_PARTY.md` records the release, archive and file hashes; `cargo xtask inventory` copies it and the licence into the notices under `fonts/` and lists the font in `dependencies.json`. Changing a font file is a dependency update. The embedded-font review above still applies and is not complete.

## Bundled place names

Events are named after the populated place nearest to their photographs, from a gazetteer compiled into the binary, so no position leaves the machine. The table is a modified extract of GeoNames' `cities15000` (populated places over 15,000 inhabitants, and capitals; 34,152 rows), licensed CC BY 4.0, which asks for the attribution, licence address and indication of changes that `crates/luxforge-core/THIRD_PARTY.md` carries. `cargo xtask inventory` copies that file into the notices under `data/luxforge-core/` and lists the data as `bundled_data` in `dependencies.json`; there is no licence file to copy, since the licence is named by its address.

- **Source and pin.** `https://download.geonames.org/export/dump/cities15000.zip`, downloaded on 2026-09-30, SHA-256 `44347468d5656101a999ba0cbb64b0cbdc7c2b48de83c8f6e129b50e697dd05d`; the extracted `cities15000.txt` is `5db14f4826ba451b779cbf25ad363f04d6dce20d334d61dfafdd3a5ff64a40e7`. The derived asset is `c62805b2e717dcd3167eaadd14d9b9a4f1cce477c080a121aa1c97cd54bbd59b`. A test checks that `THIRD_PARTY.md` records the committed asset's rows, size and hash.
- **Form and size.** `crates/luxforge-core/assets/gazetteer/places.tsv` is a sorted UTF-8 table of six columns (name, country code, GeoNames feature code, latitude, longitude, population), 1,445,433 bytes (616 KB gzipped). It is text so that a regenerated file reviews as a line diff and the lookup borrows its names from the compiled-in bytes. It is parsed and indexed on the first lookup, about 3.5 MB of heap in about 10 ms, never at startup; a binary that uses it grows by the asset's size.
- **Regeneration.** `cargo xtask gazetteer --source PATH/TO/cities15000.txt --output NEW_FILE` converts the extracted file with no network access and refuses an output that exists. Replacing the asset is a dependency update: record the new hashes, date and row count in `THIRD_PARTY.md` and here.

The manual license and asset review of this data is deferred by the owner and remains incomplete; the hashes are provenance, not an audit.

## RAW implementation dependencies

The [RAW backend selection](../research/raw-backend-selection.md) records pinned decoder/development candidates and why their processing stages are separate. The standalone Rawler comparison workspace has its own lockfile and is not an application runtime dependency. The private native adapter vendors the chosen LibRaw/librtprocess source with upstream notices and build configuration. These additions require the same source/notice and portable packaging checks; their experiments do not complete the deferred manual audit.

## Module capability dependencies

The [module capabilities](../design/module-capabilities.md) transport and secret store, which live in `luxforge-net` so that `luxforge-core` and its test binaries link none of them (only `url`, `idna_adapter` and `zeroize` below are the core's), add `rustls` 0.23.45 with only the `ring` provider (no aws-lc-rs or CMake) and TLS 1.2/1.3, `rustls-platform-verifier` 0.7.0 so certificates are checked by the operating system's own verifier on macOS and Windows (native roots with webpki on Linux), `url` 2.5.8 with `idna_adapter` pinned to 1.0.0, the IDNA back end without Unicode data (a non-ASCII host is refused, punycode is accepted, and no ICU4X crate is built), `ureq` 3.4.2 with no features for the transport's HTTP/1.1, whose agent runs on the transport's own resolver, socket and rustls session (it adds `ureq-proto` 0.6.4 with its `client` feature, which the transport also names, `http` 1.5.0, `httparse` 1.10.1, `base64` 0.23.1 and `utf8-zero` 0.8.1, all MIT or Apache-2.0, and no TLS, pool, proxy or compression code), `security-framework` 3.7.0 for the macOS Keychain (macOS only, keychain item APIs only) and `zeroize` 1.9.0. `ring` carries native C and assembly. The verifier's CDLA-licensed root bundle is a wasm32-only dependency, outside the three audited target graphs, so `deny.toml` is unchanged and the license and source checks pass. As with every other dependency, passing the automated checks is not the deferred manual review.
