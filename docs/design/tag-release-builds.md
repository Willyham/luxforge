# Tag-triggered release builds

Status: implemented and checked with focused tests, workflow lint, quick verification and an extracted local macOS rehearsal; no hosted release run or publication claimed.

## Behaviour and scope

The owner requests pushing a release tag to build macOS and Linux binaries. Accept `vMAJOR.MINOR`, `vMAJOR.MINOR.PATCH`, and SemVer prerelease/build suffixes; normalize a missing patch to zero. Reject malformed tags before expensive work. The tag supplies the compiled application version, archive names and metadata; ordinary developer builds keep the workspace version. Preserve the source checkout and lockfile. All three executables answer `--version` without opening a catalog, network connection or window.

Build Apple silicon macOS on the existing `macos-14` runner and x86-64 Linux on Ubuntu 24.04. Compile with the existing release profile and locked dependencies. Produce a macOS ZIP with `Luxforge.app` containing the editor, CLI helpers, resources, notices and help, a drag-to-Applications disk image holding the same app ([downloads](auto-update.md#macos-sparkle)), and a Linux tarball with the same executable/resource set. Inspect each packaged executable's version. Archives and metadata retain common tag/commit/version/lock identity, target, toolchain, build environment and final checksums, as the [candidate records](release-v0.1.md#candidate-records) define them. A clean tag build also embeds its source revision for the [usage statistics build identity](usage-statistics.md#build-identity-and-activation), and its metadata records the resulting `usage_release`; a dirty rehearsal embeds none. Developer packaging stays unchanged.

The destination is an **automatically published GitHub pre-release**, after both builds succeed, with Actions artifacts also retained. The owner explicitly chose automatic pre-release publication. It is a build candidate: source/licence/advisory review and native/corpus/installation qualification from the [v0.1 distribution contract](release-v0.1.md) remain separate open gates. Public notes and metadata report these limits without claiming release readiness. The owner's tag push requests building and publishing that pre-release; this implementation does not push a tag or upload any private credential.

## Release command

`cargo xtask release TAG [--dry-run]` uses the lightweight tooling. Require an attached `main` branch, a clean index and working tree including untracked files, and the release workflow committed at HEAD. Read `origin`'s single configured push destination directly, without relying on cached tracking refs, and require its `main` commit to equal HEAD. Reject a local or remote tag whose normalized version is already used, including the `v0.1`/`v0.1.0` alias. No branch, manifest, commit, existing tag or remote configuration is changed to pass these checks.

For an actual release, run quick checks and the native watcher gate, repeat the Git/remote checks and require the same commit and push destination. Create an annotated tag at that verified commit and push only its explicit ref, without force or incidental follow-tags. This starts the existing hosted build/publication workflow; local signing and building are not added. Return after the tag push rather than waiting for hosted jobs. A push failure returns failure and retains the local tag with a clear recovery message, since the remote outcome may be uncertain. Never delete or overwrite an existing tag automatically.

`--dry-run` performs the Git/remote checks and prints the planned version/commit/actions, skipping quick and native watcher checks and all Git mutations. Testing uses isolated repositories and local bare remotes, including rejected pushes; implementing this command does not invoke it against the owner's remote. Native/distribution qualification and hosted signing setup remain separate inputs.

## Signing

The local Keychain profile is `luxforge-notarization`. Hosted runners cannot access it. Signing is opt-in through the repository variable `LUXFORGE_SIGN_MACOS=true`; absent or false builds are explicitly unsigned. Enabling signing requires GitHub environment/repository secrets for a Developer ID Application certificate/private key exported as a password-protected P12, its password, the explicit signing identity, Apple Account, Team ID and an app-specific password. Export/upload is an operator step; never enumerate/export the owner's credentials while implementing tooling.

Import credentials into a temporary runner Keychain only for the macOS signing step, after building. Clean up using an always-running step and a shell trap on setup failure. Sign helpers first and the app last with hardened runtime and timestamp, verify, submit with `notarytool`, require Accepted, staple and validate the app, and recreate the ZIP. Then create the disk image from the stapled app, sign, notarize, staple and assess it, before final hashes. Signing failures fail the job; never fall back to unsigned. Keep secret values out of command logs and uploaded files. The same signing tool supports the operator's existing local Keychain profile.

Before changing signing state or invoking signing tools, `release-sign` extracts the checksum-verified unsigned ZIP into fresh staging with `ditto`. Admission compares the expanded app's complete tree with that archived app: file bytes, file and directory permissions, entry types and symbolic-link targets, including resources and framework code. The archived executables must also match `before_signing`. A mismatch refuses signing without replacing the expanded app. Admission is bounded to 100,000 archive/tree entries, 4 GiB of file data and 128 path components; extraction refuses foreign paths, escaping links and entries beneath symbolic links.

## Delivery and reruns

The standalone `update-record` command validates both target records against the same common candidate identity as assembly before signing either archive, including the source revision, dependency lock and build number of a macOS archive omitted because it is unsigned.

The identity job captures the exact commit; both build lanes check out that commit and require the tag to name it. Only a successful pair of builds reaches publication. Assemble using exactly the two target records, recompute archive hashes and require matching tag/version/commit/lock identities. Recheck the remote tag before creating a temporary draft and again before publishing it. Upload all assets to the draft, then automatically make it public with the pre-release flag. When update publication is on, one step before the draft signs the update record, the only step that receives the update key, and a separate workflow publishes the update feeds afterwards with read-only access ([automatic updates](auto-update.md#publishing-and-serving-the-feeds)). An upload or final tag-check failure leaves the draft unpublished. The write-capable job downloads finalized artifacts; it does not rebuild or receive Apple secrets. Use pinned actions and read-only default permissions. Existing releases are not overwritten; an operator must deliberately remove an unwanted failed draft before rerunning delivery. Published corrections use a new version. Retain failed build logs through normal Actions logs, never credential files.

## Acceptance and verification

- Short/full/prerelease tags normalize consistently; invalid and shell-like tags fail before build.
- Release initiation checks clean synchronized main and unused normalized versions, runs quick and native watcher checks and rechecks before pushing exactly one annotated tag. Isolated Git tests cover dry-run, rejected state, changed state and failed push without triggering the owner's workflow.
- Both complete payloads preserve executable permissions and notices after installation, and all binaries agree on the tag version.
- Metadata and final archive hashes bind both targets to one immutable source commit; a dirty local rehearsal is explicitly labelled.
- Signing plan and command failures are covered without real credentials or Apple submissions.
- Workflow builds both architectures from the pushed tag, optionally signs macOS, and automatically publishes a pre-release after all assets are uploaded; it never silently changes a tag or overwrites existing assets.
- Focused Rust tests, archive/metadata failure tests, workflow lint and a real local macOS packaging/version check pass; quick runs once at handoff. Hosted Linux/build/signing/native checks that have not run remain unclaimed.

Runtime changes under `crates/` only add one shared compile-time version constant, which logs, reports and the network user agent also state, and early `--version` exits. They read no originals, allocate no image frames, run no owner/catalog work, request no state/history/preview/upload, add no timers or caches, and affect no renderer/reference buffers. No image or performance result is claimed.
