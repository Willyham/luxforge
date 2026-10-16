//! `cargo xtask lr-align`: the Lightroom alignment rig's generator, measurement and fold
//! (`docs/design/lightroom-alignment.md`, "The rig", "Running a round" and "Changing a control").
//! `measure` is [`measure`]'s and `fold` [`fold`]'s; the rest of this module is the generator.
//!
//! `generate --round N --output NEW [--corpus CACHE] [--manifest RAW_MANIFEST] [--families ID,...]`
//! writes a round folder, or with `--families` the part of one those families make, for the owner
//! to import into a scratch Lightroom Classic catalog and export:
//!
//! - `targets/`: the synthetic JPEG targets the round uses, without XMP, the sources Luxforge
//!   renders its own side from.
//! - `lightroom/<subject>/`: one copy of the subject per variant, named for the variant, with the
//!   variant's settings as XMP: embedded in a JPEG copy of a target, in a sidecar beside a copy of a
//!   RAW photograph. This folder alone is imported into Lightroom.
//! - `manifest.json`: every target with its layout, every photograph with its hash, every family
//!   and every variant with the settings it writes, what Luxforge's preset reader makes of them,
//!   and what the round leaves out.
//! - `INSTRUCTIONS.txt`: the owner's import and export, with the round's counts.
//!
//! Every variant's XMP is read back through Luxforge's own preset reader before it is written
//! ([`readback`]). The round is deterministic: the same build, round and inputs write the same
//! bytes. The command writes only inside the new round folder, which must not exist and whose
//! parent must; RAW originals are only read, verified by their SHA-256 before and after, and a
//! failed run removes the folder it created.
mod fold;
mod measure;
mod photographs;
mod photos;
mod readback;
mod report;
mod rounds;
mod settings;
mod targets;
#[cfg(test)]
mod tests;
mod tone_study;

use crate::*;
use luxforge_core::ModuleRegistry;
use photos::{Excluded, Photo};
use rounds::{Family, NEUTRAL, Step, Subject};
use serde_json::Map;
use settings::{Field, Fields, Kind};

const FORMAT: u32 = 1;
const USAGE: &str = "lr-align generate --round 1|2 --output NEW [--corpus CACHE] [--manifest RAW_MANIFEST] [--families ID,...] | lr-align measure --round N (--folder ROUND_DIR | --exports DIR) --output NEW [--families ID,...] | lr-align fold --measured DIR [--measured DIR ...] --output NEW | lr-align tone-study --charts ROUND1_DIR --photographs ROUND1B_DIR --output NEW [--reduce N] [--candidates ID,...] [--fields ID,...|none] [--pairs ROUND2_DIR] [--cache DIR]";
/// The folder inside the round that the owner imports, and the one the exports go to.
const LIGHTROOM: &str = "lightroom";
const EXPORTS: &str = "exports";
/// Bytes per pixel of a 16-bit RGB TIFF, for the export estimate.
const TIFF_BYTES_PER_PIXEL: u64 = 6;

pub fn run(root: &Path, mut a: Args) -> Result {
    let op = a.0.first().cloned().ok_or(USAGE)?;
    a.0.remove(0);
    if op == "measure" {
        return measure::run(root, a);
    }
    if op == "fold" {
        return fold::run(root, a);
    }
    if op == "tone-study" {
        return tone_study::run(root, a);
    }
    ensure(op == "generate", USAGE)?;
    let round: u32 = a
        .value("--round")?
        .ok_or("Required: --round")?
        .to_string_lossy()
        .parse()
        .map_err(|_| "--round takes a round number")?;
    let out = absolute(root, &a.path("--output")?);
    let cache = a.value("--corpus")?.map(|p| absolute(root, Path::new(&p)));
    let supplied = a
        .value("--manifest")?
        .map(|p| absolute(root, Path::new(&p)));
    let only: Option<Vec<String>> = a
        .value("--families")?
        .map(|v| v.to_string_lossy().split(',').map(str::to_owned).collect());
    a.done()?;
    let mut families =
        rounds::round(round).ok_or(format!("lr-align defines rounds 1 and 2, not {round}"))?;
    // A part of a round: only the named families, each with every variant it has, for a session
    // that re-exports what changed or adds what an earlier session could not.
    if let Some(only) = &only {
        let unknown: Vec<&String> = only
            .iter()
            .filter(|id| !families.iter().any(|family| &family.id == *id))
            .collect();
        ensure(
            unknown.is_empty(),
            format!("round {round} has no families {unknown:?}"),
        )?;
        families.retain(|family| only.contains(&family.id));
    }
    let mut photos = Vec::new();
    let mut excluded = Vec::new();
    if let Some(cache) = &cache {
        let cache = fs::canonicalize(cache)?;
        let parent = out.parent().ok_or("the output has no parent")?;
        ensure(
            !fs::canonicalize(parent)?.starts_with(&cache),
            "the round folder may not be inside the corpus cache",
        )?;
        let corpus = read_json(&root.join(photos::CORPUS_MANIFEST))?;
        photos.extend(photos::corpus(&corpus, &cache)?);
    }
    if let Some(manifest) = &supplied {
        let (own, left_out) = photos::supplied(manifest)?;
        photos.extend(own);
        excluded.extend(left_out);
    }
    generate(
        &Plan {
            round,
            families,
            photos,
            excluded,
        },
        &out,
    )
}

/// A round to write: its families and the photographs its photograph families copy.
pub struct Plan {
    pub round: u32,
    pub families: Vec<Family>,
    pub photos: Vec<Photo>,
    /// Supplied photographs the round leaves out, and why.
    pub excluded: Vec<Excluded>,
}

/// Write `plan`'s round folder at `out`, which must not exist and whose parent must. A failure
/// removes the folder.
pub fn generate(plan: &Plan, out: &Path) -> Result {
    ensure(!out.exists(), format!("{} already exists", out.display()))?;
    fs::create_dir(out).map_err(|error| format!("{}: {error}", out.display()))?;
    let written = write_round(plan, out);
    if written.is_err() {
        // Only the folder this run created, never anything beside it.
        fs::remove_dir_all(out)?;
    }
    written
}

/// A JPEG of `target`'s pixels at quality 100 with no chroma subsampling, and no XMP.
fn encode(target: &targets::Target) -> Result<Vec<u8>> {
    let mut encoded = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut encoded, 100).encode(
        &target.rgb,
        target.width,
        target.height,
        image::ExtendedColorType::Rgb8,
    )?;
    Ok(encoded)
}

/// `jpeg` with `xmp` as its XMP packet: an APP1 segment after SOI and any JFIF APP0 segments.
fn splice(jpeg: &[u8], xmp: &str) -> Result<Vec<u8>> {
    ensure(jpeg.starts_with(&[0xff, 0xd8]), "not a JPEG")?;
    let mut at = 2;
    while jpeg.get(at..at + 2) == Some(&[0xff, 0xe0]) {
        let length = jpeg
            .get(at + 2..at + 4)
            .map(|b| usize::from(u16::from_be_bytes([b[0], b[1]])))
            .ok_or("a truncated APP0 segment")?;
        at += 2 + length;
    }
    let mut payload = b"http://ns.adobe.com/xap/1.0/\0".to_vec();
    payload.extend_from_slice(xmp.as_bytes());
    let length = u16::try_from(payload.len() + 2).map_err(|_| "XMP packet too long")?;
    let mut out = Vec::with_capacity(jpeg.len() + payload.len() + 4);
    out.extend_from_slice(&jpeg[..at]);
    out.extend_from_slice(&[0xff, 0xe1]);
    out.extend_from_slice(&length.to_be_bytes());
    out.extend_from_slice(&payload);
    out.extend_from_slice(&jpeg[at..]);
    Ok(out)
}

/// The XMP packet a JPEG holds, `None` when it holds none.
#[cfg(test)]
fn embedded_xmp(jpeg: &[u8]) -> Option<String> {
    const ID: &[u8] = b"http://ns.adobe.com/xap/1.0/\0";
    let mut at = 2;
    while jpeg.get(at) == Some(&0xff) {
        let marker = *jpeg.get(at + 1)?;
        let length = usize::from(u16::from_be_bytes([*jpeg.get(at + 2)?, *jpeg.get(at + 3)?]));
        let body = jpeg.get(at + 4..at + 2 + length)?;
        if marker == 0xe1 && body.starts_with(ID) {
            return String::from_utf8(body[ID.len()..].to_vec()).ok();
        }
        if marker == 0xda {
            return None;
        }
        at += 2 + length;
    }
    None
}

/// The variant every subject has: the baseline alone.
fn neutral() -> Family {
    Family {
        id: NEUTRAL.into(),
        area: "baseline",
        subjects: Vec::new(),
        base: Vec::new(),
        steps: vec![Step {
            label: String::new(),
            fields: Vec::new(),
        }],
    }
}

fn variant_id(subject: &str, family: &str, label: &str) -> String {
    if family == NEUTRAL {
        format!("{subject}__{NEUTRAL}")
    } else {
        format!("{subject}__{family}__{label}")
    }
}

/// `baseline` with the family's base and the step's settings written over it.
fn compose(baseline: &Fields, family: &Family, step: &Step) -> Fields {
    let mut fields = baseline.clone();
    for (name, field) in family.base.iter().chain(&step.fields) {
        fields.insert(name, field.clone());
    }
    fields
}

fn field_json(field: &Field) -> Value {
    match field {
        Field::Correction(correction) => {
            let mut value = match correction.mask {
                settings::Mask::Linear { zero, full } => {
                    json!({"kind": "linear", "zero": zero, "full": full})
                }
                settings::Mask::Radial {
                    left,
                    top,
                    right,
                    bottom,
                    angle,
                    feather,
                } => json!({
                    "kind": "radial", "left": left, "top": top, "right": right,
                    "bottom": bottom, "angle": angle, "feather": feather,
                    "intended": "inside",
                }),
            };
            value["local_exposure"] = json!(correction.exposure);
            value
        }
        Field::Curve(points) => json!(points),
        other => json!(other.text().unwrap_or_default()),
    }
}

fn fields_json<'a>(fields: impl IntoIterator<Item = &'a (&'static str, Field)>) -> Value {
    Value::Object(
        fields
            .into_iter()
            .map(|(name, field)| ((*name).to_owned(), field_json(field)))
            .collect(),
    )
}

/// A family's entry: its subjects, base, labels and the Luxforge controls its settings are
/// compared with.
fn family_json(family: &Family) -> Value {
    let mut varied: Vec<&str> = Vec::new();
    for step in &family.steps {
        for (name, _) in &step.fields {
            if !varied.contains(name) {
                varied.push(name);
            }
        }
    }
    let luxforge: Vec<Value> = varied
        .iter()
        .map(|name| match readback::counterpart(name) {
            Some((action, field, how)) => {
                json!({"lightroom": name, "action": action, "field": field, "how": how.name()})
            }
            None if name.ends_with("BasedCorrections") => json!({
                "lightroom": name, "how": readback::How::NotMapped.name(),
                "mask": if name.starts_with("Circular") { "radial" } else { "linear" },
            }),
            None => json!({"lightroom": name, "how": "held: no Luxforge counterpart"}),
        })
        .collect();
    let subjects: Vec<Value> = family
        .subjects
        .iter()
        .map(|subject| match subject {
            Subject::Target(id) => json!(id),
            Subject::Photographs => json!("photographs"),
        })
        .collect();
    json!({
        "id": family.id, "area": family.area, "subjects": subjects,
        "base": fields_json(&family.base),
        "labels": family.steps.iter().map(|step| step.label.clone()).collect::<Vec<_>>(),
        "luxforge": luxforge,
    })
}

/// What one variant's file is a copy of.
enum Source<'a> {
    Jpeg(&'a [u8]),
    Raw(&'a Photo),
}

/// Write one subject's variants into `lightroom/<subject>/`, returning their manifest entries.
#[allow(clippy::too_many_arguments)]
fn write_subject(
    out: &Path,
    subject: &str,
    kind: Kind,
    source: Source<'_>,
    families: &[&Family],
    baseline: &(Fields, Map<String, Value>),
    registry: &ModuleRegistry,
    variants: &mut Vec<Value>,
) -> Result {
    let folder = format!("{LIGHTROOM}/{subject}");
    fs::create_dir(out.join(&folder))?;
    for family in families {
        for step in &family.steps {
            let id = variant_id(subject, &family.id, &step.label);
            let fields = compose(&baseline.0, family, step);
            let xmp = settings::xmp(&fields, kind == Kind::Jpeg);
            let read = readback::read_back(&fields, &xmp, registry)
                .map_err(|error| format!("{id}: {error}"))?;
            let (file, sidecar) = match source {
                Source::Jpeg(jpeg) => {
                    let file = format!("{folder}/{id}.jpg");
                    fs::write(out.join(&file), splice(jpeg, &xmp)?)?;
                    (file, Value::Null)
                }
                Source::Raw(photo) => {
                    let file = format!("{folder}/{id}.{}", photo.extension);
                    let copied = fs::copy(&photo.path, out.join(&file))?;
                    ensure(
                        copied == photo.bytes,
                        format!(
                            "{id}: the copy is {copied} bytes, the original {}",
                            photo.bytes
                        ),
                    )?;
                    let sidecar = format!("{folder}/{id}.xmp");
                    fs::write(out.join(&sidecar), &xmp)?;
                    (file, json!(sidecar))
                }
            };
            let written: Vec<(&'static str, Field)> =
                family.base.iter().chain(&step.fields).cloned().collect();
            let mut entry = json!({
                "id": id, "subject": subject, "family": family.id, "label": step.label,
                "file": file, "sidecar": sidecar,
                "settings": fields_json(&written),
                "luxforge": readback::changes(&read.settings, &baseline.1),
                "unsupported": read.unsupported,
            });
            if !read.refused.is_empty() {
                // A range shortfall: the importer refuses this Lightroom pair, which Lightroom
                // still renders and the round still measures.
                entry["refused"] = json!(read.refused);
            }
            variants.push(entry);
        }
    }
    Ok(())
}

fn write_round(plan: &Plan, out: &Path) -> Result {
    let registry = ModuleRegistry::builtin();
    for photo in &plan.photos {
        photos::verify(photo)?;
    }
    let neutral = neutral();
    let mut target_ids: Vec<&'static str> = Vec::new();
    let mut on_photographs = false;
    for family in &plan.families {
        for subject in &family.subjects {
            match subject {
                Subject::Target(id) if !target_ids.contains(id) => target_ids.push(id),
                Subject::Target(_) => {}
                Subject::Photographs => on_photographs = true,
            }
        }
    }
    let photos: &[Photo] = if on_photographs { &plan.photos } else { &[] };
    let mut not_included: Vec<Value> = rounds::NOT_IN_ROUND
        .iter()
        .map(|(what, why)| json!({"what": what, "why": why}))
        .collect();
    if on_photographs && photos.is_empty() {
        not_included.push(json!({
            "what": "RAW photographs",
            "why": "no --corpus cache or --manifest of supplied RAW files was given, so the round's photograph families have no subject",
        }));
    }
    for (id, why) in &plan.excluded {
        not_included.push(json!({"what": id, "why": why}));
    }
    // Each kind's baseline and what the reader makes of it.
    let mut baselines = Map::new();
    let mut reader_baselines = Map::new();
    let mut kinds = Vec::new();
    for (name, kind) in [("jpeg", Kind::Jpeg), ("raw", Kind::Raw)] {
        if kind == Kind::Raw && photos.is_empty() {
            continue;
        }
        let fields = settings::baseline(kind);
        let read = readback::read_back(&fields, &settings::xmp(&fields, false), &registry)
            .map_err(|error| format!("the {name} baseline: {error}"))?;
        ensure(
            read.unsupported.is_empty(),
            format!("the {name} baseline is not neutral: {:?}", read.unsupported),
        )?;
        let entries: Vec<(&'static str, Field)> = fields.clone().into_iter().collect();
        baselines.insert(name.into(), fields_json(&entries));
        reader_baselines.insert(name.into(), Value::Object(read.settings.clone()));
        kinds.push((kind, (fields, read.settings)));
    }
    let baseline_of = |kind: Kind| {
        kinds
            .iter()
            .find(|(k, _)| *k == kind)
            .map(|(_, baseline)| baseline)
            .ok_or("no baseline for the kind")
    };
    fs::create_dir(out.join("targets"))?;
    fs::create_dir(out.join(LIGHTROOM))?;
    let mut variants = Vec::new();
    let mut targets_json = Vec::new();
    let mut export_bytes: u64 = 0;
    // Targets in the module's order, whatever order the families name them in.
    for id in targets::TARGETS.iter().filter(|id| target_ids.contains(id)) {
        let target = targets::target(id).ok_or(format!("no target {id}"))?;
        let id: &'static str = target.id;
        let jpeg = encode(&target)?;
        let file = format!("targets/{id}.jpg");
        fs::write(out.join(&file), &jpeg)?;
        targets_json.push(json!({
            "id": id, "file": file, "width": target.width, "height": target.height,
            "sha256": format!("{:x}", Sha256::digest(&jpeg)),
            "pixels_sha256": format!("{:x}", Sha256::digest(&target.rgb)),
            "layout": target.layout,
        }));
        let families: Vec<&Family> = std::iter::once(&neutral)
            .chain(
                plan.families
                    .iter()
                    .filter(|family| family.subjects.contains(&Subject::Target(id))),
            )
            .collect();
        let before = variants.len();
        write_subject(
            out,
            id,
            Kind::Jpeg,
            Source::Jpeg(&jpeg),
            &families,
            baseline_of(Kind::Jpeg)?,
            &registry,
            &mut variants,
        )?;
        export_bytes += (variants.len() - before) as u64
            * u64::from(target.width)
            * u64::from(target.height)
            * TIFF_BYTES_PER_PIXEL;
    }
    let photograph_families: Vec<&Family> = std::iter::once(&neutral)
        .chain(
            plan.families
                .iter()
                .filter(|family| family.subjects.contains(&Subject::Photographs)),
        )
        .collect();
    let mut photographs_json = Vec::new();
    for photo in photos {
        let before = variants.len();
        write_subject(
            out,
            &photo.id,
            Kind::Raw,
            Source::Raw(photo),
            &photograph_families,
            baseline_of(Kind::Raw)?,
            &registry,
            &mut variants,
        )?;
        let [width, height] = photo.dimensions;
        export_bytes += (variants.len() - before) as u64
            * u64::from(width)
            * u64::from(height)
            * TIFF_BYTES_PER_PIXEL;
        photographs_json.push(json!({
            "id": photo.id, "origin": photo.origin, "camera": photo.camera, "why": photo.why,
            "extension": photo.extension, "sha256": photo.sha256, "bytes": photo.bytes,
            "dimensions": photo.dimensions,
        }));
    }
    // The originals, read again: the round changed none of them.
    for photo in photos {
        photos::verify(photo)?;
    }
    let mut families_json: Vec<Value> = vec![family_json(&neutral)];
    families_json.extend(plan.families.iter().map(family_json));
    let raw_variants = variants
        .iter()
        .filter(|variant| !variant["sidecar"].is_null())
        .count();
    let summary = Summary {
        round: plan.round,
        variants: variants.len(),
        raw_variants,
        export_bytes,
    };
    fs::write(out.join("INSTRUCTIONS.txt"), instructions(&summary))?;
    write_json(
        &out.join("manifest.json"),
        &json!({
            "format": FORMAT,
            "tool": "cargo xtask lr-align generate",
            "round": plan.round,
            "kind": if plan.round == 1 { "single settings across each range" } else { "interacting pairs" },
            "instructions": "INSTRUCTIONS.txt",
            "lightroom_folder": LIGHTROOM,
            "exports_folder": EXPORTS,
            "export": {
                "format": "TIFF", "compression": "None", "colour_space": "sRGB",
                "bit_depth": 16, "size": "full, no resizing", "output_sharpening": "none",
                "metadata": "Copyright Only", "file_name": "<variant id>.tif",
            },
            "metadata": settings::METADATA.iter().map(|(name, value)| (name.to_string(), json!(value))).collect::<Map<_, _>>(),
            "scope": "Responses are measured against each editor's own neutral variant of the same subject. Targets are untagged sRGB JPEGs at quality 100 without chroma subsampling. Each variant writes the whole baseline with its own settings over it, so it means the same in any catalog. 'luxforge' is what Luxforge's preset reader maps, as changes from the baseline's mapping; 'unsupported' names what the importer does not map, which the families' 'luxforge' entries name a control for.",
            "baseline": baselines,
            "reader_baseline": reader_baselines,
            "targets": targets_json,
            "photographs": photographs_json,
            "not_included": not_included,
            "families": families_json,
            "estimate": {"variants": variants.len(), "raw_variants": raw_variants, "export_bytes": export_bytes},
            "variants": variants,
        }),
    )?;
    println!(
        "wrote {} variants ({} RAW) to {}; Lightroom's export needs about {:.1} GB",
        summary.variants,
        summary.raw_variants,
        out.display(),
        summary.export_bytes as f64 / 1e9
    );
    Ok(())
}

struct Summary {
    round: u32,
    variants: usize,
    raw_variants: usize,
    export_bytes: u64,
}

/// The owner's steps for a round, with its counts.
fn instructions(summary: &Summary) -> String {
    let Summary {
        round,
        variants,
        raw_variants,
        export_bytes,
    } = summary;
    let gigabytes = *export_bytes as f64 / 1e9;
    format!(
        "Luxforge Lightroom alignment, round {round}

This folder holds {variants} variants ({raw_variants} of them RAW copies with XMP sidecars), each a
copy of a test picture with one setting (or pair of settings) written into it as Lightroom XMP.
Import the `{LIGHTROOM}` folder into a scratch Lightroom Classic catalog, export every photograph
as an uncompressed 16-bit sRGB TIFF into `{EXPORTS}`, and keep this folder whole: manifest.json
says what every file is. Nothing here touches your own photographs or catalogs.

1. Catalog. File > New Catalog, somewhere outside your usual catalogs, for example beside this
   folder. In Catalog Settings > Metadata, leave \"Automatically write changes into XMP\" off.

2. Import. File > Import Photos and Video. Source: the `{LIGHTROOM}` folder inside this folder,
   with Include Subfolders on. Choose Add (not Copy or Move). File Handling: Build Previews
   Minimal, Build Smart Previews off, \"Don't Import Suspected Duplicates\" off. Apply During
   Import: Develop Settings None, Metadata None, no keywords. Import. Lightroom should report
   {variants} photographs. Each carries its settings; do not edit, sync, auto-tone or reset any.

3. Check one. Open any variant in Develop: in the Calibration panel, Process should read
   Version 5. If it reads anything else, note it in step 5.

4. Export. In Library, select All Photographs, then select all. File > Export, with:
   - Export To: Hard Drive. Export Location: Specific folder, this folder; Put in Subfolder
     `{EXPORTS}`. Existing Files: Ask what to do.
   - File Naming: Rename To off, so each file keeps its variant's name.
   - File Settings: Image Format TIFF, Compression None, Color Space sRGB, Bit Depth
     16 bits/component.
   - Image Sizing: Resize to Fit off. Output Sharpening: off.
   - Metadata: Copyright Only. Watermark: off. Post-Processing: Do nothing.
   Save these settings as an export preset (\"Luxforge alignment\") for later rounds. Export.
   Expect {variants} TIFF files named <variant>.tif, about {gigabytes:.1} GB in all.

5. Record. Write the Lightroom Classic version (Help > System Info) and anything that did not
   go as described into `{EXPORTS}/NOTES.txt`.

The scratch catalog can be deleted afterwards. The measurement of the exports is the next part of
the rig and is not part of this folder.
"
    )
}
