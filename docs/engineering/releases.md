# Building downloads from a release tag

The [Release builds workflow](../../.github/workflows/release.yml) builds Apple silicon macOS and x86-64 Linux downloads from a pushed tag. It retains Actions artifacts and **automatically publishes a GitHub pre-release** after both builds and every asset upload succeed. These downloads are build candidates; the [v0.1 distribution qualification](../design/release-v0.1.md) remains open and is reported in their notes and metadata.

## Start a release

Commit, merge and push the release tooling and application changes to `main` first. From a clean `main` checkout matching `origin/main`, run:

```sh
cargo xtask release v0.1
```

The command validates the tag, requires an attached `main` branch and a clean index/working tree including untracked files, and checks that the release workflow is committed. It queries `origin`'s actual push destination and requires its live `main` to match HEAD; a stale tracking ref cannot pass. Existing local or remote normalized versions are refused, so `v0.1.0` cannot reuse a `v0.1` release.

It runs quick checks, rechecks the checkout and remote, creates an annotated tag at the verified commit and pushes only that tag. It returns after the push; GitHub Actions builds and publishes asynchronously. It does not commit changes, update/push `main`, modify Cargo versions, export credentials or overwrite tags. This command starts public pre-release publication after the hosted builds/uploads succeed.

Preview the Git/remote checks and planned actions with:

```sh
cargo xtask release v0.1 --dry-run
```

Dry-run creates/pushes no tag and skips quick checks. If a real push fails, the local tag is retained because the remote outcome may be uncertain. Inspect the tag at `origin`'s push destination before retrying; remove a local-only failed tag deliberately only after confirming it was never published. A successful push is monitored under **Actions → Release builds** rather than rerunning the release command for the same version.

`v0.1` builds version `0.1.0`. Full tags such as `v0.1.2`, prereleases such as `v0.2.0-rc.1`, and SemVer build suffixes also work. Invalid tags fail before native compilation. The workflow binds both builds to the initially selected commit and refuses delivery if the tag moves. No separate Cargo version edit is needed: the tag supplies the compiled application version, while ordinary developer builds use the workspace version. The source checkout and lockfile stay unchanged.

Open GitHub **Actions → Release builds** for logs and the public pre-release under **Releases**. It contains:

- `luxforge-VERSION-macos-arm64.zip`, containing `Luxforge.app` with the editor, both CLIs under `Contents/Helpers`, Lensfun resources, notices and help.
- `luxforge-VERSION-linux-x86_64.tar.gz`, containing `Luxforge/` with all three executables, resources, notices and help.
- `release.json`, per-platform build records and `SHA256SUMS` for the final files.

The Linux build uses Ubuntu 24.04/glibc 2.39. Native Vulkan drivers, runtime libraries and an XDG chooser portal/backend are still required. macOS 14 is the configured build floor, not a verified installation-support claim. Read the artifact's signing status: macOS builds are **unsigned unless signing is enabled**. Pushing a release tag requests public pre-release downloads; no manual publish step follows a successful run.

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

Apple credentials reach only the macOS signing step, after compilation/checks. The runner imports them into a temporary Keychain, signs CLI helpers then the app with hardened runtime and timestamp, requires an **Accepted** notary submission, staples/validates the app and recreates its ZIP before final checksums. The temporary Keychain and P12 are removed on success or failure. Build/cache/upload steps do not receive those secret values; credential files are outside artifact/cache directories. Only the final publication job has repository write permission, and it receives no Apple credentials. Workflows do not run on pull requests.


## Retry and local rehearsal

Use **Actions → Release builds → Run workflow** with an existing tag, or rerun a failed run. Both paths automatically publish on success. Assets are uploaded to a temporary draft before it is made public; an upload failure or a moved tag leaves that draft unpublished. Existing GitHub releases are never overwritten. Remove an unwanted failed draft deliberately before rerunning delivery; do not move a published tag or replace published assets. Any corrected public release uses a new version.

Local build rehearsal, using the shared compiler cache on macOS/Linux:

```sh
tools/cargo-cached xtask release-build --tag v0.1 --build-number 1 --output artifacts/release-rehearsal --allow-dirty
```

The output must be new. `--allow-dirty` labels the output as a local rehearsal; assembly refuses it for hosted publication. Without this flag, the checkout must be clean and the tag must name HEAD. `release-sign --output CANDIDATE --identity ID --keychain-profile luxforge-notarization` explicitly signs an existing unsigned macOS candidate when the operator runs it; ordinary development packaging never signs or submits anything.

## Qualification status

A successful build is not native GPU/corpus/install evidence, a completed distribution advisory review, or a complete corresponding-source/licence review. Those gates, the application icon/desktop integration and final support requirements remain in the [distribution plan](../design/release-v0.1.md). Published pre-release notes and metadata record them as pending or not run and never claim release readiness. They do not prevent the owner-requested automatic publication of build candidates.
