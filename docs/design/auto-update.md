# Automatic updates

Status: **in implementation** (authorized 2026-10-12, Linux first and then macOS). The owner asked on 2026-10-12 for an auto-updater from the first release, so nobody is stranded on v0.1. Sparkle is used on macOS, an in-house updater on Linux, and Windows comes later. The [task plan](../../tasks/project/auto-update.json) describes the work. The update feed format, record signing and feed generation, and the core's update state, commands, preferences and flag are built; no platform mechanism or desktop interface is, so every build reports `unavailable` and updates remain manual.

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

The `luxforge-update` crate is the format's one home: the record, the appcast written and read, signing and verification, the choice of an update and `release/update.json`. It depends on no other workspace crate, which `cargo xtask check-repository` enforces; xtask builds it and the Linux updater will link it.

**The update record** is pretty-printed JSON, schema 1:

```json
{
  "schema_version": 1,
  "tag": "v0.2.0-rc.1",
  "version": "0.2.0-rc.1",
  "channel": "prerelease",
  "public_key": "<base64 public key of the signing key>",
  "archives": {
    "macos-arm64": {"file": "luxforge-0.2.0-rc.1-macos-arm64.zip", "length": 5242893, "build": 9, "ed_signature": "<base64>", "minimum_system_version": "14.0", "catalog_format": 1, "signing": "notarized"},
    "linux-x86_64": {"file": "luxforge-0.2.0-rc.1-linux-x86_64.tar.gz", "length": 5242893, "build": 9, "ed_signature": "<base64>", "catalog_format": 1, "signing": "not-applicable"}
  }
}
```

- `channel` is `prerelease` for a SemVer pre-release and `default` otherwise.
- `signing` is the release build's `target.json` status: `notarized`, `unsigned`, `pending`, `failed` or `not-applicable`.
- `windows-x86_64` is accepted as a target.
- Unknown fields, another schema, a tag that does not name the version, a channel that does not match it and a file not named `luxforge-<version>-<target>.<extension>` are refused (`malformed-record`).

`cargo xtask update-record --tag TAG --input BUILDS --output NEW_FILE --key-file FILE` writes the record. `BUILDS` is the directory `release-assemble` reads: each platform's `target.json` beside its final archive, notarized on macOS. The command:

- checks each `target.json` as `release-assemble` does, and refuses a dirty or rehearsal build;
- reads the build number, `catalog_format` and, on macOS, `minimum_system_version` from `target.json`, and requires one build number across targets. The release build records `luxforge_core::CATALOG_FORMAT` and the bundle's `LSMinimumSystemVersion` there;
- signs each archive through a read-only memory map and verifies the signature it made;
- records a macOS archive that is not notarized with its status, so feed generation leaves it out;
- writes only a new file named `luxforge-<version>-updates.json`.

`cargo xtask update-feed --records DIR --published FILE --output NEW_DIR [--withdrawn FILE] [--repository OWNER/NAME]` writes `<target>/appcast.xml` for `macos-arm64`, `linux-x86_64` and any other target a record names.

- `--records` holds only `luxforge-<version>-updates.json` files, each with its own version's record.
- `--published` is GitHub's release list as `gh api --paginate --slurp repos/Willyham/luxforge/releases` writes it; one page is also read. The command reads each release's `tag_name`, `draft` and `published_at` and each asset's `name`, `size` and `state`. Drafts, releases without a publication time and assets that are not `uploaded` are not published. An item's `pubDate` is its release's `published_at`.
- `--withdrawn` defaults to the checked-in `release/withdrawn-updates.json`: a JSON array of `{"tag": "v0.2.0", "reason": "…"}`, each tag once.
- `--repository` defaults to `Willyham/luxforge`. Enclosure URLs are `https://github.com/<repository>/releases/download/<tag>/<file>` and the item link is `…/releases/tag/<tag>`, with every character outside URL-unreserved ones, such as `+`, percent-encoded.

It refuses, by name:

- a malformed record or input;
- two records for one tag or one SemVer version (`duplicate-release`);
- build numbers that do not strictly rise with SemVer precedence within a target (`build-not-rising`). Every record counts, withdrawn or left out, because a person may have installed it;
- a listed archive whose release or asset is not published (`unpublished-asset`), or is published at another length (`asset-length-mismatch`);
- once `release/update.json` commits a public key, a record signed by another key (`record-key-mismatch`). Releases signed by a retired key are withdrawn.

It leaves out, with a note, withdrawn releases and macOS archives that are not notarized. Items are newest first, and the same inputs write the same bytes.

**The appcast as written.** The channel's title is `Luxforge`, its link the releases page and its description names the target. Each item's title is `Luxforge <version>`. The enclosure `type` is `application/octet-stream`, as Sparkle's own `generate_appcast` writes. `luxforge:catalogFormat` is in the namespace `https://luxforge.app/xmlns/update`.

**Reading a feed** (`Appcast::parse`, for the Linux updater) refuses a feed over 4 MiB, one with a DTD and a document without `<rss><channel>`. It ignores elements and attributes it does not use, and reads Sparkle's older form with the versions on the enclosure. It skips an item, recording why, when it has:

- no numeric `sparkle:version`;
- no enclosure with a URL, a length and a 64-byte `sparkle:edSignature`;
- a malformed `luxforge:catalogFormat`;
- `sparkle:informationalUpdate`.

`newest_eligible` chooses the highest build above the current one on a followed channel. A channel it does not know is never followed.

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

**Formats.**

- `release/update.json` is `{"feed_base_url": "https://api.luxforge.app/v1/updates/", "public_key": null}`. The operator replaces `null` with the key's base64 `SUPublicEDKey`. `luxforge_update::UpdateConfig` reads it for xtask and build scripts, and refuses unknown fields, a malformed key and a base URL that is not `https`, does not end in `/` or has a query.
- The private key file holds what Sparkle 2's `generate_keys -x` exports: base64 of the 32-byte seed, surrounding whitespace ignored. Sparkle's older 96-byte form, an expanded key and its public key, is refused (`legacy-key`) because its seed cannot be recovered for `ring`; Sparkle has no 64-byte form. Anything else is `malformed-key`. The file is read with a bound and zeroed after use, and no message shows it.
- A signature is byte for byte what `sign_update` prints for the same key and file, because Ed25519 is deterministic. Tests hold this with fixtures from a throwaway Sparkle 2.10.0 test key.
- `update-record` refuses a key whose public key is not the committed one (`key-mismatch`), and notes when none is committed yet. Each record names the public key that signed it.

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

**How the core does it.** The model lives in `luxforge_core::update`.

- **The port.** `HostConfig::updates` holds an optional `UpdatePort`, which the desktop implements per platform.
  - The owner calls `attach` once as it starts, then `apply` with an `UpdatePolicy`. It applies the policy again whenever `update_checks`, `update_install_automatically`, the skipped version or the pre-release flag changes.
  - `status`, `check`, `request_install`, `skip` and `move_to_applications` each return at once. The port does its work on its own workers.
  - The port reports changes through the owner's `UpdateNotifier`, which never blocks, whatever thread signals it.
- **No updater.** A host with no port, such as `luxforge-json`, reports `unavailable` with `headless`. A desktop that must not update itself passes an `UnavailablePort` naming its reason: `development-build` for a development build, `no-update-key` for a release without the key. An unavailable mechanism is never asked to check.
- **Checks are answered with their outcome.** `update.check` starts one check on the port, which ends it with a `CheckDone`. The call is held, as `events.wait` is, until the check ends or 60 seconds pass.
  - A check already running is joined, never repeated.
  - At most 8 calls are held. A call past that, or from a caller that may not wait, is answered at once with `checking: true`.
  - Dropping a `CheckDone` ends the check too.
- **Events.** A change the port reports, other than download progress alone, is announced as an `update.status` event with the request id `update:<n>`. A client watching the event log reads `update.status` again. `update.skip`, an accepted `update.install` and a changed update preference are announced under their own requests.
- **The skipped version** is `update_skipped_version` in `preferences.json`. Only `update.skip` writes it, and `preferences.read` shows it. `preferences.set` cannot change or reset it, as for the usage statistics record. `update.skip` takes any client, as `preferences.set` does.
- **Rules the core decides.** Ports cannot decide these differently:
  - `catalog_format_change` compares the feed item's `luxforge:catalogFormat` with the build's catalog format.
  - `UpdatePolicy::installs_automatically` is true only while checks and automatic installation are both on, for a followed, unskipped version with no catalog format change.
  - A pre-release item is not offered while `updates.prerelease-channel` is off.
- **Install and move.** `update.install` and `update.move-to-applications` carry a `{request_id, actor}` envelope, and the owner answers a retry with its first answer.
  - Each `not-ready` refusal names a `data.reason`: the mechanism's reason, `notify-only` (a `linux-notify` installation), `no-update` or `not-supported` (a port that cannot move).
  - A `conflict` names the running jobs in `data.jobs`. The jobs that hold an update back are export, batch export, HDR merge, develop picks, batch settings, module install and removal, and tasks. Previews, analysis, source preparation and listings do not.
  - An update that changes the catalog format installs when explicitly asked.
- **Tests** drive the commands through `luxforge_core::update::testing::FakeUpdatePort`, which the core's `test-holds` feature exposes to other crates' tests.

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
