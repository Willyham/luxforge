//! What each libjpeg warning means for the pixels a decode returns, in one table.
//!
//! libjpeg warns and carries on: past corrupt or missing data it fills with zeros or grey, and past
//! an irregularity in the file's structure it decodes the image as written. A decode goes on past a
//! warning only when every coefficient of the image is decoded as the file holds it and nothing is
//! guessed; otherwise it stops. The codes are those of this build: `mozjpeg-sys` 2.2.3's
//! libjpeg-turbo at `JPEG_LIB_VERSION` 62, without arithmetic coding (so `JWRN_ARITH_BAD_CODE` does
//! not exist here: an arithmetic-coded file is a fatal error instead) or `jdicc.c`, reading through
//! `mozjpeg` 0.10.13's own source manager, or libjpeg's memory source (`jdatasrc.c`) for a region
//! decode. Each reason names the source that emits the code. A code not listed refuses.

use mozjpeg_sys::{
    JWRN_ADOBE_XFORM, JWRN_BOGUS_ICC, JWRN_BOGUS_PROGRESSION, JWRN_EXTRANEOUS_DATA,
    JWRN_HIT_MARKER, JWRN_HUFF_BAD_CODE, JWRN_JFIF_MAJOR, JWRN_JPEG_EOF, JWRN_MUST_RESYNC,
    JWRN_NOT_SEQUENTIAL, JWRN_TOO_MUCH_DATA,
};
use std::os::raw::c_int;

/// Whether a decode goes on past a warning.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Verdict {
    /// The image data is complete and decoded as the file holds it.
    Accept,
    /// Accepted while libjpeg reads the header, before any scan's data; refused after it.
    AcceptInHeader,
    /// Image data would be missing, corrupt or guessed.
    Refuse,
}

pub(crate) struct Warning {
    pub(crate) code: c_int,
    pub(crate) name: &'static str,
    pub(crate) verdict: Verdict,
}

use Verdict::{Accept, AcceptInHeader, Refuse};

/// Every warning this build's decoder can emit, and `JWRN_BOGUS_ICC`, which it cannot, each with
/// the reason for its verdict.
#[rustfmt::skip]
pub(crate) const WARNINGS: [Warning; 11] = [
    // `mozjpeg`'s source manager (readsrc.rs) and libjpeg's memory source (jdatasrc.c): the data
    // ends before libjpeg has found EOI; it fakes one, and fills whatever image data was still to
    // come.
    Warning { code: JWRN_JPEG_EOF, name: "JWRN_JPEG_EOF", verdict: Refuse },
    // jdhuff.c: a scan's data ends at a marker before its last block; libjpeg fills the rest with
    // zero bits.
    Warning { code: JWRN_HIT_MARKER, name: "JWRN_HIT_MARKER", verdict: Refuse },
    // jdhuff.c, jdphuff.c: a code no Huffman table holds; libjpeg zeroes the coefficient and
    // decodes on out of step.
    Warning { code: JWRN_HUFF_BAD_CODE, name: "JWRN_HUFF_BAD_CODE", verdict: Refuse },
    // jdmarker.c: a restart marker is missing or out of order; libjpeg skips data or zero-fills an
    // interval to resynchronize.
    Warning { code: JWRN_MUST_RESYNC, name: "JWRN_MUST_RESYNC", verdict: Refuse },
    // jdphuff.c: a progressive scan does not follow the scans before it, so coefficients lack bits
    // or are refined from nothing.
    Warning { code: JWRN_BOGUS_PROGRESSION, name: "JWRN_BOGUS_PROGRESSION", verdict: Refuse },
    // jdhuff.c: a sequential scan declares a partial band; libjpeg decodes whole blocks anyway,
    // which is right only if the encoder wrote whole blocks, and nothing shows that it did.
    Warning { code: JWRN_NOT_SEQUENTIAL, name: "JWRN_NOT_SEQUENTIAL", verdict: Refuse },
    // jdapimin.c: the Adobe marker names an unknown colour transform and libjpeg guesses YCbCr, so
    // the colours would be a guess.
    Warning { code: JWRN_ADOBE_XFORM, name: "JWRN_ADOBE_XFORM", verdict: Refuse },
    // jdapistd.c: rows asked for past the image, which libjpeg answers with none; the decoders
    // never ask, so this would be their own bug.
    Warning { code: JWRN_TOO_MUCH_DATA, name: "JWRN_TOO_MUCH_DATA", verdict: Refuse },
    // jdmarker.c: bytes that are not a marker, skipped. Before the first scan they sit between
    // marker segments and cannot be image data; after a scan they are also what a decode that lost
    // step in corrupt data leaves over, so there they refuse.
    Warning { code: JWRN_EXTRANEOUS_DATA, name: "JWRN_EXTRANEOUS_DATA", verdict: AcceptInHeader },
    // jdmarker.c: the JFIF marker's major version is not 1; the marker carries only the pixel
    // density and a thumbnail, and libjpeg reads the frame as for version 1.
    Warning { code: JWRN_JFIF_MAJOR, name: "JWRN_JFIF_MAJOR", verdict: Accept },
    // jdicc.c, which this build does not compile: its ICC reader's chunk check. The chunks are
    // metadata, which this crate reassembles itself, refusing a broken sequence as a profile.
    Warning { code: JWRN_BOGUS_ICC, name: "JWRN_BOGUS_ICC", verdict: Accept },
];

fn find(code: c_int) -> Option<&'static Warning> {
    WARNINGS.iter().find(|warning| warning.code == code)
}

/// Whether a decode goes on past warning `code`, raised while libjpeg reads the header or after.
pub(crate) fn accepts(code: c_int, in_header: bool) -> bool {
    match find(code).map(|warning| warning.verdict) {
        Some(Accept) => true,
        Some(AcceptInHeader) => in_header,
        Some(Refuse) | None => false,
    }
}

/// libjpeg's name for warning `code`, when the table lists it.
pub(crate) fn name(code: c_int) -> Option<&'static str> {
    find(code).map(|warning| warning.name)
}
