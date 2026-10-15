# Building downloads from a release tag

The [Release builds workflow](../../.github/workflows/release.yml) builds Apple silicon macOS and x86-64 Linux downloads from a pushed tag. It retains Actions artifacts and **automatically publishes a GitHub pre-release** after both builds and every asset upload succeed. These downloads are build candidates; the [v0.1 distribution qualification](../design/release-v0.1.md) remains open and is reported in their notes and metadata.

## Start a release

Commit, merge and push the release tooling and application changes to `main` first. From a clean `main` checkout matching `origin/main`, run:

```sh
cargo xtask release v0.1
```

The command validates the tag, requires an attached `main` branch and a clean index/working tree including untracked files, and checks that the release workflow is committed. It queries `origin`'s actual push destination and requires its live `main` to match HEAD; a stale tracking ref cannot pass. Existing local or remote normalized versions are refused, so `v0.1.0` cannot reuse a `v0.1` release.

It runs quick and native watcher checks, rechecks the checkout and remote, creates an annotated tag at the verified commit and pushes only that tag. It returns after the push; GitHub Actions builds and publishes asynchronously. It does not commit changes, update/push `main`, modify Cargo versions, export credentials or overwrite tags. This command starts public pre-release publication after the hosted builds/uploads succeed.

Preview the Git/remote checks and planned actions with:

```sh
cargo xtask release v0.1 --dry-run
```

Dry-run creates/pushes no tag and skips quick and native watcher checks. If a real push fails, the local tag is retained because the remote outcome may be uncertain. Inspect the tag at `origin`'s push destination before retrying; remove a local-only failed tag deliberately only after confirming it was never published. A successful push is monitored under **Actions → Release builds** rather than rerunning the release command for the same version.

`v0.1` builds version `0.1.0`. Full tags such as `v0.1.2`, prereleases such as `v0.2.0-rc.1`, and SemVer build suffixes also work. Invalid tags fail before native compilation. The workflow binds both builds to the initially selected commit and refuses delivery if the tag moves. No separate Cargo version edit is needed: the tag supplies the compiled application version, while ordinary developer builds use the workspace version. The source checkout and lockfile stay unchanged.

Open GitHub **Actions → Release builds** for logs and the public pre-release under **Releases**. It contains:

- `luxforge-VERSION-macos-arm64.dmg`, the macOS download: a disk image holding `Luxforge.app` and an `Applications` link. Open it and drag Luxforge to Applications. The app holds the editor, both CLIs under `Contents/Helpers`, Lensfun resources, notices and help.
- `luxforge-VERSION-macos-arm64.zip`, the same `Luxforge.app`, kept as the update archive.
- `luxforge-VERSION-linux-x86_64.tar.gz`, containing `Luxforge/` with all three executables, resources, notices, help (`README.txt` and the Linux `INSTALL.txt`), the icon and `desktop-integration.sh`, which adds or removes a user-level launcher entry ([Linux payload](../design/release-v0.1.md#deliverables)). Its `build.json` and `target.json` record each executable's ELF imports.
- `luxforge-VERSION-source.tar.gz`, the complete corresponding [source archive](#the-source-archive) of the same commit.
- `release.json`, per-platform build records and `SHA256SUMS` for the final files.
- `luxforge-VERSION-updates.json`, the signed update record, once [automatic updates](#automatic-updates) are switched on. It is not listed in `SHA256SUMS` or `release.json`.

Before publishing, the publish job attests the build provenance of the disk image, the ZIP and both tarballs with `actions/attest-build-provenance`, signed through Sigstore as this repository's Release builds workflow. `gh attestation verify FILE --repo Willyham/luxforge` checks a download against it.

The macOS app embeds Sparkle 2.10.0's `Sparkle.framework` for its updater. `release-build` downloads the official archive once into `target/sparkle/2.10.0/`, refuses it unless its length (16,319,840 bytes) and SHA-256 are the pinned ones (`xtask/src/sparkle.rs`), and copies the framework into `Luxforge.app/Contents/Frameworks/` without its XPC services; no Sparkle binary is committed. The app's `Info.plist` names the macOS feed, the public key committed in `release/update.json`, daily automatic checks and verification of an archive before it is extracted. Signing signs Sparkle's `Autoupdate`, `Updater.app` and the framework before the helpers and the app.

The Linux build uses Ubuntu 24.04/glibc 2.39 and fails if an executable requires a newer glibc symbol version or links a library the packaged help does not name. Native Vulkan drivers, runtime libraries and an XDG chooser portal/backend are still required. The payload carries `packaging/icon/luxforge-256.png`, `luxforge-512.png` and `luxforge.svg` for the launcher icon; a build missing any of them, or the helper or its template under `packaging/linux/`, fails before compiling. macOS 14 is the configured build floor, not a verified installation-support claim. Read the artifact's signing status: macOS builds are **unsigned unless signing is enabled**. Pushing a release tag requests public pre-release downloads; no manual publish step follows a successful run.

## The source archive

`cargo xtask release-source --tag TAG --output NEW` writes `luxforge-VERSION-source.tar.gz` and `source.json` into a new directory. It makes the same source checks as `release-build`: without `--allow-dirty` the checkout must be clean, untracked files included, the tag must name HEAD and the Rust toolchain must be the pin. `--allow-dirty` labels a local rehearsal, which assembly refuses; it still archives the committed tree, never uncommitted changes. The release workflow builds it in its own `source` job, which receives no secrets, from the same commit as both downloads.

The archive holds one directory, `luxforge-VERSION-source/`:

- the tracked tree of the commit, exactly as `git archive` writes it, except that `.cargo/config.toml` keeps every original setting and gains the offline source replacement (`vendor/`) and `ORT_CACHE_DIR`;
- `vendor/`, every crate `Cargo.lock` names, from `cargo vendor --locked --versioned-dirs`;
- `third_party/onnxruntime-1.28.0/`, the prebuilt ONNX Runtime static libraries of both shipping targets, extracted from the archives `xtask/src/onnxruntime.rs` pins after their length and SHA-256 are checked, in the layout `ort-sys` reads, so its build script downloads nothing. The pinned archives are cached under `target/onnxruntime/`;
- `notices/macos-arm64/` and `notices/linux-x86_64/`, each target's `cargo xtask inventory` output for its dependency graph;
- `BUILD.md`, `INSTALL.md` and `RELINK.md`, rendered from `release/source/` with the commit, tag, lockfile hash and pinned toolchain, and `SOURCE.json`, the same identity with the archive's contents, exclusions and test-only omissions.

Bundled native sources and patches (`crates/luxforge-raw`, `crates/luxforge-diffusion`), compiled-in data and their notices are already tracked. The archive leaves out ignored and untracked files: build, evidence and verification output, private inputs, owner originals and the licensed corpus, generated fixtures, catalogs, caches and environment files. A tracked path that looks like one of them (a top-level `artifacts/`, `dist/`, `private/` or `target/`, any `target/` directory, a camera original, a catalog, an environment file or a key, except the throwaway TLS test certificates of `luxforge-net`) refuses the archive rather than being dropped. After packing, the archive's paths, link targets and contents are scanned for the home directory, checkout, output and Cargo and rustup homes of the machine that made it; a match refuses it. The prebuilt ONNX Runtime libraries are not scanned: they were built upstream and are identified by their hashes. Entries carry no owner, group or modification time.

`release-assemble` reads `source/source.json` and the archive beside it in its input when present, refuses another commit or lockfile, a dirty tree, a rehearsal or a size or hash mismatch, then copies the archive into the packet, names it in `release.json`'s `source_archive` and lists it in `SHA256SUMS`. Without a `source/` directory, `source_archive` stays null.

To check the offline build, extract the archive into an empty directory and, with a new empty `CARGO_HOME` and target directory, run `cargo build --release --locked --offline -p luxforge-app -p luxforge-cli` there. A source archive is a build input, not a completed licence review: the distribution review remains a separate gate.

## One-time hosted macOS signing setup

Your local `luxforge-notarization` Keychain profile stays on your Mac. GitHub-hosted runners need their own credentials. GitHub documents [installing an Apple certificate on a runner](https://docs.github.com/en/actions/how-tos/deploy/deploy-to-third-party-platforms/sign-xcode-applications); Apple documents [Developer ID](https://developer.apple.com/help/account/certificates/create-developer-id-certificates/) and [notarization](https://developer.apple.com/documentation/security/customizing-the-notarization-workflow).

In Xcode's **Manage Certificates**, export the **Developer ID Application** identity to a password-protected `.p12` containing its private key. This is an operator action; repository tooling never exports it. Encode the file as base64 locally, and place the values in GitHub **Settings → Secrets and variables → Actions → Repository secrets**:

| Secret | Value |
| --- | --- |
| `MACOS_CERTIFICATE_P12_BASE64` | Base64-encoded password-protected P12 containing the signing certificate and private key |
| `MACOS_CERTIFICATE_PASSWORD` | Its export password |
| `MACOS_SIGNING_IDENTITY` | Exact `Developer ID Application: … (TEAM_ID)` identity to select |
| `APPLE_ID` | Enrolled Apple Account email |
| `APPLE_TEAM_ID` | Team ID from Developer Account membership details |
| `APPLE_APP_SPECIFIC_PASSWORD` | App-specific password for the notarization service; enter it directly into the GitHub secret |

Then add the **repository variable** `LUXFORGE_SIGN_MACOS` with value `true`. Unset or `false` creates explicitly unsigned candidates; any other value is rejected. If `true`, missing credentials or a signing/notarization failure fail the macOS build and prevent publication. No unsigned fallback is uploaded as a successful signed build.

Apple credentials reach only the macOS signing step, after compilation/checks. The runner imports them into a temporary Keychain, signs Sparkle's nested code, the CLI helpers, then the app with hardened runtime and timestamp, requires an **Accepted** notary submission, staples/validates the app and recreates its ZIP. It then creates the disk image from the stapled app, signs it with the same identity and a timestamp, requires an **Accepted** submission for it too, staples/validates it and assesses it with `spctl --type open`, all before final checksums. `target.json` records both submissions and the final ZIP and disk image hashes. The temporary Keychain and P12 are removed on success or failure. Build/cache/upload steps do not receive those secret values; credential files are outside artifact/cache directories. Only the final publication job has repository write permission, with the OIDC token and attestation permissions its provenance attestation needs, and it receives no Apple credentials. Workflows do not run on pull requests.


## Automatic updates

Released builds find newer releases through the update feeds at `https://api.luxforge.app/v1/updates/<target>/appcast.xml` ([design](../design/auto-update.md)). Publication is off until you set it up:

- With the repository variable `LUXFORGE_PUBLISH_UPDATES` set to `true`, the release's publish job signs `luxforge-VERSION-updates.json` with the update key and uploads it with the other assets. A macOS build that is not notarized is left out of the record, so unsigned macOS candidates are never offered.
- After each successful release run, the **Update feeds** workflow regenerates every feed from the records of all published releases and publishes them to the API's update store. It can only read the repository: if it fails, the release stays as it was published, and you rerun **Actions → Update feeds → Run workflow** on `main` once the cause is fixed.

Only one workflow step receives each key: **Sign the update record** receives `LUXFORGE_UPDATE_SIGNING_KEY`, and **Publish the feeds to the update store** receives `LUXFORGE_UPDATE_PUBLISH_SSH_KEY`. `cargo xtask check-repository` fails if either reaches anything else.

### One-time setup

Do these in order. Until the last one, releases publish no record and no feed.

1. **Generate the update key** on your Mac with Sparkle's `generate_keys`, from Sparkle 2.10.0, the version the macOS build embeds: download [`Sparkle-2.10.0.tar.xz`](https://github.com/sparkle-project/Sparkle/releases/download/2.10.0/Sparkle-2.10.0.tar.xz) and check that `shasum -a 256` prints `c2bf58aa8387266ac179357b1415d6f2635f044da8be41042af32425dae6da0c`. Unpack it into a new folder, then run:

   ```sh
   ./bin/generate_keys --account luxforge-updates
   ```

   It creates the key in your login Keychain and prints its public key, the `SUPublicEDKey` value. Running it again prints the same public key; it never replaces a key that exists. Repository tooling never creates, reads or exports this key.
2. **Commit the public key.** In `release/update.json`, replace `null` with the printed base64 string, `{"feed_base_url": "https://api.luxforge.app/v1/updates/", "public_key": "…"}`, and merge it to `main`. Builds compiled from then on verify updates against it. `update-record` refuses to sign with any other key (`key-mismatch`), and feed generation refuses a record signed by one (`record-key-mismatch`).
3. **Store the private key.** Export it once, then store it as the repository secret `LUXFORGE_UPDATE_SIGNING_KEY`:

   ```sh
   ./bin/generate_keys --account luxforge-updates -x luxforge-update-key
   gh secret set LUXFORGE_UPDATE_SIGNING_KEY < luxforge-update-key
   ```

   The file holds base64 of the key's 32-byte seed, the form `generate_keys -x` writes. Keep an **offline backup** of it, encrypted and away from this Mac and GitHub, for example on an encrypted removable drive in a safe place, then delete the exported file. The Keychain copy stays on your Mac.
4. **Provision the update store** on the production API host, with its publication account and key, as [step 16 of the deployment checklist](../../services/usage-statistics/DEPLOYMENT.md#16-the-update-feed-route) describes. It ends with three settings in **Settings → Secrets and variables → Actions**:

   | Name | Kind | Value |
   | --- | --- | --- |
   | `LUXFORGE_UPDATE_PUBLISH_SSH_KEY` | Secret | The publication key's private half, used for nothing else |
   | `LUXFORGE_UPDATE_PUBLISH_DESTINATION` | Variable | `ssh://luxforge-feeds@api.luxforge.app:PORT` |
   | `LUXFORGE_UPDATE_PUBLISH_KNOWN_HOSTS` | Variable | The host key line, as `ssh-keyscan` prints it and checked against the host's fingerprint |

5. **Switch publication on** with the repository variable `LUXFORGE_PUBLISH_UPDATES` set to `true`. Unset or `false` publishes nothing; any other value fails the release before it builds. The next release signs its record and publishes the feeds. To publish the feeds of earlier releases with records straight away, run **Update feeds** on `main`.

The signing step refuses to run while `release/update.json` has no public key or the secret is missing, so a release fails before anything is published rather than shipping an unsigned record.

### Withdrawing a release

A withdrawn release is left out of every feed. Its tag and assets are never altered, and installations that already have it keep it until a newer release is offered.

1. Add it to `release/withdrawn-updates.json`, one entry per tag, with a reason: `[{"tag": "v0.2.0", "reason": "Loses masks on export"}]`.
2. Merge the change to `main`.
3. Run **Actions → Update feeds → Run workflow** on `main`. Its log names the release as withdrawn and not listed.

Build numbers must still rise across every release, withdrawn ones included, because someone may have installed it.

### If the update key is lost or leaked

The key cannot be recovered from the public key, and anyone holding it can sign an archive that every installation accepts.

- **macOS** can move to a new key, because Sparkle accepts a key change while the update keeps the installed app's Developer ID signature. A release built with the new public key and signed by the same Developer ID carries installations across.
- **Linux** installations cannot move. Their updater trusts only the public key it was compiled with, so after a key change each Linux installation needs one manual download of a release built with the new key.

To change keys, generate a new one under a new account name, commit its public key, replace `LUXFORGE_UPDATE_SIGNING_KEY` and its offline backup, and withdraw every release signed by the retired key, in the same change. Once the new key is committed, feed generation refuses a retired key's record unless its release is withdrawn, and macOS installations move to the first release signed by the new key.

## OmaStore

[OmaStore](https://github.com/KitsuneSemCalda/OmaStore), the Omarchy app store, lists repositories with an `omastore.toml` at the root of the default branch. Luxforge's [`omastore.toml`](../../omastore.toml) names the store's display name, summary, categories, icon and screenshots, and for x86-64 Linux the asset `luxforge-*-linux-x86_64.tar.gz` and the executable `Luxforge/luxforge` inside it. The asset is matched by a wildcard because OmaStore's `{version}` is the tag without its `v`, which is not the normalized version for a short tag such as `v0.1`. OmaStore checks a download against GitHub's digest and verifies the release workflow's [build provenance](#start-a-release) attestation.

OmaStore installs only a repository's latest **stable** release. Every Luxforge release is published as a pre-release, so OmaStore lists Luxforge but cannot install it until a stable release exists; the release workflow does not change that. OmaStore installs a release into `$XDG_DATA_HOME/omastore/apps/<owner>__<repo>/<tag>/` and updates it itself, so the Linux updater stands down for a payload there and offers new versions as their release page ([auto-update](../design/auto-update.md#linux-the-payload-updater)).

After changing `omastore.toml`, check it with OmaStore's strict lint, `omastore lint-manifest .`, and the whole listing with `omastore check --manifest omastore.toml Willyham/luxforge`. The repository's `omarchy` topic, which lets OmaStore find it without a GitHub token, is a repository setting.

## Retry and local rehearsal

Use **Actions → Release builds → Run workflow** with an existing tag, or rerun a failed run. Both paths automatically publish on success. Assets are uploaded to a temporary draft before it is made public; an upload failure or a moved tag leaves that draft unpublished. Existing GitHub releases are never overwritten. Remove an unwanted failed draft deliberately before rerunning delivery; do not move a published tag or replace published assets. Any corrected public release uses a new version.

Local build rehearsal, using the shared compiler cache on macOS/Linux:

```sh
tools/cargo-cached xtask release-build --tag v0.1 --build-number 1 --output artifacts/release-rehearsal --allow-dirty
```

The output must be new. On macOS it holds the ZIP and the disk image, both labelled unsigned in `target.json`; inspect the image with `hdiutil attach -readonly -nobrowse -mountpoint DIR IMAGE` and `hdiutil detach DIR`. `--allow-dirty` labels the output as a local rehearsal; assembly refuses it for hosted publication. Without this flag, the checkout must be clean, untracked files included, the tag must name HEAD and the Rust toolchain must be the pinned one. `target.json` and `release.json` follow the [candidate records](../design/release-v0.1.md#candidate-records): source, toolchain and build environment, before-signing and final executable hashes, and every distribution and platform gate, each `not-run` until its qualification records a result. `release-sign --output CANDIDATE --identity ID --keychain-profile luxforge-notarization` explicitly signs an existing unsigned macOS candidate when the operator runs it; ordinary development packaging never signs or submits anything.

## Qualification status

A successful build is not native GPU/corpus/install evidence, a completed distribution advisory review, or a completed source/licence review; the source archive is its input, not its result. The Linux offline build from the archive has not been run. Those gates and final support requirements remain in the [distribution plan](../design/release-v0.1.md). Published pre-release notes and metadata record them as pending or not run and never claim release readiness. They do not prevent the owner-requested automatic publication of build candidates.

## Recording gate results and verifying readiness

Download every asset of the candidate's pre-release into a new directory: the downloads, `release.json`, both platform records, `release-notes.md`, the source archive and `SHA256SUMS`. This local copy is the packet. Published assets are never changed.

Record each gate's result as its qualification finishes ([shape](../design/release-v0.1.md#gate-results-and-readiness)):

```sh
cargo xtask release-gate --packet PACKET --gate qualification.macos-arm64.native_full_verification \
  --status passed --evidence m4-full-2026-10-20/summary.json \
  --scope "M4 full tier in the background test bundle; not Finder launch or Gatekeeper" \
  --bundle background-test --executable-identity before-signing \
  --executable luxforge=SHA256 --executable luxforge-json=SHA256 --executable luxforge-ctl=SHA256

cargo xtask release-gate --packet PACKET --gate distribution.advisory_disposition \
  --status passed --evidence distribution-review/advisories.md \
  --scope "Fresh audit of the locked graph for both targets" --disposition distribution
```

Gates are `distribution.NAME` or `qualification.PLATFORM.NAME`, as `release.json` lists them. `--executable` gives the SHA-256 (`shasum -a 256`) of each executable the evidence ran; it must be the candidate's `final` hash, or on macOS its `before_signing` hash for evidence from before signing. Linux results take no `--bundle`. `--status not-run` with no other option withdraws a result. The command refuses an inconsistent packet, a malformed result, evidence from another build and evidence or scope text that looks like a private path, coordinates, a photo name, a secret or an email address. It rewrites only `release.json` and its `SHA256SUMS` line. Keep the evidence itself in the private evidence store.

Check the packet and write its public summary, outside the packet:

```sh
cargo xtask release-verify --packet PACKET --summary artifacts/release-summary.json
```

It lists every reason the packet is not ready and fails unless it is ready. The summary is written either way, with `release_ready` and `status` (`release-ready` or `not-ready`), and states the identity, assets, signing and each gate's status and scope; evidence references other than `https` links become `private`. It is not written if anything in it still looks private. An unsigned candidate, a rehearsal, a one-platform packet or any gate other than `passed` is never ready.
