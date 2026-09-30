# Most-used RAW cameras

Which cameras, and which recording modes, most photographers' RAW files come from, so that RAW support and decoder work is weighed by the people it serves rather than by the owner's own bodies. Researched on 2026-09-29. Phones are excluded; premium compacts and drones were in scope but none ranked. The work it drives is [popular camera support](../design/popular-camera-support.md) and [RawSpeed unpacking](../design/rawspeed-unpack.md).

## Method

- **In use:** Flickr Camera Finder upload counts from September 2025 to August 2026, summed for about 190 non-phone bodies. Flickr leans towards hobbyists and high-volume event accounts, and so towards long-lived full-frame Canon and Sony bodies. Its counts are broken for some popular bodies (Canon 5D Mark IV, Nikon Z8, D850, Z6III) and missing for others (Sony A7 V, Nikon Z5II).
- **Being bought:** Map Camera's FY2025 and August 2026 new and used rankings, BCN's 2025 and first-half 2026 mirrorless top 10s, and the Amazon.com mirrorless best-seller list on 2026-09-29.
- **Score:** Flickr points on a log scale (top body 40) plus points for each sales-list placement (top 5 = 5, 6–10 = 4, 11–20 = 2, 21–30 = 1). The formula is simple and arbitrary; ranks within about 3 points are ties.
- **Defaults:** the maker's defaults table where one exists (Sony A7 IV, A7C II, A7 V; Nikon Z6II, Z8, Z9, Zf, Z6III). Others are inferred.
- **Samples:** CC0 files from [raw.pixls.us](https://raw.pixls.us/), SHA-256 checked, kept outside the repository. Some samples are unlabelled and their mode was inferred from size, then confirmed from the decoder LibRaw selects.

## The cameras

| # | Camera | Container, default RAW mode |
| --- | --- | --- |
| 1 | Canon EOS R6 Mark II | CR3; RAW or C-RAW |
| 2 | Sony A7 III | ARW; compressed |
| 3 | Sony A7 IV | ARW; compressed (verified) |
| 4 | Sony A7C II | ARW; compressed (verified) |
| 5 | Sony a6400 | ARW; compressed |
| 6 | Canon EOS R50 | CR3; RAW or C-RAW |
| 7 | Canon EOS R6 | CR3; RAW or C-RAW |
| 8 | Fujifilm X-T5 | RAF; uncompressed reported as default, lossless compressed common |
| 9 | Canon EOS R10 | CR3; RAW or C-RAW |
| 10 | Nikon Z50II | NEF; High Efficiency likely |
| 11 | Sony a6700 | ARW; compressed |
| 12 | Canon EOS R5 | CR3; RAW or C-RAW |
| 13 | Canon EOS R5 Mark II | CR3; RAW or C-RAW |
| 14 | Nikon Z f | NEF; High Efficiency★ (verified) |
| 15 | Nikon Z6II | NEF; lossless compressed 14-bit (verified) |
| 16 | Canon EOS R7 | CR3; RAW or C-RAW |
| 17 | Nikon D750 | NEF; lossless compressed |
| 18 | Sony ZV-E10 | ARW; compressed |
| 19 | Nikon Z9 | NEF; High Efficiency★ (verified) |
| 20 | Sony A7R V | ARW; compressed |

Alternates, next by score or excluded only by missing data: Canon 5D Mark III, 90D, 6D Mark II, 5D Mark IV, R3, R8; Sony A7 V; Nikon Z5II, Z8, D850, Z6III; Fujifilm X100VI, X-M5; Ricoh GR III; OM System OM-1; Panasonic S5II. The top drone, the DJI Mini 4 Pro, has about a twentieth of the X-T5's uploads and no public sample.

Upload share by decoder family: Canon CR3 38%, Sony ARW 23%, Nikon NEF on bodies without High Efficiency 15%, Canon CR2 11%, Fujifilm RAF 4%, Nikon NEF on High Efficiency bodies at least 3.4% (understated by the missing Z8, Z6III and Z5II data), ORF 2.7%, DNG 1.3%, RW2 1.1%, Pentax 0.4%.

## What the adapter opened

Of 52 samples, the adapter rejected 29 on 2026-09-30. Apart from crop-sensor sizes and the two unsupported formats below, the rejected files are ones LibRaw decodes and the catalog does not list:

- **Sony compressed ARW** on the A7 III, A7 IV, A7C II and A7R V, the default on each. The catalog listed only their uncompressed mode.
- **Fujifilm X-T5** uncompressed and lossy compressed. Only lossless compressed was listed.
- **Nikon D850** lossless and lossy; **Canon 5D Mark IV** single-frame RAW (only Dual Pixel was listed).
- **Bodies missing from the catalog:** Nikon Z6III, Z50II, Z5II; Sony ZV-E10; Fujifilm X-M5; Canon 90D, 5D Mark III.
- **Crop-sensor sizes** (APS-C crop on full-frame Sony and Canon bodies), which have a different sensor size from the catalogued one.

Two formats decode in neither LibRaw 0.22.2 nor RawSpeed:

- **Nikon High Efficiency (HE and HE★)** NEF, the factory default on the Z8, Z9, Zf and Z6III and likely on the Z50II and Z5II. The payload is a JPEG XS (ISO/IEC 21122) codestream from intoPIX's TicoRAW encoder with vendor header changes. darktable does not decode it either: up to 5.6.0 it shows the embedded JPEG in the lighttable and fails in the darkroom, and its maintainers point users to Adobe DNG Converter. An experimental open-source decoder exists as [dnglab pull request 835](https://github.com/dnglab/dnglab/pull/835) (Rawler, unmerged), which decoded all six High Efficiency samples plausibly in 290–670 ms; the dnglab maintainer expects LibRaw to publish its own decoder. The owner decided on 2026-09-30 not to support the format until upstream LibRaw does, and to reject it explicitly.
- **Sony A7 V compressed ARW**, a new compressed format and the A7 V's default.

LibRaw 0.22.2 detects High Efficiency data only on the Z8, Z9, Zf and Z6III. On the Z50II and Z5II samples it selects the ordinary lossless NEF decoder, logs "data corrupted" and returns a garbage mosaic without failing.

## Sources

- [Flickr Camera Finder](https://www.flickr.com/cameras)
- [Map Camera FY2025 ranking](https://news.mapcamera.com/maptimes/year2025ranking/) and [August 2026 ranking](https://news.mapcamera.com/maptimes/202608ranking/)
- BCN via [PetaPixel, 2026-01-12](https://petapixel.com/2026/01/12/aps-c-cameras-dominate-list-of-2025-best-sellers/) and [Digital Camera World, 2026](https://www.digitalcameraworld.com/cameras/mirrorless-cameras/so-far-japans-1-best-selling-camera-in-2026-is-a-3-year-old-aps-c-mirrorless-with-some-unusual-shooting-modes)
- [Amazon mirrorless best sellers](https://www.amazon.com/Best-Sellers-Electronics-Mirrorless-Cameras/zgbs/electronics/3109924011)
- Defaults: [Sony A7 IV](https://helpguide.sony.net/ilc/2110/v1/en/contents/TP1001803633.html), [Sony A7C II](https://helpguide.sony.net/ilc/2360/v1/en/contents/231h_default_value_list_shooting_ilc2360.html), [Sony A7 V](https://helpguide.sony.net/ilc/2540/v1/en/contents/251h_raw_file_type.html), [Nikon Z6II](https://onlinemanual.nikonimglib.com/z7II_z6II/en/09_menu_guide_01.html), [Nikon Z9](https://onlinemanual.nikonimglib.com/z9/en/15_menu_guide_01.html), [Nikon Zf](https://onlinemanual.nikonimglib.com/zf/en/psm_menu_items_and_initial_setting%20_112.html), [Nikon Z6III](https://onlinemanual.nikonimglib.com/z6III/en/psm_menu_items_and_initial_setting_123.html)
- High Efficiency: [darktable issue 15873](https://github.com/darktable-org/darktable/issues/15873), [darktable issue 19993](https://github.com/darktable-org/darktable/issues/19993), [darktable camera support](https://www.darktable.org/resources/camera-support/), [pixls.us discussion](https://discuss.pixls.us/t/nikon-z-series-high-efficiency-raw-support/50428), [dnglab pull request 835](https://github.com/dnglab/dnglab/pull/835)
- [raw.pixls.us repository index](https://raw.pixls.us/json/getrepository.php?set=all)
