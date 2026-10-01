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

## Native trackpad input

Trackpad input uses the existing `objc2` 0.6.4, `objc2-app-kit` 0.3.2, `block2` 0.6.2 and `raw-window-handle` 0.6.2 packages through `luxforge-input` on macOS, with event, window, view and callback bindings only. Their versions remain pinned. The borrowed window handle binds the monitor to the editor's own window, excluding native dialogs. This introduces no new external package or separate GUI stack; the manual native and notice review remains deferred.

## Bundled UI font

The workspace's text is set in Inter 4.1 (SIL Open Font License 1.1), vendored as two unmodified static TTF instances in `crates/luxforge-ui/assets/fonts/inter-4.1/` with the upstream licence beside them and compiled in with `include_bytes!`. `crates/luxforge-ui/THIRD_PARTY.md` records the release, archive and file hashes; `cargo xtask inventory` copies it and the licence into the notices under `fonts/` and lists the font in `dependencies.json`. Changing a font file is a dependency update. The embedded-font review above still applies and is not complete.

## RAW implementation dependencies

The [RAW backend selection](../research/raw-backend-selection.md) records pinned decoder/development candidates and why their processing stages are separate. The standalone Rawler comparison workspace has its own lockfile and is not an application runtime dependency. The private native adapter vendors the chosen LibRaw/librtprocess source with upstream notices and build configuration. These additions require the same source/notice and portable packaging checks; their experiments do not complete the deferred manual audit.

## Module capability dependencies

The [module capabilities](../design/module-capabilities.md) transport and secret store, which live in `luxforge-net` so that `luxforge-core` and its test binaries link none of them (only `url`, `idna_adapter` and `zeroize` below are the core's), add `rustls` 0.23.45 with only the `ring` provider (no aws-lc-rs or CMake) and TLS 1.2/1.3, `rustls-platform-verifier` 0.7.0 so certificates are checked by the operating system's own verifier on macOS and Windows (native roots with webpki on Linux), `url` 2.5.8 with `idna_adapter` pinned to 1.0.0, the IDNA back end without Unicode data (a non-ASCII host is refused, punycode is accepted, and no ICU4X crate is built), `ureq` 3.4.2 with no features for the transport's HTTP/1.1, whose agent runs on the transport's own resolver, socket and rustls session (it adds `ureq-proto` 0.6.4 with its `client` feature, which the transport also names, `http` 1.5.0, `httparse` 1.10.1, `base64` 0.23.1 and `utf8-zero` 0.8.1, all MIT or Apache-2.0, and no TLS, pool, proxy or compression code), `security-framework` 3.7.0 for the macOS Keychain (macOS only, keychain item APIs only) and `zeroize` 1.9.0. `ring` carries native C and assembly. The verifier's CDLA-licensed root bundle is a wasm32-only dependency, outside the three audited target graphs, so `deny.toml` is unchanged and the license and source checks pass. As with every other dependency, passing the automated checks is not the deferred manual review.
