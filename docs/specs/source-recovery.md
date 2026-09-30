# Referenced originals and source recovery

Status: stable identity, fingerprints and missing or changed-source protection exist since M1; manual Locate and availability tracking work through the API (`source.locate`, `source.check`), and the desktop's **Locate original…** is to come. The owner references local files and syncs them to external storage, so this matters early.

`source.locate {asset_id, path, mutation}` follows the steps below as a `source-locate` job. Before reading anything, on the catalog owner, it refuses a relative path, a folder, a file that cannot be read, a file of another length and a file another asset already names by path or file identity (`conflict`, naming that asset). The catalog's library worker then streams the candidate's SHA-256 through one 1 MiB buffer, cancellable between chunks, from one open handle whose signature (length, modification and change times, file identity) is taken before the first read and after the last, so a file modified during verification is refused; the digest must be the stored fingerprint. Back on the owner the file must still have the signature it was verified with and still be unclaimed, and then one catalog transaction records its volume, sets the asset's `asset-source` item (locator, source folder, volume and file identity) as one library change and records the original available. The change is undone with `library.undo` like any library change, the owner's cached asset head moves where it commits, and it is announced as one event naming its library sequence. History, fingerprint and interpretation are untouched, and a later preparation of the relocated asset is still checked against the stored fingerprint. It is the only relocation write: the relinks `source.check` makes for a file moved within its volume, found by its file identity in the catalog's index and confirmed by fingerprint, go through it too.

## Data model

An asset has a permanent catalog ID. Its path is a changeable locator. A content fingerprint verifies which bytes the locator currently names; it is not the asset's identity. Recipe and history stay attached to the asset ID through moves, offline periods and relinking.

Store the source root separately from the relative path, plus availability state and the last verified fingerprint. Import records a full streamed hash in bounded background work. Read, hash and decode from a consistent snapshot and detect concurrent modification; never pair one version's hash with another's pixels. Size, mtime and filesystem IDs are hints, not proof. Byte-identical copies can share a fingerprint without being the same asset: never merge assets or edits because hashes match. A content change, including a metadata rewrite, invalidates the strict fingerprint; preserving edits across such a replacement is a future reviewed workflow.

## Locate missing original

1. Detect an unavailable source and retain the asset, recipe and history. Show the last known path and any cached preview labelled as unavailable-original content. Missing means unavailable, not deleted.
2. Offer **Locate original** with a file picker. Never scan disks or drives automatically.
3. Verify the candidate against the stored fingerprint with bounded, cancellable I/O. A match may be renamed or on another volume. A mismatch is reported without replacing the source, however similar it looks.
4. If the candidate already belongs to another asset, say so and require an explicit decision; never silently merge.
5. Atomically update locator, availability and source revision, preserve asset, recipe and history identity, invalidate only locator-dependent state and notify all clients. Preview and export resume against the verified source.

Cancellation, inaccessible candidates, mismatches and write failures leave the prior record intact. Recheck identity if the file changes between verification and use. Export must never use a stale preview or an unrelated file at the old path. The same operation is exposed programmatically as `source.locate` with the asset ID and candidate path under the `{request_id, actor}` envelope, following the same validation and conflict rules; it moves no history, so it takes no expected revision, and an edit committed while it verifies stands. Luxforge never moves, copies, syncs or deletes originals; the user points Locate at the copy they want after their own sync.

## Later: folder moves and assistance

Research how photographers use internal SSDs, removable drives, NAS, managed copies, sidecars and multiple computers before promising support, especially how they distinguish a working copy from an archive and which application syncs. Candidate behavior: relink a root with a dry run showing matched, missing, changed and ambiguous files; search only user-selected roots with progress, cancellation and bounded I/O, filtering by cheap metadata and verifying by fingerprint; treat an offline volume differently from a changed file; use filesystem notifications only as accelerators. Filename alone never authorizes reassignment. Relinking is not a backup strategy, and Luxforge does not become a sync engine by implication. Sidecars may help later but add write, conflict and identity choices of their own.

## Locate acceptance

- Import and edit, close, rename or move the original, reopen, Locate the same bytes and export the original edits.
- Repeat with a byte-identical copy on another mounted volume; asset ID and history unchanged.
- Reject a different photo with the same name, a changed file and a source modified during verification.
- A candidate already cataloged under another ID produces no silent merge or edit loss.
- Unmounting, cancelling verification and a failed locator commit all leave the last valid state.
- Relinking through the API while the GUI is open behaves identically, with revision checks against concurrent edits.

Native external-drive acceptance uses an explicitly chosen test directory with copied fixtures, never the user's archive. VM shared-folder behavior is not proof of native removable-drive support.
