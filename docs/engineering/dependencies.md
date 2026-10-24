# Dependency policy

Project code is GPL-3.0-or-later. Dependencies are pinned in `Cargo.lock`. `deny.toml` requires an explicit license allowlist, rejects unknown registries and git sources and checks the macOS arm64, Windows x64 and Linux x64 graphs. Those graphs include packages that only a development dependency reaches, so their advisories and sources are checked; the licence check skips them, as cargo-deny does by default and `deny.toml` leaves unchanged. `cargo xtask audit` runs pinned cargo-deny 0.20.2:

```sh
cargo install --locked --version 0.20.2 --root .tools/cargo-deny cargo-deny
cargo xtask audit
```

## Expiring advisory exceptions

`deny.toml` has no static ignores. The audit wrapper validates each exception below against its UTC expiry, the exact resolved version and registry source, and an open task in the [dependency advisories](../../tasks/project/dependency-advisories.json) plan, then runs cargo-deny with a temporary configuration containing only those IDs. Expired, changed or retired exceptions and any new advisory fail.

| Advisory | Crate | Why it is tolerated | Expires (exclusive) | Task |
| --- | --- | --- | --- | --- |
| RUSTSEC-2024-0436 | paste 1.0.15 via metal and wgpu-hal | Build-time proc macro on dependency source, never on image data | 2026-12-18 | TASK-002 |
| RUSTSEC-2026-0192 | ttf-parser 0.25.1 via fontdb 0.23 in the Iced text stack, and on Linux via ab_glyph in winit's Wayland decorations | Parses installed system fonts and the bundled Inter files only; Luxforge has no font import. Upstream has an undisclosed, unfixed denial-of-service report, so the window is short and must be re-reviewed before distribution or any font-input feature | 2026-10-29 | TASK-001 |

No released Iced, wgpu, cosmic-text or metal line removes either crate, so bumping transitive versions alone is not a supported fix. Do not replace the GUI stack or carry a private fork to clear a maintenance advisory.

The ttf-parser exception rests on this review:

- **Path.** `iced` 0.14.0 → `iced_graphics` 0.14.0 and `cryoglyph` 0.1.0 → `cosmic-text` 0.15.0 → `fontdb` 0.23.0 → `ttf-parser` 0.25.1 on every target. On Linux, `winit` 0.30.13's `wayland` feature (enabled by Iced's `wayland` feature) also reaches it through `sctk-adwaita` 0.10.1 → `ab_glyph` 0.2.32 → `owned_ttf_parser` 0.25.1, which draws client-side window decorations with a system font.
- **Upstream.** `fontdb` 0.24.0 does not depend on ttf-parser, and `cosmic-text`'s main branch has moved to it, but the latest `cosmic-text` release (0.19.0) still requires `fontdb` 0.23, and Iced 0.14 requires `cosmic-text` 0.15, which has no later patch release. Iced's unreleased 0.15 line pins a `cosmic-text` fork by git revision. `winit` 0.30 requires `sctk-adwaita` 0.10; newer `sctk-adwaita` releases still use `ab_glyph` by default and need a newer `smithay-client-toolkit` than `winit` 0.30 uses (`winit` 0.31 is in beta). No lockfile-only or minor update removes the crate; the only route is a future Iced major release.
- **Advisory.** The advisory is informational (unmaintained) with no patched version. The upstream issue behind it describes an algorithmic-complexity denial of service (excessive CPU on a crafted font); its details are still private and no fix has been released. Upstream states a bug-fixes-only maintenance scope, and a 0.26.0 was prepared and then withdrawn.
- **Exposure.** Fonts reach the parser only from the operating system's installed fonts and the two compiled-in Inter files; no command, recipe, catalog or import path accepts a font. A malicious installed font would already be a local compromise. The window stays at 30 days because the report is undisclosed; review sooner if its details are published, a `cosmic-text` or Iced release adopts `fontdb` 0.24, or distribution or font input is planned.

## Review status

License and source checks pass (BSL-1.0 in `clipboard-win` and `error-code` is GPL-compatible). `cargo xtask inventory` records resolved packages and copies top-level license files; it is not a notice audit. `cargo xtask release-source` assembles the corresponding source archive ([source archive](releases.md#the-source-archive)), with each shipping target's inventory and notices; it refuses an archive that names the paths of the machine that made it. Before any distribution: inspect embedded fonts and assets, native linking and license-file declarations against that archive. The manual license, native and asset review is deferred by the owner and remains incomplete.

## Source archive tooling

`release-source` decompresses the pinned prebuilt ONNX Runtime archives with `lzma-rust2` 0.15.8 (Apache-2.0), with only its `std` and `optimization` features, the version and features `ort-sys`'s build script already builds it with, so it adds no package to the lockfile. Only `xtask` names it; no shipped executable links it.

## Usage statistics ingress

`services/usage-ingest` is a standalone package outside the desktop workspace, with its own `Cargo.lock`, so none of its crates is in the desktop graph or inventory. It uses `hyper` 1.12.0 (server and client, HTTP/1 only, no HTTP/2, TLS or proxy support), `hyper-util` 0.1.21 (only its Tokio adapter), `http-body-util` 0.1.5, `bytes` 1.12.1, `serde` 1.0.229, `serde_json` 1.0.149 and `tokio` 1.53.3 (the version the desktop already locks; multi-thread runtime, `net`, `time`, `sync`, `macros`, `signal`), all pinned exactly in its manifest, with the rest of its 36 locked packages (itself included) (`http`, `httparse`, `mio`, `socket2`, `libc` and `smallvec` at the desktop's versions) pinned by its lockfile. Its manifest and lockfile are held by `check-repository`, and `cargo xtask audit` runs the licence, source and advisory policy over its lockfile with no exceptions. It has `NOTICE.md` and, like every dependency, no completed manual licence review.

## Job monitoring transport

The core uses the workspace's existing pinned Tokio 1.53.3 with `rt`, `net`, `io-util` and the workspace's `time` feature for the loopback transport's current-thread event loop. Socket EOF can cancel an indefinitely held `job.wait` without a polling timer or another thread. The socket features add `mio` 1.2.4 and `socket2` 0.6.5, pinned in `Cargo.lock`; no new GUI stack, background runtime worker or thread per held request is introduced. The manual license, native and notice review remains deferred and incomplete.

## Native trackpad input

Trackpad input uses the existing `objc2` 0.6.4, `objc2-app-kit` 0.3.2, `block2` 0.6.2 and `raw-window-handle` 0.6.2 packages through `luxforge-input` on macOS, with event, window, view and callback bindings only. Their versions remain pinned. The borrowed window handle binds the monitor to the editor's own window, excluding native dialogs. This introduces no new external package or separate GUI stack; the manual native and notice review remains deferred.

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

## Folder watcher dependencies

`luxforge-watch` watches indexed folders and volumes through thin platform code rather than a watcher crate: `notify` cannot resume FSEvents from a stored event ID, so it cannot catch up on what changed while Luxforge was closed. It adds one package to the lockfile, `objc2-core-services` 0.3.2 (Zlib, Apache-2.0 or MIT, from crates.io), macOS only, built with its `FSEvents` bindings and no default features, for the event streams, their replay and each volume's history identifier. Everything else it names was already in the lockfile and is pinned to the version it resolves to: `objc2-core-foundation` 0.3.2 (the CoreFoundation strings, arrays and UUIDs FSEvents takes) and `dispatch2` 0.3.1 (the serial queue every stream and the Disk Arbitration session deliver on), both Zlib, Apache-2.0 or MIT, and `libc` 0.2.189, on macOS; `rustix` 1.1.5 (Apache-2.0 with the LLVM exception, Apache-2.0 or MIT) with its `fs` and `event` features for inotify, `poll` and `eventfd` on Linux; and `windows-sys` 0.61.2 (MIT or Apache-2.0) with the features for directory change reads, completion ports and the configuration manager's notifications on Windows. The Disk Arbitration callbacks are declared in the crate itself (`macos/disks.rs`), since `objc2-disk-arbitration` does not declare them, so that crate is not used. The mount table (`mounts()`, which the catalog's index reads) lives here too. As with every other dependency, passing the automated checks is not the deferred manual review.

## Module capability dependencies

The [module capabilities](../design/module-capabilities.md) transport and secret store, which live in `luxforge-net` so that `luxforge-core` and its test binaries link none of them (only `url`, `idna_adapter` and `zeroize` below are the core's), add `rustls` 0.23.46 with only the `ring` provider (no aws-lc-rs or CMake) and TLS 1.2/1.3, `rustls-platform-verifier` 0.7.0 so certificates are checked by the operating system's own verifier on macOS and Windows (native roots with webpki on Linux), `url` 2.5.8 with `idna_adapter` pinned to 1.0.0, the IDNA back end without Unicode data (a non-ASCII host is refused, punycode is accepted, and no ICU4X crate is built), `ureq` 3.4.2 with no features for the transport's HTTP/1.1, whose agent runs on the transport's own resolver, socket and rustls session (it adds `ureq-proto` 0.6.4 with its `client` feature, which the transport also names, `http` 1.5.0, `httparse` 1.10.1, `base64` 0.23.1 and `utf8-zero` 0.8.1, all MIT or Apache-2.0, and no TLS, pool, proxy or compression code), `security-framework` 3.7.0 for the macOS Keychain (macOS only, keychain item APIs only) and `zeroize` 1.9.0. `ring` carries native C and assembly. The verifier's CDLA-licensed root bundle is a wasm32-only dependency, outside the three audited target graphs, so `deny.toml` needs no exception and the license and source checks pass. As with every other dependency, passing the automated checks is not the deferred manual review.

## Update feed dependencies

The [update feed](../design/auto-update.md#the-feed) crate, `luxforge-update`, adds no package to the lockfile. Every dependency is pinned in the workspace manifest to the version and features already resolved:

- `ring` 0.17.14 (Apache-2.0 AND ISC; native C and assembly), whose Ed25519 signs and verifies, with the default features `rustls` already builds;
- `base64` 0.23.1 (MIT or Apache-2.0) with `std`, as `ureq` builds it;
- `memmap2` 0.9.11 (MIT or Apache-2.0), as `fontdb` builds it;
- `roxmltree` 0.20.0, `semver` 1.0.28, `serde`, `serde_json` and `zeroize` 1.9.0.
- for the Linux payload updater, `flate2` 1.1.10 (MIT or Apache-2.0) on its default `miniz_oxide` backend and `tar` 0.4.46 (MIT or Apache-2.0) without its default `xattr` feature, the versions xtask already builds the Linux archive with; and, on every unix target, `rustix` 1.1.5 with the features the folder watcher pins (only `fs` is used here), for the writability check, `syncfs` and the `renameat2` exchange as safe calls. On macOS that brings `rustix` into the desktop's and `xtask-cli`'s graphs; it adds no package to the lockfile.

`ring`'s Ed25519 takes the whole message as one slice. An archive is therefore mapped read-only rather than read onto the heap, in one function that allows `unsafe` by name beside its `SAFETY:` comment; the rest of the crate denies it. `xtask-cli` builds this crate, and so `ring`'s native code, for `update-record` and `update-feed`, and names the same `tar` 0.4.46, without `xattr`, for the deploy archive `update-feed --deploy` writes; the desktop links it for the payload updater. It is the one workspace crate `xtask-cli` may build, and it builds no image, codec, database or GUI crate; `cargo xtask check-repository` enforces both. As with every other dependency, passing the automated checks is not the deferred manual review.

## Sparkle on macOS

The macOS updater is [Sparkle](../design/auto-update.md#macos-sparkle) 2.10.0 (MIT, with the external licences its `LICENSE` lists), a native framework rather than a Cargo package. `xtask/src/sparkle.rs` pins the official archive `https://github.com/sparkle-project/Sparkle/releases/download/2.10.0/Sparkle-2.10.0.tar.xz` by its length (16,319,840 bytes) and SHA-256 (`c2bf58aa8387266ac179357b1415d6f2635f044da8be41042af32425dae6da0c`), downloads it over HTTPS only into `target/sparkle/2.10.0/`, and checks both before extracting `Sparkle.framework` and `LICENSE`; no Sparkle binary is committed. `release-build` embeds the framework in the macOS bundle without its XPC services, and `package` copies the licence and `crates/luxforge-sparkle/THIRD_PARTY.md` (the tag, its commit and the archive) into the notices under `native/sparkle-2.10.0/`, on macOS only. The editor loads the framework at run time; nothing links it.

The bridge crate `luxforge-sparkle` adds no external package to the lockfile: it uses the pinned `objc2` 0.6.4, `objc2-foundation` 0.3.2 with only the Foundation classes it names, `block2` 0.6.2 and the workspace's `dispatch2`. It depends on no workspace crate *(enforced)*. xtask also builds `luxforge-update`, to read `release/update.json` for the bundle's feed and key. As with every other dependency, passing the automated checks is not the deferred manual review.

## Export encoders: WebP and TIFF

The [export settings](../design/export-settings.md#encoding) WebP encoder is libwebp, named only by the leaf crate `luxforge-webp` *(enforced: only it names `libwebp-sys`, only `luxforge-core` depends on it, and it depends on no workspace crate outside its tests)*. It adds two external packages to the lockfile:

- `libwebp-sys` 0.14.4 (MIT for its bindings; the libwebp 1.6.0 source it vendors is BSD-3-Clause with Google's patent grant), built with no default features: its build script compiles every C file under the package's `vendor/src` and `vendor/sharpyuv` with `cc` and the platform's C compiler and links them statically, never a system libwebp, with no `bindgen` (the crate's committed bindings) and, without `std`, no libwebp threads. NEON on aarch64; SSE2 on x86_64, with SSE4.1 and AVX2 only for targets that enable them. `crates/luxforge-webp/THIRD_PARTY.md` records the package's checksum and source commit; `cargo xtask inventory` copies it and libwebp's `COPYING` and `PATENTS` (committed copies under `crates/luxforge-webp/notices/libwebp-1.6.0/`, since the package has no top-level licence file) into the notices under `native/`.
- `glob` 0.3.4 (MIT or Apache-2.0), a build dependency of `libwebp-sys` that lists its C files. Its other build dependencies, `cc` and `pkg-config`, were already in the lockfile.

The TIFF writer is the core's own: its strips are compressed with `flate2` 1.1.10 (MIT or Apache-2.0) on its default `miniz_oxide` backend, the version and features the Linux updater and xtask already build, so it adds no package. The `tiff` crate's encoder is not used: in 0.11.3 its strip-by-strip writer ignores the compression it declares in the file (only its whole-image write compresses) and its directory encoder borrows the encoder for its whole life, so it can neither compress strips as bands arrive nor be held between bands.

Three packages are for the export tests only, as independent decoders: `image-webp` 0.2.4 (MIT or Apache-2.0) for WebP, and `tiff` 0.11.3 (MIT), with only its `deflate` feature, for TIFF, which brings `quick-error` 2.0.1 (MIT or Apache-2.0). Their other dependencies (`byteorder-lite`, `half`, `flate2`) were already in the lockfile. As with every other dependency, passing the automated checks is not the deferred manual review.

## MCP schema tests

The [MCP adapter](../design/mcp.md#schema-generation)'s generated input schemas are checked, in `luxforge-cli`'s tests only, by an independent validator: `jsonschema` 0.60.1 (MIT), pinned exactly in the workspace manifest with no default features, so it brings no HTTP or file retrieval, TLS stack or IDNA data. It validates every generated schema against the bundled JSON Schema 2020-12 metaschema and cross-checks the adapter's own request check. It is a development dependency of `luxforge-cli` alone; no shipped binary links it, and the adapter's request check is its own code over the core's value check. It adds 27 packages to the lockfile:

- MIT: `jsonschema`, `jsonschema-regex`, `jsonschema-value` and `referencing` 0.60.1, `fancy-regex` 0.19.2, `fluent-uri` 0.4.1, `email_address` 0.2.9, `uuid-simd` and `vsimd` 0.8.0, `outref` 0.5.2, `data-encoding` 2.11.1, `micromap` 0.3.0, and `strum` and `strum_macros` 0.28.0;
- MIT or Apache-2.0: `fraction` 0.17.0, `num` 0.4.3, `num-bigint` 0.4.8, `num-iter` 0.1.46, `num-rational` 0.4.2, `num-cmp` 0.1.0, `bytecount` 0.6.9, `heck` 0.5.0, `ref-cast` and `ref-cast-impl` 1.0.27, and `allocator-api2` 0.2.21;
- Apache-2.0: `unicode-general-category` 1.1.0;
- MIT-0: `borrow-or-share` 0.2.4.

These packages are in the graph `cargo xtask audit` checks, so their advisories and sources are checked there. Its licence check skips packages that only a development dependency reaches, so their licences are recorded here rather than checked; MIT-0 is on `deny.toml`'s allowlist. No build that ships links any of these packages.

The adapter binary, `luxforge-mcp`, adds no package to the lockfile: it names `base64` 0.23.1 (MIT or Apache-2.0) with `std`, as `ureq` builds it, to encode the pictures it attaches. Its process tests hold every message it writes to the MCP specification's own `schema.json` for 2026-07-28 and 2025-11-25, vendored unmodified under `crates/luxforge-cli/tests/mcp/spec/` with their source commit, checksums and the specification project's licence (Apache-2.0, with earlier contributions under MIT) in `THIRD_PARTY.md`. They are test inputs; no binary includes them.

The adapter's [independent client test](../design/mcp.md#verification), `tests/mcp/interop.rs`, drives the real `luxforge-mcp` through `rmcp` 3.7.0 (Apache-2.0), the official MCP Rust SDK, pinned exactly in the workspace manifest with no default features and only `client` and `transport-child-process`, so no server, macros or HTTP transport. It is a development dependency of `luxforge-cli` used by that test alone, on a current-thread runtime of the Tokio 1.53.3 the workspace already locks, with `rt`, `sync` and `process` asked for. It adds 19 packages to the lockfile:

- Apache-2.0: `rmcp` 3.7.0;
- MIT: `tokio-util` 0.7.20 and `tokio-stream` 0.1.19, which `rmcp` needs, and `tokio-macros` 2.7.2, from the Tokio `macros` feature it asks for;
- MIT or Apache-2.0: `chrono` 0.4.45, which `rmcp`'s protocol types use, with `iana-time-zone` 0.1.65; `process-wrap` 10.0.1, its child-process transport; `signal-hook-registry` 1.4.8, from Tokio's `process`; and, on Windows and Haiku only, `iana-time-zone-haiku` 0.1.2 and `windows` 0.62.2, `windows-core` 0.62.2, `windows-collections` 0.3.2, `windows-future` 0.3.2, `windows-implement` 0.60.2, `windows-interface` 0.59.3, `windows-numerics` 0.3.1, `windows-result` 0.4.1, `windows-strings` 0.5.1 and `windows-threading` 0.2.1.

Every one is under a licence `deny.toml` allows, though as development-only packages the licence check skips them. `cargo xtask audit`'s graph includes them: their sources pass, and they bring no advisory of their own, though `rmcp` and `process-wrap` join the paths to the workspace's Tokio, so a Tokio advisory lists them too. No build that ships links any of them: the Tokio features they add are unified only into a build of `luxforge-cli`'s tests.

As with every other dependency, passing the automated checks is not the deferred manual review.

## Usage statistics dependencies

The [usage statistics](../design/usage-statistics.md) consent record's image key is 32 bytes of the operating system's cryptographic randomness from `getrandom` 0.4.3 (MIT or Apache-2.0), a direct dependency of `luxforge-core` with no features. It is the version `uuid` already draws its v4 IDs from, so it adds no package to the lockfile.

## Hardware SHA-256 on aarch64

`sha2` stays at 0.10.9, whose aarch64 SHA-2 backend is compiled only with its `asm` feature. Every manifest that names `sha2` turns the feature on in an aarch64 target table that excludes Windows, so it is in the aarch64 macOS and Linux graphs and in no x86_64 graph (`cargo tree -e features -i sha2 --target TARGET` shows it). The feature adds one package, `sha2-asm` 0.6.4 (MIT; `cc`, already in the lock file, assembles its sources; copyright notices in its `LICENSE` file for RustCrypto Developers, Project Nayuki and Artyom Pavlov, which `cargo xtask inventory` copies to `licenses/sha2-asm-0.6.4`). `sha2` 0.10.9 calls `sha2-asm` on x86 only, so on aarch64 the assembly is built and not used, and the crate refuses to build for Windows. Moving to `sha2` 0.11, which needs no feature for the aarch64 backend, would remove both, but its digest output has no hexadecimal formatting, which 83 call sites use. The manual native and notice review stays deferred.

`jxl-oxide`'s `rayon` feature, which the JPEG XL DNG decoder runs on, adds no package: `jxl-threadpool` uses the `rayon` and `rayon-core` already in the lock file. `half`'s `std` feature, which lets x86_64 detect F16C at run time (`iced_graphics` already enables it in a build with Iced; the core and UI manifests name it too), adds none either.
