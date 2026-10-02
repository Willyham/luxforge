//! Which decoder fills a recording mode's sensor mosaic, and the one table of LibRaw decoders that
//! RawSpeed may replace ([RawSpeed unpacking](../../../docs/design/rawspeed-unpack.md)).
//!
//! [`REPLACEABLE`] is the single source of truth. The build script includes this file, validates
//! every catalog mode's `unpacker` against the table, and writes the table into the native
//! adapter's generated `rawspeed_decoders.h`, so the catalog check and the native substitution
//! read the same rows. A crate test proves the generated header is this table.
//!
//! Each row's rule is a property of the LibRaw 0.22.2 decoder being replaced, decided from its
//! source, cited on the row. RawSpeed is configured to return uncorrected values
//! (`uncorrectedRawValues`), so where LibRaw writes `curve[value]` into the mosaic the adapter
//! writes RawSpeed's value through the same `curve`, and where LibRaw writes the decoded value
//! the adapter writes RawSpeed's unchanged. A guard names what else the LibRaw decoder does to
//! the samples or their placement that RawSpeed does not; a file on which a guard fails is
//! refused before unpack, never approximated.
use serde::Deserialize;

/// The decoder that fills a mode's mosaic. Omitted in the catalog, a mode uses LibRaw's own.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Unpacker {
    #[default]
    Libraw,
    Rawspeed,
    JxlOxide,
}

/// What the replaced LibRaw decoder writes for each decoded value `v`. The native adapter applies
/// it to RawSpeed's values with LibRaw's `curve` as identify left it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub(crate) enum Curve {
    /// `v` itself: the decoder never reads `curve`.
    Unchanged = 0,
    /// `curve[v]`.
    Libraw = 1,
    /// `curve[min(v, 0x3fff)]`. LibRaw's Nikon decoder clamps its signed predictor to
    /// `0..=0x3fff` before the lookup; RawSpeed clamps the same predictor to `0..=0x7fff`, so
    /// the two agree for every predictor in `-0x8000..0x8000`.
    LibrawClamped14 = 2,
}

/// A precondition on LibRaw's identify-time state for the replacement to be exact, beyond the
/// ones every row shares (a one-channel CFA mosaic, no Fujifilm SuperCCD rotation, a row pitch of
/// exactly `raw_width` samples and a RawSpeed image of exactly `raw_width` × `raw_height`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub(crate) enum Guard {
    None = 0,
    /// LibRaw decodes rows `0..height` only, leaving the rest of the buffer unwritten: require
    /// `height == raw_height`.
    NikonRows = 1,
    /// LibRaw interleaves rows (`load_flags & 1`) or shifts every sample two columns when
    /// `raw_width == 3984`: require neither.
    LosslessJpegPlacement = 2,
    /// LibRaw reads the frame `dng_frames[shot_select]` names and, for two samples per pixel,
    /// skips one: require one sample per pixel and one raw frame.
    DngSingleFrame = 3,
    /// LibRaw decodes whole 16-row strips of whole 11- or 14-pixel blocks and leaves any
    /// remainder unwritten: require `raw_height` and `raw_width` to be whole multiples.
    PanasonicC6Blocks = 4,
}

/// One LibRaw decoder RawSpeed may replace.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Replaceable {
    /// LibRaw's exact decoder name, as `get_decoder_info` reports it and the catalog records it.
    pub decoder: &'static str,
    pub curve: Curve,
    pub guard: Guard,
}

/// The replaceable decoders. Line references are to `vendor/libraw-0.22.2/src/decoders/`.
pub(crate) const REPLACEABLE: [Replaceable; 10] = [
    // decoders_dcraw.cpp:941 `RAW(row, col) = curve[LIM((short)hpred[col & 1], 0, 0x3fff)]`,
    // over rows `0..height` (:918).
    Replaceable {
        decoder: "nikon_load_raw()",
        curve: Curve::LibrawClamped14,
        guard: Guard::NikonRows,
    },
    // decoders_dcraw.cpp:574 `val = curve[*rp++]`; row interleave :570, 3984 shift :588.
    Replaceable {
        decoder: "lossless_jpeg_load_raw()",
        curve: Curve::Libraw,
        guard: Guard::LosslessJpegPlacement,
    },
    // fuji_compressed.cpp:362 and :416 copy the decoded line buffers into `raw_image`.
    Replaceable {
        decoder: "fuji_compressed_load_raw()",
        curve: Curve::Unchanged,
        guard: Guard::None,
    },
    // olympus14.cpp:314 `raw_image[row * raw_width + col] = (ushort)pixel_final_value`.
    Replaceable {
        decoder: "olympus_load_raw()",
        curve: Curve::Unchanged,
        guard: Guard::None,
    },
    // decoders_dcraw.cpp:833 `RAW(row, col) = hpred[col & 1]`.
    Replaceable {
        decoder: "pentax_load_raw()",
        curve: Curve::Unchanged,
        guard: Guard::None,
    },
    // dng.cpp:43 `adobe_copy_pixel` writes `curve[**rp]` (the DNG LinearizationTable); frame
    // selection :64, second sample :38.
    Replaceable {
        decoder: "lossless_dng_load_raw()",
        curve: Curve::Libraw,
        guard: Guard::DngSingleFrame,
    },
    // dng.cpp:167 and decoders_libraw_dcrdefs.cpp:66 through `adobe_copy_pixel` (dng.cpp:43);
    // frame selection dng.cpp:150.
    Replaceable {
        decoder: "packed_dng_load_raw()",
        curve: Curve::Libraw,
        guard: Guard::DngSingleFrame,
    },
    // decoders_dcraw.cpp:1113-1150 (encoding 5) and :1180 `RAW(row, col) = pred[col & 1]`.
    Replaceable {
        decoder: "panasonic_load_raw()",
        curve: Curve::Unchanged,
        guard: Guard::None,
    },
    // decoders_libraw.cpp:525-529 write the block's pixels; whole strips :473, blocks :457.
    Replaceable {
        decoder: "panasonicC6_load_raw()",
        curve: Curve::Unchanged,
        guard: Guard::PanasonicC6Blocks,
    },
    // pana8.cpp:324-338 write the decoded pixels, through the file's own gamma table only when
    // it is not the identity (:196, :390-398). RawSpeed refuses any gamma parameters but the
    // identity and any clip value but 0xffff (Rw2Decoder.cpp:138, :202-208), under which
    // LibRaw's `gammaCurve` (pana8.cpp:447) is the identity too, so a RawSpeed decode is always
    // one where LibRaw writes the decoded values unchanged.
    Replaceable {
        decoder: "panasonicC8_load_raw()",
        curve: Curve::Unchanged,
        guard: Guard::None,
    },
];

/// The row for LibRaw decoder `decoder`, if RawSpeed may replace it.
pub(crate) fn replaceable(decoder: &str) -> Option<&'static Replaceable> {
    REPLACEABLE.iter().find(|row| row.decoder == decoder)
}

/// The generated native header: the curve and guard enumerations and the table as a C array.
/// The build script writes it; a crate test compares it with the header the build wrote.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn native_header() -> String {
    let mut out = String::from(
        "// Generated from src/unpacker.rs; do not edit.\n#pragma once\n#include <cstdint>\n\n\
         enum LfCurve : uint32_t {\n",
    );
    for (name, value) in [
        ("UNCHANGED", Curve::Unchanged),
        ("LIBRAW", Curve::Libraw),
        ("LIBRAW_CLAMPED_14", Curve::LibrawClamped14),
    ] {
        out.push_str(&format!("  LF_CURVE_{name} = {},\n", value as u32));
    }
    out.push_str("};\n\nenum LfGuard : uint32_t {\n");
    for (name, value) in [
        ("NONE", Guard::None),
        ("NIKON_ROWS", Guard::NikonRows),
        ("LOSSLESS_JPEG_PLACEMENT", Guard::LosslessJpegPlacement),
        ("DNG_SINGLE_FRAME", Guard::DngSingleFrame),
        ("PANASONIC_C6_BLOCKS", Guard::PanasonicC6Blocks),
    ] {
        out.push_str(&format!("  LF_GUARD_{name} = {},\n", value as u32));
    }
    out.push_str(
        "};\n\nstruct LfReplaceable { const char *decoder; LfCurve curve; LfGuard guard; };\n\
         static const LfReplaceable lf_replaceable[] = {\n",
    );
    for row in REPLACEABLE {
        out.push_str(&format!(
            "  {{\"{}\", LfCurve({}), LfGuard({})}},\n",
            row.decoder, row.curve as u32, row.guard as u32
        ));
    }
    out.push_str("};\n");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The header the native adapter compiled is this table, row for row.
    #[test]
    fn native_table_is_generated_from_this_table() {
        let built = include_str!(concat!(env!("OUT_DIR"), "/rawspeed_decoders.h"));
        assert_eq!(built, native_header());
        for row in REPLACEABLE {
            assert!(
                built.contains(&format!(
                    "{{\"{}\", LfCurve({}), LfGuard({})}}",
                    row.decoder, row.curve as u32, row.guard as u32
                )),
                "{row:?}"
            );
        }
        assert_eq!(built.matches("\", LfCurve(").count(), REPLACEABLE.len());
    }

    #[test]
    fn replaceable_decoders_are_unique_and_named_as_libraw_names_them() {
        for (index, row) in REPLACEABLE.iter().enumerate() {
            assert!(row.decoder.ends_with("_load_raw()"), "{row:?}");
            assert!(
                REPLACEABLE[..index]
                    .iter()
                    .all(|other| other.decoder != row.decoder),
                "{row:?}"
            );
            assert_eq!(replaceable(row.decoder), Some(row));
        }
        assert_eq!(replaceable("unpacked_load_raw()"), None);
        assert_eq!(replaceable("crxLoadRaw()"), None);
        assert_eq!(replaceable("nikon_load_raw"), None);
    }
}
