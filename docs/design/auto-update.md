# Automatic updates

Status: **implemented; publication and qualification open** (authorized 2026-10-12, Linux first and then macOS). The owner asked on 2026-10-12 for an auto-updater from the first release, so nobody is stranded on v0.1. Sparkle is used on macOS, an in-house updater on Linux, and Windows comes later. The [task plan](../../tasks/project/auto-update.json) describes the work. Built: the update feed format, record signing and feed generation; the core's update state, commands, preferences and flag; [launching past an unsupported catalog](#launching-past-an-unsupported-catalog); the macOS disk image; the [Linux payload updater](#linux-the-payload-updater) with its desktop port; the [desktop's offer, Settings group and palette entry](#the-desktop); the [move into Applications](#moving-into-applications) with its launch question; [feed publication and the feed route](#publishing-and-serving-the-feeds); and the [macOS Sparkle port](#macos-sparkle), with Sparkle embedded in the release bundle and a local end-to-end update journey. A Linux or macOS release build updates itself once the update key is committed; until then it reports `no-update-key`. The production key, the API host and its update store are not in place, so no build updates itself today. Qualification (TASK-010) remains.

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
- leaves out a macOS archive that is not notarized, with a note: it checks that build's `target.json` and archive as it does the others, but neither signs nor records it, so the update key signs nothing a feed may not list;
- writes only a new file named `luxforge-<version>-updates.json`.

`cargo xtask update-feed --records DIR --published FILE --output NEW_DIR [--withdrawn FILE] [--repository OWNER/NAME] [--deploy NEW_FILE.tar]` writes `<target>/appcast.xml` for `macos-arm64`, `linux-x86_64` and any other target a record names.

- `--records` holds only `luxforge-<version>-updates.json` files, each with its own version's record.
- `--published` is GitHub's release list as `gh api --paginate --slurp repos/Willyham/luxforge/releases` writes it; one page is also read. The command reads each release's `tag_name`, `draft` and `published_at` and each asset's `name`, `size` and `state`. Drafts, releases without a publication time and assets that are not `uploaded` are not published. An item's `pubDate` is its release's `published_at`.
- `--withdrawn` defaults to the checked-in `release/withdrawn-updates.json`: a JSON array of `{"tag": "v0.2.0", "reason": "…"}`, each tag once.
- `--repository` defaults to `Willyham/luxforge`. Enclosure URLs are `https://github.com/<repository>/releases/download/<tag>/<file>` and the item link is `…/releases/tag/<tag>`, with every character outside URL-unreserved ones, such as `+`, percent-encoded.
- `--deploy` also writes the deploy archive the update store installs: an uncompressed ustar archive holding exactly `<target>/appcast.xml` for each feed, as regular files in target order with mode 0644, owner 0 and time 0, so the same feeds write the same bytes. A feed over 4 MiB is refused, as no client reads one.

It refuses, by name:

- a malformed record or input;
- two records for one tag or one SemVer version (`duplicate-release`);
- build numbers that do not strictly rise with SemVer precedence within a target (`build-not-rising`). Every record counts, withdrawn or left out, because a person may have installed it;
- a listed archive whose release or asset is not published (`unpublished-asset`), or is published at another length (`asset-length-mismatch`);
- once `release/update.json` commits a public key, a record signed by another key (`record-key-mismatch`), unless its release is withdrawn. Releases signed by a retired key are withdrawn, and then left out.

It leaves out, with a note, withdrawn releases and macOS archives that are not notarized. Items are newest first, and the same inputs write the same bytes.

**The appcast as written.** The channel's title is `Luxforge`, its link the releases page and its description names the target. Each item's title is `Luxforge <version>`. The enclosure `type` is `application/octet-stream`, as Sparkle's own `generate_appcast` writes. `luxforge:catalogFormat` is in the namespace `https://luxforge.app/xmlns/update`.

**Reading a feed** (`Appcast::parse`, for the Linux updater) refuses a feed over 4 MiB, one with a DTD and a document without `<rss><channel>`. It ignores elements and attributes it does not use, and reads Sparkle's older form with the versions on the enclosure. It skips an item, recording why, when it has:

- no numeric `sparkle:version`;
- no enclosure with a URL, a length and a 64-byte `sparkle:edSignature`;
- a malformed `luxforge:catalogFormat`;
- `sparkle:informationalUpdate`.

`newest_eligible` chooses the highest build above the current one on a followed channel. A channel it does not know is never followed.

### Publishing and serving the feeds

Publication is off until the operator sets the repository variable `LUXFORGE_PUBLISH_UPDATES` to `true` ([operator guide](../engineering/releases.md#automatic-updates)). Unset or `false` publishes no record and no feed; any other value fails the release's identity job.

**Signing the record.** The [release workflow](tag-release-builds.md)'s publish job signs the record after `release-assemble` has verified both candidates and before the draft release is created:

1. A step without the key builds the xtask binary.
2. The step **Sign the update record** is the only one that receives `LUXFORGE_UPDATE_SIGNING_KEY`. It refuses a missing secret and a `release/update.json` with no public key. It writes the key to a private temporary file, removes it from the environment and runs `update-record` with the built binary, so no compiler or build script runs beside the key. The file is deleted when the step ends, whatever its outcome.
3. `luxforge-<version>-updates.json` is uploaded with the other assets. `SHA256SUMS` and `release.json` do not list it.

A failure in either step fails the job before anything is published.

**Publishing the feeds.** The **Update feeds** workflow (`.github/workflows/update-feeds.yml`) runs after every successful release run, and by hand from `main` after a withdrawal. It holds only `contents: read`, so it cannot alter a release, and its failure leaves the release as it was published. Its runs are serialised, and each regenerates every feed from all releases, so the last to finish is current. It:

1. lists the releases with `gh api --paginate --slurp` and downloads each published release's uploaded `luxforge-*-updates.json` (a release without one is not listed);
2. builds the xtask binary and runs `update-feed --deploy` over them, with the repository it runs in;
3. keeps the deploy archive as the `update-feeds` Actions artifact for 30 days;
4. in the step **Publish the feeds to the update store**, the only one that receives `LUXFORGE_UPDATE_PUBLISH_SSH_KEY`, sends the archive over SSH to `LUXFORGE_UPDATE_PUBLISH_DESTINATION` (`ssh://USER@HOST[:PORT]`), checking the host key against `LUXFORGE_UPDATE_PUBLISH_KNOWN_HOSTS`.

`cargo xtask check-repository` holds every workflow to these rules: each action pinned to a full commit, a top-level `permissions` block, write permission for the release's publish job `contents` only, no `secrets: inherit`, `toJSON(secrets)`, `pull_request_target` or `write-all`, and each of the two keys named exactly once, in its own script step.

**The store and the publication mechanism** were delegated to the backend's deployment. The choice, recorded here, is the simplest that fits the [usage statistics reference](../../services/usage-statistics/README.md): one host and one API process, behind a proxy that forwards every path unchanged.

- **The store** is a directory on the API host. `STORE/current` is a link to the latest publication, `STORE/feeds-<UTC time>/`, which holds `<target>/appcast.xml`.
- **Publication** uses an SSH key that is `restrict`ed in `authorized_keys` and forced to run `install-feeds.py STORE` ([script](../../services/usage-statistics/install-feeds.py)), for an account that owns only the store. The key can do nothing but hand over a deploy archive.
  - The script refuses, changing nothing, an archive over 16 MiB, a member other than a released target's `appcast.xml` regular file, a feed over 4 MiB, a missing `macos-arm64` or `linux-x86_64` feed, and a feed that is not UTF-8 RSS with a channel. A DTD or entity declaration is refused before parsing.
  - Otherwise it writes and syncs a new publication, then replaces the `current` link at once, so the route answers the old feeds or the new ones and never a mixture. It keeps the previous publication for a manual rollback and removes older ones. Publications hold a lock on `STORE/.lock`.
- **The route** is served by the API's existing process, the [usage statistics ingress](../../services/usage-ingest/README.md), from `LUXFORGE_INGEST_UPDATES_DIR` (`STORE/current`, mounted read-only). Without that setting the route does not exist. For `GET` and `HEAD /v1/updates/<target>/appcast.xml`, the route:
  - answers `200` with `Content-Type: application/rss+xml; charset=utf-8` and `Cache-Control: public, max-age=900`, so a new release reaches clients within an hour, through any cache;
  - answers `404` for a target with no feed, a feed over 4 MiB or any query string, and `405` with `Allow: GET, HEAD` for another method;
  - reads no request header, cookie or address and sets no cookie;
  - logs nothing but two aggregate counters, `feeds_served` and `feeds_missing`;
  - reads a feed again only when its file changes, so at most one copy of each target's feed is held in memory.

  The statistics route's global and per-installation limits do not apply to it; the connection cap and the request timeout do.
- **The local reference stack** mounts `LUXFORGE_UPDATE_STORE` (default `./update-store`) into its ingress, so the route can be exercised before production exists.

The operator provisions:

- the production `api.luxforge.app`, which usage statistics also awaits;
- the update store and the publication account ([deployment checklist](../../services/usage-statistics/DEPLOYMENT.md#16-the-update-feed-route));
- the secrets and variables.

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
| `update.move-to-applications` | macOS only. Moves the app into Applications and relaunches it from there ([below](#moving-into-applications)). Needs `ClientAuthority::Permissions`. Without `replace: true` it refuses `conflict` with `data.reason` `existing-app` and the app in the way as `data.path`; with it, that app goes to the Trash. |

Two preferences are set through `preferences.set`, with the existing preference rules:

- `update_checks`, default `true`;
- `update_install_automatically`, default `false`;
- `update_ask_to_move`, default `true`: whether a macOS release build launched outside Applications asks to move ([below](#moving-into-applications)).

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

The desktop decides nothing about updates: it shows what `update.status` reports and every control sends one command. It reads the status at launch, whenever the event sync sees an `update.*`, preference or flag event, when the Settings sheet opens and after its own update preference is saved. Every read and call runs off the update loop.

- **The offer.** An available, unskipped update from a mechanism that is not `unavailable` appears as a non-modal card at the bottom of the workspace, above the usage statistics notice where both show and in Select as well as Develop. It takes no focus and holds no key, and it sits between the side panels above the tool strip, so it covers none of the photograph's controls. It reads "Luxforge 0.2.0 is available", with Release notes, Install and relaunch, Later and Skip this version.
  - While the update downloads it says "Downloading… 45%" over a progress bar (the received size while the length is unknown); once verified it says "Downloaded and ready to install."; an update that installs automatically adds "It installs when you next quit Luxforge."
  - Release notes opens the release page through the checked HTTPS opener.
  - Install and relaunch sends `update.install`. An accepted install says Luxforge quits and relaunches when it is ready, and leaves only Release notes. A refusal is shown in the card: `conflict` names the running work from `data.jobs` ("Installing waits until this work finishes: an export."), `not-ready` gives its reason in words.
  - Later sends nothing. It hides the offer until the next check ends, whatever its outcome.
  - Skip this version sends `update.skip` with the offered version; the offer goes at once.
  - Install and Skip wait while either is being answered.
  - A Check now or Check for Updates adds "Checking for updates…" to an offer already shown; a scheduled check shows nothing.
- **A catalog format change.** The offer's first line says: "This version needs a new catalog. Your current catalog stays as it is, and the version you have now keeps opening it."
- **A notify-only Linux installation** (`linux-notify`) says it cannot replace itself and offers Download, which opens the release page, in place of Release notes and Install and relaunch.
- **Settings › General, an Updates group,** first on the tab:
  - the current version, with the build when there is one ("Luxforge 0.2.0 (build 42)");
  - Check now, which sends `update.check`, with the last check's time and outcome ("Last checked 3 minutes ago: found Luxforge 0.2.0", or "Not checked yet") and a failed check's reason;
  - Check for updates automatically (`update_checks`), with the line "Checks contact api.luxforge.app and send only the app version.";
  - Install updates automatically (`update_install_automatically`), disabled while checks are off.

  The switches write through `preferences.set` like the other General rows. When the mechanism is `unavailable` the group shows the version and the reason in plain words instead of the controls, with a sentence for each `UnavailableReason` and a general one for a reason this build does not know.
- **The palette** gains Check for Updates, which sends `update.check` and reports the outcome in the status bar.

An evidence launch scripts what the port reports with `--update-fixture FILE` ([development](../engineering/development.md#evidence-scripts)), and the `update` evidence step waits for the offer, presses Check now or presses one of the offer's buttons.

## macOS: Sparkle

**Pinning and embedding**

- Sparkle 2.10.0 is pinned: the official archive `Sparkle-2.10.0.tar.xz` (16,319,840 bytes, SHA-256 `c2bf58aa8387266ac179357b1415d6f2635f044da8be41042af32425dae6da0c`, tag commit `eef1a539`). `release-build` downloads it once into `target/sparkle/2.10.0/` and checks its length and hash before every extraction (`xtask/src/sparkle.rs`); no Sparkle binary is committed. Its MIT licence, with the external licences it lists, is in the notices (`native/sparkle-2.10.0/`, macOS only) and its provenance in `crates/luxforge-sparkle/THIRD_PARTY.md`.
- The release bundle embeds `Contents/Frameworks/Sparkle.framework`, copied with `ditto` so its symbolic links stay, with `Autoupdate` and `Updater.app`. Luxforge is not sandboxed, so the XPC services are removed, and the changed framework is sealed ad hoc until `release-sign` signs it. The development `package` bundle has no framework.
- The editor loads the framework at run time from its own bundle with `NSBundle`; nothing links it. The bridge is the leaf crate `luxforge-sparkle` (objc2, block2 and dispatch2). A bundle without the framework, as any development build, reports `development-build`; one whose framework does not load says why on standard error and reports the same.

**Info.plist** (`release_build::info_plist`) gains:

- `SUFeedURL`, the macOS feed from `release/update.json` (`…/v1/updates/macos-arm64/appcast.xml`);
- `SUPublicEDKey`, only when `release/update.json` commits a key; without one the app reports `no-update-key`;
- `SUEnableAutomaticChecks` true, which suppresses Sparkle's permission prompt;
- `SUScheduledCheckInterval` 86,400 seconds;
- `SUVerifyUpdateBeforeExtraction` true.

**`SURequireSignedFeed` is not set.** Sparkle 2.10 could check a signed feed, and signing one is a small addition to `update-feed`, but the feed is regenerated in the **Update feeds** workflow, and the update key reaches only the release workflow's one signing step. Signing the feed would hand the key to a second workflow that runs on every release and by hand. What a signed feed adds is small: archives are EdDSA-verified before extraction and must keep the app's Developer ID signature, the feed comes over HTTPS from the API, and Sparkle never installs an older build. A tampered feed could still mislabel a release's catalog format or point its release notes elsewhere; a mislabelled catalog format change leads to the [unsupported-catalog launch](#launching-past-an-unsupported-catalog), not to lost data. Signed archives over HTTPS are the guarantee.

**Which launch gets it.** A release build (compiled with `LUXFORGE_RELEASE_VERSION`) launched by a person, from a bundle that embeds the framework and names `SUPublicEDKey`, is classified with `luxforge-install`'s `classify`, through an injectable location check. In `/Applications` or `~/Applications` it gets the Sparkle port. Anywhere else, translocated or on a disk image or read-only volume, it gets the move port: `unavailable` with `outside-applications` and `can_move_to_applications` ([below](#moving-into-applications)). Hidden, evidence and test-receiver launches stay `development-build`, except the update journey's bundles ([evidence](#acceptance-and-evidence)).

**The bridge** (`luxforge-sparkle`). Sparkle runs on AppKit's main thread, which is iced's update loop. Every command the port sends is queued onto the main dispatch queue and returns at once; Sparkle reads the feed, downloads, verifies and extracts on its own threads. An `SPUUpdater` runs with a user driver and a delegate defined in Rust (`define_class!`), so no Sparkle window is shown. Each Sparkle callback becomes a plain `Event` for the port, which only records it and wakes whoever waits:

- **User driver.** The found update (version, build, release notes link, channel, length and `luxforge:catalogFormat` from the item's properties), download progress, extraction, readiness, installation, errors and "no update". The found and ready reply blocks are held and answered from the port's commands, with Install, Dismiss or Skip. The permission request is answered with automatic checks on.
- **Delegate.** `allowedChannelsForUpdater:` follows the pre-release flag. `updater:shouldPostponeRelaunchForUpdate:untilInvokingBlock:` postpones a relaunch until the catalog has closed. The cycle's end, aborts and "no update" end a check. Sparkle's own release notes and permission prompt are refused.

**The port** (`app/sparkle_port.rs`). Every `UpdatePort` call returns at once.

- **Preferences.** Every `apply` sets Sparkle's `automaticallyChecksForUpdates` to `update_checks`, at launch and on every change. Sparkle's `automaticallyDownloadsUpdates` is always off: Sparkle's automatic driver installs without the user driver, so it could not tell a catalog format change from any other update. The port downloads per item instead, when the person asks or `installs_automatically` allows it, so an update that changes the catalog format is never prepared silently. Luxforge's skip is its own: an offer is dismissed, never skipped in Sparkle, and Sparkle's skip keys are cleared at start.
- **Checks.** Sparkle schedules the daily check; the port asks for one more 10 seconds after the first policy with checks on. `update.check` asks Sparkle to check now or joins the running check; an offer Sparkle holds is dismissed first so the feed is read again.
- **Installing.** `update.install` downloads what is missing, then posts the ordinary close through the same wake signal as the Linux port. The close's joins run on the runtime's blocking pool, so the main thread stays free for Sparkle. Once the catalog owner has joined, `at_quit` answers Install on the ready update and waits up to 20 seconds for Sparkle's installer to take it, then the app exits; the installer replaces the bundle and relaunches it. An update installed automatically is left to the installer, which replaces the app after it terminates without relaunching. An update no longer wanted is cancelled at quit.
- **Updater.app** shows its own progress window only when replacing the app takes more than 0.7 seconds after the app has exited. It is Sparkle's, not Luxforge's, and is accepted.

**Signing.** `release-sign` signs `Sparkle.framework/Versions/B/Autoupdate`, then `Versions/B/Updater.app`, then the framework, then the helpers and the app, each with the hardened runtime and a secure timestamp and never with `--deep`.

**Downloads.** The macOS download becomes a disk image, `luxforge-<version>-macos-arm64.dmg`:

- it holds `Luxforge.app` and an Applications link;
- it is Developer ID signed, notarized and stapled.

The notarized ZIP stays as the update archive, and both are listed in `SHA256SUMS` and `release.json`. The update record signs only the ZIP.

Implemented choices:

- **Format.** A read-only, zlib-compressed `UDZO` image with an HFS+ volume named `Luxforge`, made by `hdiutil create -srcfolder` from a staging folder holding a `ditto` copy of the app and a symbolic link `Applications` → `/Applications`.
- **Layout.** Finder's default window. No `.DS_Store`, background or icon positions are written; Finder scripting is never used.
- **Order.** `release-sign` creates the image after the app is stapled and its final ZIP recreated. It signs the image with the same identity and `--timestamp`, without the hardened runtime option, then verifies it, submits it to `notarytool` and requires Accepted. It staples and validates the image and assesses it with `spctl --assess --type open --context context:primary-signature`. `target.json` records the image's name, size and hash as `disk_image`, and its submission under `signing.disk_image`.
- **Unsigned candidates.** `release-build` makes the same image from the unsigned app, labelled `signing.disk_image.status` `unsigned`. Assembly refuses a macOS record whose image is missing, altered or labelled differently from the app.

### Moving into Applications

Updates cannot replace an app that runs from a disk image, from a read-only volume or from a translocated location. App Translocation is how macOS runs a quarantined app that was not moved into place.

**When Luxforge asks.** A macOS release build (one compiled with `LUXFORGE_RELEASE_VERSION`) launched by a person, with its window shown, checks where it runs once the workspace is usable, while `update_ask_to_move` is on. Development builds and hidden, background, evidence, smoke and verify launches never ask. The check runs off the update loop and reads where the running bundle is:

- **translocated**, detected as LetsMove does with `SecTranslocateIsTranslocatedURL` and `SecTranslocateCreateOriginalPathForURL`, looked up at run time in the Security framework; where they are missing the app counts as not translocated, and the original's own volume decides the rest;
- **on a disk image**, a read-only volume (`statfs` `MNT_RDONLY`) whose Disk Arbitration device model is `Disk Image`;
- **on another read-only volume**;
- **in `/Applications` or `~/Applications`**, where it does not ask;
- **anywhere else**.

The question is a non-modal card in the stack of workspace notices, under the update offer, once no sheet, dialog or menu is open and the usage statistics notice is answered, so the two never show together. It reads "Move Luxforge to Applications?" and "Luxforge can only update itself from the Applications folder.", with **Move and relaunch**, **Not now** and a **Don't ask again** switch. Not now with the switch on stores `update_ask_to_move: false` through `preferences.set`, like any other preference write; the question is not asked again in that launch either way.

**What Move does** (`luxforge-install`):

1. It copies the bundle with `ditto`, which keeps its signature, into a hidden staging copy in `/Applications`, or in `~/Applications` (created when needed) when `/Applications` is not writable.
2. It verifies the copy with `codesign --verify --deep --strict` and clears its quarantine (`com.apple.quarantine`, recursively) as LetsMove does. A copy that fails either is removed, and nothing else has changed.
3. If a `Luxforge.app` is already there, it stops and asks: "Replace the Luxforge in Applications?", naming it, with Replace and relaunch and Cancel. Only after Replace does that app go to the Trash (`NSFileManager trashItemAtURL`), and only then is the copy renamed into place.
4. It closes the window through the ordinary quit path: the remembered frame and the last preference write are stored, and the catalog owner closes the catalog and its thread is joined.
5. Only then does it move the original to the Trash, and start a detached waiter that waits for this process to exit (at most 30 seconds), opens the new copy with `open -n -a`, and ejects the original's disk image with `hdiutil detach`. An app on another read-only volume is left where it is.

A failure is shown in the card in plain words, with Try again and Not now.

Until the app moves, `update.status` reports `unavailable` with the reason `outside-applications` and `can_move_to_applications`.

**The hand-off to the Sparkle port.** The move is one crate, `luxforge-install`, which depends on no workspace crate, so the desktop's question and the macOS update port call the same code:

- `classify(bundle) -> Placement` (`running_bundle()` names the bundle), and `classify_with(bundle, roots, probes)` with injectable Applications folders and translocation and volume probes for tests;
- `move_to_applications(placement, roots, replace, env) -> Result<Moved, MoveError>`, with `Roots::standard()` and the `System` environment; `MoveError::Existing(path)` is the `existing-app` refusal, and each error names a `reason()`;
- `Relaunch::moved(&moved).start(&System)`, which trashes the original and starts the waiter.

The desktop's relaunch hook (`app/relaunch.rs`) runs work once the quit path has closed the catalog: `relaunch(Relaunch)` holds one relaunch for the quit path, and `request_quit()` closes the window the way its close button does, from any thread. A release outside Applications gets the move port (`app/update_port.rs`), whose `move_to_applications(replace)` answers at once: an app already at the destination without `replace` is refused there and then as `existing-app` (a `conflict`); otherwise the move runs on its own thread, then `relaunch(Relaunch::moved(..))` and `request_quit()`. A move that fails becomes the status's `error`, and `can_move_to_applications` stays true so it can be tried again. The Sparkle port, which runs only from Applications, refuses the call as `not-supported`. While the port's `update.status` reports `can_move_to_applications`, the desktop's question sends `update.move-to-applications` with `replace` instead of moving itself; until then it calls `luxforge-install` directly.

**Evidence.** `--evidence-move-to-applications`, honoured only with `--evidence-dir`, shows the question once the workspace is usable wherever the run's app is, so it can be captured; its buttons move nothing and log `move_to_applications_skipped`. The `move-to-applications` smoke scenario captures it, with its state under `move_to_applications` in the frame and `move_question_classified` and `move_question_shown` in the log. The real translocated first launch from the disk image is part of final qualification.

## Linux: the payload updater

The Linux download is a relocatable `Luxforge/` directory. The updater serves the payload the running executable lives in, identified by the `build.json` beside it: its `version`, its `build_number` and its `target`.

The engine is `luxforge_update::payload`, in the leaf crate. It reaches the network only through a `Fetch` its host hands it. The desktop's port, `LinuxPayloadPort` in `crates/luxforge-app/src/app/update_port.rs`, adapts the checked `luxforge-net` transport to that fetch and runs the engine on its own threads.

**When it can update in place.** All of these must hold:

- the payload carries no package-manager marker, `Luxforge/.package-managed`, which a future distribution package writes to stand the updater down;
- the person can write the payload and its parent;
- a staging directory can be created beside the payload on the same filesystem. A launch probes this by creating and removing `.luxforge-update-probe-<pid>` in the parent.

Otherwise it reports `linux-notify`. It still checks and offers the update, `update.install` is refused as `not-ready` with `notify-only`, and the offer opens the release page (the update's `notes_url`).

**An update, step by step:**

1. **Check.** Read `<feed base>linux-x86_64/appcast.xml`, at most 4 MiB, and choose the newest item on a followed channel with a higher build number. An item without `luxforge:catalogFormat` is never offered, because the updater could not tell whether it changes the catalog format.
2. **Download.** Stream the archive into `<parent>/.luxforge-update-<build>/` through a 64 KiB file buffer.
   - Bytes past the item's stated length are refused (`oversize`), as is an item stating more than 2 GiB.
   - A download that ends short is refused (`length-mismatch`).
   - The archive's request may follow up to three redirects, to GitHub's release download hosts only.
3. **Verify** its Ed25519 signature against the compiled public key through a read-only memory map, so the archive is never held in heap memory.
4. **Extract** into `Luxforge.new/` in the staging directory:
   - Every entry must lie under the archive's one `Luxforge/` directory. No path may be absolute or contain `..`.
   - A symbolic link's target must be relative and descend only, so nothing reached through any chain of the archive's links leaves the payload.
   - A hard link must name an earlier regular file of the archive.
   - Devices, FIFOs, sparse files and other special entries are refused, as is a path written twice.
   - The archive may hold at most 100,000 entries and 4 GiB of file bytes.
   - Files keep their permission bits, executable bits included, with set-id and sticky bits, group and other write removed. Directories gain owner access.
   - The extracted `build.json` must name the item's version and build and this payload's target, and the payload's executable must be present and executable (`build-mismatch` otherwise).

   On Linux the filesystem is then synced, so a crash after the swap cannot leave files that never reached the disk. The archive is deleted.
5. **Install** at the end of the quit path, after the catalog is closed. The staged `build.json` is checked again first.
   - On Linux, `renameat2(RENAME_EXCHANGE)` swaps `Luxforge` and the staging's `Luxforge.new` atomically. It is called through `rustix`, so the crate needs no `unsafe`. The old payload then moves from the staging directory to `Luxforge.previous` beside the new one.
   - Where the filesystem cannot exchange, and on other unix systems, two renames are used instead. The payload moves to `Luxforge.previous` and the new one into its place. If the second rename fails, the first is undone.
   - An earlier `Luxforge.previous` is replaced. The staging directory is removed.
   - The new executable is then exec'd with the launch's original arguments. There is no relaunch when installing automatically at quit.
6. **Clean up.** Once the new version's catalog owner has started, its first successful launch removes `Luxforge.previous` and every `.luxforge-update-*` entry beside the payload. Until then, `Luxforge.previous` is a manual way back. Two instances running from one payload share these directories, and a launch's cleanup can end the other instance's download, which then reports its failure.

Any failure before the swap, a cancelled download included, removes its staging directory and leaves the installed payload untouched. Every refusal names its code in `update.status`'s `error`.

**The desktop entry.** After a swap, a user-level desktop entry is refreshed when its `Exec` program lies inside this payload. The entry is `$XDG_DATA_HOME/applications/luxforge.desktop`, or `~/.local/share/applications/luxforge.desktop`. The refresh rewrites `Exec` and `TryExec` for the installed executable, and `X-Luxforge-Version` and `X-Luxforge-Build` for the new build, then replaces the file with a rename. That prompts the desktop environment to read the entry and its icon again. The install helper that would write the entry does not exist yet.

**The port** (`LinuxPayloadPort`):

- **Calls return at once.** Every `UpdatePort` call returns without waiting. One worker thread runs checks and the cleanup after launch, and sleeps on a condition variable until the next check is due. A download runs on its own thread, one at a time.
- **Schedule.** While `update_checks` is on, the worker checks 10 seconds after the owner attaches, once the workspace is up, and then 24 hours after the last check that finished. `update.check` checks at once, or joins the check already running.
- **Progress** is reported at most once per percent of the archive, and at least every 256 KiB.
- **Automatic updates.** An update the policy installs automatically downloads in the background and is swapped at an ordinary quit, without relaunching. A background download the policy stops wanting is cancelled. `update.skip` of the offered version cancels its download and removes what was staged.
- **Install on request.** `update.install` downloads what is still missing, then posts the desktop's ordinary close (`Message::Close`) through a wake signal only this port's launches subscribe to. The close stops the live server and the catalog owner. Once the owner thread has joined, it calls the port's quit hook, which swaps and execs.
- **Quitting** cancels any running check or download and waits for the cancelled download to end.

**Which launches get it.** The port is chosen in the desktop's `host_config`:

- **A Linux release build** gets the port when a person launches it and the build has a public key. A release build is one compiled with `LUXFORGE_RELEASE_VERSION`, and the feed base URL and key come from the `release/update.json` it was compiled with. A release build without the key reports `no-update-key`. A binary outside a payload, with no `build.json` beside it, is treated as a development build.
- **Every other launch** reports `unavailable` with `development-build`. That covers development and test builds, and hidden, evidence and test-receiver launches, which is how smoke, verify and background runs start. None of them contacts the real feed.
- **macOS** stays `unavailable` until its Sparkle port is selected in the same place.
- **A debug Linux build** can be pointed at a local feed and key for a manual test ([development](../engineering/development.md#a-local-update-feed)). A release build never reads those variables.

## Windows later

Windows distribution has not been planned. When it is, updates follow this design:

- the `windows-x86_64` feed;
- the same Ed25519 key;
- the same commands, preferences and desktop interface through the port;
- Authenticode-signed downloads, so SmartScreen accepts them.

WinSparkle reads this feed format and verifies EdDSA signatures, but it brings its own windows. The in-house updater could instead hand off to an installer. That choice is made with the Windows installer format, and nothing here precludes either.

## Launching past an unsupported catalog

Until 1.0.0, an update can bring a new catalog format.

This part is built (`crates/luxforge-app/src/app/unsupported_catalog.rs`). A launch that failed whenever its remembered catalog was refused would look like a broken app after an update started from Finder.

When the catalog the desktop chose itself, the remembered one or the default, is refused as `incompatible`, the window opens on a sheet instead of the launch failing. The sheet:

- names the refused catalog's file;
- names its format marker and the format this version opens, from the refusal's `data.catalog_format` and `data.expected_format`, or says the file is not marked as a Luxforge catalog;
- says the catalog and its originals are unchanged, and that the version of Luxforge that made it can still open it;
- links to `https://github.com/Willyham/luxforge/releases` through the checked HTTPS opener;
- offers **Quit** and **Start a new catalog**.

**Start a new catalog** creates `<stem> (format N).sqlite` beside the refused catalog, where N is the format this version makes. A refused catalog's own ` (format M)` suffix is replaced rather than repeated. When the name is taken by any file a catalog keeps beside itself (the file, its `-wal`, `-shm` or `-journal`, its `.artifacts` or `.index` directory, or its live-session file), the next of `<stem> (format N) 2.sqlite`, `3` and so on is used. The new catalog is opened off the update loop through the same path a launch takes, and remembered with `preferences.set {"catalog": …}`. The window then becomes the editor over it. A failure says why and offers both choices again. **Quit** creates and writes nothing. Closing the window is Quit.

The refused catalog is never modified, renamed or deleted. The core refuses a catalog before setting its journal. A refusal closes without checkpointing a write-ahead log it found beside the file, so the catalog, its `-wal`, its artifacts and its index keep every byte; tests check this byte for byte.

These keep the command line's refusal, an error and a non-zero exit:

- an explicit `--catalog` path;
- another instance owning the catalog (`conflict`);
- every other open error.

No catalog is open while the sheet shows, so there is no owner or command registry to call. The programmatic equivalent of Start a new catalog is a launch with `--catalog NEW_PATH`, then `preferences.set {"catalog": NEW_PATH}` to keep it.

Hidden and evidence launches show the sheet too, never taking focus. An evidence run opens a catalog of its own as its remembered one with `--evidence-catalog CATALOG`. It captures the sheet as `unsupported-catalog.png`, with its state in `unsupported-catalog.json`, then takes the choice `--evidence-unsupported-catalog start|quit` names. The `unsupported-catalog` smoke scenario captures the sheet over a seeded format-16 catalog, starts a new catalog and opens a photograph in it.

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
- **Feed publication.** The workflow rules in `check-repository`, the deploy archive, the store's installer and the route, against fixture releases. A local dry run goes from fixture builds through `update-record`, the workflow's record selection and `update-feed --deploy` to `install-feeds.py` and the ingress answering the feeds.
- **Linux engine.** Refusing a bad signature, a truncated or oversized download, path traversal and a mismatched `build.json`. Swap and recovery in temporary directories. The notify-only fallback.
- **Command and preference schema and authority**, and UI/API parity.
- **A local end-to-end macOS update** from one bundle to a newer one, through Sparkle with a temporary test signing identity, in background-only bundles that never take focus: `cargo xtask update-journey --output NEW` ([development](../engineering/development.md#the-macos-update-journey)). Build 1 checks a loopback feed signed with a throwaway Ed25519 key, offers and installs build 2 through the port, quits; Sparkle's installer replaces the bundle and relaunches it, and the relaunch reports build 2 through `luxforge-ctl`. Every launch keeps its files under the data root the bundle's `LuxforgeUpdateJourneyRoot` names. `codesign` refuses the temporary self-signed identity as untrusted, and trusting it would change the person's trust settings, so the journey signs ad hoc and records the refusal; Sparkle accepts the ad hoc signatures because both builds share the EdDSA key. A Developer ID signature, the hardened runtime and Gatekeeper on an installed update are left to qualification.
- **A rendered check** of the offer, the Settings group, the move question and the unsupported-catalog launch, with correlated state and logs.

Qualification comes last, on real signed release candidates through the pre-release channel:

- an M4 update from one candidate to the next;
- a clean-account or second-Mac update from the DMG, including the move into Applications, in a foreground session the owner authorizes;
- a native Ubuntu 24.04 payload update;
- the feed route answering at `api.luxforge.app`, published by a hosted Update feeds run;
- a catalog-format-change offer and launch.
