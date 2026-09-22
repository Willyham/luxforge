//! Narrow, bounded container inspection for the qualified modes. LibRaw still
//! owns decompression; these reads validate recording-mode and crop semantics.
use super::profiles::{Camera, Catalog, CompressionProbe, Dng, DngCalibration, DngContainer, Mode};
use super::{DngCalibrationMetadata, NativeMetadata, RawError, RawRect};
use sha2::{Digest, Sha256};

#[derive(Clone, Copy)]
enum Endian {
    Little,
    Big,
}
fn u16_at(b: &[u8], p: usize, e: Endian) -> Option<u16> {
    let a = b.get(p..p.checked_add(2)?)?;
    Some(match e {
        Endian::Little => u16::from_le_bytes([a[0], a[1]]),
        Endian::Big => u16::from_be_bytes([a[0], a[1]]),
    })
}
fn u32_at(b: &[u8], p: usize, e: Endian) -> Option<u32> {
    let a = b.get(p..p.checked_add(4)?)?;
    Some(match e {
        Endian::Little => u32::from_le_bytes(a.try_into().ok()?),
        Endian::Big => u32::from_be_bytes(a.try_into().ok()?),
    })
}

#[derive(Clone, Copy)]
struct Entry {
    tag: u16,
    kind: u16,
    count: u32,
    value: u32,
    inline: usize,
}
struct Tiff<'a> {
    data: &'a [u8],
    base: usize,
    endian: Endian,
}
impl<'a> Tiff<'a> {
    fn header(data: &'a [u8], base: usize) -> Option<(Self, u32)> {
        let endian = match data.get(base..base.checked_add(2)?)? {
            b"II" => Endian::Little,
            b"MM" => Endian::Big,
            _ => return None,
        };
        if u16_at(data, base + 2, endian)? != 42 {
            return None;
        }
        let first = u32_at(data, base + 4, endian)?;
        Some((Self { data, base, endian }, first))
    }
    fn entries(&self, rel: u32) -> Option<(Vec<Entry>, u32)> {
        let off = self.base.checked_add(rel as usize)?;
        let count = u16_at(self.data, off, self.endian)? as usize;
        if count > 256 {
            return None;
        }
        let end = off.checked_add(2)?.checked_add(count.checked_mul(12)?)?;
        self.data.get(off..end.checked_add(4)?)?;
        let mut entries = Vec::with_capacity(count);
        for i in 0..count {
            let p = off + 2 + i * 12;
            entries.push(Entry {
                tag: u16_at(self.data, p, self.endian)?,
                kind: u16_at(self.data, p + 2, self.endian)?,
                count: u32_at(self.data, p + 4, self.endian)?,
                value: u32_at(self.data, p + 8, self.endian)?,
                inline: p + 8,
            });
        }
        Some((entries, u32_at(self.data, end, self.endian)?))
    }
    fn payload(&self, e: Entry) -> Option<&'a [u8]> {
        let unit = match e.kind {
            1 | 2 | 6 | 7 => 1,
            3 | 8 => 2,
            4 | 9 | 11 => 4,
            5 | 10 | 12 => 8,
            _ => return None,
        };
        let len = (e.count as usize).checked_mul(unit)?;
        if len > 1024 * 1024 {
            return None;
        }
        let p = if len <= 4 {
            e.inline
        } else {
            self.base.checked_add(e.value as usize)?
        };
        self.data.get(p..p.checked_add(len)?)
    }
    fn scalar(&self, e: Entry) -> Option<u32> {
        if e.count != 1 {
            return None;
        }
        match e.kind {
            3 => Some(u16_at(self.data, e.inline, self.endian)? as u32),
            4 => Some(e.value),
            _ => None,
        }
    }
}

fn nef_compression(bytes: &[u8]) -> Option<u16> {
    let (root, first) = Tiff::header(bytes, 0)?;
    let (ifd, _) = root.entries(first)?;
    let exif = ifd
        .into_iter()
        .find(|e| e.tag == 0x8769)
        .and_then(|e| root.scalar(e))?;
    let (exif_entries, _) = root.entries(exif)?;
    let maker = exif_entries
        .into_iter()
        .find(|e| e.tag == 0x927c)
        .and_then(|e| root.payload(e))?;
    if maker.get(..6)? != b"Nikon\0" || maker.len() < 18 {
        return None;
    }
    // The payload's TIFF entry value gives its absolute position in this NEF.
    let maker_offset = root.base.checked_add(
        root.entries(exif)?
            .0
            .into_iter()
            .find(|e| e.tag == 0x927c)?
            .value as usize,
    )?;
    let (nested, first) = Tiff::header(bytes, maker_offset + 10)?;
    let (entries, _) = nested.entries(first)?;
    if let Some(entry) = entries.iter().copied().find(|e| e.tag == 0x51) {
        let payload = nested.payload(entry)?;
        return u16_at(payload, 10, Endian::Little);
    }
    let entry = entries.into_iter().find(|e| e.tag == 0x93)?;
    Some(nested.scalar(entry)? as u16)
}

pub(super) fn raf_default_crop(bytes: &[u8]) -> Option<RawRect> {
    if bytes.get(..8)? != b"FUJIFILM" || bytes.len() < 0x70 {
        return None;
    }
    let offset = u32_at(bytes, 92, Endian::Big)? as usize;
    let count = u32_at(bytes, offset, Endian::Big)? as usize;
    if count > 256 {
        return None;
    }
    let mut p = offset.checked_add(4)?;
    let mut top_left = None;
    let mut cropped_size = None;
    for _ in 0..count {
        let tag = u16_at(bytes, p, Endian::Big)?;
        let len = u16_at(bytes, p + 2, Endian::Big)? as usize;
        p = p.checked_add(4)?;
        let end = p.checked_add(len)?;
        bytes.get(p..end)?;
        if len == 4 && tag == 0x0110 {
            top_left = Some((
                u16_at(bytes, p + 2, Endian::Big)? as u32,
                u16_at(bytes, p, Endian::Big)? as u32,
            ));
        }
        if len == 4 && tag == 0x0111 {
            cropped_size = Some((
                u16_at(bytes, p + 2, Endian::Big)? as u32,
                u16_at(bytes, p, Endian::Big)? as u32,
            ));
        }
        p = end;
    }
    let (x, y) = top_left?;
    let (width, height) = cropped_size?;
    Some(RawRect {
        x,
        y,
        width,
        height,
    })
}

fn raf_compression(bytes: &[u8]) -> Option<u32> {
    if bytes.get(..8)? != b"FUJIFILM" {
        return None;
    }
    u32_at(bytes, 0x6c, Endian::Big)
}

#[derive(Debug, Clone)]
pub(super) struct DngOpcode {
    pub ifd: u32,
    pub list: u16,
    pub id: u32,
    pub version: u32,
    pub flags: u32,
    pub data: Vec<u8>,
}

/// Bounded DNG opcode traversal. Headers and values are big-endian even when
/// their enclosing TIFF is little-endian. Keep IFD and list placement so a
/// required operation cannot be silently accepted at the wrong stage.
pub(super) fn dng_opcodes(bytes: &[u8]) -> Result<Vec<DngOpcode>, RawError> {
    let (tiff, first) = Tiff::header(bytes, 0).ok_or(RawError::InvalidInput("DNG TIFF header"))?;
    let mut queue = vec![first];
    let mut seen = Vec::new();
    let mut opcodes = Vec::new();
    let mut aggregate_opcode_bytes = 0usize;
    while let Some(rel) = queue.pop() {
        if seen.contains(&rel) {
            continue;
        }
        if seen.len() >= 16 {
            return Err(RawError::ResourceLimit("DNG IFD count"));
        }
        seen.push(rel);
        let (entries, next) = tiff.entries(rel).ok_or(RawError::InvalidInput("DNG IFD"))?;
        if next != 0 {
            queue.push(next);
        }
        for entry in entries {
            if entry.tag == 330 {
                let values = tiff
                    .payload(entry)
                    .ok_or(RawError::InvalidInput("DNG SubIFDs"))?;
                if entry.kind != 4 || entry.count > 8 {
                    return Err(RawError::InvalidInput("DNG SubIFD type/count"));
                }
                for chunk in values.chunks_exact(4) {
                    let v = match tiff.endian {
                        Endian::Little => u32::from_le_bytes(chunk.try_into().unwrap()),
                        Endian::Big => u32::from_be_bytes(chunk.try_into().unwrap()),
                    };
                    queue.push(v);
                }
            } else if matches!(entry.tag, 51008 | 51009 | 51022) {
                if entry.kind != 7 {
                    return Err(RawError::InvalidInput("DNG opcode list type"));
                }
                // An empty UNDEFINED field carries no operations (observed in
                // native DNGs alongside a populated later-stage list). A
                // nonempty truncated count still fails below.
                if entry.count == 0 {
                    continue;
                }
                let data = tiff
                    .payload(entry)
                    .ok_or(RawError::InvalidInput("DNG opcode list bounds"))?;
                aggregate_opcode_bytes = aggregate_opcode_bytes
                    .checked_add(data.len())
                    .ok_or(RawError::ResourceLimit("DNG opcode aggregate size"))?;
                if aggregate_opcode_bytes > 1024 * 1024 {
                    return Err(RawError::ResourceLimit("DNG opcode aggregate size"));
                }
                let count = u32_at(data, 0, Endian::Big)
                    .ok_or(RawError::InvalidInput("DNG opcode count"))?
                    as usize;
                if count > 256 {
                    return Err(RawError::ResourceLimit("DNG opcode count"));
                }
                let mut p = 4usize;
                for _ in 0..count {
                    if opcodes.len() >= 256 {
                        return Err(RawError::ResourceLimit("DNG opcode aggregate count"));
                    }
                    let id = u32_at(data, p, Endian::Big)
                        .ok_or(RawError::InvalidInput("DNG opcode ID"))?;
                    let version = u32_at(data, p + 4, Endian::Big)
                        .ok_or(RawError::InvalidInput("DNG opcode version"))?;
                    let flags = u32_at(data, p + 8, Endian::Big)
                        .ok_or(RawError::InvalidInput("DNG opcode flags"))?;
                    let len = u32_at(data, p + 12, Endian::Big)
                        .ok_or(RawError::InvalidInput("DNG opcode length"))?
                        as usize;
                    let start = p
                        .checked_add(16)
                        .ok_or(RawError::ResourceLimit("DNG opcode size"))?;
                    p = start
                        .checked_add(len)
                        .ok_or(RawError::ResourceLimit("DNG opcode size"))?;
                    if p > data.len() {
                        return Err(RawError::InvalidInput("truncated DNG opcode"));
                    }
                    opcodes.push(DngOpcode {
                        ifd: rel,
                        list: entry.tag,
                        id,
                        version,
                        flags,
                        data: data[start..p].to_vec(),
                    });
                }
                if p != data.len() {
                    return Err(RawError::InvalidInput("DNG opcode trailing data"));
                }
            }
        }
    }
    Ok(opcodes)
}

/// IDs of DNG opcodes whose optional flag is unset. Malformed lists fail.
pub fn required_dng_opcodes(bytes: &[u8]) -> Result<Vec<u32>, RawError> {
    let mut required: Vec<_> = dng_opcodes(bytes)?
        .into_iter()
        .filter(|opcode| opcode.flags & 1 == 0)
        .map(|opcode| opcode.id)
        .collect();
    required.sort_unstable();
    required.dedup();
    Ok(required)
}

#[derive(Clone, Copy)]
pub(super) struct DngSensorContainer {
    pub raw_ifd: u32,
    pub active_area: RawRect,
    pub default_crop: RawRect,
}

fn rational_values(tiff: &Tiff<'_>, entry: Entry, count: usize) -> Option<Vec<(u32, u32)>> {
    if !matches!(entry.kind, 3..=5) || entry.count as usize != count {
        return None;
    }
    let bytes = tiff.payload(entry)?;
    let mut values = Vec::with_capacity(count);
    for i in 0..count {
        let (n, d) = match entry.kind {
            3 => (u16_at(bytes, i * 2, tiff.endian)? as u32, 1),
            4 => (u32_at(bytes, i * 4, tiff.endian)?, 1),
            _ => (
                u32_at(bytes, i * 8, tiff.endian)?,
                u32_at(bytes, i * 8 + 4, tiff.endian)?,
            ),
        };
        if d == 0 {
            return None;
        }
        values.push((n, d));
    }
    Some(values)
}

/// Select the authoritative raw sensor SubIFD by its one-sample, uncompressed
/// CFA encoding and dimensions. Preview IFD tags never supply correction or
/// crop semantics. The supplied camera uses unity DefaultScale and
/// BestQualityScale; reject other scaling rather than silently changing the
/// stage-three coordinate domain.
pub(super) fn dng_container(
    bytes: &[u8],
    native: &NativeMetadata,
    strategy: &DngContainer,
    decoder_active_bottom_trim: u32,
) -> Result<DngSensorContainer, RawError> {
    let (tiff, first) = Tiff::header(bytes, 0).ok_or(RawError::InvalidInput("DNG TIFF header"))?;
    let mut queue = vec![first];
    let mut seen = Vec::new();
    let mut candidate = None;
    while let Some(rel) = queue.pop() {
        if rel == 0 || seen.contains(&rel) {
            continue;
        }
        if seen.len() >= 16 {
            return Err(RawError::ResourceLimit("DNG sensor IFD count"));
        }
        seen.push(rel);
        let (entries, next) = tiff
            .entries(rel)
            .ok_or(RawError::InvalidInput("DNG sensor IFD"))?;
        queue.push(next);
        for entry in entries.iter().copied().filter(|e| e.tag == 330) {
            if entry.kind != 4 || entry.count == 0 || entry.count > 8 {
                return Err(RawError::InvalidInput("DNG SubIFD count/type"));
            }
            let offsets = tiff
                .payload(entry)
                .ok_or(RawError::InvalidInput("DNG SubIFD offsets"))?;
            for i in 0..entry.count as usize {
                queue.push(
                    u32_at(offsets, i * 4, tiff.endian)
                        .ok_or(RawError::InvalidInput("DNG SubIFD offset"))?,
                );
            }
        }
        for (i, entry) in entries.iter().enumerate() {
            if entries[..i]
                .iter()
                .any(|previous| previous.tag == entry.tag)
            {
                return Err(RawError::InvalidInput("duplicate DNG raw IFD tag"));
            }
        }
        let get = |tag| entries.iter().copied().find(|e| e.tag == tag);
        let scalar = |tag| get(tag).and_then(|e| tiff.scalar(e));
        if scalar(256) != Some(native.width)
            || scalar(257) != Some(native.height)
            || scalar(258) != Some(native.raw_bps)
            || !matches!(scalar(259), Some(1 | 7))
            || scalar(262) != Some(32803)
            || scalar(277) != Some(1)
            || scalar(284).unwrap_or(1) != 1
        {
            continue;
        }
        if *strategy == DngContainer::UncompressedU16SingleStrip
            && (native.raw_bps != 16 || scalar(259) != Some(1))
        {
            return Err(RawError::UnsupportedMode("DNG profile storage".into()));
        }
        let expected_bytes = (u64::from(native.width) * u64::from(native.raw_bps))
            .div_ceil(8)
            .checked_mul(u64::from(native.height))
            .ok_or(RawError::ResourceLimit("DNG strip size"))?;
        let (offset_tag, count_tag) = if get(273).is_some() {
            if scalar(278) != Some(native.height) || get(324).is_some() {
                return Err(RawError::UnsupportedMode("DNG single-strip layout".into()));
            }
            (273, 279)
        } else if *strategy == DngContainer::IntegerCfaSingleSegment
            && scalar(322) == Some(native.width)
            && scalar(323) == Some(native.height)
        {
            (324, 325)
        } else {
            return Err(RawError::UnsupportedMode(
                "DNG single-segment layout".into(),
            ));
        };
        let strip_offset =
            scalar(offset_tag).ok_or(RawError::InvalidInput("DNG single segment offset"))? as usize;
        let strip_bytes = scalar(count_tag)
            .ok_or(RawError::InvalidInput("DNG single segment byte count"))?
            as usize;
        let uncompressed_size_invalid = scalar(259) == Some(1)
            && if *strategy == DngContainer::UncompressedU16SingleStrip {
                strip_bytes as u64 != expected_bytes
            } else {
                (strip_bytes as u64) < expected_bytes
            };
        if strip_bytes == 0
            || uncompressed_size_invalid
            || strip_offset
                .checked_add(strip_bytes)
                .and_then(|end| bytes.get(strip_offset..end))
                .is_none()
        {
            return Err(RawError::UnsupportedMode("DNG segment encoding".into()));
        }
        // DNG's absent ActiveArea defaults to the full sensor rectangle.
        let area = if let Some(entry) = get(50829) {
            let values =
                rational_values(&tiff, entry, 4).ok_or(RawError::InvalidInput("DNG ActiveArea"))?;
            if values.iter().any(|(_, d)| *d != 1) {
                return Err(RawError::InvalidInput("DNG fractional ActiveArea"));
            }
            [values[0].0, values[1].0, values[2].0, values[3].0]
        } else {
            [0, 0, native.height, native.width]
        };
        let active_bottom = native
            .active_y
            .checked_add(native.active_height)
            .ok_or(RawError::InvalidInput("DNG active bounds"))?;
        let active_right = native
            .active_x
            .checked_add(native.active_width)
            .ok_or(RawError::InvalidInput("DNG active bounds"))?;
        let source_active = RawRect {
            x: area[1],
            y: area[0],
            width: area[3]
                .checked_sub(area[1])
                .ok_or(RawError::InvalidInput("DNG ActiveArea bounds"))?,
            height: area[2]
                .checked_sub(area[0])
                .ok_or(RawError::InvalidInput("DNG ActiveArea bounds"))?,
        };
        let expected_bottom = active_bottom
            .checked_add(decoder_active_bottom_trim)
            .ok_or(RawError::InvalidInput("DNG active trim"))?;
        crate::checked_rect(source_active, native.width, native.height)
            .map_err(|_| RawError::InvalidInput("DNG ActiveArea bounds"))?;
        if source_active.x != native.active_x
            || source_active.y != native.active_y
            || source_active.width != native.active_width
            || area[2] != expected_bottom
            || source_active.x + source_active.width > native.width
            || source_active.y + source_active.height > native.height
            || area[3] != active_right
        {
            return Err(RawError::InvalidInput(
                "DNG ActiveArea differs from decoder",
            ));
        }
        if candidate.is_some() {
            return Err(RawError::InvalidInput("ambiguous DNG raw SubIFD"));
        }
        let unity = |tag, count| -> Result<(), RawError> {
            if let Some(entry) = get(tag) {
                let values = rational_values(&tiff, entry, count)
                    .ok_or(RawError::InvalidInput("DNG scale rational"))?;
                if values.iter().any(|(num, den)| num != den) {
                    return Err(RawError::UnsupportedMode(format!(
                        "DNG nonunity scale tag {tag}"
                    )));
                }
            }
            Ok(())
        };
        unity(50718, 2)?; // DefaultScale
        unity(50780, 1)?; // BestQualityScale
        let origin = rational_values(
            &tiff,
            get(50719).ok_or(RawError::InvalidInput("DNG DefaultCropOrigin"))?,
            2,
        )
        .ok_or(RawError::InvalidInput("DNG DefaultCropOrigin"))?;
        let size = rational_values(
            &tiff,
            get(50720).ok_or(RawError::InvalidInput("DNG DefaultCropSize"))?,
            2,
        )
        .ok_or(RawError::InvalidInput("DNG DefaultCropSize"))?;
        if origin.iter().chain(size.iter()).any(|(_, d)| *d != 1) {
            return Err(RawError::UnsupportedMode("fractional default crop".into()));
        }
        let crop = RawRect {
            x: native
                .active_x
                .checked_add(origin[0].0)
                .ok_or(RawError::ResourceLimit("DNG crop origin"))?,
            y: native
                .active_y
                .checked_add(origin[1].0)
                .ok_or(RawError::ResourceLimit("DNG crop origin"))?,
            width: size[0].0,
            height: size[1].0,
        };
        if crop.width == 0
            || crop.height == 0
            || crop
                .x
                .checked_add(crop.width)
                .is_none_or(|v| v > native.width)
            || crop
                .y
                .checked_add(crop.height)
                .is_none_or(|v| v > native.height)
            || crop.x < native.active_x
            || crop.y < native.active_y
            || crop.x + crop.width > source_active.x + source_active.width
            || crop.y + crop.height > source_active.y + source_active.height
        {
            return Err(RawError::InvalidInput("DNG default crop bounds"));
        }
        candidate = Some(DngSensorContainer {
            raw_ifd: rel,
            active_area: source_active,
            default_crop: crop,
        });
    }
    candidate.ok_or(RawError::UnsupportedMode("DNG raw encoding".into()))
}

/// DNG ColorMatrix1/2 are XYZ-to-reference-camera matrices. The current RAW
/// Temperature/Tint control uses the profile-selected immutable matrix,
/// after proving AnalogBalance is identity and no camera
/// calibration/forward profile changes that relation. Record both source
/// payloads so reopening cannot confuse this fixed-matrix interpretation.
pub(super) fn dng_color_calibration(
    bytes: &[u8],
    native: &NativeMetadata,
    settings: &Dng,
) -> Result<([[f32; 3]; 4], DngCalibrationMetadata), RawError> {
    let DngCalibration::RootFixedMatrix = settings.calibration;
    let (tiff, first) = Tiff::header(bytes, 0).ok_or(RawError::InvalidInput("DNG TIFF header"))?;
    let (root, root_next) = tiff
        .entries(first)
        .ok_or(RawError::InvalidInput("DNG root IFD"))?;
    for (i, entry) in root.iter().enumerate() {
        if root[..i].iter().any(|previous| previous.tag == entry.tag) {
            return Err(RawError::InvalidInput("duplicate DNG root tag"));
        }
    }
    let get = |tag| root.iter().copied().find(|e| e.tag == tag);
    if [50729, 50964, 50965, 52525, 52526]
        .into_iter()
        .any(|tag| get(tag).is_some())
    {
        return Err(RawError::UnsupportedMode(
            "DNG camera calibration or forward profile".into(),
        ));
    }
    // DNG defaults CameraCalibration to identity. Explicit identity matrices
    // are equivalent, regardless of profile signature; nonidentity calibration
    // needs a different implemented colour strategy and cannot be skipped.
    for tag in [50723, 50724] {
        if let Some(entry) = get(tag) {
            if entry.kind != 10 || entry.count != 9 {
                return Err(RawError::MissingCalibration("DNG CameraCalibration shape"));
            }
            let payload = tiff
                .payload(entry)
                .ok_or(RawError::InvalidInput("DNG CameraCalibration payload"))?;
            for i in 0..9 {
                let num = u32_at(payload, i * 8, tiff.endian)
                    .ok_or(RawError::InvalidInput("DNG CameraCalibration numerator"))?
                    as i32;
                let den = u32_at(payload, i * 8 + 4, tiff.endian)
                    .ok_or(RawError::InvalidInput("DNG CameraCalibration denominator"))?
                    as i32;
                if den == 0 || num != if i % 4 == 0 { den } else { 0 } {
                    return Err(RawError::UnsupportedMode(
                        "DNG nonidentity CameraCalibration".into(),
                    ));
                }
            }
        }
    }
    // The root supplies this file's calibration. Do not combine it with a
    // second calibration attached to any preview, linked or sensor IFD.
    let mut queue = vec![root_next];
    if let Some(sub) = get(330) {
        if sub.kind != 4 || sub.count == 0 || sub.count > 8 {
            return Err(RawError::InvalidInput("DNG SubIFD type/count"));
        }
        let offsets = tiff
            .payload(sub)
            .ok_or(RawError::InvalidInput("DNG SubIFD offsets"))?;
        for i in 0..sub.count as usize {
            queue.push(
                u32_at(offsets, i * 4, tiff.endian)
                    .ok_or(RawError::InvalidInput("DNG SubIFD offset"))?,
            );
        }
    }
    let mut seen = vec![first];
    while let Some(rel) = queue.pop() {
        if rel == 0 || seen.contains(&rel) {
            continue;
        }
        if seen.len() >= 16 {
            return Err(RawError::ResourceLimit("DNG calibration IFD count"));
        }
        seen.push(rel);
        let (entries, next) = tiff
            .entries(rel)
            .ok_or(RawError::InvalidInput("DNG calibration IFD"))?;
        queue.push(next);
        if entries.iter().any(|e| {
            matches!(
                e.tag,
                50721
                    | 50722
                    | 50723
                    | 50724
                    | 50727
                    | 50728
                    | 50729
                    | 50778
                    | 50779
                    | 50964
                    | 50965
                    | 52525
                    | 52526
            )
        }) {
            return Err(RawError::InvalidInput("DNG calibration outside root IFD"));
        }
        for entry in entries.into_iter().filter(|e| e.tag == 330) {
            if entry.kind != 4 || entry.count > 8 {
                return Err(RawError::InvalidInput("DNG nested SubIFD type/count"));
            }
            let payload = tiff
                .payload(entry)
                .ok_or(RawError::InvalidInput("DNG nested SubIFD offsets"))?;
            for chunk in payload.chunks_exact(4) {
                let nested = match tiff.endian {
                    Endian::Little => u32::from_le_bytes(chunk.try_into().unwrap()),
                    Endian::Big => u32::from_be_bytes(chunk.try_into().unwrap()),
                };
                queue.push(nested);
            }
        }
    }
    if tiff.scalar(get(50778).ok_or(RawError::MissingCalibration("DNG illuminant 1"))?)
        != Some(settings.illuminants[0] as u32)
        || tiff.scalar(get(50779).ok_or(RawError::MissingCalibration("DNG illuminant 2"))?)
            != Some(settings.illuminants[1] as u32)
    {
        return Err(RawError::UnsupportedMode("DNG illuminant pair".into()));
    }
    if let Some(balance) = get(50727) {
        let values = rational_values(&tiff, balance, 3)
            .ok_or(RawError::MissingCalibration("DNG AnalogBalance"))?;
        if values.iter().any(|(n, d)| n != d) {
            return Err(RawError::UnsupportedMode(
                "DNG nonunity AnalogBalance".into(),
            ));
        }
    }
    let neutral = rational_values(
        &tiff,
        get(50728).ok_or(RawError::MissingCalibration("DNG AsShotNeutral"))?,
        3,
    )
    .ok_or(RawError::MissingCalibration("DNG AsShotNeutral"))?;
    let neutral = [0, 1, 2].map(|i| neutral[i].0 as f64 / neutral[i].1 as f64);
    let expected = [neutral[1] / neutral[0], 1.0, neutral[1] / neutral[2]];
    let actual = native.as_shot;
    let green = actual[1] as f64;
    if !green.is_finite()
        || green <= 0.0
        || expected
            .iter()
            .zip(actual)
            .any(|(e, a)| !e.is_finite() || (*e - a as f64 / green).abs() > 1.0e-3)
    {
        return Err(RawError::MissingCalibration(
            "DNG AsShotNeutral differs from decoder",
        ));
    }

    let parse_matrix = |tag| -> Result<([[f32; 3]; 4], String), RawError> {
        let entry = get(tag).ok_or(RawError::MissingCalibration("DNG ColorMatrix"))?;
        if entry.kind != 10 || entry.count != 9 {
            return Err(RawError::MissingCalibration("DNG ColorMatrix shape"));
        }
        let payload = tiff
            .payload(entry)
            .ok_or(RawError::InvalidInput("DNG ColorMatrix payload"))?;
        let mut matrix = [[0.0_f32; 3]; 4];
        for (row, matrix_row) in matrix.iter_mut().enumerate().take(3) {
            for (col, coefficient) in matrix_row.iter_mut().enumerate() {
                let i = row * 3 + col;
                let num = u32_at(payload, i * 8, tiff.endian)
                    .ok_or(RawError::InvalidInput("DNG ColorMatrix numerator"))?
                    as i32;
                let den = u32_at(payload, i * 8 + 4, tiff.endian)
                    .ok_or(RawError::InvalidInput("DNG ColorMatrix denominator"))?
                    as i32;
                if den == 0 {
                    return Err(RawError::MissingCalibration(
                        "DNG ColorMatrix zero denominator",
                    ));
                }
                let value = num as f64 / den as f64;
                if !value.is_finite() || value.abs() > 16.0 {
                    return Err(RawError::MissingCalibration("DNG ColorMatrix coefficient"));
                }
                *coefficient = value as f32;
            }
        }
        let m = matrix;
        let det = m[0][0] as f64
            * (m[1][1] as f64 * m[2][2] as f64 - m[1][2] as f64 * m[2][1] as f64)
            - m[0][1] as f64 * (m[1][0] as f64 * m[2][2] as f64 - m[1][2] as f64 * m[2][0] as f64)
            + m[0][2] as f64 * (m[1][0] as f64 * m[2][1] as f64 - m[1][1] as f64 * m[2][0] as f64);
        let norms = (0..3)
            .map(|row| {
                matrix[row]
                    .iter()
                    .map(|v| (*v as f64).powi(2))
                    .sum::<f64>()
                    .sqrt()
            })
            .collect::<Vec<_>>();
        if !det.is_finite()
            || norms.iter().any(|v| !v.is_finite() || *v <= 1e-8)
            || det.abs() <= norms.iter().product::<f64>() * 1e-9
        {
            return Err(RawError::MissingCalibration("DNG ColorMatrix singular"));
        }
        Ok((matrix, format!("{:x}", Sha256::digest(payload))))
    };
    let (matrix1, hash1) = parse_matrix(50721)?;
    let (matrix2, hash2) = parse_matrix(50722)?;
    Ok((
        if settings.selected_matrix == 1 {
            matrix1
        } else {
            matrix2
        },
        DngCalibrationMetadata {
            illuminants: settings.illuminants,
            color_matrix1_sha256: hash1,
            color_matrix2_sha256: hash2,
            selected: settings.calibration_identity.clone(),
        },
    ))
}

pub(super) fn classify_mode<'a>(
    catalog: &'a Catalog,
    native: &NativeMetadata,
    make: &str,
    model: &str,
    decoder: &str,
    bytes: &[u8],
) -> Result<(&'a Camera, &'a Mode), RawError> {
    let unsupported = || {
        RawError::UnsupportedMode(format!(
            "{make} {model}, {decoder}, {}bit {}x{}",
            native.raw_bps, native.width, native.height
        ))
    };
    let camera = catalog
        .cameras
        .iter()
        .find(|camera| camera.make == make && camera.model == model)
        .ok_or_else(unsupported)?;
    if !matches!(native.raw_count, 1 | 2)
        || camera.sensor_size != [native.width, native.height]
        || camera.cfa_size != [native.cfa_width, native.cfa_height]
    {
        return Err(unsupported());
    }
    let mode = camera
        .modes
        .iter()
        .find(|mode| {
            mode.bits == native.raw_bps
                && mode.raw_count == native.raw_count
                && mode.decoder == decoder
                && mode.dng_version.unwrap_or(0) == native.dng_version
                && mode.compression.as_ref().is_none_or(|compression| {
                    let value = match compression.probe {
                        CompressionProbe::NefMakerNote => nef_compression(bytes).map(u32::from),
                        CompressionProbe::RafHeader => raf_compression(bytes),
                    };
                    value == Some(compression.value)
                })
        })
        .ok_or_else(unsupported)?;
    Ok((camera, mode))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{RawSource, profiles::DngCorrections};
    #[test]
    fn truncated_inputs_are_rejected() {
        assert_eq!(raf_default_crop(b"FUJIFILM"), None);
        assert!(required_dng_opcodes(b"II*\0\x08\0\0").is_err());
        assert_eq!(nef_compression(b"II*\0\x08\0\0\0"), None);
    }
    fn opcode_tiff(flags: u32, payload_size: u32) -> Vec<u8> {
        let mut b = vec![0_u8; 46];
        b[..8].copy_from_slice(b"II*\0\x08\0\0\0");
        b[8..10].copy_from_slice(&1_u16.to_le_bytes());
        b[10..12].copy_from_slice(&51022_u16.to_le_bytes());
        b[12..14].copy_from_slice(&7_u16.to_le_bytes());
        b[14..18].copy_from_slice(&20_u32.to_le_bytes());
        b[18..22].copy_from_slice(&26_u32.to_le_bytes());
        b[26..30].copy_from_slice(&1_u32.to_be_bytes());
        b[30..34].copy_from_slice(&9_u32.to_be_bytes());
        b[34..38].copy_from_slice(&0x0103_0000_u32.to_be_bytes());
        b[38..42].copy_from_slice(&flags.to_be_bytes());
        b[42..46].copy_from_slice(&payload_size.to_be_bytes());
        b
    }
    #[test]
    fn empty_opcode_field_is_distinct_from_a_truncated_list() {
        let mut bytes = opcode_tiff(0, 0);
        bytes[14..18].copy_from_slice(&0_u32.to_le_bytes());
        assert!(dng_opcodes(&bytes).unwrap().is_empty());
        for len in 1_u32..4 {
            bytes[14..18].copy_from_slice(&len.to_le_bytes());
            assert!(matches!(
                dng_opcodes(&bytes),
                Err(RawError::InvalidInput("DNG opcode count"))
            ));
        }
    }

    #[test]
    fn mandatory_optional_and_malformed_dng_opcodes() {
        assert_eq!(required_dng_opcodes(&opcode_tiff(0, 0)).unwrap(), vec![9]);
        assert!(required_dng_opcodes(&opcode_tiff(1, 0)).unwrap().is_empty());
        assert!(matches!(
            required_dng_opcodes(&opcode_tiff(0, 1)),
            Err(RawError::InvalidInput(_))
        ));
        let mut cycle = opcode_tiff(1, 0);
        cycle[22..26].copy_from_slice(&8_u32.to_le_bytes());
        assert!(required_dng_opcodes(&cycle).unwrap().is_empty());
        let mut bad_ifd = opcode_tiff(0, 0);
        bad_ifd[4..8].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(matches!(
            required_dng_opcodes(&bad_ifd),
            Err(RawError::InvalidInput(_))
        ));
    }
    #[test]
    fn raf_camera_crop_bounds() {
        let mut b = vec![0_u8; 120];
        b[..8].copy_from_slice(b"FUJIFILM");
        b[92..96].copy_from_slice(&100_u32.to_be_bytes());
        b[100..104].copy_from_slice(&2_u32.to_be_bytes());
        b[104..106].copy_from_slice(&0x0110_u16.to_be_bytes());
        b[106..108].copy_from_slice(&4_u16.to_be_bytes());
        b[108..110].copy_from_slice(&21_u16.to_be_bytes());
        b[110..112].copy_from_slice(&12_u16.to_be_bytes());
        b[112..114].copy_from_slice(&0x0111_u16.to_be_bytes());
        b[114..116].copy_from_slice(&4_u16.to_be_bytes());
        b[116..118].copy_from_slice(&5152_u16.to_be_bytes());
        b[118..120].copy_from_slice(&7728_u16.to_be_bytes());
        assert_eq!(
            raf_default_crop(&b),
            Some(RawRect {
                x: 12,
                y: 21,
                width: 7728,
                height: 5152
            })
        );
        b[92..96].copy_from_slice(&u32::MAX.to_be_bytes());
        assert_eq!(raf_default_crop(&b), None);
    }

    fn put_entry(
        bytes: &mut [u8],
        ifd: usize,
        index: usize,
        tag: u16,
        kind: u16,
        count: u32,
        value: u32,
    ) {
        let p = ifd + 2 + index * 12;
        bytes[p..p + 2].copy_from_slice(&tag.to_le_bytes());
        bytes[p + 2..p + 4].copy_from_slice(&kind.to_le_bytes());
        bytes[p + 4..p + 8].copy_from_slice(&count.to_le_bytes());
        bytes[p + 8..p + 12].copy_from_slice(&value.to_le_bytes());
    }

    fn long_array(bytes: &mut [u8], offset: usize, values: &[u32]) {
        for (i, value) in values.iter().enumerate() {
            bytes[offset + i * 4..offset + i * 4 + 4].copy_from_slice(&value.to_le_bytes());
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn dng_fixture(
        bits: u32,
        compression: u32,
        active: Option<[u32; 4]>,
        crop_origin: [u32; 2],
        crop_size: [u32; 2],
        strip_offset: u32,
        strip_bytes: u32,
        sub_ifds: &[(u32, u32)],
    ) -> (Vec<u8>, NativeMetadata) {
        let width = 8;
        let height = 4;
        let root = 8;
        let mut bytes = vec![0_u8; 4096];
        bytes[..8].copy_from_slice(b"II*\0\x08\0\0\0");
        let root_count = if sub_ifds.is_empty() {
            if active.is_some() { 13 } else { 12 }
        } else {
            1
        };
        bytes[root..root + 2].copy_from_slice(&(root_count as u16).to_le_bytes());
        if sub_ifds.is_empty() {
            let entries = [
                (256, 4, 1, width),
                (257, 4, 1, height),
                (258, 3, 1, bits),
                (259, 3, 1, compression),
                (262, 3, 1, 32803),
                (273, 4, 1, strip_offset),
                (277, 3, 1, 1),
                (278, 4, 1, height),
                (279, 4, 1, strip_bytes),
                (284, 3, 1, 1),
            ];
            for (index, (tag, kind, count, value)) in entries.into_iter().enumerate() {
                put_entry(&mut bytes, root, index, tag, kind, count, value);
            }
        } else {
            put_entry(&mut bytes, root, 0, 330, 4, sub_ifds.len() as u32, 256);
            long_array(
                &mut bytes,
                256,
                &sub_ifds
                    .iter()
                    .map(|(offset, _)| *offset)
                    .collect::<Vec<_>>(),
            );
            for (offset, _) in sub_ifds {
                let count = if active.is_some() { 13 } else { 12 };
                bytes[*offset as usize..*offset as usize + 2]
                    .copy_from_slice(&(count as u16).to_le_bytes());
                let entries = [
                    (256, 4, 1, width),
                    (257, 4, 1, height),
                    (258, 3, 1, bits),
                    (259, 3, 1, compression),
                    (262, 3, 1, 32803),
                    (273, 4, 1, strip_offset),
                    (277, 3, 1, 1),
                    (278, 4, 1, height),
                    (279, 4, 1, strip_bytes),
                    (284, 3, 1, 1),
                ];
                for (index, (tag, kind, count, value)) in entries.into_iter().enumerate() {
                    put_entry(&mut bytes, *offset as usize, index, tag, kind, count, value);
                }
            }
        }
        let raw_ifds: Vec<usize> = if sub_ifds.is_empty() {
            vec![root]
        } else {
            sub_ifds
                .iter()
                .map(|(offset, _)| *offset as usize)
                .collect()
        };
        for ifd in raw_ifds {
            if let Some(area) = active {
                put_entry(&mut bytes, ifd, 10, 50829, 4, 4, 300);
                long_array(&mut bytes, 300, &area);
            }
            let base = if active.is_some() { 11 } else { 10 };
            put_entry(&mut bytes, ifd, base, 50719, 4, 2, 320);
            put_entry(&mut bytes, ifd, base + 1, 50720, 4, 2, 328);
        }
        long_array(&mut bytes, 320, &crop_origin);
        long_array(&mut bytes, 328, &crop_size);
        let mut native = RawSource::blank_native();
        native.width = width;
        native.height = height;
        native.active_width = width;
        native.active_height = height;
        native.raw_bps = bits;
        native.raw_count = 1;
        (bytes, native)
    }

    #[test]
    fn dng_container_root_ifd_defaults_active_area_to_full_sensor() {
        let (bytes, native) = dng_fixture(16, 1, None, [0, 0], [8, 4], 512, 64, &[]);
        let result =
            dng_container(&bytes, &native, &DngContainer::IntegerCfaSingleSegment, 0).unwrap();
        assert_eq!(
            result.default_crop,
            RawRect {
                x: 0,
                y: 0,
                width: 8,
                height: 4
            }
        );
    }

    #[test]
    fn dng_container_accepts_14bit_packed_and_compression_seven() {
        let (bytes, native) = dng_fixture(14, 7, Some([0, 0, 4, 8]), [1, 1], [6, 2], 512, 7, &[]);
        let result =
            dng_container(&bytes, &native, &DngContainer::IntegerCfaSingleSegment, 0).unwrap();
        assert_eq!(
            result.default_crop,
            RawRect {
                x: 1,
                y: 1,
                width: 6,
                height: 2
            }
        );
    }

    #[test]
    fn dng_container_accepts_padded_14bit_single_strip() {
        let (bytes, native) = dng_fixture(14, 1, Some([0, 0, 4, 8]), [0, 0], [8, 4], 512, 64, &[]);
        assert!(dng_container(&bytes, &native, &DngContainer::IntegerCfaSingleSegment, 0).is_ok());
    }

    #[test]
    fn dng_container_requires_exact_decoder_active_bottom_trim() {
        let (bytes, mut native) =
            dng_fixture(14, 1, Some([0, 0, 4, 8]), [0, 0], [8, 4], 512, 64, &[]);
        native.active_height = 2;
        assert!(dng_container(&bytes, &native, &DngContainer::IntegerCfaSingleSegment, 2).is_ok());
        assert!(matches!(
            dng_container(&bytes, &native, &DngContainer::IntegerCfaSingleSegment, 1),
            Err(RawError::InvalidInput(
                "DNG ActiveArea differs from decoder"
            ))
        ));

        let (shifted, native_shifted) =
            dng_fixture(14, 1, Some([0, 1, 4, 8]), [0, 0], [8, 4], 512, 64, &[]);
        assert!(matches!(
            dng_container(
                &shifted,
                &native_shifted,
                &DngContainer::IntegerCfaSingleSegment,
                0
            ),
            Err(RawError::InvalidInput(
                "DNG ActiveArea differs from decoder"
            ))
        ));

        let (overflow, native_overflow) =
            dng_fixture(14, 1, Some([0, 0, 4, 9]), [0, 0], [8, 4], 512, 64, &[]);
        assert!(matches!(
            dng_container(
                &overflow,
                &native_overflow,
                &DngContainer::IntegerCfaSingleSegment,
                0
            ),
            Err(RawError::InvalidInput("DNG ActiveArea bounds"))
        ));

        let (outside, mut native_outside) =
            dng_fixture(14, 1, Some([0, 0, 4, 8]), [0, 3], [8, 4], 512, 64, &[]);
        native_outside.active_height = 2;
        assert!(matches!(
            dng_container(
                &outside,
                &native_outside,
                &DngContainer::IntegerCfaSingleSegment,
                2
            ),
            Err(RawError::InvalidInput("DNG default crop bounds"))
        ));
    }

    #[test]
    fn dng_container_accepts_one_full_sensor_tile() {
        let (mut bytes, native) = dng_fixture(14, 1, None, [0, 0], [8, 4], 512, 64, &[]);
        // Turn the fixture's single strip into one full-sensor tile. Reuse
        // the crop slots for TileWidth/TileLength and move the crop tags
        // to new entries; crop defaults are independent of ActiveArea.
        bytes[8..10].copy_from_slice(&14_u16.to_le_bytes());
        put_entry(&mut bytes, 8, 5, 324, 4, 1, 2048);
        put_entry(&mut bytes, 8, 8, 325, 4, 1, 64);
        put_entry(&mut bytes, 8, 10, 322, 4, 1, 8);
        put_entry(&mut bytes, 8, 11, 323, 4, 1, 4);
        put_entry(&mut bytes, 8, 12, 50719, 4, 2, 320);
        put_entry(&mut bytes, 8, 13, 50720, 4, 2, 328);
        let result = dng_container(&bytes, &native, &DngContainer::IntegerCfaSingleSegment, 0);
        assert_eq!(
            result.unwrap().default_crop,
            RawRect {
                x: 0,
                y: 0,
                width: 8,
                height: 4
            }
        );
    }

    #[test]
    fn dng_container_rejects_ambiguous_raw_subifds_and_bad_strip_bounds() {
        let (bytes, native) = dng_fixture(
            16,
            1,
            None,
            [0, 0],
            [8, 4],
            2048,
            64,
            &[(512, 0), (1024, 0)],
        );
        let result = dng_container(&bytes, &native, &DngContainer::IntegerCfaSingleSegment, 0);
        assert!(matches!(
            result,
            Err(RawError::InvalidInput("ambiguous DNG raw SubIFD"))
        ));
        let (bytes, native) = dng_fixture(16, 1, Some([0, 0, 4, 8]), [0, 0], [8, 4], 4090, 64, &[]);
        assert!(matches!(
            dng_container(&bytes, &native, &DngContainer::IntegerCfaSingleSegment, 0),
            Err(RawError::UnsupportedMode(_))
        ));
    }

    #[test]
    fn dng_container_rejects_out_of_bounds_integer_crop() {
        let (mut bytes, native) =
            dng_fixture(16, 1, Some([0, 0, 4, 8]), [1, 1], [6, 2], 512, 64, &[]);
        bytes[328..332].copy_from_slice(&9_u32.to_le_bytes());
        let result = dng_container(&bytes, &native, &DngContainer::IntegerCfaSingleSegment, 0);
        assert!(matches!(
            result,
            Err(RawError::InvalidInput("DNG default crop bounds"))
        ));
    }

    #[test]
    fn dng_calibration_rejects_nonidentity_camera_calibration() {
        let mut bytes = vec![0_u8; 1024];
        bytes[..8].copy_from_slice(b"II*\0\x08\0\0\0");
        bytes[8..10].copy_from_slice(&1_u16.to_le_bytes());
        put_entry(&mut bytes, 8, 0, 50723, 10, 9, 128);
        for i in 0..9 {
            let value = if i == 1 || i % 4 == 0 { 1 } else { 0 };
            bytes[128 + i * 8..132 + i * 8].copy_from_slice(&(value as u32).to_le_bytes());
            bytes[132 + i * 8..136 + i * 8].copy_from_slice(&1_u32.to_le_bytes());
        }
        let native = RawSource::blank_native();
        let settings = Dng {
            container: DngContainer::IntegerCfaSingleSegment,
            calibration: DngCalibration::RootFixedMatrix,
            illuminants: [17, 21],
            selected_matrix: 1,
            calibration_identity: "test".into(),
            corrections: DngCorrections::Stage3GainMapThenWarp,
            interpretation: "test".into(),
            required_opcodes: Vec::new(),
            decoder_active_bottom_trim: 0,
        };
        assert!(
            matches!(dng_color_calibration(&bytes, &native, &settings), Err(RawError::UnsupportedMode(message)) if message == "DNG nonidentity CameraCalibration")
        );
    }
}
