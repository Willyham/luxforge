//! The numerical studies: each proves one reference's own properties, and those that freeze a
//! committed fixture corpus generate it (ignored) and reload it on every run. None of them touches
//! the core; the core's tests check production against the references and these corpora.
//!
//! One binary, one module per study, so a study is filtered by its module name: `cargo test -p
//! luxforge-reference --test studies tone::`.

mod colour;
mod colour_visual;
mod exposure;
mod mixer;
mod presence;
mod tone;
mod vignette;
mod white_balance;
mod white_balance_visual;

/// A JSON text round trip of a committed fixture against a fresh computation: `1e-12` relative is
/// four orders of magnitude tighter than the production-against-reference tolerances the fixtures
/// police, so it catches any real staleness while tolerating a parser landing one ULP away.
const FIXTURE_ROUND_TRIP_TOLERANCE: f64 = 1e-12;

fn approximately_equal(a: f64, b: f64) -> bool {
    (a - b).abs() <= FIXTURE_ROUND_TRIP_TOLERANCE + FIXTURE_ROUND_TRIP_TOLERANCE * b.abs()
}
