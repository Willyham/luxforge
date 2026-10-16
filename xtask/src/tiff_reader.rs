//! The alignment rigs' TIFF reader: what Lightroom Classic writes with compression None, an
//! uncompressed (compression 1), chunky (planar configuration 1), 8- or 16-bit RGB TIFF of either
//! byte order, and its embedded XMP packet (tag 700). Anything else is refused by name rather than
//! decoded approximately. Pinned tooling for `grade-align` and `lr-align`, never an editor
//! dependency.

use std::collections::HashMap;

/// A decoded image: row-major RGB, each value a 16-bit code (an 8-bit file's codes times 257), and
/// the XMP packet it carries, if any.
#[derive(Debug)]
pub struct Tiff {
    pub width: u32,
    pub height: u32,
    pub rgb: Vec<u16>,
    pub bits: u32,
    pub xmp: Option<String>,
}

/// Read `bytes` as a [`Tiff`]. The reader holds at most one image of its own size beside the file.
pub fn read(bytes: &[u8]) -> Result<Tiff, String> {
    let little = match bytes.get(..4) {
        Some([0x49, 0x49, 42, 0]) => true,
        Some([0x4d, 0x4d, 0, 42]) => false,
        _ => return Err("not a classic TIFF".into()),
    };
    let u16_at = |at: usize| -> Result<u32, String> {
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
    let u32_at = |at: usize| -> Result<u32, String> {
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
    let mut tags: HashMap<u32, Vec<u32>> = HashMap::new();
    let mut xmp = None;
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
        if tag == 700 {
            xmp = bytes
                .get(values_at..values_at + count)
                .map(|packet| String::from_utf8_lossy(packet).into_owned());
            continue;
        }
        let values: Vec<u32> = (0..count.min(1 << 20))
            .map(|i| match kind {
                3 => u16_at(values_at + 2 * i),
                4 => u32_at(values_at + 4 * i),
                _ => Ok(u32::from(*bytes.get(values_at + i).unwrap_or(&0))),
            })
            .collect::<Result<_, _>>()?;
        tags.insert(tag, values);
    }
    let one = |tag: u32, default: Option<u32>| -> Result<u32, String> {
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
    let pixels = width as usize * height as usize;
    let pixel_bytes = samples * bytes_per;
    let mut rgb = Vec::with_capacity(pixels * 3);
    let mut carry: Vec<u8> = Vec::new();
    for (offset, count) in offsets.iter().zip(counts) {
        let strip = bytes
            .get(*offset as usize..(*offset as usize + *count as usize))
            .ok_or("a strip lies outside the file")?;
        // A strip need not end on a pixel boundary; the remainder carries into the next.
        let mut data = std::mem::take(&mut carry);
        data.extend_from_slice(strip);
        let whole = data.len() / pixel_bytes * pixel_bytes;
        for pixel in data[..whole].chunks_exact(pixel_bytes) {
            if rgb.len() >= pixels * 3 {
                break;
            }
            for channel in 0..3 {
                let at = channel * bytes_per;
                rgb.push(if bytes_per == 1 {
                    u16::from(pixel[at]) * 257
                } else if little {
                    u16::from_le_bytes([pixel[at], pixel[at + 1]])
                } else {
                    u16::from_be_bytes([pixel[at], pixel[at + 1]])
                });
            }
        }
        carry = data[whole..].to_vec();
    }
    if rgb.len() < pixels * 3 {
        return Err("the strips hold fewer pixels than the image".into());
    }
    Ok(Tiff {
        width,
        height,
        rgb,
        bits,
        xmp,
    })
}

/// A little-endian, uncompressed 16-bit RGB TIFF of `rgb`, with `xmp` as its tag 700: what the
/// rig's tests write in place of Lightroom's exports.
#[cfg(test)]
pub fn write(width: u32, height: u32, rgb: &[u16], xmp: Option<&str>) -> Vec<u8> {
    let packet = xmp.unwrap_or_default().as_bytes();
    // A value of four bytes or fewer would sit in the entry itself; every XMP packet is longer.
    assert!(
        xmp.is_none() || packet.len() > 4,
        "an XMP packet longer than four bytes"
    );
    let entries: u32 = if xmp.is_some() { 10 } else { 9 };
    let data_at = 8 + 2 + entries * 12 + 4;
    let data_len = (rgb.len() * 2) as u32;
    let mut bytes = vec![0x49, 0x49, 42, 0];
    bytes.extend_from_slice(&8u32.to_le_bytes());
    bytes.extend_from_slice(&(entries as u16).to_le_bytes());
    let mut entry = |tag: u16, kind: u16, count: u32, value: u32| {
        bytes.extend_from_slice(&tag.to_le_bytes());
        bytes.extend_from_slice(&kind.to_le_bytes());
        bytes.extend_from_slice(&count.to_le_bytes());
        bytes.extend_from_slice(&value.to_le_bytes());
    };
    entry(256, 4, 1, width);
    entry(257, 4, 1, height);
    entry(258, 3, 1, 16);
    entry(259, 3, 1, 1);
    entry(262, 3, 1, 2);
    entry(273, 4, 1, data_at);
    entry(277, 3, 1, 3);
    entry(279, 4, 1, data_len);
    entry(284, 3, 1, 1);
    if xmp.is_some() {
        entry(700, 1, packet.len() as u32, data_at + data_len);
    }
    bytes.extend_from_slice(&[0; 4]);
    for value in rgb {
        bytes.extend_from_slice(&value.to_le_bytes());
    }
    bytes.extend_from_slice(packet);
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 16-bit uncompressed RGB TIFF of `pixels`, in either byte order, with an XMP packet.
    fn tiff(
        width: u16,
        height: u16,
        pixels: &[[u16; 3]],
        compression: u16,
        little: bool,
    ) -> Vec<u8> {
        let u16b = |v: u16| {
            if little {
                v.to_le_bytes()
            } else {
                v.to_be_bytes()
            }
        };
        let u32b = |v: u32| {
            if little {
                v.to_le_bytes()
            } else {
                v.to_be_bytes()
            }
        };
        let packet = b"<x:xmpmeta crs:ProcessVersion=\"11.0\"/>";
        let entries: u32 = 10;
        let data_at = 8 + 2 + entries * 12 + 4;
        let packet_at = data_at + (pixels.len() * 6) as u32;
        let mut bytes = if little {
            vec![0x49, 0x49, 42, 0]
        } else {
            vec![0x4d, 0x4d, 0, 42]
        };
        bytes.extend_from_slice(&u32b(8));
        bytes.extend_from_slice(&u16b(entries as u16));
        let short = |tag: u16, value: u16| {
            let mut entry = Vec::new();
            entry.extend_from_slice(&u16b(tag));
            entry.extend_from_slice(&u16b(3));
            entry.extend_from_slice(&u32b(1));
            entry.extend_from_slice(&u16b(value));
            entry.extend_from_slice(&[0, 0]);
            entry
        };
        let long = |tag: u16, kind: u16, count: u32, value: u32| {
            let mut entry = Vec::new();
            entry.extend_from_slice(&u16b(tag));
            entry.extend_from_slice(&u16b(kind));
            entry.extend_from_slice(&u32b(count));
            entry.extend_from_slice(&u32b(value));
            entry
        };
        for entry in [
            short(256, width),
            short(257, height),
            short(258, 16),
            short(259, compression),
            short(262, 2),
            long(273, 4, 1, data_at),
            short(277, 3),
            long(279, 4, 1, (pixels.len() * 6) as u32),
            short(284, 1),
            long(700, 1, packet.len() as u32, packet_at),
        ] {
            bytes.extend_from_slice(&entry);
        }
        bytes.extend_from_slice(&[0; 4]);
        for pixel in pixels {
            for value in pixel {
                bytes.extend_from_slice(&u16b(*value));
            }
        }
        bytes.extend_from_slice(packet);
        bytes
    }

    /// A 16-bit uncompressed RGB TIFF reads back exactly in either byte order, with its XMP; a
    /// compressed one and a non-TIFF are refused; and the test writer's files read back.
    #[test]
    fn an_uncompressed_tiff_reads_with_its_xmp_and_a_compressed_one_is_refused() {
        let pixels = [[0, 32768, 65535], [100, 200, 300]];
        for little in [true, false] {
            let read = read(&tiff(2, 1, &pixels, 1, little)).unwrap();
            assert_eq!((read.width, read.height, read.bits), (2, 1, 16));
            assert_eq!(read.rgb, vec![0, 32768, 65535, 100, 200, 300]);
            assert!(read.xmp.unwrap().contains("ProcessVersion=\"11.0\""));
        }
        assert!(
            read(&tiff(2, 1, &pixels, 8, true))
                .unwrap_err()
                .contains("compression None")
        );
        assert!(read(b"not a tiff").is_err());
        let written = read(&write(1, 2, &[1, 2, 3, 4, 5, 6], Some("<x:xmpmeta/>"))).unwrap();
        assert_eq!((written.width, written.height), (1, 2));
        assert_eq!(written.rgb, vec![1, 2, 3, 4, 5, 6]);
        assert_eq!(written.xmp.as_deref(), Some("<x:xmpmeta/>"));
    }
}
