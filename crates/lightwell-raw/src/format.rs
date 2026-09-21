//! Narrow, bounded container inspection for the qualified modes. LibRaw still
//! owns decompression; these reads validate recording-mode and crop semantics.
use super::{NativeMetadata, RawError, RawMode, RawRect};

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

fn nikon_compression(bytes: &[u8]) -> Option<u16> {
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

/// IDs of DNG opcodes whose optional flag is unset. The DNG1.x opcode lists
/// use a big-endian count and 16-byte per-opcode headers even in little-endian
/// TIFF containers. Malformed lists fail explicitly.
pub fn required_dng_opcodes(bytes: &[u8]) -> Result<Vec<u32>, RawError> {
    let (tiff, first) = Tiff::header(bytes, 0).ok_or(RawError::InvalidInput("DNG TIFF header"))?;
    let mut queue = vec![first];
    let mut seen = Vec::new();
    let mut required = Vec::new();
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
                let data = tiff
                    .payload(entry)
                    .ok_or(RawError::InvalidInput("DNG opcode list bounds"))?;
                let count = u32_at(data, 0, Endian::Big)
                    .ok_or(RawError::InvalidInput("DNG opcode count"))?
                    as usize;
                if count > 256 {
                    return Err(RawError::ResourceLimit("DNG opcode count"));
                }
                let mut p = 4usize;
                for _ in 0..count {
                    let id = u32_at(data, p, Endian::Big)
                        .ok_or(RawError::InvalidInput("DNG opcode ID"))?;
                    let flags = u32_at(data, p + 8, Endian::Big)
                        .ok_or(RawError::InvalidInput("DNG opcode flags"))?;
                    let len = u32_at(data, p + 12, Endian::Big)
                        .ok_or(RawError::InvalidInput("DNG opcode length"))?
                        as usize;
                    p = p
                        .checked_add(16)
                        .and_then(|v| v.checked_add(len))
                        .ok_or(RawError::ResourceLimit("DNG opcode size"))?;
                    if p > data.len() {
                        return Err(RawError::InvalidInput("truncated DNG opcode"));
                    }
                    if flags & 1 == 0 {
                        required.push(id);
                    }
                }
                if p != data.len() {
                    return Err(RawError::InvalidInput("DNG opcode trailing data"));
                }
            }
        }
    }
    required.sort_unstable();
    required.dedup();
    Ok(required)
}

pub(super) fn classify_mode(
    native: &NativeMetadata,
    make: &str,
    model: &str,
    decoder: &str,
    bytes: &[u8],
) -> Result<RawMode, RawError> {
    match (make, model) {
        ("Nikon", "Z 6")
            if native.width == 6064
                && native.height == 4040
                && decoder == "nikon_load_raw()"
                && native.cfa_width == 2
                && native.cfa_height == 2 =>
        {
            if nikon_compression(bytes) != Some(3) {
                return Err(RawError::UnsupportedMode(
                    "Nikon Z6 NEF must be lossless compressed".into(),
                ));
            }
            match native.raw_bps {
                12 => Ok(RawMode::NikonZ6Lossless12),
                14 => Ok(RawMode::NikonZ6Lossless14),
                _ => Err(RawError::UnsupportedMode(format!(
                    "Nikon Z6 {} bit",
                    native.raw_bps
                ))),
            }
        }
        ("Fujifilm", "X100VI")
            if native.width == 7872
                && native.height == 5196
                && native.raw_bps == 14
                && native.cfa_width == 6
                && native.cfa_height == 6 =>
        {
            match (raf_compression(bytes), decoder) {
                (Some(0), "unpacked_load_raw()") => Ok(RawMode::FujifilmX100ViUncompressed14),
                (Some(2), "fuji_compressed_load_raw()") => Ok(RawMode::FujifilmX100ViLossless14),
                (other, _) => Err(RawError::UnsupportedMode(format!(
                    "X100VI RAF compression {other:?} via {decoder}"
                ))),
            }
        }
        ("DJI", "FC3411") => {
            let required = required_dng_opcodes(bytes)?;
            if !required.is_empty() {
                return Err(RawError::UnsupportedRequiredOpcodes(required));
            }
            Err(RawError::UnsupportedMode(
                "DJI DNG correction path is not qualified".into(),
            ))
        }
        _ => Err(RawError::UnsupportedMode(format!(
            "{make} {model}, {decoder}, {}bit {}x{}",
            native.raw_bps, native.width, native.height
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn truncated_inputs_are_rejected() {
        assert_eq!(raf_default_crop(b"FUJIFILM"), None);
        assert!(required_dng_opcodes(b"II*\0\x08\0\0").is_err());
        assert_eq!(nikon_compression(b"II*\0\x08\0\0\0"), None);
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
}
