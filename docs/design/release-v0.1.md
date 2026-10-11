# Preparing v0.1 for distribution

Status: **tag-build tooling implemented; public distribution qualification remains open**. Local archives are build rehearsals, not qualified public release candidates. The owner requested preparation on 2026-10-08. This design and the [task plan](../../tasks/project/release-v0-1.json) describe the work; they distinguish implemented build tooling from the unrun signing/native/distribution qualification gates. [Tag-build behaviour](tag-release-builds.md) and [operator instructions](../engineering/releases.md) describe the owner-authorized automation.

## Scope

Prepare Luxforge **0.1.0**, tagged `v0.1` or `v0.1.0`, as an early preview of the implemented editor. The owner chose **macOS and Linux together**. Target Apple silicon macOS and x86-64 Linux on the existing Ubuntu 24.04 baseline, through the repository's GitHub Releases page, marked pre-release. Both downloads must satisfy their platform gates before the joint release is ready; Windows remains later scope. Platform-specific packaging work can proceed independently.

Use the existing `release` profile, pinned toolchain and lockfile. The owner-deferred `dist` profile is not a prerequisite. Release numbering does not establish a stable catalog, recipe, API or module compatibility promise: unsupported data continues to be refused without modification. No migrations or compatibility fixtures are introduced.

Release the working editor as it stands at the selected commit. MCP, AI editing, Clone/Heal, Lightroom catalog import/alignment, HDR merges, export settings and the other planned features are not release prerequisites. A feature merged before the candidate is frozen joins its qualification and documentation scope. Do not restore the controlled RAW/Detail studies the owner closed, or require all outstanding calibration and performance research. Report their remaining limits accurately.

The release does not add stores, accounts, telemetry, an updater, a website, package-manager publishing or a general release framework. GitHub hosts the files; installation and updates are manual. Linux gains release packaging and native qualification under its recorded baseline; Windows development scope stays unchanged. A Linux release is not a promise of compatibility with every distribution or native GPU/driver combination.

## What exists and what is missing

| Area | Current implementation | Preparation needed |
| --- | --- | --- |
| Identity | Developer workspace version `0.0.0`; tag builds compile one normalized version into all three executables and bundle/build records | Extend build environment/source/qualification records for the final distribution packet |
| Packaging | Development `package` stays unsigned; `release-build` creates versioned tag archives with all three executables, resources, notices, identity and final hashes, refusing dirty input outside labelled rehearsal | Final installation/source/native qualification and release-ready validation |
| macOS bundle | Tag builds include build number, CLI helpers, resources, notices and help inside `org.luxforge.app`; configured floor 14.0 | Application icon and actual advertised-floor/installation qualification |
| Linux distribution | Tag builds produce an x86-64 Ubuntu-baseline tarball with editor, both CLIs, resources, notices and help; hosted/native run remains unclaimed | Desktop/icon integration, inspected runtime requirements and native Wayland/X11/GPU installation verification |
| Signing | Explicit signing/notarization tooling and credential-free failure tests; paid membership active, local setup owner-reported, hosted secret setup still operator-owned | Real Developer ID/notary execution and final signature/ticket/archive validation |
| Source and notices | Dependency inventory plus native/font/theme/data notices | Corresponding source archive, build/relink instructions and a completed distribution-specific review |
| Dependency policy | Expiring `paste` and `ttf-parser` exceptions scoped to development | Fresh distribution review; a successful development audit alone is insufficient |
| Evidence | Native M4 checks and a corpus GPU gate exist; recent features have incomplete qualification | Same-candidate full native verification, complete required corpus gate and installation evidence |
| Delivery | Tag workflow builds both platforms, optionally signs macOS, verifies identities/hashes and automatically publishes a pre-release; no hosted run/public release claimed | Complete matching source/policy/native qualification and a release-ready packet before claiming a qualified release |

These findings come from [package.rs](../../xtask/src/package.rs), [dependency policy](../engineering/dependencies.md), [platforms](../engineering/platforms.md), [verification](../engineering/development.md#verification-tiers), [feature status](../features.md) and the current CI workflow. Configured checks and old evidence are not evidence for the candidate.

## Identity and candidate inputs

Ordinary developer builds use `workspace.package.version`, currently `0.0.0`. A tag build uses its validated normalized SemVer as the shared compile-time application version, without changing manifests or `Cargo.lock`; `v0.1` normalizes to `0.1.0`. All three executables and archive/build metadata agree on it. The workflow supplies a numeric macOS build number from its run number. Keep `org.luxforge.app` as the bundle identifier. Recipe/catalog/module format markers change only when their own shapes change.

The editor, `luxforge-json` and `luxforge-ctl` each support a non-launching `--version` that reports that application version. Reuse existing build-version reporting; do not add an About screen or duplicate authoritative version state in widgets. Any new service-visible build information follows the command registry/schema rules.

A candidate names a clean, full commit SHA, target triple, application version, build number, profile, pinned toolchain, lockfile hash and build environment (OS, SDK and native compiler). Reject unsupported targets, dirty tracked or untracked source input, a tag/version mismatch and an existing output directory. Excluded build/evidence directories do not make a source tree dirty. Refuse rather than silently editing the checkout or guessing the commit.

For macOS, the target is `aarch64-apple-darwin`. macOS 14 is the intended engineering floor, **not currently verified support**. Check the executable's actual deployment target and runtime imports, and record execution on the oldest advertised OS before publishing that support claim. A hosted build on macOS 14 is not native installation or GPU evidence. A missing floor test remains an open release input; changing the advertised floor needs a focused scope update.

For Linux, use `x86_64-unknown-linux-gnu`, built on Ubuntu 24.04 with glibc 2.39 as the existing baseline. Inspect actual ELF imports and symbol-version requirements. Require native x86-64 Linux desktop/GPU evidence on that baseline, including GNOME Wayland and X11, before claiming support. The owner's ARM64 Omarchy VM and hosted lavapipe runs supplement this evidence; neither replaces it. Other distributions, Linux ARM64 and Intel macOS are outside the initial supported download matrix.

## Deliverables

The macOS download is `luxforge-0.1.0-macos-arm64.zip`, containing one `Luxforge.app`. A ZIP reuses the current simple distribution format; a DMG is unnecessary for the first release. The app contains:

- `Contents/MacOS/luxforge`, with a normal foreground application plist for human launches.
- `Contents/Helpers/luxforge-json` and `Contents/Helpers/luxforge-ctl`, executable and signed. No automatic PATH modification or privileged installer.
- `Contents/Resources/lensfun`, the existing runtime index and its attribution.
- A bundled application icon, notices, project licence, installation/help text and build metadata under `Contents/Resources`, so copying the app to Applications keeps them.

Delegate a modest original application icon and its source asset, plist serialization, code organization and routine archive mechanics to the implementer. Do not introduce a branding approval gate or a second packaging toolchain. Preserve the existing unsigned development command and its clear label; never silently sign, submit or publish when that command runs.

The Linux download is `luxforge-0.1.0-linux-x86_64.tar.gz`, containing a relocatable `Luxforge/` directory with `luxforge`, `luxforge-json`, `luxforge-ctl`, adjacent runtime resources, icon, desktop-entry template, notices, licence, help and build metadata. Preserve executable modes and resolve resources relative to the installed payload, never the checkout or working directory. Provide an explicit user-level install/uninstall helper or documented commands for desktop-entry/icon integration; extraction and direct launch also work. Delegate those mechanics to the implementer, with no root requirement or automatic shell/PATH changes. A native file chooser requires a working XDG portal and backend; document runtime libraries and a Vulkan-capable driver rather than shipping vendor GPU drivers or promising a universally self-contained archive. AppImage, Flatpak, Snap and distro repositories are not v0.1 prerequisites.

Alongside both downloads, prepare `luxforge-0.1.0-source.tar.gz`, `SHA256SUMS`, `release.json` and release notes. The source archive includes the exact project source, vendored Rust dependencies, native sources and applied patches, compiled-in assets/data, their notices, toolchain/lockfile and build/install/relink instructions. Preserve the repository Cargo configuration while adding offline vendoring configuration. Test builds for both shipping targets from the extracted archive on their respective build hosts, without the original checkout or Cargo source cache. A Linux container may prove the offline build, but not native desktop/GPU qualification. Bit-reproducible binaries are not a v0.1 claim.

Review native static linking, selected licences and the ability to modify/rebuild/relink the LGPL components, alongside Rust and bundled font/theme/Lensfun/RawSpeed/GeoNames assets. Inventory is input to that review, not its conclusion. Supply applicable licence texts and attribution/change notices. Exclude private photographs, the licensed test corpus, catalogs, grants/secrets, credentials, developer paths, caches and verification output. Include only redistributable assets needed to build/run the shipped product; document any test-only omissions. GitHub's automatically generated repository archive does not substitute for this complete source deliverable.

`release.json` lives outside the signed app. It records both targets' final filenames, sizes and SHA-256 hashes, the common commit/version/lock identity, per-target executable and build identities, source archive identity, macOS signing/notarization results and per-platform qualification. Linux Apple-notarization fields are explicitly not applicable, not a skipped required macOS gate. A one-platform packet remains incomplete for this joint release. Keep private raw evidence separate from a sanitized public summary. Do not embed a hash of an archive inside that archive, or a bundle hash inside the bundle it describes. Label any pre-signing binary hash as pre-signing; final signed hashes are measured after signing. Generate `SHA256SUMS` only after final packaging and metadata are complete.

## Signing and notarization

Prepare tooling and failure tests without requiring account credentials. A real signed candidate later needs an Apple Developer membership, a usable **Developer ID Application** identity and notarization credentials controlled by the release operator. The owner's paid Apple Developer Program membership is active, verified read-only in the account's membership details on 2026-10-09; certificate tools are available. The owner reports completing signing setup and storing notarization credentials under the Keychain profile `luxforge-notarization`. Certificate/private-key usability and profile authentication have not been independently checked; validate them during the final candidate task before signing or submission. Read-only account inspection is authorized; this preparation does not authorize credential creation or acceptance of further agreements. Use a named Keychain profile or the supported notary API credential mechanism; keep passwords and private keys out of source, CLI logs and release assets. Do not enumerate or export the owner's credentials during preparation.

Operator setup on the build Mac:

1. In Xcode Settings, add the enrolled Apple Account, select its team and open **Manage Certificates**. Use the add button to create **Developer ID Application**, keeping its private key in the local Keychain. This is the certificate for the app; Developer ID Installer is for installer packages ([Apple certificate guidance](https://developer.apple.com/help/account/certificates/create-developer-id-certificates/), [Xcode certificate management](https://help.apple.com/xcode/mac/current/en.lproj/dev154b28f09.html)).
2. For the Apple Account authentication path, create an app-specific password labelled for Luxforge notarization under **Sign-In and Security → App-Specific Passwords** at `account.apple.com` ([Apple instructions](https://support.apple.com/en-us/102654)). Store it locally with the existing Apple tool below, using the account's Team ID from membership details. Enter the app-specific password at its secure prompt; do not pass it as a command argument or send it in chat. The command validates credentials before saving the named Keychain profile by default.

```sh
xcrun notarytool store-credentials luxforge-notarization \
  --apple-id "YOUR_APPLE_ID" --team-id "YOUR_TEAM_ID"
```

This configures Apple's existing tool, not a Luxforge release command. No certificate or credential is created by the repository's preparation work. Final signing uses an explicitly selected Developer ID identity and the `luxforge-notarization` profile; it must validate both before submitting a candidate.

Sign nested executables first, then the finished app, with hardened runtime and a secure timestamp. Start with no exception entitlements and add only demonstrated requirements. Verify all executables, the bundle seal, expected identity and runtime configuration. Do not use recursive signing as a substitute for enumerating the payload.

Submit a metadata-preserving ZIP to `notarytool`, retain the submission ID and result, and require **Accepted**. Staple the ticket to the app, validate it, then create the final ZIP from that stapled app. A ZIP itself cannot be stapled. Fail on rejection, timeout, missing tools, absent identity or invalid tickets, keeping diagnostic output for the operator. No unsigned fallback may receive a release-ready label.

Treat the signed app as immutable. Signing, stapling and final archive extraction must precede its final hashes and assessment. Credentials and the Apple service are late inputs; their absence must not hold up identity, packaging, source, verifier or documentation work.

## Qualification and evidence

Build ordinary packaging/version/verifier tests with their implementation, then deliver the usable unsigned candidate machinery. Final qualification comes after those changes are integrated and the candidate commit is frozen:

1. Run fresh dependency audit and finish the target-specific source/native/asset review. Existing exception reasons explicitly say local development or no distribution approval. Do not infer distribution suitability from those suppressions; record a fresh bounded disposition or an unresolved publication blocker without replacing the GUI stack just to clear an advisory.
2. On the native M4, run `full` with the owner RAW manifest against the candidate executable, including the required GPU-against-reference gate. Retain state, logs, adapter identity, captures and source hashes. Cover current Select/Locate, RAW looks, copy/paste and Auto tone through the existing journeys, adding a narrow journey only where a release-critical path is absent. A partial corpus, software adapter, skip or incomplete component never passes the native release gate.
3. Inspect relevant captures and the full summary. Preserve the accepted motion policy and declared GPU tolerances. Report provisional timing misses and unreliable/unmeasured figures honestly; do not invent new performance targets or convert an unqualified total-memory claim into a support promise.
4. After signing/notarization, inspect an extraction of the final ZIP for signatures, tickets, resources, versions, archive attributes and checksums. Rerun affected checks if signing, metadata or a rebuild changes the candidate. Normalized executable identity can connect pre-signing rendered evidence to the signed candidate, with the method recorded; never treat a different build as the same candidate.
5. On native x86-64 Ubuntu 24.04, run the complete platform-appropriate rendered tier and required GPU corpus gate against the Linux candidate with hardware Vulkan. Record native adapter/driver/OS and inspect correlated state/logs/captures. Cover Wayland and X11, file chooser/portal behavior, Select/index watching, Locate, RAW edits, masks, save/reopen/export, both CLIs and no-GPU-stage fallback. Reuse hosted no-adapter/lavapipe checks as functional evidence only, resolve candidate failures, and do not label the incomplete hosted software lane passed. Check extracted-archive launch without the checkout, desktop-entry/icon installation, updates and user-level uninstall retaining data and originals. Native Linux and minimum-baseline access are late external inputs; they do not block packaging implementation.
6. Record a clean installation from a quarantined downloaded final ZIP on a separate supported Apple silicon Mac or clean test account: copy to Applications, Gatekeeper acceptance, first launch, native file picker, open JPEG and supplied supported RAW, edit/undo/reopen/export with original hashes unchanged, both CLIs, replacing the app while retaining its data, and uninstall retaining originals/catalogs. Verify offline ticket assessment. Test paths containing spaces and non-ASCII characters. Name the actual OS/adapter and tested build; the minimum OS remains unverified until tested.

Automated editor checks continue to use hidden background bundles and isolated catalogs/data roots. The existing harness copies the executable into `org.luxforge.background-test`, changing its bundle context; that proves renderer/command behavior, **not Finder launch, the distributed bundle seal, Gatekeeper, installed resources or production shader-cache behavior**. Keep that distinction in `release.json`. Installation/foreground checks require an explicit interaction request and a suitable test machine/account; this preparation does not authorize activating the owner's desktop.

Missing private RAW files, a macOS floor machine, a native x86-64 Linux GPU machine, signing credentials or a clean install session affect only their final qualification steps. Prepare everything else first and record what remains unrun. No task asks an implementer to reconfirm scope or obtain approval.

## Delivery and publication

Prepare a local release packet with the final assets, notes and a machine-readable readiness result. Its verifier fails closed on missing/tampered files, candidate mismatches, development-only policy dispositions, invalid signing/notarization or incomplete required evidence. An unsigned rehearsal remains explicitly a rehearsal. Public evidence contains no private file paths, coordinates, photos or secrets.

The [tag workflow](../../.github/workflows/release.yml) is authorized by the owner's automation request: a pushed tag builds both platforms, optionally signs/notarizes macOS, verifies transport identity/hashes and automatically publishes a **GitHub pre-release** after every asset upload succeeds. The owner explicitly selected automatic pre-release publication. The tag is the build input; workflow creation does not authorize pushing one now. Hosted Apple secrets remain a separate operator input from the local `luxforge-notarization` profile. The published notes and metadata explicitly record source/advisory/native/install gates as pending or not run and never claim release readiness. Existing releases/assets are not overwritten. Only the publication job gets repository write permission; Apple secrets reach only its macOS signing step. Native qualification still needs a later ready-packet verifier over those candidates. A hosted build is not native GPU evidence, and this tooling makes no action-attestation claim.

At handoff, provide the reviewable files, evidence scope and any unmet inputs. The owner pushes a chosen release tag to request a build; this implementation does not create or push one. Pushing a release tag requests automatic pre-release publication; the workflow stages all assets in a temporary draft and then publishes it. A failed upload or moved tag leaves the draft unpublished. Once published, assets stay fixed; corrections use another version. Manual updates replace the app, preserve user data and follow the current-format refusal rule. Retain the prior app and data backups when evaluating a newer format; do not promise an older app can read newer catalogs.

The [release notes draft](../releases/v0.1.md) is prepared now. Before publication, fill in verified requirements and real download/hash/evidence links, describe only shipped features, and include installation, data preservation, current-format limitations, supported-file scope, manual updates and issue-report instructions. The README, guide, platform/status docs and package help change to downloadable-install instructions only when the corresponding artifact exists.

## Acceptance

- A clean common candidate produces both versioned downloads, complete matching source, notices, checksums, release metadata and useful installation/help text.
- All three executables agree on version; installed behavior uses no checkout-relative resources; replacing/removing the app preserves user data and originals.
- Signing/notarization and final extraction checks pass, with the actual bundle assessed separately from the background test wrapper.
- Native M4 full verification and the complete required corpus gate pass; native x86-64 Linux rendered/GPU qualification, Wayland/X11 and installation checks pass against the same commit. Advertised OS baselines and clean installations are tested. Remaining scope limits stay visible.
- Source/native/asset and advisory distribution dispositions are recorded; a development inventory/audit alone never passes readiness.
- The complete two-platform packet is reviewable before any public tag or publication. This planning change claims none of these gates has passed.

## Primary distribution references

Apple documents Developer ID signing, hardened runtime and notarization in [Notarizing macOS software](https://developer.apple.com/documentation/security/notarizing-macos-software-before-distribution), and ZIP submission/app stapling in [Customizing the notarization workflow](https://developer.apple.com/documentation/security/customizing-the-notarization-workflow). Its [packaging guide](https://developer.apple.com/documentation/xcode/packaging-mac-software-for-distribution) calls for testing the distributed product, preferably on a different Mac. GitHub documents draft/pre-release assets in [Managing releases](https://docs.github.com/en/repositories/releasing-projects-on-github/managing-releases-in-a-repository). The GNU [licence FAQ](https://www.gnu.org/licenses/gpl-faq.en.html#LGPLStaticVsDynamic) explains source and LGPL relinking obligations; apply each bundled component's actual licence in the distribution review.
