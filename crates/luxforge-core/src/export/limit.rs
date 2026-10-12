//! Limit file size: the highest quality, at most the one asked for, whose file fits a byte limit,
//! found by bisection over whole encodes of a held frame. The search knows nothing of the format;
//! JPEG ([`super::encode::encode_jpeg_limited`]) and WebP each give it their encode.
//!
//! Contract (`docs/design/export-settings.md`, encoding and bounds):
//! - The requested quality is tried first. If its file does not fit, the qualities from 10 (or the
//!   requested one, if lower) up to one below it are bisected, the higher middle first, keeping the
//!   highest that fits: at most eight encodes for any quality up to 100, and the exact highest
//!   fitting quality when the file's size grows with quality.
//! - Attempts run one after another and each one's encoder is released before the next starts, so
//!   the search holds the encoder's input, one candidate and the best result so far, each candidate
//!   at most the limit when its encode stops at the limit.
//! - Each attempt is reported as it starts, and its encode's progress within it, as one fraction
//!   of the whole search that never decreases and ends at 1. `cancel` is checked before every
//!   attempt; the encode checks it within.
//! - When nothing fits at the lowest quality tried, the search refuses with `validation` and
//!   `data.code` [`LIMIT_UNREACHABLE`], naming the limit and that quality, and answers no bytes.
// `export.write` searches through here once the export's settings arrive
// (`tasks/rendering/export-settings.json`); until then only the tests call it.
#![cfg_attr(not(test), allow(dead_code))]

use crate::Error;
use serde_json::json;

/// The lowest quality a search tries.
pub(crate) const MIN_QUALITY: u8 = 10;

/// The most encodes one search makes.
pub(crate) const MAX_ATTEMPTS: u32 = 8;

/// `data.code` of the refusal when nothing fits at the lowest quality.
pub(crate) const LIMIT_UNREACHABLE: &str = "limit-unreachable";

/// Where a search is: the `attempt` under way (from 1) at `quality`, and the fraction of the
/// whole search done.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct LimitProgress {
    pub(crate) attempt: u32,
    pub(crate) quality: u8,
    pub(crate) fraction: f64,
}

/// A search's result: the file, the quality it was encoded at and the encodes made.
#[derive(Debug)]
pub(crate) struct Limited {
    pub(crate) bytes: Vec<u8>,
    pub(crate) quality: u8,
    pub(crate) attempts: u32,
}

/// One attempt's whole encode at a quality, reporting its own fraction done: the file, or `None`
/// when it stopped because the file passed the limit.
pub(crate) type Attempt<'a> =
    dyn FnMut(u8, &mut dyn FnMut(f64)) -> Result<Option<Vec<u8>>, Error> + 'a;

/// The most attempts bisecting the qualities `low..=high` can still take, each halving them.
fn bisections(low: u8, high: u8) -> u32 {
    if low > high {
        return 0;
    }
    let candidates = u32::from(high - low) + 1;
    (candidates + 1).next_power_of_two().trailing_zeros()
}

/// Find the highest quality, at most `quality` (clamped to 1–100), whose file is at most
/// `limit_bytes`. `encode(quality, progress)` makes one whole encode, reporting its own fraction
/// done to `progress`, and answers the file, or `None` when it stopped because the file passed the
/// limit; an error from it ends the search as it is. Answers the best file with its quality and the
/// encodes made, or the `limit-unreachable` refusal.
pub(crate) fn search(
    limit_bytes: u64,
    quality: u8,
    encode: &mut Attempt<'_>,
    progress: &mut dyn FnMut(LimitProgress),
    cancel: &dyn Fn() -> Result<(), Error>,
) -> Result<Limited, Error> {
    let quality = quality.clamp(1, 100);
    let lowest = MIN_QUALITY.min(quality);
    // The qualities still possible, and the highest that fitted.
    let (mut low, mut high) = (lowest, quality);
    let mut best: Option<(u8, Vec<u8>)> = None;
    let mut attempts = 0;
    let mut reported = 0.0_f64;
    while low <= high && attempts < MAX_ATTEMPTS {
        cancel()?;
        // The requested quality first, then the higher middle of what is left.
        let (candidate, planned) = if attempts == 0 {
            (high, 1 + bisections(low, high - 1))
        } else {
            (
                low + (high - low).div_ceil(2),
                attempts + bisections(low, high),
            )
        };
        attempts += 1;
        let done = f64::from(attempts - 1);
        let planned = f64::from(planned.max(attempts));
        reported = reported.max(done / planned);
        progress(LimitProgress {
            attempt: attempts,
            quality: candidate,
            fraction: reported,
        });
        let file = encode(candidate, &mut |within| {
            reported = reported.max((done + within.clamp(0.0, 1.0)) / planned);
            progress(LimitProgress {
                attempt: attempts,
                quality: candidate,
                fraction: reported,
            });
        })?;
        match file {
            Some(bytes) if bytes.len() as u64 <= limit_bytes => {
                best = Some((candidate, bytes));
                low = candidate + 1;
            }
            // Dropped here, before the next attempt starts.
            _ => high = candidate - 1,
        }
    }
    let Some((quality, bytes)) = best else {
        return Err(Error::validation(format!(
            "no quality from {lowest} fits the size limit of {limit_bytes} bytes"
        ))
        .with_data(json!({
            "code": LIMIT_UNREACHABLE,
            "limit_bytes": limit_bytes,
            "quality": lowest,
        })));
    };
    progress(LimitProgress {
        attempt: attempts,
        quality,
        fraction: 1.0,
    });
    Ok(Limited {
        bytes,
        quality,
        attempts,
    })
}

#[cfg(test)]
#[path = "limit_tests.rs"]
mod tests;
