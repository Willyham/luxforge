//! Narrow, bounded container inspection for the qualified modes. LibRaw still
//! owns decompression; these reads validate recording-mode and crop semantics.
use super::opcodes::{OPCODE_LIST1, OPCODE_LIST2, OPCODE_LIST3};
use super::profiles::{Camera, Catalog, CompressionProbe, Dng, DngCalibration, DngContainer, Mode};
use super::{DngCalibrationMetadata, NativeIdentity, NativeMetadata, RawError, RawRect};
use sha2::{Digest, Sha256};

/// The most IFDs one [`Tiff::walk`] visits: the root chain, its SubIFDs and theirs.
const MAX_IFDS: usize = 16;
/// The most IFD offsets one SubIFDs entry lists.
const MAX_SUB_IFDS: u32 = 8;

/// The crate's one set of bounded byte readers: every container and DNG opcode field is read
/// through these, `None` when it runs past the buffer.
#[derive(Clone, Copy)]
pub(crate) enum Endian {
    Little,
    Big,
}
pub(crate) fn u16_at(b: &[u8], p: usize, e: Endian) -> Option<u16> {
    let a = b.get(p..p.checked_add(2)?)?;
    Some(match e {
        Endian::Little => u16::from_le_bytes([a[0], a[1]]),
        Endian::Big => u16::from_be_bytes([a[0], a[1]]),
    })
}
pub(crate) fn u32_at(b: &[u8], p: usize, e: Endian) -> Option<u32> {
    let a = b.get(p..p.checked_add(4)?)?;
    Some(match e {
        Endian::Little => u32::from_le_bytes(a.try_into().ok()?),
        Endian::Big => u32::from_be_bytes(a.try_into().ok()?),
    })
}
pub(crate) fn f64_at(b: &[u8], p: usize, e: Endian) -> Option<f64> {
    let a = b.get(p..p.checked_add(8)?)?;
    Some(f64::from_bits(match e {
        Endian::Little => u64::from_le_bytes(a.try_into().ok()?),
        Endian::Big => u64::from_be_bytes(a.try_into().ok()?),
    }))
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

    /// Visit every IFD reachable from `first` through next-IFD links and SubIFDs (tag 330),
    /// each once and at most [`MAX_IFDS`] of them, with its offset and entries. An IFD's SubIFDs
    /// are visited before its next link, the last listed first. A next link of zero ends its
    /// chain. The first IFD and every SubIFD offset must be nonzero, and a SubIFDs entry must
    /// list one to [`MAX_SUB_IFDS`] LONG offsets inside the file: anything else fails the walk
    /// rather than being skipped, as does an IFD that cannot be read.
    fn walk(
        &self,
        first: u32,
        mut visit: impl FnMut(u32, &[Entry]) -> Result<(), RawError>,
    ) -> Result<(), RawError> {
        if first == 0 {
            return Err(RawError::InvalidInput("DNG IFD offset"));
        }
        let mut queue = vec![first];
        let mut seen = Vec::new();
        while let Some(rel) = queue.pop() {
            if seen.contains(&rel) {
                continue;
            }
            if seen.len() >= MAX_IFDS {
                return Err(RawError::ResourceLimit("DNG IFD count"));
            }
            seen.push(rel);
            let (entries, next) = self.entries(rel).ok_or(RawError::InvalidInput("DNG IFD"))?;
            if next != 0 {
                queue.push(next);
            }
            for entry in entries.iter().copied().filter(|e| e.tag == 330) {
                if entry.kind != 4 || entry.count == 0 || entry.count > MAX_SUB_IFDS {
                    return Err(RawError::InvalidInput("DNG SubIFD type/count"));
                }
                let offsets = self
                    .payload(entry)
                    .ok_or(RawError::InvalidInput("DNG SubIFD offsets"))?;
                for i in 0..entry.count as usize {
                    match u32_at(offsets, i * 4, self.endian) {
                        Some(0) | None => return Err(RawError::InvalidInput("DNG SubIFD offset")),
                        Some(offset) => queue.push(offset),
                    }
                }
            }
            visit(rel, &entries)?;
        }
        Ok(())
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

/// What refusing Nikon High Efficiency says: the format, and the mode to record instead.
pub(crate) const NIKON_HIGH_EFFICIENCY: &str =
    "Nikon High Efficiency (HE/HE*) is not supported; record Lossless compressed RAW instead";
/// The Nikon maker-note NEF compression values LibRaw documents for High Efficiency (13) and
/// High Efficiency★ (14).
const NEF_HIGH_EFFICIENCY: [u16; 2] = [13, 14];
/// The JPEG XS start-of-codestream and capabilities markers that begin a High Efficiency raw
/// strip, as LibRaw tests them at its raw IFD's data offset.
const JPEG_XS_SOC_CAP: [u8; 4] = [0xff, 0x10, 0xff, 0x50];

/// Refuse a Nikon High Efficiency NEF before any native open or unpack, whatever the camera
/// model and whether or not the catalog lists it. A Nikon file is refused when its maker note
/// records compression 13 or 14, or when its full-size raw data begins with the JPEG XS markers.
/// Both reads are bounded: the maker note through [`nef_compression`], and four bytes at the
/// largest image IFD's first strip or tile, found by a bounded [`Tiff::walk`]. A file whose
/// structure these reads cannot follow is not refused here; the decoder then judges it.
pub(super) fn reject_nikon_high_efficiency(bytes: &[u8]) -> Result<(), RawError> {
    if nikon_high_efficiency(bytes) {
        Err(RawError::UnsupportedCompression(NIKON_HIGH_EFFICIENCY))
    } else {
        Ok(())
    }
}

fn nikon_high_efficiency(bytes: &[u8]) -> bool {
    let Some((tiff, first)) = Tiff::header(bytes, 0) else {
        return false;
    };
    if nef_compression(bytes).is_some_and(|value| NEF_HIGH_EFFICIENCY.contains(&value)) {
        return true;
    }
    nikon_make(&tiff, first)
        && raw_data_head(&tiff, first).is_some_and(|head| head == JPEG_XS_SOC_CAP)
}

/// Whether the root IFD's Make (271) names Nikon.
fn nikon_make(tiff: &Tiff<'_>, first: u32) -> bool {
    tiff.entries(first)
        .and_then(|(entries, _)| {
            let make = entries.into_iter().find(|e| e.tag == 271 && e.kind == 2)?;
            tiff.payload(make)
        })
        .is_some_and(|make| make.starts_with(b"NIKON"))
}

/// The first four bytes of the image data of the largest IFD reachable from `first`, the one
/// LibRaw decodes as the raw image: at its first StripOffsets (273) or TileOffsets (324) value.
/// A walk that fails part way keeps the IFDs it read.
fn raw_data_head(tiff: &Tiff<'_>, first: u32) -> Option<[u8; 4]> {
    let mut raw: Option<(u64, u32)> = None;
    let _ = tiff.walk(first, |_, entries| {
        let get = |tag| entries.iter().copied().find(|e| e.tag == tag);
        let dimension = |tag| get(tag).and_then(|e| tiff.scalar(e));
        let (Some(width), Some(height)) = (dimension(256), dimension(257)) else {
            return Ok(());
        };
        let offset = get(273).or_else(|| get(324)).and_then(|e| {
            let values = tiff.payload(e)?;
            match e.kind {
                3 => u16_at(values, 0, tiff.endian).map(u32::from),
                4 => u32_at(values, 0, tiff.endian),
                _ => None,
            }
        });
        let area = u64::from(width) * u64::from(height);
        if let Some(offset) = offset.filter(|&offset| offset != 0)
            && raw.is_none_or(|(largest, _)| area > largest)
        {
            raw = Some((area, offset));
        }
        Ok(())
    });
    let start = tiff.base.checked_add(raw?.1 as usize)?;
    tiff.data.get(start..start.checked_add(4)?)?.try_into().ok()
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
    let mut opcodes = Vec::new();
    let mut aggregate_opcode_bytes = 0usize;
    tiff.walk(first, |rel, entries| {
        for &entry in entries {
            if !matches!(entry.tag, OPCODE_LIST1 | OPCODE_LIST2 | OPCODE_LIST3) {
                continue;
            }
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
                let id =
                    u32_at(data, p, Endian::Big).ok_or(RawError::InvalidInput("DNG opcode ID"))?;
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
        Ok(())
    })?;
    Ok(opcodes)
}

/// The sorted, distinct IDs of `opcodes` whose optional flag is unset.
pub(crate) fn required_ids<'a>(opcodes: impl IntoIterator<Item = &'a DngOpcode>) -> Vec<u32> {
    let mut ids: Vec<_> = opcodes
        .into_iter()
        .filter(|opcode| opcode.flags & 1 == 0)
        .map(|opcode| opcode.id)
        .collect();
    ids.sort_unstable();
    ids.dedup();
    ids
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
    let mut candidate = None;
    tiff.walk(first, |rel, entries| {
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
            return Ok(());
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
        Ok(())
    })?;
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
    let (root, _) = tiff
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
    tiff.walk(first, |rel, entries| {
        let calibration = |e: &Entry| {
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
        };
        if rel != first && entries.iter().any(calibration) {
            return Err(RawError::InvalidInput("DNG calibration outside root IFD"));
        }
        Ok(())
    })?;
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
        let det = crate::mat3::determinant([0, 1, 2].map(|row| matrix[row].map(f64::from)));
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
            selected: settings.calibration_identity.to_string(),
        },
    ))
}

/// The catalogued camera and recording mode of an identified, not yet unpacked, file: its
/// identity from LibRaw's identify, and the container's compression marker where the mode
/// declares one.
pub(super) fn classify_mode<'a>(
    catalog: &'a Catalog,
    native: &NativeIdentity,
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
        || camera.cfa_size != [native.cfa_width, native.cfa_height]
    {
        return Err(unsupported());
    }
    let mode = camera
        .modes
        .iter()
        .find(|mode| {
            mode.frame(camera) == [native.width, native.height]
                && mode.bits == native.raw_bps
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

    /// IDs of DNG opcodes whose optional flag is unset. Malformed lists fail.
    fn required_dng_opcodes(bytes: &[u8]) -> Result<Vec<u32>, RawError> {
        Ok(required_ids(&dng_opcodes(bytes)?))
    }
    #[test]
    fn truncated_inputs_are_rejected() {
        assert_eq!(raf_default_crop(b"FUJIFILM"), None);
        assert!(required_dng_opcodes(b"II*\0\x08\0\0").is_err());
        assert_eq!(nef_compression(b"II*\0\x08\0\0\0"), None);
    }
    /// Where a synthetic NEF records its maker-note NEF compression, if anywhere.
    #[derive(Clone, Copy)]
    enum NefCompression {
        /// No maker note: the Exif IFD's one entry is not tag 0x927c.
        Absent,
        /// Tag 0x93, a SHORT, as older bodies write it.
        Tag93(u16),
        /// Tag 0x51, the 24-byte block whose u16 at 10 Z bodies write.
        Tag51(u16),
    }
    const NEF_RAW: usize = 512;
    const NEF_PREVIEW: usize = 600;
    const NEF_MAKER: usize = 256;
    const LOSSLESS_HEAD: [u8; 4] = [0xd2, 0xc3, 0x50, 0xfc];

    /// A little-endian NEF: a root IFD with Make, an Exif link, one SubIFD and a 2x2 preview strip
    /// at [`NEF_PREVIEW`]; the Exif IFD's Nikon maker note; and the 8x4 raw SubIFD, whose strip at
    /// [`NEF_RAW`] begins with `raw_head`.
    fn nef_fixture(make: &[u8; 18], compression: NefCompression, raw_head: [u8; 4]) -> Vec<u8> {
        let mut b = vec![0_u8; 1024];
        b[..8].copy_from_slice(b"II*\0\x08\0\0\0");
        b[8..10].copy_from_slice(&6_u16.to_le_bytes());
        put_entry(&mut b, 8, 0, 271, 2, 18, 96);
        put_entry(&mut b, 8, 1, 0x8769, 4, 1, 128);
        put_entry(&mut b, 8, 2, 330, 4, 1, 160);
        put_entry(&mut b, 8, 3, 256, 4, 1, 2);
        put_entry(&mut b, 8, 4, 257, 4, 1, 2);
        put_entry(&mut b, 8, 5, 273, 4, 1, NEF_PREVIEW as u32);
        b[96..114].copy_from_slice(make);
        b[128..130].copy_from_slice(&1_u16.to_le_bytes());
        let maker_tag = if matches!(compression, NefCompression::Absent) {
            0x927d
        } else {
            0x927c
        };
        put_entry(&mut b, 128, 0, maker_tag, 7, 96, NEF_MAKER as u32);
        b[160..162].copy_from_slice(&4_u16.to_le_bytes());
        put_entry(&mut b, 160, 0, 254, 4, 1, 0);
        put_entry(&mut b, 160, 1, 256, 4, 1, 8);
        put_entry(&mut b, 160, 2, 257, 4, 1, 4);
        put_entry(&mut b, 160, 3, 273, 4, 1, NEF_RAW as u32);
        // The maker note: signature, version, then a TIFF whose offsets are relative to it.
        let nested = NEF_MAKER + 10;
        b[NEF_MAKER..nested].copy_from_slice(b"Nikon\0\x02\x10\0\0");
        b[nested..nested + 8].copy_from_slice(b"II*\0\x08\0\0\0");
        b[nested + 8..nested + 10].copy_from_slice(&1_u16.to_le_bytes());
        match compression {
            NefCompression::Absent => {}
            NefCompression::Tag93(value) => {
                put_entry(&mut b, nested + 8, 0, 0x93, 3, 1, u32::from(value))
            }
            NefCompression::Tag51(value) => {
                put_entry(&mut b, nested + 8, 0, 0x51, 7, 24, 40);
                b[nested + 40..nested + 44].copy_from_slice(b"0102");
                b[nested + 50..nested + 52].copy_from_slice(&value.to_le_bytes());
            }
        }
        b[NEF_RAW..NEF_RAW + 4].copy_from_slice(&raw_head);
        b[NEF_PREVIEW..NEF_PREVIEW + 4].copy_from_slice(&[0x80; 4]);
        b
    }
    const NIKON: &[u8; 18] = b"NIKON CORPORATION\0";
    fn refused(bytes: &[u8]) -> bool {
        match reject_nikon_high_efficiency(bytes) {
            Ok(()) => false,
            Err(error) => {
                assert_eq!(
                    error,
                    RawError::UnsupportedCompression(NIKON_HIGH_EFFICIENCY)
                );
                true
            }
        }
    }

    /// Maker-note compression 13 (High Efficiency) and 14 (High Efficiency★) are refused through
    /// either tag Nikon writes it in; lossless (3) and other values pass to the decoder.
    #[test]
    fn nikon_high_efficiency_maker_note_values_are_refused() {
        for tag in [NefCompression::Tag93, NefCompression::Tag51] {
            for value in [13, 14] {
                let bytes = nef_fixture(NIKON, tag(value), LOSSLESS_HEAD);
                assert_eq!(nef_compression(&bytes), Some(value));
                assert!(refused(&bytes), "compression {value}");
            }
            for value in [1, 2, 3, 4, 12, 15, 0xff0d] {
                let bytes = nef_fixture(NIKON, tag(value), LOSSLESS_HEAD);
                assert_eq!(nef_compression(&bytes), Some(value));
                assert!(!refused(&bytes), "compression {value}");
            }
        }
        assert_eq!(
            RawError::UnsupportedCompression(NIKON_HIGH_EFFICIENCY).to_string(),
            "unsupported RAW compression: Nikon High Efficiency (HE/HE*) is not supported; \
             record Lossless compressed RAW instead"
        );
    }

    /// The JPEG XS start-of-codestream and capabilities markers at the full-size raw strip refuse
    /// a Nikon file whatever its maker note says. The same bytes in the smaller preview strip, or
    /// in a file whose Make is not Nikon, do not.
    #[test]
    fn jpeg_xs_markers_at_the_nikon_raw_strip_are_refused() {
        for compression in [
            NefCompression::Absent,
            NefCompression::Tag93(3),
            NefCompression::Tag51(3),
        ] {
            assert!(refused(&nef_fixture(NIKON, compression, JPEG_XS_SOC_CAP)));
            assert!(!refused(&nef_fixture(NIKON, compression, LOSSLESS_HEAD)));
            let mut preview = nef_fixture(NIKON, compression, LOSSLESS_HEAD);
            preview[NEF_PREVIEW..NEF_PREVIEW + 4].copy_from_slice(&JPEG_XS_SOC_CAP);
            assert!(!refused(&preview));
        }
        let other = nef_fixture(
            b"OTHER CORPORATION\0",
            NefCompression::Absent,
            JPEG_XS_SOC_CAP,
        );
        assert!(!refused(&other));
        // A strip offset past the end reads nothing.
        let mut outside = nef_fixture(NIKON, NefCompression::Absent, JPEG_XS_SOC_CAP);
        put_entry(&mut outside, 160, 3, 273, 4, 1, 1022);
        assert!(!refused(&outside));
    }

    /// Absent, truncated and malformed maker notes and containers are never refused and never
    /// panic: whatever the bounded reads cannot follow passes to the decoder, which judges it.
    #[test]
    fn malformed_nikon_containers_are_not_refused() {
        let lossless = nef_fixture(NIKON, NefCompression::Tag51(3), LOSSLESS_HEAD);
        let absent = nef_fixture(NIKON, NefCompression::Absent, LOSSLESS_HEAD);
        assert_eq!(nef_compression(&absent), None);
        assert!(!refused(&absent));
        for len in 0..lossless.len() {
            assert!(!refused(&lossless[..len]), "prefix {len}");
        }
        // Every prefix of a High Efficiency file is either refused or passed, without a panic.
        let he = nef_fixture(NIKON, NefCompression::Tag51(14), JPEG_XS_SOC_CAP);
        for len in 0..he.len() {
            let _ = refused(&he[..len]);
        }
        for position in 0..lossless.len() {
            for value in [0x00, 0xff] {
                let mut mutated = lossless.clone();
                mutated[position] = value;
                assert!(!refused(&mutated), "byte {position} = {value:#x}");
            }
        }
        let malformed = |edit: &dyn Fn(&mut Vec<u8>)| {
            let mut bytes = nef_fixture(NIKON, NefCompression::Tag93(13), LOSSLESS_HEAD);
            edit(&mut bytes);
            bytes
        };
        let nested = NEF_MAKER + 10;
        for bytes in [
            // Not a Nikon maker note signature.
            malformed(&|b| b[NEF_MAKER..NEF_MAKER + 6].copy_from_slice(b"Nikoo\0")),
            // A nested TIFF header that is not one.
            malformed(&|b| b[nested..nested + 2].copy_from_slice(b"XX")),
            // A nested IFD offset past the end, and an entry count past the bound.
            malformed(&|b| b[nested + 4..nested + 8].copy_from_slice(&u32::MAX.to_le_bytes())),
            malformed(&|b| b[nested + 8..nested + 10].copy_from_slice(&1000_u16.to_le_bytes())),
            // A compression entry of the wrong type or count.
            malformed(&|b| put_entry(b, nested + 8, 0, 0x93, 4, 2, 13)),
            // An Exif link or maker-note payload past the end.
            malformed(&|b| put_entry(b, 8, 1, 0x8769, 4, 1, u32::MAX)),
            malformed(&|b| put_entry(b, 128, 0, 0x927c, 7, 96, 1000)),
            // A maker note too short to hold its signature and nested header.
            malformed(&|b| put_entry(b, 128, 0, 0x927c, 7, 12, NEF_MAKER as u32)),
        ] {
            assert!(!refused(&bytes));
        }
        assert!(refused(&malformed(&|_| {})));
    }

    /// The decoder refuses High Efficiency from the container, before the native open: LibRaw
    /// could not open these synthetic bytes, so only the pre-check can give this error.
    #[test]
    fn decode_refuses_nikon_high_efficiency_before_the_native_open() {
        let cancel = std::sync::atomic::AtomicBool::new(false);
        for bytes in [
            nef_fixture(NIKON, NefCompression::Tag51(13), LOSSLESS_HEAD),
            nef_fixture(NIKON, NefCompression::Tag93(14), LOSSLESS_HEAD),
            nef_fixture(NIKON, NefCompression::Absent, JPEG_XS_SOC_CAP),
        ] {
            assert_eq!(
                RawSource::decode(bytes, &cancel).err(),
                Some(RawError::UnsupportedCompression(NIKON_HIGH_EFFICIENCY))
            );
        }
        // The same container recording lossless passes the check and fails later, in LibRaw.
        let lossless = nef_fixture(NIKON, NefCompression::Tag51(3), LOSSLESS_HEAD);
        let error = RawSource::decode(lossless, &cancel).err().unwrap();
        assert!(
            !matches!(error, RawError::UnsupportedCompression(_)),
            "{error}"
        );
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
    /// The owner's DJI Air 2S DNG requires exactly WarpRectilinear (1) and GainMap (9).
    #[test]
    #[ignore = "requires explicit local authentic DJI DNG"]
    fn owner_dji_dng_requires_warp_and_gain_map() {
        use sha2::{Digest, Sha256};
        let owner = std::env::var("LUXFORGE_RAW_OWNER_DIR").expect("owner fixture directory");
        let bytes = std::fs::read(format!("{owner}/mavic_air_2s.DNG")).expect("read DJI DNG");
        assert_eq!(
            format!("{:x}", Sha256::digest(&bytes)),
            "aab79ce1795a7dd5f1c2e52ec7bd07345cb9bda0262d1d5aa3701db212b09e1d"
        );
        assert_eq!(required_dng_opcodes(&bytes).unwrap(), vec![1, 9]);
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
        assert!(
            matches!(dng_color_calibration(&bytes, &native, &test_dng()), Err(RawError::UnsupportedMode(message)) if message == "DNG nonidentity CameraCalibration")
        );
    }

    fn test_dng() -> Dng {
        Dng {
            container: DngContainer::IntegerCfaSingleSegment,
            calibration: DngCalibration::RootFixedMatrix,
            illuminants: [17, 21],
            selected_matrix: 1,
            calibration_identity: "test".into(),
            corrections: DngCorrections::Stage3GainMapThenWarp,
            interpretation: "test".into(),
            optics: None,
            required_opcodes: Vec::new().into(),
            decoder_active_bottom_trim: 0,
        }
    }

    /// The opcode, sensor-container and calibration walks share one bounds policy: a SubIFDs
    /// entry lists one to eight nonzero LONG offsets and a walk visits at most sixteen IFDs.
    /// Anything else fails each of them the same explicit way, where they once disagreed on a
    /// zero offset or a zero count.
    #[test]
    fn every_container_walk_fails_the_same_malformed_ifds_explicitly() {
        let walks = |bytes: &[u8]| -> [Result<(), RawError>; 3] {
            let native = RawSource::blank_native();
            [
                dng_opcodes(bytes).map(drop),
                dng_container(bytes, &native, &DngContainer::IntegerCfaSingleSegment, 0).map(drop),
                dng_color_calibration(bytes, &native, &test_dng()).map(drop),
            ]
        };
        // A root IFD whose one entry lists `count` SubIFDs from `value`; an empty IFD at 64.
        let sub_ifds = |count: u32, value: u32| {
            let mut bytes = vec![0_u8; 128];
            bytes[..8].copy_from_slice(b"II*\0\x08\0\0\0");
            bytes[8..10].copy_from_slice(&1_u16.to_le_bytes());
            put_entry(&mut bytes, 8, 0, 330, 4, count, value);
            bytes
        };
        for (bytes, error) in [
            (sub_ifds(0, 64), "DNG SubIFD type/count"),
            (sub_ifds(9, 64), "DNG SubIFD type/count"),
            (sub_ifds(1, 0), "DNG SubIFD offset"),
        ] {
            for result in walks(&bytes) {
                assert_eq!(result, Err(RawError::InvalidInput(error)));
            }
        }
        let mut short_type = sub_ifds(1, 64);
        short_type[12..14].copy_from_slice(&3_u16.to_le_bytes());
        for result in walks(&short_type) {
            assert_eq!(result, Err(RawError::InvalidInput("DNG SubIFD type/count")));
        }
        let [opcodes, container, calibration] = walks(&sub_ifds(1, 64));
        assert_eq!(opcodes, Ok(()));
        assert!(matches!(container, Err(RawError::UnsupportedMode(_))));
        assert_eq!(
            calibration,
            Err(RawError::MissingCalibration("DNG illuminant 1"))
        );

        // A chain of empty IFDs, each six bytes: sixteen walk, seventeen do not.
        let chain = |ifds: usize| {
            let mut bytes = vec![0_u8; 8 + ifds * 6];
            bytes[..8].copy_from_slice(b"II*\0\x08\0\0\0");
            for ifd in 0..ifds - 1 {
                let next = (8 + (ifd + 1) * 6) as u32;
                bytes[8 + ifd * 6 + 2..8 + ifd * 6 + 6].copy_from_slice(&next.to_le_bytes());
            }
            bytes
        };
        assert_eq!(walks(&chain(16))[0], Ok(()));
        for result in walks(&chain(17)) {
            assert_eq!(result, Err(RawError::ResourceLimit("DNG IFD count")));
        }
        let mut no_root = chain(1);
        no_root[4..8].fill(0);
        assert_eq!(
            dng_opcodes(&no_root).map(drop),
            Err(RawError::InvalidInput("DNG IFD offset"))
        );
    }
}
