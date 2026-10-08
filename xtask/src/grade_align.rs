//! `cargo xtask grade-align`: the colour-grading slice of the Lightroom alignment rig
//! (`docs/design/lightroom-alignment.md`, "The rig"; `docs/design/colour-grading.md`, "Final
//! reference analysis and Lightroom refinement").
//!
//! - `generate --output NEW` writes a round folder: a synthetic sRGB patch target, one JPEG copy of
//!   it per variant with that variant's grading settings embedded as Lightroom XMP, and a manifest.
//!   Every variant's XMP is read back through Luxforge's own preset reader and must map to exactly
//!   the manifest's `set-mixer` fields. Only the round's own copies carry XMP.
//! - `render --round DIR --output NEW` writes Luxforge's own render of every variant as
//!   `<variant>.png`, laid out as the owner's exports are: measured as `--lightroom`, it must give
//!   identity fits with no residual, the tooling's end-to-end self-test.
//! - `measure --round DIR --output NEW [--lightroom DIR]` renders Luxforge's responses through the
//!   whole-frame reference renderer on a dense value grid, reads the owner's Lightroom exports of
//!   the round when a folder is given (`<variant>.tif`, `.tiff`, `.png` or `.jpg`), fits each
//!   setting with `luxforge_reference::grade_response`, and writes the figures, a summary and a
//!   contact sheet. Without exports the Lightroom figures are recorded as unmeasured; nothing is
//!   inferred from names or ranges.
//!
//! Luxforge's side reads the reference renderer's 8-bit display frame and averages each patch's
//! interior, which is the precision it reports: patch means of 8-bit codes, not a 16-bit terminal
//! conversion.
use crate::*;
use luxforge_core::{Layer, MIXER_EFFECT, ModuleRegistry, RECIPE_FORMAT, Recipe, inspect_preset};
use luxforge_reference::grade_response::{self, FitPoint, Response, Sampled};
use luxforge_reference::{colour, srgb};

const FORMAT: u32 = 1;
/// The side of one patch, in pixels, and the margin each patch's measurement leaves at its edges
/// so neither JPEG chroma subsampling nor resampling at a patch boundary reaches the mean.
const PATCH: u32 = 48;
const INSET: u32 = 10;
const COLUMNS: u32 = 12;

const WHEELS: [(&str, &str, &str, &str); 4] = [
    (
        "shadows",
        "SplitToningShadowHue",
        "SplitToningShadowSaturation",
        "ColorGradeShadowLum",
    ),
    (
        "midtones",
        "ColorGradeMidtoneHue",
        "ColorGradeMidtoneSat",
        "ColorGradeMidtoneLum",
    ),
    (
        "highlights",
        "SplitToningHighlightHue",
        "SplitToningHighlightSaturation",
        "ColorGradeHighlightLum",
    ),
    (
        "global",
        "ColorGradeGlobalHue",
        "ColorGradeGlobalSat",
        "ColorGradeGlobalLum",
    ),
];

/// The patches' linear sRGB colours: a 24-step grey wedge, then an Oklab lightness × hue × chroma
/// grid, clipped into the sRGB gamut so the target is a real encoded picture.
fn patches() -> Vec<[f64; 3]> {
    let mut patches: Vec<[f64; 3]> = (0..24)
        .map(|step| [srgb::decode((f64::from(step) * 255.0 / 23.0).round() as u8); 3])
        .collect();
    for l in [0.35, 0.55, 0.75] {
        for chroma in [0.04, 0.09] {
            for step in 0..12 {
                let angle = f64::from(step) * 30f64.to_radians();
                let rgb = colour::from_oklab(colour::Oklab {
                    l,
                    a: chroma * angle.cos(),
                    b: chroma * angle.sin(),
                });
                patches.push(rgb.map(|v| v.clamp(0.0, 1.0)));
            }
        }
    }
    patches
}

fn layout(count: usize) -> (u32, u32) {
    let rows = (count as u32).div_ceil(COLUMNS);
    (COLUMNS * PATCH, rows * PATCH)
}

fn patch_origin(index: usize) -> (u32, u32) {
    let index = index as u32;
    ((index % COLUMNS) * PATCH, (index / COLUMNS) * PATCH)
}

/// One variant: its identity, the family it is fitted in and its value there, and its full grading
/// state as Lightroom fields and as the `set-mixer` fields they map to.
struct Variant {
    id: String,
    family: String,
    value: f64,
    /// `[hue, saturation, luminance]` per wheel, then Blending and Balance.
    wheels: [[f64; 3]; 4],
    blending: f64,
    balance: f64,
}

impl Variant {
    fn new(id: String, family: String, value: f64) -> Self {
        Self {
            id,
            family,
            value,
            wheels: [[0.0; 3]; 4],
            blending: 50.0,
            balance: 0.0,
        }
    }

    fn lightroom(&self) -> Vec<(String, f64)> {
        let mut fields = Vec::new();
        for (index, (_, hue, saturation, luminance)) in WHEELS.iter().enumerate() {
            let [h, s, l] = self.wheels[index];
            fields.push(((*hue).to_owned(), h));
            fields.push(((*saturation).to_owned(), s));
            fields.push(((*luminance).to_owned(), l));
        }
        fields.push(("ColorGradeBlending".into(), self.blending));
        fields.push(("SplitToningBalance".into(), self.balance));
        fields
    }

    fn luxforge(&self) -> Value {
        let mut fields = serde_json::Map::new();
        for (index, (wheel, ..)) in WHEELS.iter().enumerate() {
            let [h, s, l] = self.wheels[index];
            fields.insert(format!("grade-{wheel}-hue"), json!(h));
            fields.insert(format!("grade-{wheel}-saturation"), json!(s));
            fields.insert(format!("grade-{wheel}-luminance"), json!(l));
        }
        fields.insert("grade-blending".into(), json!(self.blending));
        fields.insert("grade-balance".into(), json!(self.balance));
        Value::Object(fields)
    }

    fn xmp(&self) -> String {
        let attributes: Vec<String> = self
            .lightroom()
            .iter()
            .map(|(name, value)| format!("crs:{name}=\"{value}\""))
            .collect();
        format!(
            "<?xpacket begin=\"\u{feff}\" id=\"W5M0MpCehiHzreSzNTczkc9d\"?>\n<x:xmpmeta \
             xmlns:x=\"adobe:ns:meta/\"><rdf:RDF \
             xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\"><rdf:Description \
             rdf:about=\"\" xmlns:crs=\"http://ns.adobe.com/camera-raw-settings/1.0/\" \
             crs:Version=\"15.4\" crs:ProcessVersion=\"11.0\" crs:HasSettings=\"True\" {}/>\
             </rdf:RDF></x:xmpmeta>\n<?xpacket end=\"w\"?>",
            attributes.join(" ")
        )
    }
}

/// The values a magnitude family is sampled at in the round, and Luxforge's denser grid.
const SATURATIONS: [f64; 6] = [0.0, 10.0, 25.0, 50.0, 75.0, 100.0];
const LUMINANCES: [f64; 7] = [-100.0, -50.0, -25.0, 0.0, 25.0, 50.0, 100.0];
const OVERLAPS: [f64; 5] = [0.0, 25.0, 50.0, 75.0, 100.0];
const BALANCES: [f64; 5] = [-100.0, -50.0, 0.0, 50.0, 100.0];
/// The hue a saturation family tints toward, and the saturation a hue family is measured at.
const SATURATION_HUE: f64 = 30.0;
const HUE_SATURATION: f64 = 50.0;

/// Every family's variants at `values`; Luxforge's own samples use the same constructor on a
/// denser grid.
fn family(family: &str, value: f64) -> Variant {
    let mut variant = Variant::new(format!("{family}-{value}"), family.to_owned(), value);
    let (wheel, kind) = family.split_once('-').unwrap_or((family, ""));
    let index = WHEELS.iter().position(|(name, ..)| *name == wheel);
    match (index, kind) {
        (Some(index), "saturation") => variant.wheels[index] = [SATURATION_HUE, value, 0.0],
        (Some(index), "luminance") => variant.wheels[index] = [0.0, 0.0, value],
        (Some(index), "hue") => variant.wheels[index] = [value, HUE_SATURATION, 0.0],
        _ => {
            // Blending and Balance act on a split tone: shadows warm, highlights cool.
            variant.wheels[0] = [30.0, 60.0, 0.0];
            variant.wheels[2] = [210.0, 60.0, 0.0];
            if family == "blending" {
                variant.blending = value;
            } else {
                variant.balance = value;
            }
        }
    }
    variant
}

fn families() -> Vec<(String, Vec<f64>, Vec<f64>, bool)> {
    let grid = |low: f64, high: f64, step: f64| -> Vec<f64> {
        let count = ((high - low) / step).round() as usize;
        (0..=count).map(|i| low + i as f64 * step).collect()
    };
    let mut families = Vec::new();
    for (wheel, ..) in WHEELS {
        families.push((
            format!("{wheel}-saturation"),
            SATURATIONS.to_vec(),
            grid(0.0, 100.0, 5.0),
            false,
        ));
        families.push((
            format!("{wheel}-luminance"),
            LUMINANCES.to_vec(),
            grid(-100.0, 100.0, 5.0),
            false,
        ));
        families.push((
            format!("{wheel}-hue"),
            grid(0.0, 330.0, 30.0),
            grid(0.0, 355.0, 5.0),
            true,
        ));
    }
    families.push((
        "blending".into(),
        OVERLAPS.to_vec(),
        grid(0.0, 100.0, 5.0),
        false,
    ));
    families.push((
        "balance".into(),
        BALANCES.to_vec(),
        grid(-100.0, 100.0, 5.0),
        false,
    ));
    families
}

/// Whether a reader's `set-mixer` fields are the manifest's, comparing numbers by value: a
/// written `30` reads back as the integer 30, the manifest's `30.0`.
fn same_fields(read: Option<&Value>, expected: &Value) -> bool {
    let numbers = |value: &Value| -> Option<Vec<(String, f64)>> {
        let mut fields: Vec<(String, f64)> = value
            .as_object()?
            .iter()
            .map(|(name, value)| Some((name.clone(), value.as_f64()?)))
            .collect::<Option<_>>()?;
        fields.sort_by(|a, b| a.0.cmp(&b.0));
        Some(fields)
    };
    read.and_then(numbers).is_some() && read.and_then(numbers) == numbers(expected)
}

/// A JPEG of `rgba` with `xmp` as its XMP packet (an APP1 segment after SOI), or none.
fn jpeg(width: u32, height: u32, rgb: &[u8], xmp: Option<&str>) -> Result<Vec<u8>> {
    let mut encoded = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut encoded, 100).encode(
        rgb,
        width,
        height,
        image::ExtendedColorType::Rgb8,
    )?;
    let Some(xmp) = xmp else {
        return Ok(encoded);
    };
    let mut payload = b"http://ns.adobe.com/xap/1.0/\0".to_vec();
    payload.extend_from_slice(xmp.as_bytes());
    let length = u16::try_from(payload.len() + 2).map_err(|_| "XMP packet too long")?;
    let mut out = encoded[..2].to_vec();
    out.extend_from_slice(&[0xff, 0xe1]);
    out.extend_from_slice(&length.to_be_bytes());
    out.extend_from_slice(&payload);
    out.extend_from_slice(&encoded[2..]);
    Ok(out)
}

fn target_rgb() -> (u32, u32, Vec<u8>) {
    let colours = patches();
    let (width, height) = layout(colours.len());
    let mut rgb = vec![128u8; (width * height * 3) as usize];
    for (index, colour) in colours.iter().enumerate() {
        let (x0, y0) = patch_origin(index);
        let code = colour.map(srgb::code);
        for y in y0..y0 + PATCH {
            for x in x0..x0 + PATCH {
                let at = ((y * width + x) * 3) as usize;
                rgb[at..at + 3].copy_from_slice(&code);
            }
        }
    }
    (width, height, rgb)
}

pub fn run(root: &Path, mut a: Args) -> Result {
    let op =
        a.0.first()
            .cloned()
            .ok_or("grade-align needs generate or measure")?;
    a.0.remove(0);
    match op.to_str() {
        Some("generate") => {
            let out = absolute(root, &a.path("--output")?);
            a.done()?;
            generate(&out)
        }
        Some("render") => {
            let round = absolute(root, &a.path("--round")?);
            let out = absolute(root, &a.path("--output")?);
            a.done()?;
            render_round(&round, &out)
        }
        Some("measure") => {
            let round = absolute(root, &a.path("--round")?);
            let out = absolute(root, &a.path("--output")?);
            let lightroom = a.value("--lightroom")?.map(|p| absolute(root, Path::new(&p)));
            a.done()?;
            measure(&round, &out, lightroom.as_deref())
        }
        _ => Err("grade-align generate --output NEW | measure --round DIR --output NEW [--lightroom DIR]".into()),
    }
}

fn generate(out: &Path) -> Result {
    ensure(!out.exists(), format!("{} already exists", out.display()))?;
    fs::create_dir_all(out.join("variants"))?;
    let (width, height, rgb) = target_rgb();
    fs::write(out.join("target.jpg"), jpeg(width, height, &rgb, None)?)?;
    let registry = ModuleRegistry::builtin();
    let mut variants = Vec::new();
    for (name, values, _, _) in families() {
        for value in values {
            let variant = family(&name, value);
            let xmp = variant.xmp();
            // The reader must map the XMP to exactly the fields the manifest records.
            let read = inspect_preset(&xmp, None, &registry)?;
            ensure(
                same_fields(read.settings.get("set-mixer"), &variant.luxforge())
                    && read.settings.len() == 1
                    && read.report.refused.is_empty(),
                format!("{}: the preset reader maps {:?}", variant.id, read.settings),
            )?;
            let file = format!("variants/{}.jpg", variant.id);
            fs::write(out.join(&file), jpeg(width, height, &rgb, Some(&xmp))?)?;
            variants.push(json!({
                "id": variant.id, "family": variant.family, "value": variant.value, "file": file,
                "lightroom": variant.lightroom().into_iter().map(|(n, v)| (n, json!(v))).collect::<serde_json::Map<_, _>>(),
                "luxforge": {"set-mixer": variant.luxforge()},
            }));
        }
    }
    write_json(
        &out.join("manifest.json"),
        &json!({
            "format": FORMAT,
            "scope": "Colour grading alignment round: import variants/ into a scratch Lightroom Classic catalog with read metadata from files, select all and export each as 16-bit sRGB TIFF with compression None, full size, no sharpening or metadata, named <variant>.tif, into one folder; then run grade-align measure --lightroom on it. Responses are measured against each editor's own neutral variant.",
            "target": {"file": "target.jpg", "width": width, "height": height, "patch": PATCH, "inset": INSET, "columns": COLUMNS, "patches": patches().len()},
            "variants": variants,
        }),
    )?;
    println!("wrote {} variants to {}", variants.len(), out.display());
    Ok(())
}

/// Each patch's interior mean, linear sRGB, of an encoded RGB frame.
fn patch_means(width: u32, rgb: impl Fn(u32, u32) -> [f64; 3], count: usize) -> Vec<[f64; 3]> {
    (0..count)
        .map(|index| {
            let (x0, y0) = patch_origin(index);
            grade_response::patch_mean((y0 + INSET..y0 + PATCH - INSET).flat_map(|y| {
                let rgb = &rgb;
                (x0 + INSET..x0 + PATCH - INSET).map(move |x| rgb(x, y))
            }))
        })
        .inspect(|_| debug_assert!(width > 0))
        .collect()
}

/// The Luxforge render of a variant's recipe over the round's target, as an 8-bit RGBA raster.
fn luxforge_raster(
    source: &luxforge_core::SourceImage,
    variant: &Variant,
) -> Result<luxforge_core::Raster> {
    let recipe = Recipe {
        format: RECIPE_FORMAT,
        layers: vec![Layer::new(MIXER_EFFECT, variant.luxforge())],
        ..Recipe::default()
    };
    crate::basic_acceptance::render(source, &recipe)
}

/// `render`: Luxforge's own render of every round variant, written as `<variant>.png` into a new
/// folder laid out as the owner's Lightroom exports are. Measured as `--lightroom`, it must give
/// identity fits with no residual: the tooling's end-to-end self-test.
fn render_round(round: &Path, out: &Path) -> Result {
    ensure(!out.exists(), format!("{} already exists", out.display()))?;
    fs::create_dir_all(out)?;
    let manifest = read_json(&round.join("manifest.json"))?;
    let source = luxforge_core::open_source(&round.join("target.jpg"))?;
    let mut written = 0;
    for entry in manifest["variants"].as_array().ok_or("no variants")? {
        let family_name = entry["family"].as_str().ok_or("a family")?;
        let value = entry["value"].as_f64().ok_or("a value")?;
        let variant = family(family_name, value);
        ensure(
            entry["id"] == json!(variant.id),
            format!(
                "the manifest's {} is not this command's variant",
                entry["id"]
            ),
        )?;
        let raster = luxforge_raster(&source, &variant)?;
        let rgb: Vec<u8> = raster
            .rgba
            .chunks_exact(4)
            .flat_map(|pixel| [pixel[0], pixel[1], pixel[2]])
            .collect();
        image::RgbImage::from_raw(raster.width, raster.height, rgb)
            .ok_or("a whole frame")?
            .save(out.join(format!("{}.png", variant.id)))?;
        written += 1;
    }
    println!("wrote {written} Luxforge renders to {}", out.display());
    Ok(())
}

fn luxforge_means(source: &luxforge_core::SourceImage, variant: &Variant) -> Result<Vec<[f64; 3]>> {
    let raster = luxforge_raster(source, variant)?;
    let width = raster.width;
    Ok(patch_means(
        width,
        |x, y| {
            let at = ((y * width + x) * 4) as usize;
            [0, 1, 2].map(|c| srgb::decode(raster.rgba[at + c]))
        },
        patches().len(),
    ))
}

fn lightroom_means(folder: &Path, id: &str) -> Result<Option<Vec<[f64; 3]>>> {
    let Some(path) = ["tif", "tiff", "png", "jpg"]
        .iter()
        .map(|extension| folder.join(format!("{id}.{extension}")))
        .find(|path| path.exists())
    else {
        return Ok(None);
    };
    let is_tiff = path
        .extension()
        .is_some_and(|extension| extension == "tif" || extension == "tiff");
    let (width, pixels) = if is_tiff {
        let tiff =
            read_tiff(&fs::read(&path)?).map_err(|error| format!("{}: {error}", path.display()))?;
        (tiff.width, tiff.encoded)
    } else {
        let decoded = image::open(&path)?.to_rgb32f();
        let pixels = decoded
            .pixels()
            .map(|pixel| pixel.0.map(f64::from))
            .collect();
        (decoded.width(), pixels)
    };
    Ok(Some(patch_means(
        width,
        |x, y| pixels[(y * width + x) as usize].map(srgb::decode_encoded),
        patches().len(),
    )))
}

/// An uncompressed baseline RGB TIFF's pixels, as encoded values in `[0, 1]`.
#[derive(Debug)]
struct Tiff {
    width: u32,
    encoded: Vec<[f64; 3]>,
}

/// Read an uncompressed (compression 1), chunky (planar configuration 1), 8- or 16-bit RGB TIFF
/// of either byte order: what Lightroom writes with compression None. Anything else is refused by
/// name rather than decoded approximately; the reader holds at most one image of its own size.
fn read_tiff(bytes: &[u8]) -> std::result::Result<Tiff, String> {
    let little = match bytes.get(..4) {
        Some([0x49, 0x49, 42, 0]) => true,
        Some([0x4d, 0x4d, 0, 42]) => false,
        _ => return Err("not a classic TIFF".into()),
    };
    let u16_at = |at: usize| -> std::result::Result<u32, String> {
        let pair: [u8; 2] = bytes
            .get(at..at + 2)
            .and_then(|b| b.try_into().ok())
            .ok_or("truncated")?;
        Ok(u32::from(if little {
            u16::from_le_bytes(pair)
        } else {
            u16::from_be_bytes(pair)
        }))
    };
    let u32_at = |at: usize| -> std::result::Result<u32, String> {
        let quad: [u8; 4] = bytes
            .get(at..at + 4)
            .and_then(|b| b.try_into().ok())
            .ok_or("truncated")?;
        Ok(if little {
            u32::from_le_bytes(quad)
        } else {
            u32::from_be_bytes(quad)
        })
    };
    let directory = u32_at(4)? as usize;
    let entries = u16_at(directory)? as usize;
    let mut tags = std::collections::HashMap::new();
    for entry in 0..entries {
        let at = directory + 2 + entry * 12;
        let (tag, kind, count) = (u16_at(at)?, u16_at(at + 2)?, u32_at(at + 4)? as usize);
        let size = match kind {
            3 => 2,
            4 => 4,
            _ => 1,
        };
        let values_at = if size * count <= 4 {
            at + 8
        } else {
            u32_at(at + 8)? as usize
        };
        let values: Vec<u32> = (0..count.min(1 << 20))
            .map(|i| match kind {
                3 => u16_at(values_at + 2 * i),
                4 => u32_at(values_at + 4 * i),
                _ => Ok(u32::from(*bytes.get(values_at + i).unwrap_or(&0))),
            })
            .collect::<std::result::Result<_, _>>()?;
        tags.insert(tag, values);
    }
    let one = |tag: u32, default: Option<u32>| -> std::result::Result<u32, String> {
        tags.get(&tag)
            .and_then(|values| values.first().copied())
            .or(default)
            .ok_or(format!("TIFF tag {tag} is missing"))
    };
    let (width, height) = (one(256, None)?, one(257, None)?);
    if one(259, Some(1))? != 1 {
        return Err("compressed TIFF: export with compression None".into());
    }
    if one(284, Some(1))? != 1 {
        return Err("planar TIFF: only chunky RGB is read".into());
    }
    if one(277, None)? < 3 || one(262, None)? != 2 {
        return Err("not an RGB TIFF".into());
    }
    let samples = one(277, None)? as usize;
    let bits = one(258, None)?;
    if bits != 8 && bits != 16 {
        return Err(format!("{bits}-bit TIFF: only 8 and 16 bits are read"));
    }
    let bytes_per = (bits / 8) as usize;
    let offsets = tags.get(&273).ok_or("no strip offsets")?;
    let counts = tags.get(&279).ok_or("no strip byte counts")?;
    let mut data = Vec::with_capacity(width as usize * height as usize * samples * bytes_per);
    for (offset, count) in offsets.iter().zip(counts) {
        let strip = bytes
            .get(*offset as usize..(*offset + *count) as usize)
            .ok_or("a strip lies outside the file")?;
        data.extend_from_slice(strip);
    }
    let pixel_bytes = samples * bytes_per;
    if data.len() < width as usize * height as usize * pixel_bytes {
        return Err("the strips hold fewer pixels than the image".into());
    }
    let encoded = data
        .chunks_exact(pixel_bytes)
        .take(width as usize * height as usize)
        .map(|pixel| {
            std::array::from_fn(|channel| {
                let at = channel * bytes_per;
                if bytes_per == 1 {
                    f64::from(pixel[at]) / 255.0
                } else {
                    let pair = [pixel[at], pixel[at + 1]];
                    let value = if little {
                        u16::from_le_bytes(pair)
                    } else {
                        u16::from_be_bytes(pair)
                    };
                    f64::from(value) / 65535.0
                }
            })
        })
        .collect();
    Ok(Tiff { width, encoded })
}

fn responses(neutral: &[[f64; 3]], adjusted: &[[f64; 3]]) -> Vec<Response> {
    neutral
        .iter()
        .zip(adjusted)
        .map(|(n, a)| grade_response::response(*n, *a))
        .collect()
}

fn fit_json(points: &[FitPoint]) -> Value {
    json!(points
        .iter()
        .map(|p| json!({"lightroom": p.lightroom, "luxforge": p.luxforge, "span": p.span, "residual": p.residual, "shortfall": p.shortfall}))
        .collect::<Vec<_>>())
}

fn measure(round: &Path, out: &Path, lightroom: Option<&Path>) -> Result {
    ensure(!out.exists(), format!("{} already exists", out.display()))?;
    let manifest = read_json(&round.join("manifest.json"))?;
    ensure(
        manifest["format"] == json!(FORMAT),
        "the round manifest is not this command's format",
    )?;
    fs::create_dir_all(out)?;
    let source = luxforge_core::open_source(&round.join("target.jpg"))?;
    let count = patches().len();
    let neutral_variant = Variant::new("neutral".into(), "neutral".into(), 0.0);
    let luxforge_neutral = luxforge_means(&source, &neutral_variant)?;
    // Every grading field at its default: the round's own neutral variant.
    let lightroom_neutral = match lightroom {
        Some(folder) => lightroom_means(folder, &family("midtones-saturation", 0.0).id)?,
        None => None,
    };
    let mut results = Vec::new();
    let mut sheet: Vec<(String, Vec<[f64; 3]>)> = Vec::new();
    for (name, round_values, dense, hue) in families() {
        let mut samples = Vec::new();
        for value in &dense {
            let means = luxforge_means(&source, &family(&name, *value))?;
            if round_values.contains(value) {
                sheet.push((format!("{name} {value}"), means.clone()));
            }
            samples.push((*value, responses(&luxforge_neutral, &means)));
        }
        let luxforge_sampled = Sampled {
            neutrals: luxforge_neutral.clone(),
            samples,
        };
        // The response's size per value: Luxforge's mean and largest CIEDE2000 from its own
        // neutral over the round's patches.
        let strength: Vec<Value> = luxforge_sampled
            .samples
            .iter()
            .map(|(value, responses)| {
                let differences: Vec<f64> = responses
                    .iter()
                    .zip(&luxforge_neutral)
                    .map(|(r, n)| grade_response::response_difference(*n, Response::default(), *r))
                    .collect();
                let mean = differences.iter().sum::<f64>() / differences.len().max(1) as f64;
                let largest = differences.iter().copied().fold(0.0, f64::max);
                json!({"value": value, "mean_de2000": mean, "max_de2000": largest})
            })
            .collect();
        let mut lightroom_fit = Value::Null;
        let mut missing = Vec::new();
        if let (Some(folder), Some(neutral)) = (lightroom, &lightroom_neutral) {
            let mut samples = Vec::new();
            for value in &round_values {
                let id = family(&name, *value).id;
                match lightroom_means(folder, &id)? {
                    Some(means) => samples.push((*value, responses(neutral, &means))),
                    None => missing.push(id),
                }
            }
            if !samples.is_empty() {
                let measured = Sampled {
                    neutrals: neutral.clone(),
                    samples,
                };
                let points = if hue {
                    grade_response::fit_hue(&luxforge_sampled, &measured)
                } else {
                    grade_response::fit_monotone(&luxforge_sampled, &measured)
                };
                // The alignment programme's C threshold: a median CIEDE2000 above 2 at Lightroom
                // values of ±25 and beyond, after the best value; hue residuals are degrees.
                let candidate = !hue
                    && points
                        .iter()
                        .any(|p| p.lightroom.abs() >= 25.0 && p.residual > 2.0);
                lightroom_fit =
                    json!({"points": fit_json(&points), "behaviour_candidate": candidate});
            }
        }
        results.push(json!({
            "family": name, "hue": hue, "luxforge_strength": strength,
            "lightroom": if lightroom.is_none() { json!("unmeasured: no Lightroom exports were given") } else { lightroom_fit },
            "missing_exports": missing,
        }));
    }
    contact_sheet(&out.join("contact-sheet.png"), &sheet, count)?;
    write_json(
        &out.join("grade-align.json"),
        &json!({
            "round": round, "lightroom": lightroom,
            "status": if lightroom_neutral.is_some() { "measured" } else { "lightroom-unmeasured" },
            "scope": "Luxforge: the whole-frame reference renderer's 8-bit frame, patch-interior means, responses relative to its own neutral render. Lightroom: the owner's exports of the same round, relative to the neutral variant. Fits: luxforge_reference::grade_response, median CIEDE2000 residuals (hue: degrees). No figure is claimed for anything not measured.",
            "families": results,
        }),
    )?;
    println!(
        "wrote {} ({})",
        out.join("grade-align.json").display(),
        if lightroom_neutral.is_some() {
            "measured"
        } else {
            "Lightroom unmeasured"
        }
    );
    Ok(())
}

/// A sheet of every round variant's Luxforge patches, one row per variant, for review.
fn contact_sheet(path: &Path, rows: &[(String, Vec<[f64; 3]>)], count: usize) -> Result {
    const CELL: u32 = 8;
    let width = count as u32 * CELL;
    let height = rows.len() as u32 * CELL;
    let mut sheet = image::RgbImage::new(width.max(1), height.max(1));
    for (row, (_, means)) in rows.iter().enumerate() {
        for (column, mean) in means.iter().enumerate() {
            let code = image::Rgb(mean.map(srgb::code));
            for y in 0..CELL {
                for x in 0..CELL {
                    sheet.put_pixel(column as u32 * CELL + x, row as u32 * CELL + y, code);
                }
            }
        }
    }
    sheet.save(path)?;
    let labels: Vec<&str> = rows.iter().map(|(label, _)| label.as_str()).collect();
    fs::write(path.with_extension("txt"), labels.join("\n") + "\n")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every generated variant's XMP reads back to its manifest fields, and the families cover
    /// every wheel's saturation, luminance and hue, Blending and Balance.
    #[test]
    fn every_variant_round_trips_through_the_preset_reader() {
        let registry = ModuleRegistry::builtin();
        let families = families();
        assert_eq!(families.len(), 14);
        for (name, values, dense, _) in families {
            for value in &values {
                assert!(
                    dense.contains(value),
                    "{name} {value} is not on Luxforge's grid"
                );
                let variant = family(&name, *value);
                let read = inspect_preset(&variant.xmp(), None, &registry).unwrap();
                assert!(
                    same_fields(read.settings.get("set-mixer"), &variant.luxforge()),
                    "{name}: {:?}",
                    read.settings
                );
                assert!(read.report.refused.is_empty());
            }
        }
    }

    /// A 16-bit big-endian uncompressed RGB TIFF reads back exactly; a compressed one is refused.
    #[test]
    fn an_uncompressed_tiff_reads_and_a_compressed_one_is_refused() {
        let (width, height) = (2u32, 1u32);
        let pixels: [[u16; 3]; 2] = [[0, 32768, 65535], [100, 200, 300]];
        let mut data = Vec::new();
        for pixel in pixels {
            for value in pixel {
                data.extend_from_slice(&value.to_be_bytes());
            }
        }
        let tiff = |compression: u16| {
            let mut bytes = vec![0x4d, 0x4d, 0, 42, 0, 0, 0, 8];
            let entries: [(u16, u16, u32, u32); 9] = [
                (256, 3, 1, u32::from(width as u16) << 16),
                (257, 3, 1, u32::from(height as u16) << 16),
                (258, 3, 1, 16 << 16),
                (259, 3, 1, u32::from(compression) << 16),
                (262, 3, 1, 2 << 16),
                (273, 4, 1, 8 + 2 + 9 * 12 + 4),
                (277, 3, 1, 3 << 16),
                (279, 4, 1, data.len() as u32),
                (284, 3, 1, 1 << 16),
            ];
            bytes.extend_from_slice(&(entries.len() as u16).to_be_bytes());
            for (tag, kind, count, value) in entries {
                bytes.extend_from_slice(&tag.to_be_bytes());
                bytes.extend_from_slice(&kind.to_be_bytes());
                bytes.extend_from_slice(&count.to_be_bytes());
                bytes.extend_from_slice(&value.to_be_bytes());
            }
            bytes.extend_from_slice(&[0; 4]);
            bytes.extend_from_slice(&data);
            bytes
        };
        let read = read_tiff(&tiff(1)).unwrap();
        assert_eq!(read.width, 2);
        assert_eq!(read.encoded[0], [0.0, 32768.0 / 65535.0, 1.0]);
        assert_eq!(read.encoded[1][2], 300.0 / 65535.0);
        assert!(
            read_tiff(&tiff(8))
                .unwrap_err()
                .contains("compression None")
        );
        assert!(read_tiff(b"not a tiff").is_err());
    }

    /// The XMP segment sits right after SOI and the file still decodes to the target.
    #[test]
    fn a_variant_jpeg_carries_its_xmp_and_decodes() {
        let (width, height, rgb) = target_rgb();
        let bytes = jpeg(width, height, &rgb, Some("<x/>")).unwrap();
        assert_eq!(&bytes[..4], &[0xff, 0xd8, 0xff, 0xe1]);
        let decoded = image::load_from_memory(&bytes).unwrap().to_rgb8();
        assert_eq!((decoded.width(), decoded.height()), (width, height));
        let (x0, y0) = patch_origin(30);
        let pixel = decoded.get_pixel(x0 + PATCH / 2, y0 + PATCH / 2).0;
        let expected = patches()[30].map(srgb::code);
        for channel in 0..3 {
            assert!(pixel[channel].abs_diff(expected[channel]) <= 2);
        }
    }
}
