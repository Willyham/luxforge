# Automatic updates

Status: **in implementation** (authorized 2026-10-12, Linux first and then macOS). The owner asked on 2026-10-12 for an auto-updater from the first release, so nobody is stranded on v0.1. Sparkle is used on macOS, an in-house updater on Linux, and Windows comes later. The [task plan](../../tasks/project/auto-update.json) describes the work. Nothing here is built yet. Updates remain manual until it is.

## Outcome and scope

A released Luxforge finds its newer releases and offers to install them. The download is verified against a key Luxforge holds, the new version replaces the installed one, and the app relaunches. The person's catalogs, settings and originals are never touched by an update. Every step a person can take is a command with a schema. The desktop's offer, Settings rows and palette entry call those commands.

Updates ship together with the v0.1 [distribution contract](release-v0.1.md). A release-ready v0.1 includes the updater, the update feed, the signing key and a drag-to-Applications disk image.

There is no launcher application. On macOS and Linux the app replaces its own installation. A launcher would only be a second program that itself needs updating. It would gain nothing with Gatekeeper, because the downloaded app must be signed and notarized anyway.

Out of scope:

- App stores, Homebrew casks, distro repositories, Flatpak, Snap and AppImage.
- Delta updates. Sparkle can add them later without a client change.
- Staged rollouts.
- Automatic rollback.
- Any reporting from the update check.

## Decisions

These were made by the owner on 2026-10-12.

- **No launcher.** On macOS, Luxforge offers to move itself into Applications when it runs from anywhere else.
- **macOS uses Sparkle 2.** It is the standard updater for Developer ID apps.
- **Linux uses an in-house updater.** It replaces the user-level `Luxforge/` payload, reading the same feed format and checking the same Ed25519 signatures. When the installation is not writable, it falls back to offering a download.
- **Windows comes later.** Its outline is [below](#windows-later). The feed, key, commands and preferences take it without change.
- **Feed host.** Feeds are a route of the one Luxforge backend API, under `https://api.luxforge.app/v1/updates/`, beside the usage statistics route `/v1/usage/batches`. That URL is compiled into every release, so the API keeps the route for as long as any release that uses it is in use. What serves the route behind the API can change.
- **Default behaviour: check, then offer.** A release build checks automatically, with no permission prompt, at launch and then daily. When it finds an update it offers to install it. Two Settings toggles control this:
  - **Check for updates automatically**, on by default.
  - **Install updates automatically**, off by default.

  Settings states what the check sends.
- **Catalog compatibility.** The catalog format was reset to 1 on 2026-10-12. Until 1.0.0, a release may still change the catalog format with no migration. From 1.0.0 onwards, every catalog format change ships with an automatic migration from each released format ([decisions](../decisions.md#catalog-compatibility)).

  Until then the updater makes a format change visible:
  - The feed records each release's catalog format.
  - An offer that changes it says so.
  - Such an update is never installed automatically.
  - The new version starts cleanly past a catalog it cannot open ([launching past an unsupported catalog](#launching-past-an-unsupported-catalog)).

## The feed

One Sparkle-format RSS appcast exists per download target, under `https://api.luxforge.app/v1/updates/`:

| Target | Feed |
| --- | --- |
| Apple silicon macOS | `macos-arm64/appcast.xml` |
| x86-64 Linux | `linux-x86_64/appcast.xml` |
| Windows (later) | `windows-x86_64/appcast.xml` |

Each published release contributes one item to each feed it has a download for. An item carries:

- the title and `pubDate`;
- the GitHub release page as its link;
- `sparkle:version`, the build number;
- `sparkle:shortVersionString`, the version;
- `sparkle:minimumSystemVersion`, on macOS;
- `sparkle:channel`, only on SemVer pre-release tags such as `v0.2.0-rc.1`, whose items go to the `prerelease` channel;
- `luxforge:catalogFormat`, in a Luxforge XML namespace;
- one enclosure: the release asset URL, its byte length, its MIME type and `sparkle:edSignature`.

The macOS enclosure is the notarized ZIP. The Linux enclosure is the tarball.

**Versions.** The build number orders releases on every platform. It is `CFBundleVersion` on macOS and the build number in `build.json` on Linux, and today it is the workflow run number. The feed generator refuses a set of releases whose build numbers do not rise with their versions, so renaming the workflow cannot silently reorder them.

**Channels.** Clients follow only the default channel. The pre-release channel is followed only when the Experiments flag `updates.prerelease-channel` is on, which is how release candidates are qualified.

**Regeneration.** The feeds are regenerated on every release from the update records of every published release, so the release history is the only source of truth.

- Each release's update record is a release asset, `luxforge-<version>-updates.json`. It holds each archive's length, build number, Ed25519 signature, minimum OS, catalog format and channel.
- A release can be withdrawn from the feeds through a checked-in list. Its assets are never altered.
- An item is never listed for an asset that is not yet published.
- A macOS archive that is not Developer ID signed and notarized is never listed.

**Serving.** The release workflow generates the feeds and publishes them to the API's update store. The API answers `GET /v1/updates/<target>/appcast.xml` with them as static content. The route:

- takes no query string, cookies or authentication;
- sets cache headers that let a new release reach clients within an hour;
- keeps no record of client addresses, headers or bodies, as for usage statistics.

The store and the publication mechanism are delegated to the backend's deployment. One example is object storage behind the API's reverse proxy, written with a scoped credential that only the workflow's feed step receives. The route joins the backend's deployment checklist.

The operator provisions:

- the production `api.luxforge.app`, which usage statistics also awaits;
- the update store;
- the publication credential.

No v0.1 build may be published as release-ready until the route answers over HTTPS.

## The update key

One Ed25519 key signs every platform's archives, in the format Sparkle's `sign_update` produces. WinSparkle will use the same key later.

- **Creation.** The operator generates the key with the pinned Sparkle's `generate_keys` on the owner's Mac. Implementation never creates, exports or reads a production key. Tests use generated test keys.
- **Public key.** It is committed in `release/update.json` beside the feed base URL. That file is the single source for the macOS `SUPublicEDKey` and for the constant compiled into the Linux updater.
- **Private key.** It lives in the GitHub secret `LUXFORGE_UPDATE_SIGNING_KEY`, which only the update-record signing step receives. Keep an offline backup.
- **If the key is lost:**
  - macOS can move to a new key, because Sparkle accepts a key change while the Developer ID signature stays the same.
  - Linux installations cannot, and need one manual update.

  The [operator guide](../engineering/releases.md) says so.

A release build without the public key has no working updater. Its status reports `unavailable` with the reason `no-update-key`. A rehearsal may lack the key; the release-ready verifier refuses to pass one.

## Commands and preferences

The core owns the update state, commands and preferences. A host port carries out the platform's mechanism; the desktop provides Sparkle on macOS and the payload updater on Linux. `luxforge-json` and development builds have no port, so they report `unavailable`. Background, hidden, evidence, smoke and verify launches never contact the real feed.

| Method | Does |
| --- | --- |
| `update.status` | Reads the update state:<br>• the mechanism: `sparkle`, `linux-payload`, `linux-notify` or `unavailable`, with a reason such as `development-build`, `headless`, `no-update-key`, `outside-applications` or `read-only-install`<br>• the current version and build<br>• the two preferences<br>• the last check's time and outcome<br>• the available update, if any: version, build, release-notes URL, whether it changes the catalog format, whether it was skipped<br>• download progress and readiness<br>• the last error |
| `update.check` | Checks the feed now. The answer reports the check's outcome. The check runs on a bounded worker, and the owner and UI threads never wait on the network. |
| `update.install` | Installs the available update and relaunches.<br>• Needs `ClientAuthority::Permissions`, so it comes from the desktop or a permission-authority client.<br>• Refuses `not-ready` when no update is available.<br>• Refuses `conflict` while a job (export, merge, import) runs, naming the jobs. |
| `update.skip` | Stops offering a given version. A later version is offered as usual. |
| `update.move-to-applications` | macOS only. Moves the app into Applications and relaunches it from there ([below](#moving-into-applications)). Needs `ClientAuthority::Permissions`. |

Two preferences are set through `preferences.set`, with the existing preference rules:

- `update_checks`, default `true`;
- `update_install_automatically`, default `false`.

Luxforge's preferences are authoritative: the macOS port applies them to Sparkle at every launch and on every change. A skipped version is held in the same configuration. The commands are listed in `schema.list`, so `luxforge-ctl` exposes them with no further work.

**Installing.** Install-and-relaunch goes through the app's ordinary quit path:

1. Drafts follow their usual rules.
2. The catalog owner closes the catalog cleanly.
3. Only then does the platform mechanism replace the app and start the new version.

Automatic installation, when the person turns it on, downloads in the background and installs at the next ordinary quit, without relaunching. An update that changes the catalog format is always offered and never installed automatically.

## The desktop

All update interface is Luxforge's own, themed like the rest of the workspace. On macOS that means a custom Sparkle user driver, not Sparkle's standard windows.

- **The offer.** A found update appears as a non-modal notice in the workspace: "Luxforge 0.2.0 is available", with Release notes, Install and relaunch, Later and Skip this version.
  - It shows download progress.
  - It never takes focus or covers the photograph's controls.
  - Later hides it until the next check.
  - Release notes open the GitHub release through the existing checked HTTPS opener.
- **A catalog format change.** The offer says the new version needs a new catalog. Your current catalog stays as it is, and the version you have now keeps opening it.
- **Settings › General, an Updates group:**
  - the current version;
  - Check for updates automatically;
  - Install updates automatically, disabled while checks are off;
  - Check now, with the last check's time and outcome;
  - one line saying that checks contact `api.luxforge.app` and send only the app version.

  A build without an updater shows the reason instead of the controls.
- **The palette** gains Check for Updates.

Layout and wording are delegated, within the [develop workspace](develop-workspace.md)'s visual language.

## macOS: Sparkle

**Pinning and embedding**

- Pin the latest stable Sparkle 2 release at implementation time, 2.9 or later. Use the official release archive, fetched by `xtask` and verified by SHA-256. Its MIT licence joins the notices.
- The release bundle embeds `Contents/Frameworks/Sparkle.framework` with its `Autoupdate` and `Updater.app`. Luxforge is not sandboxed, so Sparkle's XPC services are removed.
- The editor loads the framework at run time from its own bundle. A development build or test bundle without it reports `unavailable` rather than failing to start.
- The bridge uses the existing `objc2` crates.

**Info.plist** gains:

- `SUFeedURL`;
- `SUPublicEDKey`;
- `SUEnableAutomaticChecks` set to true, which suppresses Sparkle's permission prompt;
- `SUScheduledCheckInterval` of 86,400 seconds;
- `SUVerifyUpdateBeforeExtraction`;
- `SURequireSignedFeed`, if the pinned Sparkle can sign the generated feed in the release workflow. Otherwise signed archives alone are the guarantee, and the design records that.

**The updater**

- An `SPUUpdater` runs with a custom user driver implemented in Rust. It forwards every Sparkle event into the port's state and answers Sparkle's reply blocks from the commands.
- The delegate filters channels.
- The delegate also stops an update that changes the catalog format from being installed silently.
- Relaunch is postponed until the quit path has closed the catalog.

**Signing.** The release signing sequence signs Sparkle's nested code before the helpers and the app: `Autoupdate`, then `Updater.app`, then the framework. It still never uses recursive signing.

**Downloads.** The macOS download becomes a disk image, `luxforge-<version>-macos-arm64.dmg`:

- it holds `Luxforge.app` and an Applications link;
- it is Developer ID signed, notarized and stapled.

The notarized ZIP stays as the update archive, and both are listed in `SHA256SUMS` and `release.json`.

### Moving into Applications

Updates cannot replace an app that runs from a disk image, from a read-only volume or from a translocated location. App Translocation is how macOS runs a quarantined app that was not moved into place.

**When Luxforge asks.** A release build launched by a person checks where it runs. It asks when it is:

- translocated, detected as LetsMove does with `SecTranslocateIsTranslocatedURL`;
- on a disk image or read-only volume;
- outside `/Applications` and `~/Applications`.

The question is "Move Luxforge to Applications? Luxforge can only update itself from the Applications folder", with **Move and relaunch** and **Not now**, plus Don't ask again, which is a preference.

**What Move does:**

1. It copies the bundle into `/Applications`, or into `~/Applications` when `/Applications` is not writable, preserving its signature.
2. If an existing Luxforge is in the way, it moves that one to the Trash after the person confirms.
3. It clears the copy's quarantine as LetsMove does.
4. It relaunches from the new place.
5. It moves the original to the Trash, or ejects its disk image.

Until the app moves, `update.status` reports `unavailable` with the reason `outside-applications`. Background and evidence launches never ask.

## Linux: the payload updater

The Linux download is a relocatable `Luxforge/` directory. The updater serves the payload the running executable lives in, identified by its `build.json`.

**When it can update in place:**

- the payload and its parent are writable by the person;
- staging can be on the same filesystem;
- the payload carries no package-manager marker, which future distribution packages use to stand the updater down.

Otherwise it reports `linux-notify`, and the offer opens the release page.

**An update, step by step:**

1. Read the feed over the existing `luxforge-net` HTTPS transport and choose the newest item on a followed channel with a higher build number.
2. Stream the archive into a staging directory beside the payload, refusing anything longer than the item's stated length.
3. Verify its Ed25519 signature against the compiled public key without holding the archive in heap memory, for example through a memory map.
4. Extract into `Luxforge.new/`:
   - refuse absolute paths, `..`, links that leave the payload, device files and archives past a size cap;
   - preserve executable modes;
   - check that the extracted `build.json` names the item's version, build and target.
5. Install at the end of the quit path, after the catalog is closed. Swap the directories atomically with `renameat2(RENAME_EXCHANGE)` where the filesystem supports it, and with two renames otherwise. Keep the old payload as `Luxforge.previous`. Then exec the new executable with the original launch arguments (no relaunch when installing automatically at quit).
6. Remove `Luxforge.previous` after the new version's first successful launch. Until then it is a manual way back.

If the user-level desktop entry from the install helper points at this payload, the new payload's integration step refreshes the entry and icon. An interrupted download, a failed verification or a failed extraction removes its staging directory and leaves the installed payload untouched.

## Windows later

Windows distribution has not been planned. When it is, updates follow this design:

- the `windows-x86_64` feed;
- the same Ed25519 key;
- the same commands, preferences and desktop interface through the port;
- Authenticode-signed downloads, so SmartScreen accepts them.

WinSparkle reads this feed format and verifies EdDSA signatures, but it brings its own windows. The in-house updater could instead hand off to an installer. That choice is made with the Windows installer format, and nothing here precludes either.

## Launching past an unsupported catalog

Until 1.0.0, an update can bring a new catalog format.

**Today**, the desktop fails to launch when its remembered catalog is refused. After an update started from Finder, that looks like a broken app.

**Instead**, the desktop opens and says the catalog was made by a different Luxforge version, naming both formats. It offers **Start a new catalog** and **Quit**.

- Start a new catalog creates and remembers a new catalog beside the old one; the naming is delegated.
- The refused catalog is never modified, renamed or deleted.
- The message says the earlier Luxforge can still open it, and links to the releases page.

An explicit `--catalog` path keeps today's refusal on the command line.

## Engineering constraints

- Network, download, verification and extraction never run on the UI or owner thread.
- Downloads stream to disk with a bounded buffer, and quitting cancels them.
- Memory stays bounded whatever the archive size.
- Every refusal names its reason in `update.status`.
- The [performance rules](../engineering/performance-rules.md) checklist applies to changes under `crates/`.
- Pin new dependencies. Prefer the `ring` Ed25519 implementation already in the lockfile through rustls.

## Acceptance and evidence

Ordinary tests are built with each part:

- **Feed generation and signing**, against test keys:
  - a cross-check with the pinned Sparkle's own signature verification;
  - refusing unsigned macOS archives and non-rising build numbers.
- **Linux engine.** Refusing a bad signature, a truncated or oversized download, path traversal and a mismatched `build.json`. Swap and recovery in temporary directories. The notify-only fallback.
- **Command and preference schema and authority**, and UI/API parity.
- **A local end-to-end macOS update** from one bundle to a newer one, through Sparkle with a temporary test signing identity, in background-only bundles that never take focus.
- **A rendered check** of the offer, the Settings group, the move question and the unsupported-catalog launch, with correlated state and logs.

Qualification comes last, on real signed release candidates through the pre-release channel:

- an M4 update from one candidate to the next;
- a clean-account or second-Mac update from the DMG, including the move into Applications, in a foreground session the owner authorizes;
- a native Ubuntu 24.04 payload update;
- the feed route answering at `api.luxforge.app`;
- a catalog-format-change offer and launch.
