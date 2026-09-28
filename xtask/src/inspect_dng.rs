//! `inspect-dng`: a bounded, read-only DNG/TIFF metadata inspector for adding a camera. It walks
//! IFD0, its SubIFDs and the next-IFD chain, and reports every entry with its value and SHA-256,
//! each IFD's geometry and colour-calibration tags, and the layout of each opcode list's
//! operations. It never decodes pixels or applies corrections: unknown or malformed metadata is
//! reported in `warnings` rather than given a default.
use crate::*;
use serde_json::Map;
use std::{
    collections::{BTreeMap, HashSet},
    io::Write,
};

const MAX_INPUT: u64 = 512 * 1024 * 1024;
const MAX_IFDS: usize = 32;
const MAX_PAYLOAD: u64 = 1024 * 1024;

fn type_size(typ: u16) -> Option<u64> {
    Some(match typ {
        1 | 2 | 6 | 7 | 17 => 1,
        3 | 8 => 2,
        4 | 9 | 11 | 13 => 4,
        5 | 10 | 12 | 16 | 18 => 8,
        _ => return None,
    })
}

fn type_name(typ: u16) -> String {
    match typ {
        1 => "BYTE",
        2 => "ASCII",
        3 => "SHORT",
        4 => "LONG",
        5 => "RATIONAL",
        6 => "SBYTE",
        7 => "UNDEFINED",
        8 => "SSHORT",
        9 => "SLONG",
        10 => "SRATIONAL",
        11 => "FLOAT",
        12 => "DOUBLE",
        13 => "IFD",
        16 => "LONG8",
        17 => "SLONG8",
        18 => "IFD8",
        _ => return format!("UNKNOWN-{typ}"),
    }
    .into()
}

fn tag_name(tag: u16) -> Option<&'static str> {
    Some(match tag {
        254 => "NewSubfileType",
        256 => "ImageWidth",
        257 => "ImageLength",
        258 => "BitsPerSample",
        259 => "Compression",
        262 => "PhotometricInterpretation",
        273 => "StripOffsets",
        277 => "SamplesPerPixel",
        278 => "RowsPerStrip",
        279 => "StripByteCounts",
        284 => "PlanarConfiguration",
        322 => "TileWidth",
        323 => "TileLength",
        324 => "TileOffsets",
        325 => "TileByteCounts",
        330 => "SubIFDs",
        33421 => "CFARepeatPatternDim",
        33422 => "CFAPattern",
        50706 => "DNGVersion",
        50708 => "UniqueCameraModel",
        50718 => "DefaultScale",
        50719 => "DefaultCropOrigin",
        50720 => "DefaultCropSize",
        50721 => "ColorMatrix1",
        50722 => "ColorMatrix2",
        50723 => "CameraCalibration1",
        50724 => "CameraCalibration2",
        50725 => "ReductionMatrix1",
        50726 => "ReductionMatrix2",
        50727 => "AnalogBalance",
        50728 => "AsShotNeutral",
        50729 => "AsShotWhiteXY",
        50734 => "LinearizationTable",
        50778 => "CalibrationIlluminant1",
        50779 => "CalibrationIlluminant2",
        50780 => "BestQualityScale",
        50829 => "ActiveArea",
        50964 => "NoiseProfile",
        51008 => "OpcodeList1",
        51009 => "OpcodeList2",
        51022 => "OpcodeList3",
        _ => return None,
    })
}

/// The tags reported under each IFD's `geometry`, in order.
const GEOMETRY_TAGS: [u16; 11] = [
    256, 257, 258, 259, 262, 277, 284, 33421, 33422, 50706, 50708,
];
/// The tags reported under each IFD's `color_calibration`.
const COLOR_TAGS: [u16; 14] = [
    50708, 50718, 50719, 50720, 50721, 50722, 50723, 50724, 50725, 50726, 50727, 50728, 50778,
    50779,
];
/// The tags a sensor IFD needs before its geometry and colour are known.
const REQUIRED: [u16; 6] = [50829, 50719, 50720, 50721, 50722, 50728];

fn sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

/// The walker over one file's bytes.
struct Inspector<'a> {
    data: &'a [u8],
    little: bool,
    warnings: Vec<String>,
    ifds: Vec<Value>,
    seen: HashSet<u64>,
}

impl<'a> Inspector<'a> {
    fn warn(&mut self, message: String) {
        if !self.warnings.contains(&message) {
            self.warnings.push(message);
        }
    }

    fn need(&self, offset: u64, size: u64, label: &str) -> Result<&'a [u8]> {
        let len = self.data.len() as u64;
        ensure(
            offset <= len && size <= len - offset,
            format!("{label} exceeds input bounds"),
        )?;
        Ok(&self.data[offset as usize..(offset + size) as usize])
    }

    fn u16_at(&self, offset: u64) -> Result<u16> {
        let b: [u8; 2] = self.need(offset, 2, "value")?.try_into()?;
        Ok(if self.little {
            u16::from_le_bytes(b)
        } else {
            u16::from_be_bytes(b)
        })
    }

    fn u32_at(&self, offset: u64) -> Result<u32> {
        let b: [u8; 4] = self.need(offset, 4, "value")?.try_into()?;
        Ok(if self.little {
            u32::from_le_bytes(b)
        } else {
            u32::from_be_bytes(b)
        })
    }

    /// An entry's value: the text of an ASCII field up to its first NUL, a byte list for byte
    /// types, numerator/denominator pairs for rationals, otherwise the numbers, a single one
    /// unwrapped.
    fn values(&mut self, typ: u16, count: u64, raw: &[u8]) -> Value {
        if typ == 2 {
            let text = raw.split(|b| *b == 0).next().unwrap_or_default();
            return json!(
                text.iter()
                    .map(|&b| if b.is_ascii() {
                        char::from(b)
                    } else {
                        char::REPLACEMENT_CHARACTER
                    })
                    .collect::<String>()
            );
        }
        if matches!(typ, 1 | 6 | 7 | 17) {
            return json!(raw);
        }
        let unit = match typ {
            3 | 8 => 2,
            4 | 9 | 11 | 13 => 4,
            5 | 10 | 12 | 16 | 18 => 8,
            _ => {
                self.warn(format!("unknown TIFF type {typ}; raw value retained"));
                return json!({"hex": hex(raw)});
            }
        };
        if count * unit > raw.len() as u64 {
            self.warn("short TIFF value; raw value retained".into());
            return json!({"hex": hex(raw)});
        }
        let little = self.little;
        let word = |b: &[u8]| -> u64 {
            let mut bytes = [0; 8];
            if little {
                bytes[..b.len()].copy_from_slice(b);
                u64::from_le_bytes(bytes)
            } else {
                bytes[8 - b.len()..].copy_from_slice(b);
                u64::from_be_bytes(bytes)
            }
        };
        let out: Vec<Value> = raw
            .chunks_exact(unit as usize)
            .take(count as usize)
            .map(|b| match typ {
                3 | 4 | 13 | 16 | 18 => json!(word(b)),
                8 => json!(word(b) as u16 as i16),
                9 => json!(word(b) as u32 as i32),
                5 => json!([word(&b[..4]), word(&b[4..])]),
                10 => json!([word(&b[..4]) as u32 as i32, word(&b[4..]) as u32 as i32]),
                11 => json!(f64::from(f32::from_bits(word(b) as u32))),
                _ => json!(f64::from_bits(word(b))),
            })
            .collect();
        if count == 1 {
            out.into_iter().next().unwrap_or(Value::Null)
        } else {
            Value::Array(out)
        }
    }

    fn entry(&mut self, offset: u64) -> Result<Value> {
        let tag = self.u16_at(offset)?;
        let typ = self.u16_at(offset + 2)?;
        let count = u64::from(self.u32_at(offset + 4)?);
        let unit = type_size(typ).unwrap_or_else(|| {
            self.warn(format!("tag {tag} uses unknown TIFF type {typ}"));
            1
        });
        let total = unit * count;
        ensure(
            total <= MAX_PAYLOAD,
            format!("tag {tag} payload exceeds {MAX_PAYLOAD} bytes"),
        )?;
        let (raw, value_offset) = if total <= 4 {
            (
                &self.data[(offset + 8) as usize..(offset + 8 + total) as usize],
                None,
            )
        } else {
            let at = u64::from(self.u32_at(offset + 8)?);
            (
                self.need(at, total, &format!("tag {tag} payload"))?,
                Some(at),
            )
        };
        let values = self.values(typ, count, raw);
        let mut item = json!({
            "tag": tag,
            "name": tag_name(tag).map_or_else(|| format!("Unknown-{tag}"), str::to_owned),
            "type": type_name(typ),
            "count": count,
            "sha256": sha256(raw),
            "values": values,
        });
        if let Some(at) = value_offset {
            item["offset"] = json!(at);
        }
        if tag_name(tag).is_none() {
            self.warn(format!("unknown tag {tag} present in IFD"));
        }
        if matches!(tag, 51008 | 51009 | 51022) {
            item["opcode_list"] = self.opcodes(raw, tag);
        }
        Ok(item)
    }

    /// An opcode list's operations, whose fields are big-endian whatever the file's byte order.
    fn opcodes(&mut self, raw: &[u8], tag: u16) -> Value {
        if raw.len() < 4 {
            self.warn(format!("OpcodeList{tag} is truncated"));
            return json!({"count": null, "operations": []});
        }
        let be = |at: usize| u32::from_be_bytes(raw[at..at + 4].try_into().unwrap());
        let count = be(0);
        if count > 32 {
            self.warn(format!("OpcodeList{tag} count {count} exceeds 32"));
            return json!({"count": count, "operations": []});
        }
        let (mut pos, mut operations) = (4usize, Vec::new());
        for index in 0..count {
            if pos + 16 > raw.len() {
                self.warn(format!("OpcodeList{tag} header {index} exceeds payload"));
                break;
            }
            let (id, version, flags, byte_count) =
                (be(pos), be(pos + 4), be(pos + 8), be(pos + 12));
            let (start, end) = (pos + 16, pos + 16 + byte_count as usize);
            if u64::from(byte_count) > MAX_PAYLOAD || end > raw.len() {
                self.warn(format!(
                    "OpcodeList{tag} opcode {index} payload is out of bounds"
                ));
                break;
            }
            let payload = &raw[start..end];
            let mut op = json!({"index": index, "id": id, "version": version, "flags": flags,
                "byte_count": byte_count, "payload_sha256": sha256(payload)});
            let layout = match id {
                9 => gain_map(payload),
                1 => warp(payload),
                _ => {
                    op["layout"] = json!("unknown opcode payload; correction not interpreted");
                    self.warn(format!("unsupported opcode id {id} in OpcodeList{tag}"));
                    Ok(Map::new())
                }
            };
            match layout {
                Ok(fields) => op.as_object_mut().unwrap().extend(fields),
                Err(error) => self.warn(format!(
                    "opcode {id} in OpcodeList{tag} layout unknown: {error}"
                )),
            }
            operations.push(op);
            pos = end;
        }
        json!({"count": count, "operations": operations})
    }

    fn walk(&mut self, offset: u64, role: &str) -> Result {
        if self.ifds.len() >= MAX_IFDS {
            self.warn(format!("IFD traversal capped at {MAX_IFDS}"));
            return Ok(());
        }
        if !self.seen.insert(offset) {
            return Ok(());
        }
        self.need(offset, 2, "IFD entry count")?;
        let count = u64::from(self.u16_at(offset)?);
        ensure(count <= 4096, "IFD entry count exceeds bound")?;
        self.need(offset + 2, count * 12 + 4, "IFD entries")?;
        let entries = (0..count)
            .map(|i| self.entry(offset + 2 + 12 * i))
            .collect::<Result<Vec<_>>>()?;
        let next = u64::from(self.u32_at(offset + 2 + count * 12)?);
        let by_tag: BTreeMap<u64, &Value> = entries
            .iter()
            .map(|entry| (entry["tag"].as_u64().unwrap(), entry))
            .collect();
        let mut info = json!({"offset": offset, "role": role, "entries": entries});
        let pick = |tags: &[u16]| -> Map<String, Value> {
            tags.iter()
                .filter_map(|&tag| {
                    let entry = by_tag.get(&u64::from(tag))?;
                    Some((tag_name(tag).unwrap().to_owned(), entry["values"].clone()))
                })
                .collect()
        };
        let (geometry, color) = (pick(&GEOMETRY_TAGS), pick(&COLOR_TAGS));
        let children = by_tag.get(&330).map(|sub| match &sub["values"] {
            Value::Array(children) => children.clone(),
            one => vec![one.clone()],
        });
        if !geometry.is_empty() {
            info["geometry"] = Value::Object(geometry);
        }
        if !color.is_empty() {
            info["color_calibration"] = Value::Object(color);
        }
        self.ifds.push(info);
        for child in children.unwrap_or_default() {
            let child = child.as_u64().ok_or("SubIFDs value is not an offset")?;
            self.walk(child, "SubIFD")?;
        }
        if next != 0 {
            self.walk(next, "next")?;
        }
        Ok(())
    }

    fn inspect(mut self) -> Result<Value> {
        ensure(self.data.len() >= 8, "input is shorter than TIFF header")?;
        self.little = match &self.data[..2] {
            b"II" => true,
            b"MM" => false,
            _ => return Err("not a TIFF/DNG byte-order marker".into()),
        };
        let magic = self.u16_at(2)?;
        if magic != 42 {
            self.warn(format!("unexpected TIFF magic {magic}"));
        }
        let root = u64::from(self.u32_at(4)?);
        self.walk(root, "root")?;
        let mut missing = Vec::new();
        for ifd in &self.ifds {
            if ifd["geometry"]["PhotometricInterpretation"] != 32803 {
                continue;
            }
            let present: HashSet<u64> = ifd["entries"]
                .as_array()
                .unwrap()
                .iter()
                .filter_map(|entry| entry["tag"].as_u64())
                .collect();
            for tag in REQUIRED {
                if !present.contains(&u64::from(tag)) {
                    missing.push(format!(
                        "sensor IFD at {} lacks {}; value remains unknown",
                        ifd["offset"],
                        tag_name(tag).unwrap()
                    ));
                }
            }
        }
        for message in missing {
            self.warn(message);
        }
        Ok(json!({
            "format": "TIFF/DNG",
            "byte_order": if self.little { "little" } else { "big" },
            "input_bytes": self.data.len(),
            "ifd_count": self.ifds.len(),
            "ifds": self.ifds,
            "warnings": self.warnings,
        }))
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn be_u32s<const N: usize>(payload: &[u8], at: usize) -> [u32; N] {
    std::array::from_fn(|i| {
        u32::from_be_bytes(payload[at + 4 * i..at + 4 * i + 4].try_into().unwrap())
    })
}

fn be_f64s<const N: usize>(payload: &[u8], at: usize) -> [f64; N] {
    std::array::from_fn(|i| {
        f64::from_be_bytes(payload[at + 8 * i..at + 8 * i + 8].try_into().unwrap())
    })
}

/// A GainMap's (opcode 9) area, grid and gain range.
fn gain_map(payload: &[u8]) -> Result<Map<String, Value>> {
    ensure(payload.len() >= 76, "GainMap header is truncated")?;
    let area: [u32; 8] = be_u32s(payload, 0);
    let points: [u32; 2] = be_u32s(payload, 32);
    let [spacing_v, spacing_h, origin_v, origin_h] = be_f64s(payload, 40);
    let [planes] = be_u32s(payload, 72);
    let cells = u64::from(points[0]) * u64::from(points[1]) * u64::from(planes);
    ensure(
        cells <= MAX_PAYLOAD / 4 && 76 + cells * 4 <= payload.len() as u64,
        "GainMap grid exceeds bounded payload",
    )?;
    let gains = payload[76..76 + 4 * cells as usize]
        .chunks_exact(4)
        .map(|b| f64::from(f32::from_be_bytes(b.try_into().unwrap())));
    // The first of equals, and the first value kept, as a comparison-only minimum would.
    let (min, max) = gains
        .fold(None, |range: Option<(f64, f64)>, g| {
            Some(range.map_or((g, g), |(lo, hi)| {
                (if g < lo { g } else { lo }, if g > hi { g } else { hi })
            }))
        })
        .ok_or("GainMap grid is empty")?;
    Ok(Map::from_iter([
        ("layout".into(), json!("GainMap")),
        ("area".into(), json!(area)),
        ("points".into(), json!(points)),
        ("spacing".into(), json!([spacing_v, spacing_h])),
        ("origin".into(), json!([origin_v, origin_h])),
        ("planes".into(), json!(planes)),
        ("gain_min".into(), json!(min)),
        ("gain_max".into(), json!(max)),
    ]))
}

/// A WarpRectilinear's (opcode 1) planes, coefficient hash, centre and whether every coefficient
/// is finite.
fn warp(payload: &[u8]) -> Result<Map<String, Value>> {
    ensure(payload.len() >= 4, "WarpRectilinear header is truncated")?;
    let [planes] = be_u32s(payload, 0);
    let planes = planes as usize;
    ensure(
        planes <= 64 && 4 + planes * 48 + 16 <= payload.len(),
        "WarpRectilinear coefficients exceed bounded payload",
    )?;
    let coefficients: Vec<[f64; 6]> = (0..planes).map(|i| be_f64s(payload, 4 + 48 * i)).collect();
    let center: [f64; 2] = be_f64s(payload, 4 + 48 * planes);
    Ok(Map::from_iter([
        ("layout".into(), json!("WarpRectilinear")),
        ("planes".into(), json!(planes)),
        (
            "coefficients_sha256".into(),
            json!(sha256(&payload[4..4 + planes * 48])),
        ),
        ("center".into(), json!(center)),
        (
            "coefficient_finite".into(),
            json!(coefficients.iter().flatten().all(|c| c.is_finite())),
        ),
    ]))
}

/// Inspect one DNG or TIFF file, at most 512 MiB, read-only.
pub fn inspect(path: &Path) -> Result<Value> {
    ensure(
        fs::metadata(path)?.len() <= MAX_INPUT,
        "input exceeds 512 MiB bound",
    )?;
    let data = fs::read(path)?;
    Inspector {
        data: &data,
        little: true,
        warnings: Vec::new(),
        ifds: Vec::new(),
        seen: HashSet::new(),
    }
    .inspect()
}

/// `cargo xtask inspect-dng --source FILE [--json NEW_FILE]`: print the report, or write it to a
/// new file.
pub fn run(source: &Path, out: Option<&Path>) -> Result {
    let text = format!("{}\n", serde_json::to_string_pretty(&inspect(source)?)?);
    match out {
        Some(out) => {
            // create_new: an existing report is never overwritten.
            fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(out)?
                .write_all(text.as_bytes())?;
        }
        None => print!("{text}"),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A little-endian TIFF: IFD0 with a SubIFD holding a sensor's PhotometricInterpretation, a
    /// CFA pattern, a rational DefaultCropSize and an OpcodeList3 whose GainMap is truncated and
    /// whose WarpRectilinear is whole.
    fn fixture() -> Vec<u8> {
        let mut warp = 1u32.to_be_bytes().to_vec();
        for value in [1.0f64, 0.0, 0.0, 0.0, 0.0, 0.0, 0.5, 0.5] {
            warp.extend(value.to_be_bytes());
        }
        let mut list = 2u32.to_be_bytes().to_vec();
        for (id, payload) in [(9u32, vec![0u8; 8]), (1, warp)] {
            for word in [id, 0x0103_0000, 0, payload.len() as u32] {
                list.extend(word.to_be_bytes());
            }
            list.extend(payload);
        }
        let mut file = b"II*\0".to_vec();
        file.extend(8u32.to_le_bytes());
        let entry = |file: &mut Vec<u8>, tag: u16, typ: u16, count: u32, value: u32| {
            file.extend(tag.to_le_bytes());
            file.extend(typ.to_le_bytes());
            file.extend(count.to_le_bytes());
            file.extend(value.to_le_bytes());
        };
        // IFD0 at 8: one entry, SubIFDs -> 26.
        file.extend(1u16.to_le_bytes());
        entry(&mut file, 330, 4, 1, 26);
        file.extend(0u32.to_le_bytes());
        // SubIFD at 26: four entries (54 bytes), then the rational at 80 and the list at 96.
        file.extend(4u16.to_le_bytes());
        entry(&mut file, 262, 3, 1, 32803);
        entry(&mut file, 33422, 1, 4, u32::from_le_bytes([0, 1, 1, 2]));
        entry(&mut file, 50720, 5, 2, 80);
        entry(&mut file, 51022, 7, list.len() as u32, 96);
        file.extend(0u32.to_le_bytes());
        for value in [5464u32, 1, 3640, 1] {
            file.extend(value.to_le_bytes());
        }
        file.extend(list);
        file
    }

    #[test]
    fn a_sensor_ifd_reports_its_geometry_opcodes_and_what_it_lacks() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("fixture.dng");
        fs::write(&path, fixture()).unwrap();
        let report = inspect(&path).unwrap();
        assert_eq!(report["byte_order"], "little");
        assert_eq!(report["ifd_count"], 2);
        let sub = &report["ifds"][1];
        assert_eq!(
            (sub["offset"].as_u64(), sub["role"].as_str()),
            (Some(26), Some("SubIFD"))
        );
        assert_eq!(sub["geometry"]["PhotometricInterpretation"], 32803);
        assert_eq!(sub["geometry"]["CFAPattern"], json!([0, 1, 1, 2]));
        assert_eq!(
            sub["color_calibration"]["DefaultCropSize"],
            json!([[5464, 1], [3640, 1]])
        );
        let list = &sub["entries"][3];
        assert_eq!(
            (list["name"].as_str(), list["offset"].as_u64()),
            (Some("OpcodeList3"), Some(96))
        );
        let operations = &list["opcode_list"]["operations"];
        // The truncated GainMap keeps its header and is reported; the warp is read whole.
        assert!(operations[0].get("layout").is_none());
        assert_eq!(operations[1]["layout"], "WarpRectilinear");
        assert_eq!(operations[1]["center"], json!([0.5, 0.5]));
        assert_eq!(operations[1]["coefficient_finite"], true);
        let warnings: Vec<&str> = report["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .map(|w| w.as_str().unwrap())
            .collect();
        assert_eq!(
            warnings,
            [
                "opcode 9 in OpcodeList51022 layout unknown: GainMap header is truncated",
                "sensor IFD at 26 lacks ActiveArea; value remains unknown",
                "sensor IFD at 26 lacks DefaultCropOrigin; value remains unknown",
                "sensor IFD at 26 lacks ColorMatrix1; value remains unknown",
                "sensor IFD at 26 lacks ColorMatrix2; value remains unknown",
                "sensor IFD at 26 lacks AsShotNeutral; value remains unknown",
            ]
        );
    }

    #[test]
    fn malformed_input_fails_and_an_existing_report_is_kept() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("bad.dng");
        fs::write(&path, b"XX*\0\x08\0\0\0").unwrap();
        assert!(
            inspect(&path)
                .unwrap_err()
                .to_string()
                .contains("byte-order")
        );
        let mut outside = fixture();
        outside[4..8].copy_from_slice(&4096u32.to_le_bytes());
        fs::write(&path, outside).unwrap();
        assert!(
            inspect(&path)
                .unwrap_err()
                .to_string()
                .contains("exceeds input bounds")
        );
        let good = tmp.path().join("good.dng");
        fs::write(&good, fixture()).unwrap();
        let report = tmp.path().join("report.json");
        fs::write(&report, "kept").unwrap();
        assert!(run(&good, Some(&report)).is_err());
        assert_eq!(fs::read_to_string(&report).unwrap(), "kept");
    }
}
