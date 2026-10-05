# Embedded previews in RAW files

Question: which cameras' RAW files carry an embedded preview the catalog can show at each of its sizes — a grid cell, the loupe and the 100% focus check — and which need a Luxforge development instead ([catalog design](../design/catalog.md#the-index-and-previews-cache), proposal P6)?

Answer: **53 of the 103 cameras carry a full-size JPEG**, so the 100% check can read it; **43 carry only a smaller JPEG** (1616 to 4416 px on the long edge), enough for the loupe but not for 100%; **the five DJI drones carry only 960 px previews**, enough for a grid cell but not the loupe; and **the Canon EOS R5 Mark II and R8 files carry no JPEG at all**, only H.265 previews. Every file opened, and listing its previews read 17 to 205 KB of it.

## Scope

- **Corpus.** 120 RAW files from 103 camera models, as LibRaw names them: a local selection of 103 CC0 [raw.pixls.us](https://raw.pixls.us/) samples of widely used cameras and drones in their common modes (99 of them from the [modern camera selection](../../fixtures/modern-camera-selection.json)), 15 more samples of [most-used cameras](popular-cameras.md), and the owner's Nikon Z6 NEF, Fujifilm X100VI RAF and DJI Air 2S DNG originals. The owner's other captures are the same files as four of the samples and count once. The files stay outside the repository and were only read; each file's SHA-256 was checked unchanged after it was read.
- **Reader.** LibRaw 0.22.2's identify and `unpack_thumb_ex`, through `luxforge-raw`'s `EmbeddedPreviews` ([its README](../../crates/luxforge-raw/README.md#embedded-previews)): each file read from disk by position through eight 16 KiB blocks, nothing unpacked. Every image the crate hands over was extracted; each JPEG was decoded with `image` 0.25.9 as an independent check and measured from its SOF marker. Through this stream every file lists exactly what LibRaw's own in-memory stream lists for it.
- **Date and host.** 2026-09-30, on the owner's M4 Mac. Nothing was timed.
- **Regenerate.** The inventory writes the table below and one JSON record per file under the output directory:

```sh
LUXFORGE_PREVIEW_DIRS=/path/to/selection:/path/to/popular-extra:/path/to/owner/raw \
LUXFORGE_PREVIEW_LABELS=/path/to/raw-pixls-index.json:/path/to/popular-extra/results.json \
LUXFORGE_PREVIEW_OUTPUT=target/embedded-previews \
  cargo test --release -p luxforge-raw --locked --test embedded_previews embedded_preview_inventory \
  -- --ignored --nocapture
```

## Rules

- **Thumbnail** is the smallest image that extracts and decodes; **largest usable preview** the largest in pixels. Dimensions are the image's own (a JPEG's SOF), not what the container lists.
- **Full size**: the preview's long edge and short edge are each at least 95% of the visible image LibRaw would develop (`sizes.width` × `sizes.height`), long edge against long edge, so a preview stored in either orientation compares the same way. A camera's JPEG is its default crop of the visible area, a few pixels short of it on some bodies: every full-size preview here is 0.99 to 1.00 of the long edge, and every other preview 0.71 or less, so the threshold separates them with room on both sides. A camera set to crop to another aspect ratio in camera would have a JPEG short of the visible image on one edge and count as not full size.
- **Class**, from the largest usable preview: **full size**; **reduced**, at least 1024 px on the long edge (a loupe can show it); **small only**, below 1024 px (a grid cell can); **none usable**, nothing that extracts and decodes.
- **Read to list** is what opening a file and listing its previews read from it; **read to list and extract it** the same on a new handle followed by extracting the largest usable preview. Both are the bytes fetched from the file: whole 16 KiB blocks for identify's small reads, exact lengths for an image.
- A camera would be split by recording mode where its files answer differently. None did: every camera's files, in every mode sampled, carry the same previews.

## Results

| Class | Cameras |
| --- | --- |
| Full size (53) | Canon: every CR2 and CR3 body except the R5 Mark II and R8 (5D Mark IV, 6D Mark II, 7D Mark II, 80D, 90D, M6, R, R3, R5, R6, R6 Mark II, R7, R10, R50, RP, 1D X Mark III). Nikon: every body (D500, D5600, D6, D750, D7500, D780, D850, Z 30, Z 5, Z5II, Z 50, Z 6, Z 6II, Z 7, Z 7II, Z 8, Z 9, Z f, Z fc). Leica CL, M10, M10-R, Q2, SL2. Pentax K-1 Mark II, K-3 Mark III, K-70, KP. Ricoh GR III, GR IIIx. Sony A1, A7 IV, A7C II, A7CR, A7R V, A7S III, a6700 |
| Reduced (43) | Every Fujifilm body: 4416 × 2944 on the 26 and 40 MP X bodies, 1920 × 1280 on the X-T20 and X100F, 4000 × 3000 on the GFX 50S II, GFX100S and GFX100 II. Every OM System and Olympus body (OM-1, OM-1 Mark II, OM-3, OM-5, E-M1 Mark III, E-M1X, E-M10 Mark IV): 3200 × 2400. Every Panasonic body (G9 II, GH5, GH6, GH7, GX7 Mark III, S5, S5 II): 1920 × 1440 or 1920 × 1280. Sony A7 III, A7R II, A7R III, A7R IV, A7C, A9, a6000, a6100, a6400, a6600: 1616 × 1080 |
| Small only (5) | DJI FC220, FC3411 (the owner's Air 2S), FC4382, FC6310 and FC7303: 960 × 720 or 960 × 640 |
| None usable (2) | Canon EOS R5 Mark II and R8: only H.265 previews (1620 × 1080, and one LibRaw lists as 2 × 320) |

Full-size JPEGs take 0.5 to 12.3 MB (3.0 MB median); opening, listing and extracting the largest usable preview read at most 12.4 MB of any file, and a median 6% of it. Thumbnails are 160 × 120 on almost every body (720 × 480 on the Leica CL, Q2 and SL2, 256 × 192 on the DJI FC7303): a JPEG, or on most Nikon bodies and some Pentax and Ricoh ones an uncompressed RGB bitmap. OM System, Olympus and Panasonic files list one preview only, so the grid decodes that preview at a reduced size. No file stores a preview across the sensor's orientation, and no JPEG failed to decode.

The table uses LibRaw's names: Sony's ILCE-7M3 is the A7 III, ILCE-6400 the a6400, and so on; Nikon's Z 6_2 is the Z 6II and Z5_2 the Z5II.

### Per camera

| Camera | Files | Thumbnail | Largest usable preview | Full size | Long edge / visible | Read to list | Read to list and extract it | Notes |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| Canon EOS 5D Mark IV | 1 | JPEG 160×120, 16 KB | JPEG 6720×4480, 2.1 MB | yes | 1.00 | 82 KB | 2.2 MB | also lists LibRaw Kodak kind 430×284, not extracted |
| Canon EOS 6D Mark II | 1 | JPEG 160×120, 21 KB | JPEG 6240×4160, 4.5 MB | yes | 1.00 | 66 KB | 4.6 MB | also lists LibRaw Kodak kind 624×416, not extracted |
| Canon EOS 7D Mark II | 1 | JPEG 160×120, 8 KB | JPEG 5472×3648, 807 KB | yes | 1.00 | 66 KB | 872 KB | also lists LibRaw Kodak kind 464×309, not extracted |
| Canon EOS 80D | 1 | JPEG 160×120, 22 KB | JPEG 6000×4000, 5.2 MB | yes | 1.00 | 66 KB | 5.3 MB | also lists LibRaw Kodak kind 524×338, not extracted |
| Canon EOS 90D | 1 | JPEG 160×120, 15 KB | JPEG 6960×4640, 2.5 MB | yes | 1.00 | 145 KB | 2.6 MB |  |
| Canon EOS M6 | 1 | JPEG 160×120, 16 KB | JPEG 6000×4000, 7.4 MB | yes | 1.00 | 82 KB | 7.5 MB | also lists LibRaw Kodak kind 600×400, not extracted |
| Canon EOS R | 1 | JPEG 160×120, 16 KB | JPEG 6720×4480, 1.5 MB | yes | 1.00 | 140 KB | 1.6 MB |  |
| Canon EOS R10 | 3 | JPEG 160×120, 7 KB–15 KB | JPEG 6000×4000, 1.6 MB–3.8 MB | yes | 1.00 | 151 KB–160 KB | 1.8 MB–3.9 MB |  |
| Canon EOS R3 | 1 | JPEG 160×120, 19 KB | JPEG 6000×4000, 3.8 MB | yes | 0.99 | 161 KB | 4.0 MB |  |
| Canon EOS R5 | 2 | JPEG 160×120, 14 KB | JPEG 8192×5464, 3.1 MB | yes | 1.00 | 159 KB–160 KB | 3.2 MB–3.3 MB |  |
| Canon EOS R5 Mark II | 2 | – | – | – | – | 166 KB | – | also lists H.265 1620×1080, not extracted; also lists H.265 2×320, not extracted; no preview extracts and decodes |
| Canon EOS R50 | 1 | JPEG 160×120, 17 KB | JPEG 6000×4000, 3.9 MB | yes | 1.00 | 162 KB | 4.1 MB |  |
| Canon EOS R6 | 2 | JPEG 160×120, 17 KB | JPEG 5472×3648, 2.3 MB–2.4 MB | yes | 1.00 | 163 KB–164 KB | 2.4 MB–2.5 MB |  |
| Canon EOS R6 Mark II | 2 | JPEG 160×120, 17 KB | JPEG 6000×4000, 4.0 MB | yes | 1.00 | 165 KB–166 KB | 4.2 MB |  |
| Canon EOS R7 | 2 | JPEG 160×120, 18 KB | JPEG 6960×4640, 1.9 MB | yes | 1.00 | 162 KB–165 KB | 2.0 MB–2.1 MB |  |
| Canon EOS R8 | 1 | – | – | – | – | 161 KB | – | also lists H.265 1620×1080, not extracted; also lists H.265 2×320, not extracted; no preview extracts and decodes |
| Canon EOS RP | 1 | JPEG 160×120, 15 KB | JPEG 6240×4160, 1.7 MB | yes | 1.00 | 142 KB | 1.8 MB |  |
| Canon EOS-1D X Mark III | 1 | JPEG 160×120, 22 KB | JPEG 5472×3648, 1.7 MB | yes | 1.00 | 147 KB | 1.9 MB |  |
| DJI FC220 | 1 | JPEG 160×120, 7 KB | JPEG 960×720, 294 KB | no | 0.24 | 33 KB | 327 KB |  |
| DJI FC3411 | 1 | JPEG 160×108, 11 KB | JPEG 960×640, 328 KB | no | 0.18 | 33 KB | 361 KB |  |
| DJI FC4382 | 1 | JPEG 160×120, 16 KB | JPEG 960×720, 693 KB | no | 0.24 | 33 KB | 726 KB |  |
| DJI FC6310 | 1 | JPEG 160×120, 11 KB | JPEG 960×640, 247 KB | no | 0.18 | 17 KB | 264 KB |  |
| DJI FC7303 | 1 | JPEG 256×192, 10 KB | JPEG 960×720, 354 KB | no | 0.24 | 74 KB | 424 KB |  |
| Fujifilm GFX100 II | 1 | JPEG 160×120, 10 KB | JPEG 4000×3000, 1.3 MB | no | 0.34 | 132 KB | 1.4 MB |  |
| Fujifilm GFX100S | 1 | JPEG 160×120, 10 KB | JPEG 4000×3000, 3.4 MB | no | 0.34 | 132 KB | 3.5 MB |  |
| Fujifilm GFX50S II | 1 | JPEG 160×120, 9 KB | JPEG 4000×3000, 3.2 MB | no | 0.48 | 132 KB | 3.4 MB |  |
| Fujifilm X-E4 | 1 | JPEG 160×120, 9 KB | JPEG 4416×2944, 4.0 MB | no | 0.71 | 132 KB | 4.1 MB |  |
| Fujifilm X-H2 | 1 | JPEG 160×120, 9 KB | JPEG 4416×2944, 4.8 MB | no | 0.57 | 132 KB | 4.9 MB |  |
| Fujifilm X-H2S | 1 | JPEG 160×120, 9 KB | JPEG 4416×2944, 5.4 MB | no | 0.70 | 132 KB | 5.6 MB |  |
| Fujifilm X-M5 | 1 | JPEG 160×120, 9 KB | JPEG 4416×2944, 5.6 MB | no | 0.70 | 132 KB | 5.7 MB |  |
| Fujifilm X-Pro3 | 1 | JPEG 160×120, 9 KB | JPEG 4416×2944, 3.1 MB | no | 0.71 | 132 KB | 3.3 MB |  |
| Fujifilm X-S10 | 1 | JPEG 160×120, 10 KB | JPEG 4416×2944, 2.8 MB | no | 0.71 | 132 KB | 2.9 MB |  |
| Fujifilm X-S20 | 1 | JPEG 160×120, 9 KB | JPEG 4416×2944, 5.8 MB | no | 0.71 | 132 KB | 5.9 MB |  |
| Fujifilm X-T20 | 1 | JPEG 160×120, 9 KB | JPEG 1920×1280, 707 KB | no | 0.32 | 132 KB | 822 KB |  |
| Fujifilm X-T3 | 1 | JPEG 160×120, 9 KB | JPEG 4416×2944, 2.8 MB | no | 0.71 | 132 KB | 2.9 MB |  |
| Fujifilm X-T30 | 1 | JPEG 160×120, 10 KB | JPEG 4416×2944, 3.6 MB | no | 0.71 | 132 KB | 3.7 MB |  |
| Fujifilm X-T4 | 1 | JPEG 160×120, 9 KB | JPEG 4416×2944, 4.2 MB | no | 0.71 | 132 KB | 4.3 MB |  |
| Fujifilm X-T5 | 1 | JPEG 160×120, 10 KB | JPEG 4416×2944, 4.0 MB | no | 0.57 | 132 KB | 4.1 MB |  |
| Fujifilm X-T50 | 1 | JPEG 160×120, 9 KB | JPEG 4416×2944, 5.7 MB | no | 0.57 | 132 KB | 5.8 MB |  |
| Fujifilm X100F | 1 | JPEG 160×120, 9 KB | JPEG 1920×1280, 860 KB | no | 0.32 | 132 KB | 975 KB |  |
| Fujifilm X100V | 1 | JPEG 160×120, 9 KB | JPEG 4416×2944, 5.6 MB | no | 0.71 | 132 KB | 5.7 MB |  |
| Fujifilm X100VI | 3 | JPEG 160×120, 9 KB–10 KB | JPEG 4416×2944, 3.2 MB–5.8 MB | no | 0.57 | 132 KB | 3.3 MB–5.9 MB |  |
| Leica CL | 1 | JPEG 720×480, 87 KB | JPEG 6000×4000, 1.3 MB | yes | 1.00 | 17 KB | 1.3 MB |  |
| Leica M10 | 1 | JPEG 160×120, 16 KB | JPEG 5952×3968, 2.5 MB | yes | 0.99 | 17 KB | 2.5 MB |  |
| Leica M10-R | 1 | JPEG 160×120, 20 KB | JPEG 7840×5184, 3.4 MB | yes | 1.00 | 17 KB | 3.5 MB |  |
| Leica Q2 | 1 | JPEG 720×480, 80 KB | JPEG 8368×5584, 3.9 MB | yes | 1.00 | 17 KB | 3.9 MB |  |
| Leica SL2 | 1 | JPEG 720×480, 78 KB | JPEG 8368×5584, 5.5 MB | yes | 1.00 | 17 KB | 5.5 MB |  |
| Nikon D500 | 1 | RGB bitmap 160×120, 58 KB | JPEG 5568×3712, 1.6 MB | yes | 0.99 | 117 KB | 1.7 MB |  |
| Nikon D5600 | 1 | RGB bitmap 160×120, 58 KB | JPEG 6000×4000, 2.7 MB | yes | 1.00 | 99 KB | 2.8 MB |  |
| Nikon D6 | 1 | RGB bitmap 160×120, 58 KB | JPEG 5568×3712, 503 KB | yes | 1.00 | 174 KB | 661 KB |  |
| Nikon D750 | 1 | RGB bitmap 160×120, 58 KB | JPEG 6016×4016, 975 KB | yes | 1.00 | 115 KB | 1.1 MB |  |
| Nikon D7500 | 1 | RGB bitmap 160×120, 58 KB | JPEG 5568×3712, 2.7 MB | yes | 0.99 | 115 KB | 2.8 MB |  |
| Nikon D780 | 1 | RGB bitmap 160×120, 58 KB | JPEG 6048×4024, 3.6 MB | yes | 1.00 | 152 KB | 3.7 MB |  |
| Nikon D850 | 1 | RGB bitmap 160×120, 58 KB | JPEG 8256×5504, 5.3 MB | yes | 1.00 | 115 KB | 5.4 MB |  |
| Nikon Z 30 | 1 | RGB bitmap 160×120, 58 KB | JPEG 5568×3712, 1.3 MB | yes | 0.99 | 174 KB | 1.4 MB |  |
| Nikon Z 5 | 1 | RGB bitmap 160×120, 58 KB | JPEG 6016×4016, 2.4 MB | yes | 1.00 | 175 KB | 2.5 MB |  |
| Nikon Z 50 | 1 | RGB bitmap 160×120, 58 KB | JPEG 5568×3712, 3.3 MB | yes | 0.99 | 171 KB | 3.5 MB |  |
| Nikon Z 6 | 4 | RGB bitmap 160×120, 58 KB | JPEG 6048×4024, 1.3 MB–1.6 MB | yes | 1.00 | 171 KB | 1.4 MB–1.7 MB |  |
| Nikon Z 6_2 | 1 | RGB bitmap 160×120, 58 KB | JPEG 6048×4024, 3.0 MB | yes | 1.00 | 176 KB | 3.2 MB |  |
| Nikon Z 7 | 1 | RGB bitmap 160×120, 58 KB | JPEG 8256×5504, 3.7 MB | yes | 1.00 | 171 KB | 3.8 MB |  |
| Nikon Z 7_2 | 1 | RGB bitmap 160×120, 58 KB | JPEG 8256×5504, 4.0 MB | yes | 1.00 | 176 KB | 4.2 MB |  |
| Nikon Z 8 | 1 | JPEG 160×120, 11 KB | JPEG 8256×5504, 5.8 MB | yes | 1.00 | 199 KB | 6.0 MB |  |
| Nikon Z 9 | 1 | JPEG 160×120, 10 KB | JPEG 8256×5504, 6.5 MB | yes | 1.00 | 198 KB | 6.7 MB |  |
| Nikon Z f | 1 | JPEG 160×120, 12 KB | JPEG 6048×4032, 2.7 MB | yes | 1.00 | 205 KB | 2.9 MB |  |
| Nikon Z fc | 1 | RGB bitmap 160×120, 58 KB | JPEG 5568×3712, 1.6 MB | yes | 0.99 | 176 KB | 1.8 MB |  |
| Nikon Z5_2 | 1 | JPEG 160×120, 9 KB | JPEG 6048×4032, 2.3 MB | yes | 1.00 | 189 KB | 2.5 MB |  |
| OM Digital OM-1 | 1 | JPEG 3200×2400, 963 KB | JPEG 3200×2400, 963 KB | no | 0.61 | 50 KB | 999 KB |  |
| OM Digital OM-1MarkII | 1 | JPEG 3200×2400, 928 KB | JPEG 3200×2400, 928 KB | no | 0.61 | 50 KB | 964 KB |  |
| OM Digital OM-3 | 1 | JPEG 3200×2400, 920 KB | JPEG 3200×2400, 920 KB | no | 0.61 | 50 KB | 956 KB |  |
| OM Digital OM-5 | 1 | JPEG 3200×2400, 1.0 MB | JPEG 3200×2400, 1.0 MB | no | 0.61 | 50 KB | 1.0 MB |  |
| Olympus E-M10MarkIV | 1 | JPEG 3200×2400, 1.1 MB | JPEG 3200×2400, 1.1 MB | no | 0.62 | 50 KB | 1.1 MB |  |
| Olympus E-M1MarkIII | 1 | JPEG 3200×2400, 971 KB | JPEG 3200×2400, 971 KB | no | 0.61 | 50 KB | 1.0 MB |  |
| Olympus E-M1X | 1 | JPEG 3200×2400, 980 KB | JPEG 3200×2400, 980 KB | no | 0.61 | 50 KB | 1.0 MB |  |
| Panasonic DC-G9M2 | 1 | JPEG 1920×1440, 844 KB | JPEG 1920×1440, 844 KB | no | 0.33 | 91 KB | 924 KB |  |
| Panasonic DC-GH5 | 1 | JPEG 1920×1440, 706 KB | JPEG 1920×1440, 706 KB | no | 0.37 | 89 KB | 782 KB |  |
| Panasonic DC-GH6 | 1 | JPEG 1920×1440, 1.0 MB | JPEG 1920×1440, 1.0 MB | no | 0.33 | 91 KB | 1.1 MB |  |
| Panasonic DC-GH7 | 1 | JPEG 1920×1440, 832 KB | JPEG 1920×1440, 832 KB | no | 0.33 | 92 KB | 913 KB |  |
| Panasonic DC-GX7MK3 | 1 | JPEG 1920×1440, 658 KB | JPEG 1920×1440, 658 KB | no | 0.37 | 89 KB | 734 KB |  |
| Panasonic DC-S5 | 1 | JPEG 1920×1280, 524 KB | JPEG 1920×1280, 524 KB | no | 0.32 | 90 KB | 602 KB |  |
| Panasonic DC-S5M2 | 1 | JPEG 1920×1280, 1.0 MB | JPEG 1920×1280, 1.0 MB | no | 0.32 | 92 KB | 1.1 MB |  |
| Pentax K-1 Mark II | 1 | RGB bitmap 160×120, 58 KB | JPEG 7360×4912, 4.4 MB | yes | 1.00 | 50 KB | 4.5 MB |  |
| Pentax K-3 Mark III | 1 | JPEG 160×120, 8 KB | JPEG 6192×4128, 3.1 MB | yes | 0.99 | 66 KB | 3.2 MB |  |
| Pentax K-70 | 1 | JPEG 160×120, 5 KB | JPEG 6000×4000, 2.9 MB | yes | 1.00 | 50 KB | 3.0 MB |  |
| Pentax KP | 1 | RGB bitmap 160×120, 58 KB | JPEG 6016×4000, 3.0 MB | yes | 1.00 | 50 KB | 3.0 MB |  |
| Ricoh GR III | 1 | RGB bitmap 160×120, 58 KB | JPEG 6000×4000, 2.9 MB | yes | 1.00 | 33 KB | 3.0 MB |  |
| Ricoh GR IIIx | 1 | RGB bitmap 160×120, 58 KB | JPEG 6000×4000, 2.9 MB | yes | 1.00 | 33 KB | 2.9 MB |  |
| Sony ILCE-1 | 1 | JPEG 160×120, 14 KB | JPEG 8640×5760, 12.3 MB | yes | 1.00 | 135 KB | 12.4 MB |  |
| Sony ILCE-6000 | 1 | JPEG 160×120, 12 KB | JPEG 1616×1080, 1.1 MB | no | 0.27 | 142 KB | 1.3 MB |  |
| Sony ILCE-6100 | 1 | JPEG 160×120, 7 KB | JPEG 1616×1080, 581 KB | no | 0.27 | 115 KB | 683 KB |  |
| Sony ILCE-6400 | 1 | JPEG 160×120, 7 KB | JPEG 1616×1080, 621 KB | no | 0.27 | 128 KB | 736 KB |  |
| Sony ILCE-6600 | 1 | JPEG 160×120, 8 KB | JPEG 1616×1080, 536 KB | no | 0.27 | 129 KB | 653 KB |  |
| Sony ILCE-6700 | 2 | JPEG 160×120, 11 KB | JPEG 6192×4128, 7.2 MB–7.3 MB | yes | 0.99 | 151 KB | 7.4 MB |  |
| Sony ILCE-7C | 1 | JPEG 160×120, 10 KB | JPEG 1616×1080, 448 KB | no | 0.27 | 127 KB | 562 KB |  |
| Sony ILCE-7CM2 | 2 | JPEG 160×120, 8 KB–9 KB | JPEG 7008×4672, 6.4 MB–6.7 MB | yes | 1.00 | 151 KB | 6.5 MB–6.9 MB |  |
| Sony ILCE-7CR | 1 | JPEG 160×120, 9 KB | JPEG 9504×6336, 9.5 MB | yes | 0.99 | 151 KB | 9.6 MB |  |
| Sony ILCE-7M3 | 2 | JPEG 160×120, 11 KB | JPEG 1616×1080, 1.1 MB–1.2 MB | no | 0.27 | 141 KB–142 KB | 1.3 MB |  |
| Sony ILCE-7M4 | 2 | JPEG 160×120, 13 KB | JPEG 7008×4672, 6.6 MB–7.4 MB | yes | 1.00 | 151 KB | 6.8 MB–7.6 MB |  |
| Sony ILCE-7RM2 | 1 | JPEG 160×120, 11 KB | JPEG 1616×1080, 584 KB | no | 0.20 | 142 KB | 711 KB |  |
| Sony ILCE-7RM3 | 1 | JPEG 160×120, 8 KB | JPEG 1616×1080, 294 KB | no | 0.20 | 143 KB | 424 KB |  |
| Sony ILCE-7RM4 | 1 | JPEG 160×120, 7 KB | JPEG 1616×1080, 651 KB | no | 0.17 | 128 KB | 766 KB |  |
| Sony ILCE-7RM5 | 2 | JPEG 160×120, 10 KB | JPEG 9504×6336, 11.5 MB–11.6 MB | yes | 0.99 | 135 KB | 11.6 MB–11.8 MB |  |
| Sony ILCE-7SM3 | 1 | JPEG 160×120, 6 KB | JPEG 4240×2832, 1.6 MB | yes | 1.00 | 137 KB | 1.8 MB |  |
| Sony ILCE-9 | 1 | JPEG 160×120, 10 KB | JPEG 1616×1080, 394 KB | no | 0.27 | 140 KB | 521 KB |  |

## What LibRaw does with previews

- **It lists Canon CR3's full-size JPEG as 0 × 0.** Eleven CR3 bodies' largest JPEGs have no listed dimensions, so listed dimensions cannot rank a file's previews. The stored length can: in every one of the 117 files with a JPEG, the longest JPEG was the largest in pixels, and ranking by listed dimensions chose another in 17. `PreviewListing::largest_jpeg` ranks by length.
- **It calls what is not a JPEG a JPEG.** It lists Canon's H.265 previews, and any TIFF preview whose compression it does not name, as JPEGs, and it writes `FF D8` over the first two bytes of every JPEG it extracts, whatever they were. The crate therefore reads each listed JPEG's first stored bytes itself: the R5 Mark II's and R8's previews begin with Canon's H.265 header, and every other listed JPEG in the corpus begins with SOI.
- **It lists Canon CR2's third image, a 16-bit uncompressed RGB bitmap (430 × 284 to 624 × 416), as a Kodak thumbnail**, which it would decode through a small development of its own. The crate does not extract it; each CR2 also has a JPEG thumbnail and a full-size JPEG.
- **Many JPEGs have bytes after their last EOI marker**: every Canon CR3 body's full-size JPEG, the Leica CL's, Q2's and SL2's, and one DJI file's. Decoders stop at EOI, and all of them decode.
- **The largest preview is sometimes the only one.** OM System, Olympus and Panasonic files list a single JPEG (3200 × 2400 and 1920 × 1440 or 1280), so a grid cell comes from a scaled decode of it.
- **Most Nikon thumbnails are bitmaps**: a 160 × 120 uncompressed RGB image, 57,600 bytes, on every NEF body but the Z 8, Z 9, Z f and Z5II, and on the Pentax K-1 Mark II and KP and the Ricoh GR III and GR IIIx. The crate extracts them as RGB.

## Conclusions

- **The 100% focus check needs the development fallback** ([P6](../design/catalog.md#proposals)) for every Fujifilm, OM System, Olympus, Panasonic and DJI camera, the Sony A7 III, A7R II, A7R III, A7R IV, A7C, A9, a6000, a6100, a6400 and a6600, and the Canon EOS R5 Mark II and R8: 50 of the 103 cameras. Every Nikon, Pentax, Ricoh and Leica camera, every other Canon, and Sony's A1, A7 IV, A7C II, A7CR, A7R V, A7S III and a6700 carry a full-size JPEG it can read instead.
- **The loupe needs a development** only for the DJI drones, whose largest preview is 960 px wide, and the Canon EOS R5 Mark II and R8. Every other camera's largest JPEG is at least 1616 px wide.
- **A grid cell needs a development** only for the Canon EOS R5 Mark II and R8, whose files hold no JPEG: until an H.265 decoder is added, a card of either camera shows nothing until the preview lane develops it.
- **Identify's reads are small and near the start of the file**: 121 KB a file on average and 205 KB at most (the Nikon Z f), so listing the previews of a thousand files reads about 120 MB. A full-size preview is most of what the loupe reads.
