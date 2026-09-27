# JPEG export

Status: implemented and verified on the M4 Mac; measured in the [performance plan](../specs/performance.md#jpeg-export). This design implements the accepted export contract in [decisions](../decisions.md#editing-and-storage) and [crop and export](../specs/single-image.md#export): quality 90, a native destination picker suggesting `-edited.jpg`, never overwriting an existing file or a source alias, and optional metadata stripped by default with a Keep metadata option. What this design chose beyond that contract, the encoder included, was accepted by the owner on 2026-09-27 and is listed under [decisions](#decisions).

Export writes one committed entry's exact render to a new JPEG file. It never touches the original, the catalog's history or any existing file.

## Behavior

1. **Freeze.** `export.jpeg` resolves the target on the catalog owner: the asset, the entry (the current entry unless `entry_id` names another), its snapshot, the recipe hash and the source fingerprint, the same identity an analysis job freezes. It binds the verified prepared source, the registry, the render context, the recipe and the original's capture metadata into one job, in `O(layers)`, and returns the job id. Later commits by any client, including the caller, cannot change a job already accepted. A draft is never exported: only a saved entry can be named.
2. **Render.** The export worker renders that snapshot through `luxforge_core::render` with the exact phase and the job's cancellation token. A JPEG source renders in the byte domain and a RAW source through the linear domain, which converts the high-precision recipe to 8-bit sRGB once, here. Output is the recipe's output stage: source scale, after orientation, transforms and crop. Nothing is resized.
3. **Encode.** libjpeg-turbo writes a baseline JPEG at quality 90, 8-bit sRGB with full-resolution (4:4:4) chroma in one interleaved scan and the standard Huffman tables, always with an embedded sRGB ICC profile ([decision 5](#decisions)). The encoder reads the rendered frame in place, 16 rows per call, checking cancellation and reporting progress between calls; the encoded bytes stream into the temporary file through libjpeg's buffer of at most 64 KiB rather than into a whole buffer. A libjpeg error fails the export with its message; it never aborts the process.
4. **Metadata.** Off by default: the file carries no EXIF, IPTC or XMP at all, only the JFIF header and the ICC profile. With Keep metadata, one EXIF segment carries the supported fields read from the original (below) plus the structural fields the export itself defines: Orientation 1, ColorSpace sRGB, the output's pixel dimensions and `Software`. No thumbnail, maker note, serial number or preview survives in either mode.
5. **Publish.** The temporary file is created exclusively in the destination directory, written, flushed and synced, then published under the final name without replacement: a hard link that fails if the name exists, or, on a file system without hard links, an exclusive create of the final name and a copy. The temporary file is then removed and the directory synced. Failure or cancellation removes only the temporary file, and a final file this job itself created and did not finish.
6. **Report.** The job ends `ready` with the written path, byte length, dimensions and the metadata fields kept, or `failed` or `cancelled` with its reason. A finished export records an event on the owner's log, so every client sees it. Progress is published to the activity board as the phases `rendering`, `encoding` and `writing`.

A stack the host cannot evaluate — an unavailable provider, a refused payload, a missing or changed original — fails the export explicitly with that reason and writes nothing. An unprepared source is answered `preparation-required` with a source job, exactly as `analysis.request` is, and the client asks again once that job ends.

## Destination rules

- The destination is an absolute path whose extension is `.jpg` or `.jpeg`, in either case, whose parent is an existing directory.
- Anything already at the destination path is refused with `conflict`: a file, a directory, a symlink (dangling or not) or a hard link to an original. Because nothing that exists is ever replaced, no path can reach an original or its alias. The check is repeated by the publish step itself, which never replaces, so a file that appears between the request and the write is refused too.
- `export.plan` suggests `<source stem>-edited.jpg` in the original's directory, or `-edited-2.jpg`, `-edited-3.jpg` and so on when that name is taken, reading at most 64 names; past that it suggests no name rather than guessing.

## Keep metadata

The original's EXIF is read once, when the source is prepared, from the bytes the source worker has already read: the JPEG's APP1 segment, the TIFF structure of a NEF or DNG, or the EXIF segment of the JPEG a RAF embeds. The reader is bounded (at most 64 KiB of EXIF for a JPEG, 256 entries per IFD, no IFD visited twice) and never fails a preparation: an unreadable block is simply no metadata. Each retained field must have its expected type and count, or it is dropped; a text field longer than 1 KiB, or blank, is dropped too. `Software` is written as `Luxforge`.

| Group | Fields |
| --- | --- |
| Descriptive | ImageDescription, Artist, Copyright |
| Camera | Make, Model, LensMake, LensModel, LensSpecification |
| Capture | DateTimeOriginal, DateTimeDigitized, OffsetTimeOriginal, OffsetTimeDigitized, SubSecTimeOriginal, ExposureTime, FNumber, ExposureProgram, PhotographicSensitivity, ExposureBiasValue, MeteringMode, Flash, FocalLength, FocalLengthIn35mmFilm |
| GPS | GPSVersionID, GPSLatitudeRef, GPSLatitude, GPSLongitudeRef, GPSLongitude, GPSAltitudeRef, GPSAltitude, GPSTimeStamp, GPSDateStamp, GPSImgDirectionRef, GPSImgDirection |

Everything else is dropped: maker notes, thumbnails, serial numbers, user comments, the original's orientation, dimensions, colour space and profile, and all IPTC and XMP. Private manufacturer metadata is not round-tripped.

## API

| Method | Envelope | Behavior |
| --- | --- | --- |
| `export.plan {asset_id, entry_id?}` | none | `{asset_id, entry_id, snapshot_id, width, height, suggested}`: the output dimensions from the compiled recipe and the suggested destination. Renders nothing |
| `export.jpeg {asset_id, entry_id?, destination, keep_metadata?}` | request (`request_id`, `actor`) | Validates the destination, freezes the target and queues one job; `{job_id, status, asset_id, entry_id, snapshot_id, destination, width, height, keep_metadata}`. A retry with the same request id is answered from the stored answer and writes no second file |
| `export.read {job_id}` | none | `{job_id, status, progress, result?, error?}`; `result` is `{path, bytes, width, height, metadata}` where `metadata` lists the EXIF fields written |
| `export.cancel {job_id}` | none | Cancels the job for every client: a queued job never starts, a running one stops at its next row or block and removes its temporary file |

Export jobs run on their own lane of the shared lane runner: one running and four waiting, refused beyond that with `resource-limit`. The lane never supersedes a job, so a newer export waits instead of replacing an older one, and it never takes the preview or analysis worker.

## Desktop

The title bar's Export button, beside Open, offers **Export JPEG…** and **Export JPEG, keep metadata…**. `Cmd+E` runs the first and `Shift+Cmd+E` the second; the command palette lists both. Each asks `export.plan` for the suggested name, opens the native save dialog in the original's folder with that name, and sends `export.jpeg` for the displayed entry: the current entry, or the entry being previewed from history. The status bar reads *Exporting DSC_0042-edited.jpg…*, then *Exported DSC_0042-edited.jpg · 6000 × 4000 · 8.4 MB*, or the refusal's reason. The Performance section shows the running export with its phase, as it shows every other long job.

## Bounds and cost

- The frame is the render's own exact frame, inside the existing 512 MiB evaluated-frame limit; the encoder reads it in place and streams its output. A RAW export keeps the source's developed planes alive until it ends, like any render holding them.
- Nothing runs on the catalog owner beyond planning in `O(layers)` and short file-system checks on the destination. The render uses the shared Rayon pool and render context, so an export and a preview pace each other rather than one starving the other.
- Wall time and peak additional memory for 24 and 60 MP JPEG sources and one RAW per supported camera are measured once the feature is complete and recorded in the [performance plan](../specs/performance.md).

## Decisions

Accepted by the owner on 2026-09-27:

1. The title-bar Export button opens a two-item menu, with `Cmd+E` and `Shift+Cmd+E`, rather than a dialog or a state-panel section.
2. The desktop exports the displayed entry, including a history preview, and never a draft.
3. The suggested name counts up (`-edited-2.jpg`) when `-edited.jpg` is taken, so the save dialog does not open on a name that will be refused.
4. Keep metadata carries exactly the EXIF fields in the table above and no IPTC or XMP.
5. The encoder is libjpeg-turbo through the pinned `mozjpeg` crate, whose `mozjpeg-sys` bundles the C source and builds it with `cc`, set to its fastest profile: baseline, 4:4:4, one interleaved scan and the standard Huffman tables, which is plain libjpeg-turbo output. Encode only on the M4 at quality 90 and 4:4:4 (shared host, median of 7), it takes 47 to 48 ms at 24 MP and 117 to 121 ms at 60 MP against 111 and 274 ms for the `image` encoder it replaces, and 64 and 105 ms against 149 and 245 ms on Z6 and X100VI renders, at the same file size and PSNR within 0.04 dB; `turbojpeg` 3.1.0 took 43 and 103 ms, and `jpeg-encoder` 0.7.1 110 and 273 ms (61 and 152 ms only at 4:2:0). `jpeg-encoder` was not chosen because it has no NEON path, its speed comes only from point-sampled 4:2:0 chroma, and its optimized and progressive modes buffer the whole image; `turbojpeg` because it needs CMake at build time and encodes a whole in-memory image in one call, with no marker writing or per-strip progress. Optimized Huffman tables would cost about twice the encode time and are not used. aarch64 builds libjpeg-turbo's NEON code through the C compiler; its x86_64 SIMD needs NASM, which is not required, so x86_64 builds the slower portable C path (not yet measured). The same library decodes originals on import ([decisions](../decisions.md#export)), so one adapter module, `crates/luxforge-core/src/jpeg.rs`, holds the libjpeg session handling for both: export keeps its policy (quality, 4:4:4, the EXIF and ICC segments, progress and cancellation) and the adapter the error recovery.
6. The method names `export.plan`, `export.jpeg`, `export.read` and `export.cancel`. When one job table and one job API land, `export.read` and `export.cancel` fold into them.

Also accepted: the build follows the accepted export contract rather than the earlier state-panel proposal with presets, resizing, unique names by default and durable export records, which stays unadopted; and the export lane is its own instance of the existing lane runner until one job table exists. Not in this build: presets, resizing, other formats, output sharpening, batch export and durable export records; each would need its own decision.
