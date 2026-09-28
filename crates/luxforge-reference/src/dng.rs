//! Independent f64 reference for the DNG 1.4 GainMap and WarpRectilinear corrections the supplied
//! DJI Air 2S (FC3411) DNG requires, with an explicitly simulated Adobe SDK float32 reconstruction
//! comparator, and the white-balance references its calibration tags give.
//!
//! Written from the [DNG 1.7.1 specification] and the Adobe SDK's GainMap, lens-correction and
//! resampling equations, never from `luxforge-raw`: this module reads the little-endian TIFF
//! container and the big-endian opcode payloads itself. It is a reader for this demonstrated
//! container, not the production untrusted-input parser. The study in `tests/studies/dng.rs`
//! turns its results into `fixtures/raw-dng-reference.json`, which `luxforge-raw`'s tests check
//! production against.
//!
//! Production follows the SDK's exclusive `dng_rect` bottom/right bounds;
//! [`warp_source_position_spec_endpoints`] keeps the specification prose's bottom-right-pixel
//! reading beside it so the small difference stays reviewable. The specification clips each
//! stage-2/3 opcode's output to [0, 1]; Luxforge keeps float headroom instead, and
//! [`SparseSample`] carries both.
//!
//! The white-balance responses and the synthetic bicubic sums use [`compensated_sum`], the
//! Neumaier summation the committed fixture was first produced with, so its values are reproduced
//! exactly; the SDK reconstruction's float32 accumulations stay plain, as the SDK's are.
//!
//! [DNG 1.7.1 specification]: https://helpx.adobe.com/content/dam/help/en/photoshop/pdf/DNG_Spec_1_7_1_0.pdf

use std::collections::BTreeMap;

/// A reading error: the container is not the shape this reference reads.
pub type Result<T> = std::result::Result<T, String>;

/// The DNG facts the reference reads: the raw IFD's geometry fields, the colour stage's
/// calibration tags and the raw IFD's OpcodeList3.
#[derive(Clone, Debug, PartialEq)]
pub struct Dng {
    pub file_bytes: usize,
    /// The raw IFD: the first SubIFD of IFD0.
    pub raw_ifd: usize,
    /// OpcodeList3's value offset and byte count in the raw IFD.
    pub opcode_list3_offset: usize,
    pub opcode_list3_bytes: u32,
    /// The raw IFD's ImageWidth, ImageLength, DefaultScale, DefaultCropOrigin, DefaultCropSize,
    /// BestQualityScale and ActiveArea, where they are LONG or RATIONAL.
    pub raw_fields: BTreeMap<u16, Field>,
    pub color_stage: ColorStage,
    /// The count OpcodeList3 declares.
    pub opcode_count: u32,
    pub opcodes: Vec<Opcode>,
}

/// A raw IFD field as stored: LONG values or RATIONAL numerator/denominator pairs.
#[derive(Clone, Debug, PartialEq)]
pub enum Field {
    Longs(Vec<u32>),
    Rationals(Vec<[u32; 2]>),
}

/// The camera white-balance and profile tags, read from IFD0 and then each SubIFD (the DNG
/// specification permits the colour matrices in a camera profile IFD), a later IFD's tag
/// replacing an earlier one's. They describe the stage after the opcode list: AsShotNeutral is
/// the camera-space white-balance reference and ColorMatrix1/2 the XYZ-to-camera matrices
/// CalibrationIlluminant1/2 select. They stay rational pairs, so no implementation's rounding is
/// baked into the fixture.
#[derive(Clone, Debug, PartialEq)]
pub struct ColorStage {
    pub unique_camera_model: String,
    pub calibration_illuminants: [u16; 2],
    pub analog_balance: Vec<[u32; 2]>,
    pub as_shot_neutral: Vec<[u32; 2]>,
    pub color_matrix1: Vec<[i32; 2]>,
    pub color_matrix2: Vec<[i32; 2]>,
}

/// The order the colour stage applies in.
pub const COLOR_STAGE_ORDERING: &str =
    "opcode list 3 -> camera-space WB (AsShotNeutral) -> inverse XYZ-to-camera matrix to XYZ";

impl ColorStage {
    pub fn as_shot_neutral_values(&self) -> Vec<f64> {
        unsigned_values(&self.as_shot_neutral)
    }

    /// The as-shot sensor gains, green-normalised: `[g/r, 1, g/b]` of AsShotNeutral.
    pub fn as_shot_green_normalized_sensor_gains(&self) -> [f64; 3] {
        let v = self.as_shot_neutral_values();
        [v[1] / v[0], 1.0, v[1] / v[2]]
    }
}

fn unsigned_values(pairs: &[[u32; 2]]) -> Vec<f64> {
    pairs
        .iter()
        .map(|[n, d]| f64::from(*n) / f64::from(*d))
        .collect()
}

fn signed_values(pairs: &[[i32; 2]]) -> Vec<f64> {
    pairs
        .iter()
        .map(|[n, d]| f64::from(*n) / f64::from(*d))
        .collect()
}

/// One OpcodeList3 operation.
#[derive(Clone, Debug, PartialEq)]
pub struct Opcode {
    pub index: usize,
    pub id: u32,
    pub version: u32,
    pub flags: u32,
    pub byte_count: u32,
    pub payload: Payload,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Payload {
    GainMap(GainMap),
    Warp(Warp),
    /// An operation this reference does not interpret.
    Other,
}

/// A GainMap (opcode 9).
#[derive(Clone, Debug, PartialEq)]
pub struct GainMap {
    /// Top, left, bottom, right, plane, planes, row pitch, column pitch.
    pub area: [u32; 8],
    /// Map points, vertical then horizontal.
    pub points: [u32; 2],
    pub spacing: [f64; 2],
    pub origin: [f64; 2],
    pub planes: u32,
    /// The map's float32 gains widened to f64, row-major then column then plane.
    pub values: Vec<f64>,
}

impl GainMap {
    pub fn value(&self, row: usize, col: usize, plane: usize) -> f64 {
        let planes = self.planes as usize;
        self.values[plane + planes * (col + self.points[1] as usize * row)]
    }

    /// The area's top, left, bottom and right, the bounds its pixel coordinates are relative to.
    pub fn bounds(&self) -> [i64; 4] {
        [0, 1, 2, 3].map(|i| i64::from(self.area[i]))
    }
}

/// A WarpRectilinear (opcode 1).
#[derive(Clone, Debug, PartialEq)]
pub struct Warp {
    pub planes: u32,
    /// Per plane: kr0, kr1, kr2, kr3, kt0, kt1.
    pub coefficients: Vec<[f64; 6]>,
    /// The optical centre, `[x, y]` as fractions of the bounds.
    pub center: [f64; 2],
}

impl Dng {
    pub fn gain_map(&self) -> Option<&GainMap> {
        self.opcodes.iter().find_map(|op| match &op.payload {
            Payload::GainMap(map) => Some(map),
            _ => None,
        })
    }

    pub fn warp(&self) -> Option<&Warp> {
        self.opcodes.iter().find_map(|op| match &op.payload {
            Payload::Warp(warp) => Some(warp),
            _ => None,
        })
    }

    /// ActiveArea as top, left, bottom, right; DNG's default is the whole raw image.
    pub fn active_area(&self) -> Result<[i64; 4]> {
        if let Some(Field::Longs(area)) = self.raw_fields.get(&50829) {
            let area: [u32; 4] = area[..]
                .try_into()
                .map_err(|_| "ActiveArea is not four values".to_string())?;
            return Ok(area.map(i64::from));
        }
        let long = |tag: u16| match self.raw_fields.get(&tag) {
            Some(Field::Longs(values)) if !values.is_empty() => Ok(i64::from(values[0])),
            _ => Err(format!("raw IFD lacks tag {tag}")),
        };
        Ok([0, 0, long(257)?, long(256)?])
    }
}

// ---------------------------------------------------------------------------------------------
// Reading the container.
// ---------------------------------------------------------------------------------------------

fn slice(buf: &[u8], offset: usize, len: usize) -> Result<&[u8]> {
    offset
        .checked_add(len)
        .and_then(|end| buf.get(offset..end))
        .ok_or_else(|| format!("{len} bytes at {offset} exceed the input"))
}

fn le16(buf: &[u8], offset: usize) -> Result<u16> {
    Ok(u16::from_le_bytes(
        slice(buf, offset, 2)?.try_into().unwrap(),
    ))
}

fn le32(buf: &[u8], offset: usize) -> Result<u32> {
    Ok(u32::from_le_bytes(
        slice(buf, offset, 4)?.try_into().unwrap(),
    ))
}

fn be32(buf: &[u8], offset: usize) -> Result<u32> {
    Ok(u32::from_be_bytes(
        slice(buf, offset, 4)?.try_into().unwrap(),
    ))
}

fn be_f32(buf: &[u8], offset: usize) -> Result<f64> {
    Ok(f64::from(f32::from_be_bytes(
        slice(buf, offset, 4)?.try_into().unwrap(),
    )))
}

fn be_f64(buf: &[u8], offset: usize) -> Result<f64> {
    Ok(f64::from_be_bytes(
        slice(buf, offset, 8)?.try_into().unwrap(),
    ))
}

fn type_size(typ: u16) -> Result<usize> {
    Ok(match typ {
        1 | 2 | 6 | 7 => 1,
        3 | 8 => 2,
        4 | 9 | 11 | 13 => 4,
        5 | 10 | 12 => 8,
        _ => return Err(format!("unknown TIFF type {typ}")),
    })
}

/// One IFD entry: tag, type, count, its value bytes and the raw value/offset word.
struct Entry<'a> {
    tag: u16,
    typ: u16,
    count: u32,
    data: &'a [u8],
    word: u32,
}

fn entries(buf: &[u8], ifd: usize) -> Result<Vec<Entry<'_>>> {
    let count = le16(buf, ifd)?;
    (0..usize::from(count))
        .map(|i| {
            let at = ifd + 2 + 12 * i;
            let tag = le16(buf, at)?;
            let typ = le16(buf, at + 2)?;
            let count = le32(buf, at + 4)?;
            let word = le32(buf, at + 8)?;
            let total = type_size(typ)? * count as usize;
            let data = if total <= 4 {
                slice(buf, at + 8, total)?
            } else {
                slice(buf, word as usize, total)?
            };
            Ok(Entry {
                tag,
                typ,
                count,
                data,
                word,
            })
        })
        .collect()
}

fn le_u32s(data: &[u8]) -> Vec<u32> {
    data.chunks_exact(4)
        .map(|c| u32::from_le_bytes(c.try_into().unwrap()))
        .collect()
}

fn le_pairs(data: &[u8]) -> Vec<[u32; 2]> {
    data.chunks_exact(8)
        .map(|c| {
            [
                u32::from_le_bytes(c[..4].try_into().unwrap()),
                u32::from_le_bytes(c[4..].try_into().unwrap()),
            ]
        })
        .collect()
}

/// Read the facts the reference needs from a little-endian DNG.
pub fn parse(buf: &[u8]) -> Result<Dng> {
    if buf.get(..4) != Some(b"II*\0") {
        return Err("not a little-endian TIFF".into());
    }
    let root = le32(buf, 4)? as usize;
    let sub_ifds: Vec<usize> = entries(buf, root)?
        .iter()
        .filter(|e| e.tag == 330 && e.typ == 4)
        .flat_map(|e| le_u32s(e.data))
        .map(|offset| offset as usize)
        .collect();
    let raw_ifd = *sub_ifds.first().ok_or("IFD0 has no SubIFDs")?;
    let mut opcode_list = None;
    let mut raw_fields = BTreeMap::new();
    for entry in entries(buf, raw_ifd)? {
        if entry.tag == 51022 {
            opcode_list = Some((entry.word as usize, entry.count));
        }
        if [256, 257, 50718, 50719, 50720, 50780, 50829].contains(&entry.tag) {
            match entry.typ {
                4 => {
                    raw_fields.insert(entry.tag, Field::Longs(le_u32s(entry.data)));
                }
                5 => {
                    raw_fields.insert(entry.tag, Field::Rationals(le_pairs(entry.data)));
                }
                _ => {}
            }
        }
    }
    let (opcode_list3_offset, opcode_list3_bytes) =
        opcode_list.ok_or("Raw IFD lacks OpcodeList3 (51022)")?;
    let opcode_count = be32(buf, opcode_list3_offset)?;
    Ok(Dng {
        file_bytes: buf.len(),
        raw_ifd,
        opcode_list3_offset,
        opcode_list3_bytes,
        raw_fields,
        color_stage: color_stage(buf, root, &sub_ifds)?,
        opcode_count,
        opcodes: opcode_list3(buf, opcode_list3_offset + 4, opcode_count)?,
    })
}

fn color_stage(buf: &[u8], root: usize, sub_ifds: &[usize]) -> Result<ColorStage> {
    const WANTED: [u16; 7] = [50708, 50721, 50722, 50727, 50728, 50778, 50779];
    let mut fields: BTreeMap<u16, (u16, &[u8])> = BTreeMap::new();
    for &ifd in std::iter::once(&root).chain(sub_ifds) {
        for entry in entries(buf, ifd)? {
            if WANTED.contains(&entry.tag) {
                fields.insert(entry.tag, (entry.typ, entry.data));
            }
        }
    }
    let field = |tag: u16| {
        fields
            .get(&tag)
            .ok_or(format!("colour stage lacks tag {tag}"))
    };
    let rationals = |tag: u16| -> Result<Vec<[u32; 2]>> {
        let (typ, data) = field(tag)?;
        if !matches!(typ, 5 | 10) {
            return Err(format!("tag {tag} is not a rational"));
        }
        Ok(le_pairs(data))
    };
    let signed = |tag: u16| -> Result<Vec<[i32; 2]>> {
        Ok(rationals(tag)?
            .into_iter()
            .map(|[n, d]| [n as i32, d as i32])
            .collect())
    };
    let model = field(50708)?.1;
    let model = model.split(|b| *b == 0).next().unwrap_or_default();
    if !model.is_ascii() {
        return Err("UniqueCameraModel is not ASCII".into());
    }
    Ok(ColorStage {
        unique_camera_model: String::from_utf8(model.to_vec()).map_err(|e| e.to_string())?,
        calibration_illuminants: [le16(field(50778)?.1, 0)?, le16(field(50779)?.1, 0)?],
        analog_balance: rationals(50727)?,
        as_shot_neutral: rationals(50728)?,
        color_matrix1: signed(50721)?,
        color_matrix2: signed(50722)?,
    })
}

/// Decode OpcodeList3's operations; their numeric fields are big-endian.
fn opcode_list3(buf: &[u8], offset: usize, count: u32) -> Result<Vec<Opcode>> {
    let mut opcodes = Vec::new();
    let mut pos = offset;
    for index in 0..count as usize {
        let id = be32(buf, pos)?;
        let version = be32(buf, pos + 4)?;
        let flags = be32(buf, pos + 8)?;
        let byte_count = be32(buf, pos + 12)?;
        let payload = pos + 16;
        let end = payload + byte_count as usize;
        if end > buf.len() {
            return Err("opcode payload exceeds input".into());
        }
        let payload = match id {
            9 => {
                let mut area = [0; 8];
                for (i, value) in area.iter_mut().enumerate() {
                    *value = be32(buf, payload + 4 * i)?;
                }
                let p = payload + 32;
                let points = [be32(buf, p)?, be32(buf, p + 4)?];
                let spacing = [be_f64(buf, p + 8)?, be_f64(buf, p + 16)?];
                let origin = [be_f64(buf, p + 24)?, be_f64(buf, p + 32)?];
                let planes = be32(buf, p + 40)?;
                let cells = points[0] as usize * points[1] as usize * planes as usize;
                let values = (0..cells)
                    .map(|i| be_f32(buf, p + 44 + 4 * i))
                    .collect::<Result<_>>()?;
                Payload::GainMap(GainMap {
                    area,
                    points,
                    spacing,
                    origin,
                    planes,
                    values,
                })
            }
            1 => {
                let planes = be32(buf, payload)?;
                let mut p = payload + 4;
                let mut coefficients = Vec::new();
                for _ in 0..planes {
                    let mut plane = [0.0; 6];
                    for (j, value) in plane.iter_mut().enumerate() {
                        *value = be_f64(buf, p + 8 * j)?;
                    }
                    coefficients.push(plane);
                    p += 48;
                }
                Payload::Warp(Warp {
                    planes,
                    coefficients,
                    center: [be_f64(buf, p)?, be_f64(buf, p + 8)?],
                })
            }
            _ => Payload::Other,
        };
        opcodes.push(Opcode {
            index,
            id,
            version,
            flags,
            byte_count,
            payload,
        });
        pos = end;
    }
    Ok(opcodes)
}

// ---------------------------------------------------------------------------------------------
// Arithmetic helpers.
// ---------------------------------------------------------------------------------------------

/// Neumaier's compensated sum (the improved Kahan–Babuška algorithm): the running compensation
/// is added once at the end when it is finite and nonzero.
pub fn compensated_sum(values: impl IntoIterator<Item = f64>) -> f64 {
    let mut sum = 0.0_f64;
    let mut compensation = 0.0_f64;
    for x in values {
        let t = sum + x;
        if sum.abs() >= x.abs() {
            compensation += (sum - t) + x;
        } else {
            compensation += (x - t) + sum;
        }
        sum = t;
    }
    if compensation != 0.0 && compensation.is_finite() {
        sum += compensation;
    }
    sum
}

/// The smaller of two values, the first when neither is smaller.
fn smaller(a: f64, b: f64) -> f64 {
    if b < a { b } else { a }
}

/// The larger of two values, the first when neither is larger.
fn larger(a: f64, b: f64) -> f64 {
    if b > a { b } else { a }
}

/// `value` clamped to the specification's per-opcode [0, 1] range.
pub fn strict_clip(value: f64) -> f64 {
    larger(0.0, smaller(1.0, value))
}

/// A value rounded to float32, as the SDK's reconstruction stores it.
pub fn float32(value: f64) -> f64 {
    f64::from(value as f32)
}

// ---------------------------------------------------------------------------------------------
// GainMap.
// ---------------------------------------------------------------------------------------------

/// The SDK's GainMap interpolation at a pixel of `bounds` (top, left, bottom, right): pixel
/// centres relative to the bounds, bilinear between map points, edge replication beyond them. A
/// plane past the map's last repeats the last.
pub fn gain_interpolate(gain: &GainMap, row: i64, col: i64, plane: usize, bounds: [i64; 4]) -> f64 {
    let [top, left, bottom, right] = bounds;
    let plane = plane.min(gain.planes as usize - 1);
    // The +0.5 is the pixel-centre convention. The integer image origin stays explicit: height
    // and width may happen to be equal.
    let axis =
        |pixel: i64, image_origin: i64, extent: i64, points: u32, spacing: f64, origin: f64| {
            let x = ((pixel as f64 + 0.5 - image_origin as f64) / extent as f64 - origin) / spacing;
            if x <= 0.0 {
                return (0, 0, 0.0);
            }
            let last = points as usize - 1;
            if x >= last as f64 {
                return (last, last, 0.0);
            }
            let lo = x.floor();
            (lo as usize, lo as usize + 1, x - lo)
        };
    let (r0, r1, rf) = axis(
        row,
        top,
        bottom - top,
        gain.points[0],
        gain.spacing[0],
        gain.origin[0],
    );
    let (c0, c1, cf) = axis(
        col,
        left,
        right - left,
        gain.points[1],
        gain.spacing[1],
        gain.origin[1],
    );
    let v = |r, c| gain.value(r, c, plane);
    let a = v(r0, c0) * (1.0 - rf) + v(r1, c0) * rf;
    let b = v(r0, c1) * (1.0 - rf) + v(r1, c1) * rf;
    a * (1.0 - cf) + b * cf
}

// ---------------------------------------------------------------------------------------------
// WarpRectilinear.
// ---------------------------------------------------------------------------------------------

/// A warp's source position, row then column: exact integer coordinates where an identity
/// coefficient plane skips resampling, mapped float coordinates otherwise.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Source {
    Exact([i64; 2]),
    Mapped([f64; 2]),
}

impl Source {
    pub fn as_f64(self) -> [f64; 2] {
        match self {
            Self::Exact([row, col]) => [row as f64, col as f64],
            Self::Mapped(position) => position,
        }
    }

    /// The same position moved by `[rows, cols]`.
    pub fn offset(self, by: [i64; 2]) -> Self {
        match self {
            Self::Exact([row, col]) => Self::Exact([row + by[0], col + by[1]]),
            Self::Mapped([row, col]) => Self::Mapped([row + by[0] as f64, col + by[1] as f64]),
        }
    }
}

/// The radial and tangential polynomial shared by both endpoint conventions, from the normalised
/// offset `(nv, nh)`, returning the source offset in normalised units, row then column.
fn warp_polynomial(coefficients: &[f64; 6], nv: f64, nh: f64, pixel_scale_v: f64) -> [f64; 2] {
    let [kr0, kr1, kr2, kr3, kt0, kt1] = *coefficients;
    let (nvs, nhs) = (nv * pixel_scale_v, nh);
    let rr = smaller(nvs * nvs + nhs * nhs, 1.0);
    let ratio = kr0 + rr * (kr1 + rr * (kr2 + rr * kr3));
    let tan_h = kt0 * (2.0 * nvs * nhs) + kt1 * (rr + 2.0 * nhs * nhs);
    let tan_v = kt0 * (rr + 2.0 * nvs * nvs) + kt1 * (2.0 * nvs * nhs);
    [nv * ratio + tan_v / pixel_scale_v, nh * ratio + tan_h]
}

fn coefficients(warp: &Warp, plane: usize) -> &[f64; 6] {
    &warp.coefficients[plane.min(warp.planes as usize - 1)]
}

/// The Adobe SDK's `GetSrcPixelPosition` for WarpRectilinear: the source position, row then
/// column, of the destination pixel at `(row, col)`, with the SDK's exclusive bottom/right bounds.
pub fn warp_source_position(
    warp: &Warp,
    row: f64,
    col: f64,
    bounds: [i64; 4],
    pixel_scale_v: f64,
    plane: usize,
) -> [f64; 2] {
    let [top, left, bottom, right] = bounds.map(|v| v as f64);
    let center_v = top + (bottom - top) * warp.center[1];
    let center_h = left + (right - left) * warp.center[0];
    let square_bottom = top + (pixel_scale_v * (bottom - top)).round_ties_even();
    let square_center_v = top + (square_bottom - top) * warp.center[1];
    let square_center_h = left + (right - left) * warp.center[0];
    let max_dv = larger(
        (square_center_v - top).abs(),
        (square_center_v - square_bottom).abs(),
    );
    let max_dh = larger(
        (square_center_h - left).abs(),
        (square_center_h - right).abs(),
    );
    let norm_radius = max_dv.hypot(max_dh);
    let (nv, nh) = (
        (row - center_v) / norm_radius,
        (col - center_h) / norm_radius,
    );
    let [src_v, src_h] = warp_polynomial(coefficients(warp, plane), nv, nh, pixel_scale_v);
    [
        center_v + norm_radius * src_v,
        center_h + norm_radius * src_h,
    ]
}

/// The production policy for an identity coefficient plane (`kr0 = 1`, every other term zero):
/// the destination coordinates exactly, with resampling skipped. Evaluating the general
/// polynomial there can land at 3.9999999999997726 instead of 4 at an edge.
pub fn warp_source_position_exact_identity(
    warp: &Warp,
    row: i64,
    col: i64,
    bounds: [i64; 4],
    pixel_scale_v: f64,
    plane: usize,
) -> Source {
    if *coefficients(warp, plane) == [1.0, 0.0, 0.0, 0.0, 0.0, 0.0] {
        return Source::Exact([row, col]);
    }
    Source::Mapped(warp_source_position(
        warp,
        row as f64,
        col as f64,
        bounds,
        pixel_scale_v,
        plane,
    ))
}

/// The specification prose's literal reading, with x1/y1 the bottom-right pixel's coordinates,
/// where the SDK uses `dng_rect`'s exclusive right and bottom.
pub fn warp_source_position_spec_endpoints(
    warp: &Warp,
    row: f64,
    col: f64,
    bounds: [i64; 4],
    pixel_scale_v: f64,
    plane: usize,
) -> [f64; 2] {
    let [top, left, bottom, right] = bounds.map(|v| v as f64);
    let (last_v, last_h) = (bottom - 1.0, right - 1.0);
    let center_v = top + (last_v - top) * warp.center[1];
    let center_h = left + (last_h - left) * warp.center[0];
    let square_last_v = top + (pixel_scale_v * (last_v - top)).round_ties_even();
    let square_center_v = top + (square_last_v - top) * warp.center[1];
    let square_center_h = left + (last_h - left) * warp.center[0];
    let max_dv = larger(
        (square_center_v - top).abs(),
        (square_center_v - square_last_v).abs(),
    );
    let max_dh = larger(
        (square_center_h - left).abs(),
        (square_center_h - last_h).abs(),
    );
    let norm_radius = max_dv.hypot(max_dh);
    let (nv, nh) = (
        (row - center_v) / norm_radius,
        (col - center_h) / norm_radius,
    );
    let [src_v, src_h] = warp_polynomial(coefficients(warp, plane), nv, nh, pixel_scale_v);
    [
        center_v + norm_radius * src_v,
        center_h + norm_radius * src_h,
    ]
}

// ---------------------------------------------------------------------------------------------
// The SDK's bicubic resampling.
// ---------------------------------------------------------------------------------------------

/// Keys' cubic kernel with A = -0.75.
pub fn bicubic_kernel(value: f64) -> f64 {
    let a = -0.75;
    let x = value.abs();
    if x >= 2.0 {
        return 0.0;
    }
    if x >= 1.0 {
        return ((a * x - 5.0 * a) * x + 8.0 * a) * x - 4.0 * a;
    }
    ((a + 2.0) * x - (a + 3.0)) * x * x + 1.0
}

/// The float32 4×4 weights of `dng_resample_weights_2d` at 1/128 phases (0 to 127), row-major
/// from the tap one above and left of the base pixel.
pub fn sdk_bicubic_weights(phase_v: i64, phase_h: i64) -> [f64; 16] {
    let (fv, fh) = (phase_v as f64 / 128.0, phase_h as f64 / 128.0);
    let mut values = [0.0; 16];
    let mut total = 0.0;
    for (n, value) in values.iter_mut().enumerate() {
        let (j, i) = (n as i64 / 4 - 1, n as i64 % 4 - 1);
        *value = float32(bicubic_kernel(i as f64 - fh) * bicubic_kernel(j as f64 - fv));
        total += *value;
    }
    let scale = float32(1.0 / total);
    values.map(|value| float32(value * scale))
}

// ---------------------------------------------------------------------------------------------
// White balance.
// ---------------------------------------------------------------------------------------------

/// The documented Luxforge Planckian/daylight locus the white-balance solver uses: Planckian
/// below 3800 K, daylight above 4500 K, a smoothstep blend between.
pub fn base_whitepoint_xy(temperature_kelvin: f64) -> [f64; 2] {
    let t = temperature_kelvin;
    let x = if t <= 4_000.0 {
        -0.2661239e9 / t.powf(3.0) - 0.2343580e6 / t.powf(2.0) + 0.8776956e3 / t + 0.179910
    } else {
        -3.0258469e9 / t.powf(3.0) + 2.1070379e6 / t.powf(2.0) + 0.2226347e3 / t + 0.240390
    };
    let y = if t <= 2_222.0 {
        -1.1063814 * x.powf(3.0) - 1.3481102 * x.powf(2.0) + 2.18555832 * x - 0.20219683
    } else if t <= 4_000.0 {
        -0.9549476 * x.powf(3.0) - 1.37418593 * x.powf(2.0) + 2.09137015 * x - 0.16748867
    } else {
        3.0817580 * x.powf(3.0) - 5.87338670 * x.powf(2.0) + 3.75112997 * x - 0.37001483
    };
    let daylight_x = if t <= 7_000.0 {
        0.244063 + 0.09911e3 / t + 2.9678e6 / t.powf(2.0) - 4.6070e9 / t.powf(3.0)
    } else {
        0.237040 + 0.24748e3 / t + 1.9018e6 / t.powf(2.0) - 2.0064e9 / t.powf(3.0)
    };
    let daylight = [
        daylight_x,
        -3.0 * daylight_x.powf(2.0) + 2.870 * daylight_x - 0.275,
    ];
    let planck = [x, y];
    let blend = larger(0.0, smaller(1.0, (t - 3_800.0) / 700.0));
    let blend = blend * blend * (3.0 - 2.0 * blend);
    [0, 1].map(|i| planck[i] + (daylight[i] - planck[i]) * blend)
}

fn xy_to_uv([x, y]: [f64; 2]) -> [f64; 2] {
    let d = -2.0 * x + 12.0 * y + 3.0;
    [4.0 * x / d, 6.0 * y / d]
}

fn uv_to_xy([u, v]: [f64; 2]) -> [f64; 2] {
    let d = 2.0 * u - 8.0 * v + 4.0;
    [3.0 * u / d, 2.0 * v / d]
}

/// The locus white point offset along the CIE 1960 uv normal by 1e-4 per Luxforge tint unit.
pub fn tinted_whitepoint_xy(temperature_kelvin: f64, tint: f64) -> [f64; 2] {
    let uv = xy_to_uv(base_whitepoint_xy(temperature_kelvin));
    let lo = xy_to_uv(base_whitepoint_xy(larger(
        2_000.0,
        temperature_kelvin - 1.0,
    )));
    let hi = xy_to_uv(base_whitepoint_xy(smaller(
        12_000.0,
        temperature_kelvin + 1.0,
    )));
    let tangent = [hi[0] - lo[0], hi[1] - lo[1]];
    let length = tangent[0].hypot(tangent[1]);
    let normal = [tangent[1] / length, -tangent[0] / length];
    uv_to_xy([
        uv[0] + normal[0] * tint * 1.0e-4,
        uv[1] + normal[1] * tint * 1.0e-4,
    ])
}

/// Standard Light A and D65, the two calibration illuminants' correlated colour temperatures.
pub const CALIBRATION_CCT_KELVIN: [f64; 2] = [2_856.0, 6_504.0];

/// One white-balance reference: the XYZ-to-camera matrix it selects and the sensor gains that
/// make the tinted white point neutral.
#[derive(Clone, Debug, PartialEq)]
pub struct WhiteBalance {
    pub temperature_kelvin: f64,
    pub tint: f64,
    /// ColorMatrix2's inverse-CCT weight, for the dual-calibration comparator only.
    pub matrix2_weight: Option<f64>,
    pub whitepoint_xy: [f64; 2],
    pub whitepoint_xyz: [f64; 3],
    /// Row-major XYZ-to-camera, AnalogBalance applied.
    pub matrix: [f64; 9],
    pub camera_white_response: [f64; 3],
    pub green_normalized_sensor_gains: [f64; 3],
}

impl WhiteBalance {
    fn solve(
        matrix: Vec<f64>,
        stage: &ColorStage,
        temperature_kelvin: f64,
        tint: f64,
        matrix2_weight: Option<f64>,
    ) -> Self {
        let analog = unsigned_values(&stage.analog_balance);
        let matrix: [f64; 9] = std::array::from_fn(|k| matrix[k] * analog[k / 3]);
        let [x, y] = tinted_whitepoint_xy(temperature_kelvin, tint);
        let xyz = [x / y, 1.0, (1.0 - x - y) / y];
        let response: [f64; 3] =
            std::array::from_fn(|i| compensated_sum((0..3).map(|j| matrix[i * 3 + j] * xyz[j])));
        Self {
            temperature_kelvin,
            tint,
            matrix2_weight,
            whitepoint_xy: [x, y],
            whitepoint_xyz: xyz,
            matrix,
            camera_white_response: response,
            green_normalized_sensor_gains: [
                response[1] / response[0],
                1.0,
                response[1] / response[2],
            ],
        }
    }

    /// Whether every gain is finite and within the RAW adapter's (0, 32] contract.
    pub fn valid_for_gain_contract(&self) -> bool {
        self.green_normalized_sensor_gains
            .iter()
            .all(|g| g.is_finite() && 0.0 < *g && *g <= 32.0)
    }
}

/// The comparator: ColorMatrix1 and ColorMatrix2 interpolated by inverse CCT between the two
/// calibration illuminants. Not the production policy.
pub fn wb_dual_calibration(stage: &ColorStage, temperature_kelvin: f64, tint: f64) -> WhiteBalance {
    let [cct1, cct2] = CALIBRATION_CCT_KELVIN;
    let weight2 = larger(
        0.0,
        smaller(
            1.0,
            (1.0 / temperature_kelvin - 1.0 / cct1) / (1.0 / cct2 - 1.0 / cct1),
        ),
    );
    let cm1 = signed_values(&stage.color_matrix1);
    let cm2 = signed_values(&stage.color_matrix2);
    let matrix = cm1
        .iter()
        .zip(&cm2)
        .map(|(a, b)| a + (b - a) * weight2)
        .collect();
    WhiteBalance::solve(matrix, stage, temperature_kelvin, tint, Some(weight2))
}

/// The production FC3411 policy: ColorMatrix2, the D65 XYZ-to-camera matrix, at every
/// temperature.
pub fn wb_fixed_cm2(stage: &ColorStage, temperature_kelvin: f64, tint: f64) -> WhiteBalance {
    WhiteBalance::solve(
        signed_values(&stage.color_matrix2),
        stage,
        temperature_kelvin,
        tint,
        None,
    )
}

// ---------------------------------------------------------------------------------------------
// The sparse camera-plane comparison.
// ---------------------------------------------------------------------------------------------

/// `luxforge-raw`'s sparse dump (its ignored `dump_dji_sparse_uncorrected_reference` test): per
/// output sample, the corrected production value, the production source position and the 8×8
/// uncorrected camera-plane neighbourhood around it.
#[derive(Clone, Debug, Default)]
pub struct SparseDump {
    /// Each output sample's production corrected value.
    results: BTreeMap<SampleKey, f64>,
    /// Each output sample's production source position, raw x then y.
    mapped: BTreeMap<SampleKey, [f64; 2]>,
    /// Each output sample's uncorrected taps, by raw `(x, y)`, as float32.
    inputs: BTreeMap<SampleKey, BTreeMap<(i64, i64), f64>>,
}

/// An output sample: its raw x and y, and its channel.
type SampleKey = (i64, i64, usize);

/// Read the dump's `kind,out_x,out_y,channel,sensor_x,sensor_y,value` CSV.
pub fn parse_sparse(csv: &str) -> Result<SparseDump> {
    let mut lines = csv.lines();
    if lines.next() != Some("kind,out_x,out_y,channel,sensor_x,sensor_y,value") {
        return Err("unexpected sparse dump header".into());
    }
    let mut dump = SparseDump::default();
    for line in lines.filter(|line| !line.is_empty()) {
        let fields: Vec<&str> = line.split(',').collect();
        let [kind, out_x, out_y, channel, sensor_x, sensor_y, value] = fields[..] else {
            return Err(format!("malformed sparse row {line:?}"));
        };
        let int = |s: &str| s.parse::<i64>().map_err(|e| format!("{s:?}: {e}"));
        let float = |s: &str| s.parse::<f64>().map_err(|e| format!("{s:?}: {e}"));
        let key = (int(out_x)?, int(out_y)?, int(channel)? as usize);
        match kind {
            "result" => {
                dump.results.insert(key, float(value)?);
            }
            "mapped" => {
                dump.mapped
                    .insert(key, [float(sensor_x)?, float(sensor_y)?]);
            }
            "input" => {
                dump.inputs
                    .entry(key)
                    .or_default()
                    .insert((int(sensor_x)?, int(sensor_y)?), float32(float(value)?));
            }
            _ => {}
        }
    }
    Ok(dump)
}

/// One sparse output sample recomputed independently from its uncorrected taps.
#[derive(Clone, Debug, PartialEq)]
pub struct SparseSample {
    /// The output pixel, raw x then y.
    pub out_raw_xy: [i64; 2],
    pub plane: usize,
    /// The warp's source position in active-area-local coordinates, row then column.
    pub source_active_local: Source,
    /// The same position in raw coordinates, row then column.
    pub source_raw: Source,
    /// Production's source position from the dump, raw x then y.
    pub mapped_csv_source_raw_xy: [f64; 2],
    pub max_abs_error_to_mapped_csv_pixels: f64,
    pub phase_128: [i64; 2],
    /// The float32 SDK reconstruction with float headroom kept.
    pub reference_float_headroom: f64,
    /// The same with the specification's per-opcode [0, 1] clipping.
    pub reference_strict_dng_clipped: f64,
    pub production_sparse_csv_value: f64,
    pub abs_error_to_csv_value: f64,
}

/// Recompute each dumped sample: the warp's source position, the SDK's float32 bicubic
/// reconstruction at its phase over GainMap-corrected taps, with and without per-opcode clipping.
/// `active_area` is top, left, bottom, right in raw coordinates; stage 3 is the active-area
/// image, so edge replication clamps to it rather than to the sensor's hidden margins.
pub fn sparse_reference(
    dump: &SparseDump,
    gain: &GainMap,
    warp: &Warp,
    active_area: [i64; 4],
) -> Result<Vec<SparseSample>> {
    let [top, left, bottom, right] = active_area;
    let bounds = [0, 0, bottom - top, right - left];
    let mut samples = Vec::new();
    for (&(out_x, out_y, plane), &production) in &dump.results {
        let key = (out_x, out_y, plane);
        let taps = dump
            .inputs
            .get(&key)
            .ok_or(format!("no inputs for {key:?}"))?;
        let mapped = *dump
            .mapped
            .get(&key)
            .ok_or(format!("no mapped position for {key:?}"))?;
        let source = warp_source_position_exact_identity(
            warp,
            out_y - top,
            out_x - left,
            bounds,
            1.0,
            plane,
        );
        let [src_v, src_h] = source.as_f64();
        let (base_v, base_h) = (src_v.floor(), src_h.floor());
        let phase = [
            ((src_v - base_v) * 128.0).floor() as i64,
            ((src_h - base_h) * 128.0).floor() as i64,
        ];
        let weights = sdk_bicubic_weights(phase[0], phase[1]);
        let (mut headroom, mut strict) = (0.0, 0.0);
        for (n, weight) in weights.iter().enumerate() {
            let (j, i) = (n as i64 / 4 - 1, n as i64 % 4 - 1);
            let raw_x = (left + base_h as i64 + i).clamp(left, right - 1);
            let raw_y = (top + base_v as i64 + j).clamp(top, bottom - 1);
            let value = *taps
                .get(&(raw_x, raw_y))
                .ok_or(format!("no tap ({raw_x}, {raw_y}) for {key:?}"))?;
            let map_gain = gain_interpolate(gain, raw_y - top, raw_x - left, plane, bounds);
            let corrected = float32(value * map_gain);
            headroom = float32(headroom + float32(weight * corrected));
            strict = float32(strict + float32(weight * strict_clip(corrected)));
        }
        let source_raw = source.offset([top, left]);
        let [raw_v, raw_h] = source_raw.as_f64();
        samples.push(SparseSample {
            out_raw_xy: [out_x, out_y],
            plane,
            source_active_local: source,
            source_raw,
            mapped_csv_source_raw_xy: mapped,
            max_abs_error_to_mapped_csv_pixels: larger(
                (raw_h - mapped[0]).abs(),
                (raw_v - mapped[1]).abs(),
            ),
            phase_128: phase,
            reference_float_headroom: headroom,
            reference_strict_dng_clipped: smaller(1.0, larger(0.0, strict)),
            production_sparse_csv_value: production,
            abs_error_to_csv_value: (headroom - production).abs(),
        });
    }
    Ok(samples)
}

/// The largest of `values`, the first of equals.
pub fn maximum(values: impl IntoIterator<Item = f64>) -> Option<f64> {
    values.into_iter().reduce(larger)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compensated_sum_recovers_what_a_plain_sum_loses() {
        assert_eq!(compensated_sum([1.0, 1e100, 1.0, -1e100]), 2.0);
        assert_eq!([1.0, 1e100, 1.0, -1e100].iter().sum::<f64>(), 0.0);
        assert_eq!(compensated_sum([]), 0.0);
    }

    #[test]
    fn bicubic_weights_are_normalised_float32_and_centred_at_phase_zero() {
        let weights = sdk_bicubic_weights(0, 0);
        // At phase zero the kernel is 1 at the base pixel and 0 at every other integer tap.
        assert_eq!(weights[5], 1.0);
        assert!(weights.iter().enumerate().all(|(n, w)| n == 5 || *w == 0.0));
        for phase in [(1, 127), (64, 64), (33, 113)] {
            let weights = sdk_bicubic_weights(phase.0, phase.1);
            assert!(weights.iter().all(|w| float32(*w) == *w));
            assert!((weights.iter().sum::<f64>() - 1.0).abs() < 1e-6);
        }
    }

    #[test]
    fn an_identity_plane_is_exact_and_another_is_mapped() {
        let warp = Warp {
            planes: 2,
            coefficients: vec![
                [1.0, 0.0, 0.0, 0.0, 0.0, 0.0],
                [1.001, 0.0, 0.0, 0.0, 0.0, 0.0],
            ],
            center: [0.5, 0.5],
        };
        let bounds = [0, 0, 8, 8];
        assert_eq!(
            warp_source_position_exact_identity(&warp, 4, 4, bounds, 1.0, 0),
            Source::Exact([4, 4])
        );
        // A plane past the last repeats the last.
        let Source::Mapped([row, col]) =
            warp_source_position_exact_identity(&warp, 0, 0, bounds, 1.0, 5)
        else {
            panic!("a non-identity plane is resampled");
        };
        assert!(row < 0.0 && col < 0.0);
    }
}
