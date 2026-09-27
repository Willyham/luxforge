//! The JPEG container before the first scan, walked without decoding or allocating from declared
//! dimensions: the marker segments ([`segments`]) and the frame they declare ([`header`]).
//!
//! Markers are found as libjpeg's `next_marker` finds them, so this walk and libjpeg read the same
//! segments: `0xFF` fill bytes before a marker are skipped, and so are bytes between two segments
//! that are not a marker (libjpeg's `JWRN_EXTRANEOUS_DATA` there, which the decoder accepts); a
//! stuffed `FF 00` is one of those. `TEM` and `RST0` to `RST7` carry no length.

use crate::JpegError;

const SOI: [u8; 2] = [0xff, 0xd8];
const EOI: [u8; 2] = [0xff, 0xd9];
const SOS: u8 = 0xda;
const EOI_MARKER: u8 = 0xd9;

/// What the frame header declares, read before libjpeg sees the file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Header {
    pub width: u32,
    pub height: u32,
    /// The frame's component count as declared: 1 for greyscale, 3 for colour, anything else a
    /// colour space the decoder refuses.
    pub components: u8,
}

/// One marker segment before the first scan: its marker code and the payload after the length.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Segment<'a> {
    pub marker: u8,
    pub payload: &'a [u8],
}

/// The marker segments of `bytes` from SOI to the first SOS or EOI, in file order. It ends there,
/// or with one error where the container cannot be walked: no SOI, a marker or a length past the
/// end, or a length that does not fit.
pub fn segments(bytes: &[u8]) -> Segments<'_> {
    Segments {
        bytes,
        at: 0,
        done: false,
    }
}

/// The iterator [`segments`] returns.
pub struct Segments<'a> {
    bytes: &'a [u8],
    at: usize,
    done: bool,
}

impl<'a> Segments<'a> {
    fn byte(&mut self) -> Result<u8, JpegError> {
        let byte = *self
            .bytes
            .get(self.at)
            .ok_or_else(|| JpegError::Malformed("marker".into()))?;
        self.at += 1;
        Ok(byte)
    }

    /// The next marker code, as libjpeg's `next_marker` reads it.
    fn marker(&mut self) -> Result<u8, JpegError> {
        loop {
            let mut byte = self.byte()?;
            while byte != 0xff {
                byte = self.byte()?;
            }
            while byte == 0xff {
                byte = self.byte()?;
            }
            if byte != 0 {
                return Ok(byte);
            }
        }
    }

    fn step(&mut self) -> Result<Option<Segment<'a>>, JpegError> {
        if self.at == 0 {
            if !self.bytes.starts_with(&SOI) {
                return Err(JpegError::Malformed("missing JPEG SOI".into()));
            }
            self.at = 2;
        }
        loop {
            let marker = self.marker()?;
            match marker {
                SOS | EOI_MARKER => return Ok(None),
                0x01 | 0xd0..=0xd7 => continue,
                _ => {}
            }
            let length = self
                .bytes
                .get(self.at..self.at + 2)
                .ok_or_else(|| JpegError::Malformed("segment length".into()))?;
            let length = usize::from(u16::from_be_bytes([length[0], length[1]]));
            if length < 2 || self.at + length > self.bytes.len() {
                return Err(JpegError::Malformed("segment bounds".into()));
            }
            let payload = &self.bytes[self.at + 2..self.at + length];
            self.at += length;
            return Ok(Some(Segment { marker, payload }));
        }
    }
}

impl<'a> Iterator for Segments<'a> {
    type Item = Result<Segment<'a>, JpegError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.done {
            return None;
        }
        let step = self.step();
        self.done = !matches!(step, Ok(Some(_)));
        step.transpose()
    }
}

/// The frame `bytes` declares: a file from SOI to a final EOI whose first frame header before the
/// first scan is baseline, extended-sequential or progressive Huffman (SOF0 to SOF2) at 8-bit
/// precision. Any other frame type, or none, is [`JpegError::FrameType`].
pub fn header(bytes: &[u8]) -> Result<Header, JpegError> {
    if !bytes.starts_with(&SOI) || !bytes.ends_with(&EOI) {
        return Err(JpegError::Malformed("missing JPEG SOI/EOI".into()));
    }
    for segment in segments(bytes) {
        let Segment { marker, payload } = segment?;
        if (0xc0..=0xc2).contains(&marker) {
            let [8, h0, h1, w0, w1, components, ..] = *payload else {
                return Err(JpegError::Precision);
            };
            return Ok(Header {
                width: u32::from(u16::from_be_bytes([w0, w1])),
                height: u32::from(u16::from_be_bytes([h0, h1])),
                components,
            });
        }
    }
    Err(JpegError::FrameType)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> Vec<u8> {
        std::fs::read(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../fixtures/s0")
                .join(name),
        )
        .unwrap()
    }

    #[test]
    fn malformed_headers_never_panic_or_allocate_from_dimensions() {
        let valid = fixture("orientation-1.jpg");
        for end in 0..valid.len().min(1024) {
            assert!(header(&valid[..end]).is_err());
            // A walk of any prefix ends, whatever it finds, with at most one error.
            let walked: Vec<_> = segments(&valid[..end]).collect();
            assert!(walked.iter().filter(|segment| segment.is_err()).count() <= 1);
        }
        for size in [0u16, 1, 2, 7, u16::MAX] {
            let mut bytes = vec![0xff, 0xd8, 0xff, 0xc0];
            bytes.extend(size.to_be_bytes());
            bytes.extend([8, 0xff, 0xff, 0xff, 0xff, 3, 0xff, 0xd9]);
            let _ = header(&bytes);
        }
    }

    #[test]
    fn the_header_is_the_first_frame_and_its_refusals_are_typed() {
        assert_eq!(
            header(&fixture("orientation-1.jpg")).unwrap(),
            Header {
                width: 480,
                height: 320,
                components: 3
            }
        );
        assert_eq!(header(&fixture("greyscale.jpg")).unwrap().components, 1);
        assert_eq!(header(&fixture("cmyk.jpg")).unwrap().components, 4);
        let frame = |marker: u8, precision: u8| {
            let mut bytes = vec![0xff, 0xd8, 0xff, marker, 0, 8, precision, 0, 1, 0, 1, 3];
            bytes.extend([0xff, 0xda, 0, 2, 0xff, 0xd9]);
            bytes
        };
        assert!(matches!(
            header(&frame(0xc0, 12)),
            Err(JpegError::Precision)
        ));
        // Lossless and arithmetic-coded frames are skipped, so none is found before the scan.
        for marker in [0xc3, 0xc9, 0xca] {
            assert!(matches!(
                header(&frame(marker, 8)),
                Err(JpegError::FrameType)
            ));
        }
        assert!(matches!(
            header(b"not a jpeg"),
            Err(JpegError::Malformed(_))
        ));
    }

    /// Fill bytes, bytes between segments that are not a marker, a stuffed `FF 00` among them and
    /// markers without a length are passed over as libjpeg passes over them.
    #[test]
    fn segments_are_found_as_libjpeg_finds_them() {
        let bytes = [
            0xff, 0xd8, // SOI
            0xff, 0xff, 0xe0, 0, 4, 1, 2, // fill, then APP0 with two bytes
            9, 9, 0xff, 0x00, 7, // not a marker, with a stuffed FF 00
            0xff, 0xd0, // RST0, no length
            0xff, 0xe1, 0, 2, // an empty APP1
            0xff, 0xda, 0, 2, 0xff, 0xd9, // SOS ends the walk
        ];
        let found: Vec<_> = segments(&bytes).collect::<Result<_, _>>().unwrap();
        assert_eq!(
            found,
            [
                Segment {
                    marker: 0xe0,
                    payload: &[1, 2]
                },
                Segment {
                    marker: 0xe1,
                    payload: &[]
                }
            ]
        );
        let cut = [0xff, 0xd8, 0xff, 0xe0, 0, 9, 1];
        let walked: Vec<_> = segments(&cut).collect();
        assert!(matches!(walked.as_slice(), [Err(JpegError::Malformed(_))]));
    }
}
